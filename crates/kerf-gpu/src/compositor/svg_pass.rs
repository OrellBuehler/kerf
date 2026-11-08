//! The pass that lays a floating animated SVG title on the composite (`svg.wgsl`; the arithmetic and what it was held to are in
//! [`crate::svg`]): the title's picture is FFmpeg's own conversion of the PNG resvg drew ([`crate::svg::make`], kept in the
//! [`PictureCache`]), uploaded as four planes, and blended over a copy of the canvas with `overlay`'s integers.

use super::*;
use crate::svg::{self, Picture, PictureCache, Weighed};
use kerf_core::SvgRect;

/// `svg.wgsl`'s uniform.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SvgParams {
    origin: [i32; 2],
    size: [i32; 2],
    csize: [i32; 2],
    pad: [i32; 2],
}

/// The pipeline of a title, and the pictures it has been asked for.
pub(super) struct SvgPass {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    cache: PictureCache<Resident>,
}

/// A converted picture and, once a frame has drawn it, its planes on the device: a title that holds still is uploaded once, not by every
/// frame it is on. The cache charges both copies.
pub(super) struct Resident {
    picture: Picture,
    planes: OnceLock<[Plane; 4]>,
}

impl Resident {
    fn new(picture: Picture) -> Self {
        Self {
            picture,
            planes: OnceLock::new(),
        }
    }
}

impl Weighed for Resident {
    fn bytes(&self) -> usize {
        2 * self.picture.bytes()
    }
}

/// A title of a frame with the picture it is drawn from.
pub(super) struct PreparedSvg {
    rect: SvgRect,
    picture: Arc<Resident>,
}

impl SvgPass {
    pub(super) fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kerf svg"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/svg.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("svg"),
            entries: &[
                uniform_entry(0),
                texture_entry(1),
                texture_entry(2),
                texture_entry(3),
                texture_entry(4),
                texture_entry(5),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("svg"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("svg"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_svg"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: CANVAS_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            layout,
            pipeline,
            cache: PictureCache::default(),
        }
    }
}

impl Compositor {
    /// The pictures of the floating titles on `plan`'s frame at `size`, made (a resvg draw, a PNG and one `ffmpeg` child for each the
    /// cache does not hold) or served from the cache. A title with no rectangle on the frame (wholly off it, or too large to draw) is
    /// left out of the export's graph and is left out here. Blocks for the child, so this is for a blocking thread with no lock held that
    /// anything else waits on.
    pub(super) fn prepare_svg(&self, plan: &RenderPlan, size: (u32, u32)) -> Result<Vec<PreparedSvg>, GpuError> {
        let limit = self.gpu.device.limits().max_texture_dimension_2d;
        let mut out = Vec::new();
        for s in &plan.svg {
            let Some(title) = &s.title else {
                return Err(GpuError::Unsupported(
                    "a title clip is a clip chain, which no pass draws".into(),
                ));
            };
            let Some(rect) = s.rect(size) else {
                continue;
            };
            if rect.w.max(rect.h) > limit {
                return Err(GpuError::Unsupported(format!(
                    "a {}x{} floating title is over this device's {limit}px texture limit",
                    rect.w, rect.h
                )));
            }
            let text = title
                .text()
                .map_err(|e| GpuError::Unsupported(format!("a floating title cannot be read: {e}")))?;
            let picture = self.svg.cache.get(&svg::cache_key(title, &text, rect), || {
                svg::make(title, &text, rect).map(Resident::new)
            })?;
            out.push(PreparedSvg { rect, picture });
        }
        if !out.is_empty() && plan.layers.is_empty() {
            return Err(GpuError::Unsupported(
                "a floating title over a frame with no layer has no YUV composite to be blended into".into(),
            ));
        }
        Ok(out)
    }

    /// Have the floating titles of `plan` converted and held, drawing nothing: a picture the cache lacks costs a resvg draw and an
    /// `ffmpeg` child (tens of milliseconds), which a playing frame must not wait for. A plan with none costs nothing.
    pub fn warm_svg(&self, plan: &RenderPlan, size: (u32, u32)) -> Result<(), GpuError> {
        if plan.svg.is_empty() {
            return Ok(());
        }
        self.prepare_svg(plan, size).map(|_| ())
    }

    /// Pin whether this FFmpeg's `overlay` of a title is the model, instead of what [`prepare`] measured ([`crate::svg::measured`]; a
    /// render never runs the probe): for a host that knows its ffmpeg, and for tests. Anything but `Match` takes the titles out of
    /// [`Compositor::caps`].
    #[must_use]
    pub fn with_svg(mut self, overlay: crate::svg::Overlay) -> Self {
        self.svg_pin = Some(overlay);
        self
    }

