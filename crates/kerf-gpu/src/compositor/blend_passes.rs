//! The passes of a blended clip (`blend.wgsl`; the arithmetic and what it was held to are in [`crate::blend`]): the clip is laid on a
//! copy of the canvas opaque (the compositor's own pass, so its placement, its odd edges and its chroma blocks are the plain
//! layer's), both frames go to RGB where the mode mixes them, and the mix goes back to 4:2:0 and is laid onto the composite
//! by the clip's alpha — `overlay`'s integer blend, as for a translucent layer.

use super::*;
use crate::blend::{mode_index, Tables};

/// `blend.wgsl`'s uniform.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlendParams {
    /// [`Tables::words`].
    k: [i32; 16],
    origin: [i32; 2],
    layer: [i32; 2],
    canvas: [i32; 2],
    mode: u32,
    has_alpha: u32,
    alpha: u32,
    ramp_in: u32,
    ramp_out: u32,
    /// The clip's colours are held to the legal range (`lutyuv`'s `clipval`): set for a clip laid by the draw, not for pictures given whole.
    legal: u32,
}

/// The pipelines of a blended layer.
pub(super) struct BlendPasses {
    layout: wgpu::BindGroupLayout,
    rgb: wgpu::RenderPipeline,
    over: wgpu::RenderPipeline,
}

/// Canvas-sized planes the blend works in, made by the first blended layer of a frame.
pub(super) struct BlendScratch {
    /// The canvas as it was before the layer (the bottom frame, and what the result is laid onto).
    before: Plane,
    /// The canvas with the clip laid on it, opaque (the top frame).
    laid: Plane,
    /// The mode's RGB.
    mixed: Plane,
}

/// What one blended layer needs of the draw: the placement the plain layer is drawn with, and its alpha.
pub(super) struct BlendLayer<'a> {
    pub mode: kerf_core::BlendMode,
    pub tables: Tables,
    /// The layer's own pass ([`ComposeParams`]): origin, picture, chroma and the odd layer's tail.
    pub params: ComposeParams,
    pub views: [&'a wgpu::TextureView; 3],
    /// The clip's alpha plane (a mask, a key), when it has one.
    pub alpha_plane: Option<&'a Plane>,
    /// The part of the canvas the layer covers (its picture, clipped to the canvas) and the extent of its odd tail, if any.
    pub area: (u32, u32, u32, u32),
    pub tail: Option<(u32, u32)>,
    pub fades: LayerFades,
    pub canvas: (u32, u32),
}

impl BlendPasses {
    pub(super) fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kerf blend"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/blend.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blend"),
            entries: &[
                uniform_entry(0),
                texture_entry(1),
                texture_entry(2),
                texture_entry(3),
                texture_entry(4),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blend"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |label: &str, fs: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
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
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: CANVAS_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            rgb: pipeline("blend rgb", "fs_blend_rgb"),
            over: pipeline("blend over", "fs_blend_over"),
            layout,
        }
    }
}

impl Compositor {
    fn blend_scratch(&self, w: u32, h: u32) -> BlendScratch {
        let target = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST;
        BlendScratch {
            before: self.plane("blend before", w, h, CANVAS_FORMAT, wgpu::TextureUsages::COPY_DST),
            laid: self.plane("blend laid", w, h, CANVAS_FORMAT, target),
            mixed: self.plane("blend mixed", w, h, CANVAS_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT),
        }
    }