    /// Lay each of `svgs` on `canvas` (the composite's Y, U, V in its channels), in order: one pass each, reading a copy of the canvas
    /// as the one before left it. `before` is the copy the layers made, made here if none did.
    pub(super) fn draw_svg(
        &self,
        enc: &mut wgpu::CommandEncoder,
        canvas: &Plane,
        before: &mut Option<Plane>,
        svgs: &[PreparedSvg],
    ) {
        for PreparedSvg { rect, picture: resident } in svgs {
            let picture = &resident.picture;
            // The filter rounds the position down to even; the rest is the picture's own grid.
            let (x, y) = (rect.x & !1, rect.y & !1);
            let (cw, ch) = picture.chroma_size();
            // The pixels it can touch: its luma, and the whole chroma blocks it ends in (an odd picture's last block reaches one pixel
            // past it), on the canvas.
            let x0 = x.max(0);
            let y0 = y.max(0);
            let x1 = (x + 2 * i64::from(cw)).min(i64::from(canvas.w));
            let y1 = (y + 2 * i64::from(ch)).min(i64::from(canvas.h));
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            let copy = before.get_or_insert_with(|| {
                self.plane(
                    "canvas before",
                    canvas.w,
                    canvas.h,
                    CANVAS_FORMAT,
                    wgpu::TextureUsages::COPY_DST,
                )
            });
            let origin = wgpu::Origin3d {
                x: x0 as u32,
                y: y0 as u32,
                z: 0,
            };
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &canvas.texture,
                    mip_level: 0,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &copy.texture,
                    mip_level: 0,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                extent((x1 - x0) as u32, (y1 - y0) as u32),
            );
            let planes = resident.planes.get_or_init(|| {
                [
                    self.upload("svg y", picture.w, picture.h, &picture.y),
                    self.upload("svg u", cw, ch, &picture.u),
                    self.upload("svg v", cw, ch, &picture.v),
                    self.upload("svg a", picture.w, picture.h, &picture.a),
                ]
            });
            let params = SvgParams {
                // (Within the texture limit, so within i32.)
                origin: [x as i32, y as i32],
                size: [picture.w as i32, picture.h as i32],
                csize: [cw as i32, ch as i32],
                pad: [0; 2],
            };
            let buf = self.gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("svg params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let texture = |binding: u32, view| wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::TextureView(view),
            };
            let bind = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("svg"),
                layout: &self.svg.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    },
                    texture(1, &copy.view),
                    texture(2, &planes[0].view),
                    texture(3, &planes[1].view),
                    texture(4, &planes[2].view),
                    texture(5, &planes[3].view),
                ],
            });
            self.pass(
                enc,
                &self.svg.pipeline,
                &canvas.view,
                &bind,
                None,
                Some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)),
            );
        }
    }

    /// `pic` laid on `base` at `at` by the pass a frame's titles are drawn with: the planes `overlay` would write (the reference is
    /// [`svg::blend_onto`]). It exists so that the pass can be held to the model, sample for sample, on pictures of any size and any
    /// offset, without a plan.
    pub fn overlay_picture(&self, base: &YuvFrame, pic: &Picture, at: (i64, i64)) -> Result<YuvFrame, GpuError> {
        if !base.is_consistent() || !base.width.is_multiple_of(2) || !base.height.is_multiple_of(2) {
            return Err(GpuError::Unsupported("an overlay needs an even, consistent composite".into()));
        }
        let (w, h) = (base.width, base.height);
        let (cw, _) = base.chroma_size();
        let rgba: Vec<u8> = (0..h as usize)
            .flat_map(|y| (0..w as usize).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let c = (y / 2) * cw as usize + x / 2;
                [base.y[y * w as usize + x], base.u[c], base.v[c], 255]
            })
            .collect();
        self.gpu
            .guarded("overlaying a picture", || {
                let canvas = self.plane(
                    "overlay canvas",
                    w,
                    h,
                    CANVAS_FORMAT,
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
                );
                self.gpu.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &canvas.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(w * 4),
                        rows_per_image: Some(h),
                    },
                    extent(w, h),
                );
                let mut enc = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("kerf overlay picture"),
                });
                let rect = SvgRect {
                    x: at.0,
                    y: at.1,
                    w: pic.w,
                    h: pic.h,
                };
                let mut before = None;
                self.draw_svg(
                    &mut enc,
                    &canvas,
                    &mut before,
                    &[PreparedSvg {
                        rect,
                        picture: Arc::new(Resident::new(pic.clone())),
                    }],
                );
                self.read_back(enc, &canvas)
            })
            .map(|frame| {
                let (w, h) = (frame.width as usize, frame.height as usize);
                let at = |x: usize, y: usize, c: usize| frame.data[(y * w + x) * 4 + c];
                let chroma = |c: usize| -> Vec<u8> {
                    (0..h / 2)
                        .flat_map(|y| (0..w / 2).map(move |x| (x, y)))
                        .map(|(x, y)| at(x * 2, y * 2, c))
                        .collect()
                };
                YuvFrame {
                    width: frame.width,
                    height: frame.height,
                    y: (0..h)
                        .flat_map(|y| (0..w).map(move |x| (x, y)))
                        .map(|(x, y)| at(x, y, 0))
                        .collect(),
                    u: chroma(1),
                    v: chroma(2),
                }
            })
    }
}