    fn blend_bind(
        &self,
        params: &BlendParams,
        top: &wgpu::TextureView,
        bottom: &wgpu::TextureView,
        mixed: &wgpu::TextureView,
        alpha: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        let device = &self.gpu.device;
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("blend params"),
            contents: bytemuck::bytes_of(params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        fn entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::TextureView(view),
            }
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blend"),
            layout: &self.blend.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                },
                entry(1, top),
                entry(2, bottom),
                entry(3, mixed),
                entry(4, alpha),
            ],
        })
    }

    /// Lay a blended layer onto `canvas`: the clip opaque on a copy of it, both frames through the mode in RGB, and the mix onto the
    /// canvas by the clip's alpha, over the 2x2 blocks the layer touches.
    pub(super) fn blend_layer(
        &self,
        enc: &mut wgpu::CommandEncoder,
        canvas: &Plane,
        scratch: &mut Option<BlendScratch>,
        job: &BlendLayer<'_>,
    ) -> Result<(), GpuError> {
        let Some(mode) = mode_index(job.mode) else {
            return Err(GpuError::Unsupported("a blend mode that is plain alpha-over".into()));
        };
        let (ow, oh) = job.canvas;
        let scratch = scratch.get_or_insert_with(|| self.blend_scratch(ow, oh));
        let (x0, y0, x1, y1) = job.area;
        let (tx1, ty1) = job.tail.unwrap_or((x1, y1));
        // Whole chroma blocks: the pairs the way back reads and the block alpha's neighbours are all inside the region.
        let (rx0, ry0) = (x0 & !1, y0 & !1);
        let (rx1, ry1) = (((x1.max(tx1) + 1) & !1).min(ow), ((y1.max(ty1) + 1) & !1).min(oh));
        let region = wgpu::Extent3d {
            width: rx1 - rx0,
            height: ry1 - ry0,
            depth_or_array_layers: 1,
        };
        let origin = wgpu::Origin3d { x: rx0, y: ry0, z: 0 };
        for target in [&scratch.before, &scratch.laid] {
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &canvas.texture,
                    mip_level: 0,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &target.texture,
                    mip_level: 0,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                region,
            );
        }
        // The clip laid on the copy, opaque: the plain layer's own pass (hardware blend, alpha one), and the half block an
        // odd layer's last pixel shares with the one past it.
        let mut params = job.params;
        params.opacity = [1.0, 0.0, 0.0, 0.0];
        params.has_alpha = 0;
        params.tail = 0;
        let bind = self.compose_bind(&params, job.views);
        self.pass(
            enc,
            &self.compose,
            &scratch.laid.view,
            &bind,
            None,
            Some((x0, y0, x1 - x0, y1 - y0)),
        );
        if job.tail.is_some() {
            params.tail = 1;
            let bind = self.compose_bind(&params, job.views);
            self.pass(
                enc,
                &self.compose_chroma,
                &scratch.laid.view,
                &bind,
                None,
                Some((x0, y0, tx1 - x0, ty1 - y0)),
            );
        }
        let blend = BlendParams {
            k: job.tables.words(),
            origin: job.params.origin,
            layer: [job.params.layer[0], job.params.layer[1]],
            canvas: [ow as i32, oh as i32],
            mode,
            has_alpha: u32::from(job.alpha_plane.is_some()),
            alpha: job.fades.alpha,
            ramp_in: job.fades.ramp,
            ramp_out: job.fades.ramp_out,
            legal: 1,
        };
        self.blend_mix(enc, canvas, scratch, &blend, job.alpha_plane, (rx0, ry0, rx1, ry1));
        Ok(())
    }

    /// The two passes after the clip is laid: the mode in RGB on `scratch.laid` over `scratch.before`, and the mix onto `canvas` by
    /// the clip's alpha, over the whole chroma blocks of `region`.
    fn blend_mix(
        &self,
        enc: &mut wgpu::CommandEncoder,
        canvas: &Plane,
        scratch: &BlendScratch,
        blend: &BlendParams,
        alpha_plane: Option<&Plane>,
        region: (u32, u32, u32, u32),
    ) {
        let (rx0, ry0, rx1, ry1) = region;
        let alpha = alpha_plane.map_or(&self.text.blank, |p| &p.view);
        let scissor = Some((rx0, ry0, rx1 - rx0, ry1 - ry0));
        let bind = self.blend_bind(blend, &scratch.laid.view, &scratch.before.view, &scratch.before.view, alpha);
        self.pass(enc, &self.blend.rgb, &scratch.mixed.view, &bind, None, scissor);
        let bind = self.blend_bind(blend, &scratch.laid.view, &scratch.before.view, &scratch.mixed.view, alpha);
        self.pass(enc, &self.blend.over, &canvas.view, &bind, None, scissor);
    }

    /// The blend passes alone, on pictures the caller made: `base` the composite, `laid` the composite with the clip laid on it,
    /// `alpha` the clip's luma-resolution alpha plane over the whole frame; the composite after the blend. What
    /// [`crate::blend::reference`] computes, drawn (`tests/blend_kernels.rs`).
    pub fn blend_planes(
        &self,
        mode: kerf_core::BlendMode,
        matrix: YuvMatrix,
        base: &crate::blend::Planes,
        laid: &crate::blend::Planes,
        alpha: &[u8],
    ) -> Result<crate::blend::Planes, GpuError> {
        let (w, h) = (base.w as u32, base.h as u32);
        if base.w % 2 == 1 || base.h % 2 == 1 || (laid.w, laid.h) != (base.w, base.h) || alpha.len() != base.w * base.h {
            return Err(GpuError::Unsupported("a blend on pictures that are not one even size".into()));
        }
        let mode = mode_index(mode).ok_or_else(|| GpuError::Unsupported("a blend mode that is plain alpha-over".into()))?;
        self.gpu.guarded("blending planes", || {
            let canvas_of = |p: &crate::blend::Planes, label: &str, extra: wgpu::TextureUsages| {
                let mut rgba = Vec::with_capacity(base.w * base.h * 4);
                for y in 0..base.h {
                    for x in 0..base.w {
                        let c = (y / 2) * (base.w / 2) + x / 2;
                        rgba.extend([p.y[y * base.w + x], p.u[c], p.v[c], 255]);
                    }
                }
                let plane = self.plane(label, w, h, CANVAS_FORMAT, wgpu::TextureUsages::COPY_DST | extra);
                self.gpu.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &plane.texture,
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
                plane
            };
            let canvas = canvas_of(
                base,
                "blend canvas",
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            );
            let scratch = BlendScratch {
                before: canvas_of(base, "blend before", wgpu::TextureUsages::empty()),
                laid: canvas_of(laid, "blend laid", wgpu::TextureUsages::empty()),
                mixed: self.plane("blend mixed", w, h, CANVAS_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT),
            };
            let alpha_plane = self.upload("blend alpha", w, h, alpha);
            let params = BlendParams {
                k: Tables::new(matrix).words(),
                origin: [0, 0],
                layer: [w as i32, h as i32],
                canvas: [w as i32, h as i32],
                mode,
                has_alpha: 1,
                alpha: 255,
                ramp_in: FADE_FULL,
                ramp_out: FADE_FULL,
                legal: 0,
            };
            let mut enc = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kerf blend planes"),
            });
            self.blend_mix(&mut enc, &canvas, &scratch, &params, Some(&alpha_plane), (0, 0, w, h));
            let rgba = self.read_back(enc, &canvas)?;
            let (cw, ch) = (base.w / 2, base.h / 2);
            let mut out = base.clone();
            for y in 0..base.h {
                for x in 0..base.w {
                    out.y[y * base.w + x] = rgba.data[(y * base.w + x) * 4];
                }
            }
            for y in 0..ch {
                for x in 0..cw {
                    let i = ((y * 2) * base.w + x * 2) * 4;
                    out.u[y * cw + x] = rgba.data[i + 1];
                    out.v[y * cw + x] = rgba.data[i + 2];
                }
            }
            Ok(out)
        })
    }
}
