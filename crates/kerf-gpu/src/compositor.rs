//! Compositor v0: draw a [`RenderPlan`] onto a black canvas on the GPU and read
//! it back as RGBA.
//!
//! Per layer, the passes mirror what the FFmpeg still graph does to the same
//! pictures, in the same order and at the same precision — that is what makes a
//! pixel comparison against FFmpeg meaningful rather than aspirational:
//!
//! 1. **Scale each plane** (`resample_h` + `resample_v` in `composite.wgsl`) with
//!    swscale's bicubic. Luma and the two chroma planes are scaled
//!    *independently at their own size*, like swscale does, and the result is an
//!    8-bit plane. One stage for the fit, a second when the clip's transform has
//!    its own scale — FFmpeg runs those as two scalers too. A stage that changes
//!    nothing is skipped, as FFmpeg's `scale` skips it.
//! 2. **`eq` tables** (colour correction) are applied to the last stage's 8-bit
//!    planes — in YUV, before any RGB exists, as the filter does.
//! 3. **Compose**: rotate (bilinear, as `rotate`), place at FFmpeg's integer
//!    position and blend onto the canvas — which holds **Y, U, V and alpha**, not
//!    RGB, because `overlay` blends the encoded YUV planes. (Converting each layer
//!    to RGB first would clamp it before the blend, and legal-but-out-of-gamut
//!    footage then composites differently.)
//! 4. **Convert once**: the finished composite goes YUV -> RGB with the plan's
//!    matrix (limited range, chroma replicated 2x2) and is read back.
//!
//! Everything is 8-bit between stages, in encoded gamma — FFmpeg's own working
//! space — so there is no linear-light round trip to disagree about.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use kerf_core::RenderPlan;
use wgpu::util::DeviceExt;

use crate::eq;
use crate::geometry::{LayerGeometry, Rect, ScaleStage};
use crate::gpu::{Gpu, GpuError};
use crate::source::{chroma_size, decode_layers, YuvFrame};

/// A rendered frame: tightly packed 8-bit RGBA, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Where the time of a [`Compositor::render_plan`] went. The decode is FFmpeg's
/// (and v0 spawns one process per layer), the composite is the GPU's upload,
/// passes and readback.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderTimings {
    pub decode: Duration,
    pub composite: Duration,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ResampleParams {
    src_off: [i32; 2],
    src_size: [i32; 2],
    dst_full: [i32; 2],
    dst_off: [i32; 2],
    out_size: [i32; 2],
    apply_lut: u32,
    plane: u32,
    pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ComposeParams {
    opacity: [f32; 4],
    origin: [i32; 2],
    layer: [i32; 2],
    pic: [i32; 2],
    cpic: [i32; 2],
    clayer: [i32; 2],
    rot: [f32; 2],
    rotate: u32,
    pad: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ConvertParams {
    coef: [f32; 4],
    coef2: [f32; 4],
}

/// One plane on the GPU. A handle: cloning shares the texture.
#[derive(Clone)]
struct Plane {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    w: u32,
    h: u32,
}

const PLANE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
const MID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The GPU compositor: pipelines created once, textures per frame.
///
/// v0 allocates its textures and buffers per call and keeps no cache — simple
/// to reason about while the numbers are what matters; pooling is A1.
pub struct Compositor {
    gpu: Arc<Gpu>,
    resample_layout: wgpu::BindGroupLayout,
    compose_layout: wgpu::BindGroupLayout,
    convert_layout: wgpu::BindGroupLayout,
    resample_h: wgpu::RenderPipeline,
    resample_v: wgpu::RenderPipeline,
    compose: wgpu::RenderPipeline,
    convert: wgpu::RenderPipeline,
    /// A 256x3 table that changes nothing, bound when a layer has no `eq`.
    identity_lut: wgpu::TextureView,
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        // Non-filterable accepts every format used here; the shader only ever
        // `textureLoad`s, so no sampler (and no filtering) is involved.
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn extent(w: u32, h: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    }
}

impl Compositor {
    pub fn new(gpu: Arc<Gpu>) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kerf composite"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/composite.wgsl").into()),
        });
        let resample_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("resample"),
            entries: &[texture_entry(0), uniform_entry(1), texture_entry(2)],
        });
        let compose_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compose"),
            entries: &[uniform_entry(0), texture_entry(1), texture_entry(2), texture_entry(3)],
        });
        let convert_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("convert"),
            entries: &[uniform_entry(0), texture_entry(1)],
        });
        let pipeline = |label: &str, layout: &wgpu::BindGroupLayout, fs: &str, format, blend| {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
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
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let over = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let resample_h = pipeline("resample_h", &resample_layout, "fs_h", MID_FORMAT, None);
        let resample_v = pipeline("resample_v", &resample_layout, "fs_v", PLANE_FORMAT, None);
        let compose = pipeline("compose", &compose_layout, "fs_compose", CANVAS_FORMAT, Some(over));
        let convert = pipeline("convert", &convert_layout, "fs_convert", CANVAS_FORMAT, None);

        let identity: Vec<u8> = (0..3).flat_map(|_| 0..=255u8).collect();
        let identity_lut = Self::lut_texture(&gpu, &identity);
        Self {
            gpu,
            resample_layout,
            compose_layout,
            convert_layout,
            resample_h,
            resample_v,
            compose,
            convert,
            identity_lut,
        }
    }

    fn lut_texture(gpu: &Gpu, rows: &[u8]) -> wgpu::TextureView {
        let tex = gpu.device.create_texture_with_data(
            &gpu.queue,
            &wgpu::TextureDescriptor {
                label: Some("eq tables"),
                size: extent(256, 3),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PLANE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            rows,
        );
        tex.create_view(&wgpu::TextureViewDescriptor::default())
    }

    fn plane(&self, label: &str, w: u32, h: u32, format: wgpu::TextureFormat, extra: wgpu::TextureUsages) -> Plane {
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: extent(w, h),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | extra,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Plane { texture, view, w, h }
    }

    fn upload(&self, label: &str, w: u32, h: u32, data: &[u8]) -> Plane {
        let p = self.plane(label, w, h, PLANE_FORMAT, wgpu::TextureUsages::COPY_DST);
        self.gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &p.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w),
                rows_per_image: Some(h),
            },
            extent(w, h),
        );
        p
    }

    fn pass(
        &self,
        enc: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        target: &wgpu::TextureView,
        bind_group: &wgpu::BindGroup,
        clear: Option<wgpu::Color>,
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some((x, y, w, h)) = scissor {
            pass.set_scissor_rect(x, y, w, h);
        }
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// One swscale stage over one plane: horizontal pass into a float
    /// intermediate, vertical pass into the 8-bit result.
    #[allow(clippy::too_many_arguments)]
    fn scale_plane(
        &self,
        enc: &mut wgpu::CommandEncoder,
        input: &Plane,
        src: Rect,
        scaled: (u32, u32),
        keep: Rect,
        lut: Option<(&wgpu::TextureView, u32)>,
    ) -> Plane {
        let device = &self.gpu.device;
        // Horizontal: (kept width) x (source window height).
        let mid = self.plane("mid", keep.w, src.h, MID_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let out = self.plane(
            "plane",
            keep.w,
            keep.h,
            PLANE_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let base = ResampleParams {
            src_off: [src.x as i32, src.y as i32],
            src_size: [src.w as i32, src.h as i32],
            dst_full: [scaled.0 as i32, scaled.1 as i32],
            dst_off: [keep.x as i32, keep.y as i32],
            out_size: [keep.w as i32, keep.h as i32],
            apply_lut: 0,
            plane: 0,
            pad: [0; 2],
        };
        let (lut_view, lut_row) = lut.map_or((&self.identity_lut, 0), |(v, r)| (v, r));
        let bind = |params: ResampleParams, view: &wgpu::TextureView| {
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("resample params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.resample_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(lut_view),
                    },
                ],
            })
        };
        let h = bind(base, &input.view);
        self.pass(enc, &self.resample_h, &mid.view, &h, Some(wgpu::Color::BLACK), None);
        let v = bind(
            ResampleParams {
                apply_lut: u32::from(lut.is_some()),
                plane: lut_row,
                ..base
            },
            &mid.view,
        );
        self.pass(enc, &self.resample_v, &out.view, &v, Some(wgpu::Color::BLACK), None);
        out
    }

    /// Composite `frames` (one decoded picture per plan layer) at `size`.
    ///
    /// Refuses a plan that is not [`RenderPlan::gpu_supported`], so a caller that
    /// forgot to ask gets an error naming why instead of a wrong picture.
    pub fn composite(&self, plan: &RenderPlan, frames: &[YuvFrame], size: (u32, u32)) -> Result<RgbaFrame, GpuError> {
        if !plan.gpu_supported() {
            return Err(GpuError::Unsupported(plan.unsupported_reasons().join("; ")));
        }
        if frames.len() != plan.layers.len() {
            return Err(GpuError::Decode(format!(
                "{} decoded frames for {} layers",
                frames.len(),
                plan.layers.len()
            )));
        }
        let (ow, oh) = size;
        if ow == 0 || oh == 0 || ow % 2 != 0 || oh % 2 != 0 {
            return Err(GpuError::Unsupported(format!("a {ow}x{oh} canvas (4:2:0 needs even sides)")));
        }
        let device = &self.gpu.device;
        let max_side = device.limits().max_texture_dimension_2d;
        if ow > max_side || oh > max_side {
            return Err(GpuError::Unsupported(format!(
                "a {ow}x{oh} canvas is over this device's {max_side}px texture limit"
            )));
        }
        // The canvas holds Y, U, V (and alpha) in an RGBA8 texture: FFmpeg's
        // `overlay` blends the encoded YUV planes, so this does too, and the one
        // conversion to RGB happens at the very end.
        let canvas = self.plane("canvas", ow, oh, CANVAS_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("kerf composite"),
        });

        // The empty canvas is black — limited-range Y 16, U = V = 128 — and every
        // layer's pass loads on top of it.
        {
            let _clear = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &canvas.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 16.0 / 255.0,
                            g: 128.0 / 255.0,
                            b: 128.0 / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        let (kr, kb) = plan.canvas.matrix.weights();
        let kg = 1.0 - kr - kb;
        let chroma_gain = 255.0 / 224.0;
        let coef = [
            (255.0 / 219.0) as f32,
            (2.0 * (1.0 - kr) * chroma_gain) as f32,
            (2.0 * (1.0 - kb) * chroma_gain) as f32,
            (-(2.0 * (1.0 - kb) * kb / kg) * chroma_gain) as f32,
        ];
        let gv = (-(2.0 * (1.0 - kr) * kr / kg) * chroma_gain) as f32;

        for (n, (layer, frame)) in plan.layers.iter().zip(frames).enumerate() {
            let geom = LayerGeometry::resolve((frame.width, frame.height), size, plan.canvas.fit, &layer.transform)
                .map_err(|e| GpuError::Unsupported(format!("layer {n}: {e}")))?;
            // wgpu raises a validation error (a panic, by default) for a texture
            // over the device's limit — a 10x zoom of a 4K layer would be one.
            // Refuse it instead, so the frame goes to FFmpeg.
            let limit = device.limits().max_texture_dimension_2d;
            let biggest = geom
                .stages
                .iter()
                .flat_map(|s| [s.src.w, s.src.h, s.keep.w, s.keep.h])
                .chain([frame.width, frame.height, geom.layer.0, geom.layer.1])
                .max()
                .unwrap_or(0);
            if biggest > limit {
                return Err(GpuError::Unsupported(format!(
                    "layer {n}: a {biggest}px texture is over this device's {limit}px limit"
                )));
            }
            let (cw, ch) = frame.chroma_size();
            let planes = [
                self.upload("y", frame.width, frame.height, &frame.y),
                self.upload("u", cw, ch, &frame.u),
                self.upload("v", cw, ch, &frame.v),
            ];

            // `eq` tables, applied on the final stage only.
            let tables = eq::luts(&layer.color).map(|rows| {
                let flat: Vec<u8> = rows.iter().flatten().copied().collect();
                Self::lut_texture(&self.gpu, &flat)
            });

            let mut current = planes;
            for (si, stage) in geom.stages.iter().enumerate() {
                let last = si + 1 == geom.stages.len();
                let mut next: Vec<Plane> = Vec::with_capacity(3);
                for (pi, input) in current.iter().enumerate() {
                    let (src, scaled, keep) = plane_stage(stage, pi > 0);
                    let lut = last.then_some(tables.as_ref()).flatten().map(|t| (t, pi as u32));
                    // A stage that changes nothing — the whole plane, at its own
                    // size, no table to apply — is the plane itself. FFmpeg's
                    // `scale` is a passthrough in the same case, and it is by far
                    // the common one (footage already at the delivery size).
                    let identity = lut.is_none()
                        && src == Rect::whole(input.w, input.h)
                        && keep == Rect::whole(src.w, src.h)
                        && scaled == (src.w, src.h);
                    next.push(if identity {
                        input.clone()
                    } else {
                        self.scale_plane(&mut enc, input, src, scaled, keep, lut)
                    });
                }
                current = next.try_into().unwrap_or_else(|_| unreachable!("three planes in, three out"));
            }
            let [py, pu, pv] = &current;

            let (rotate, rot) = match geom.rotation {
                Some(r) => (1, [r.angle.cos() as f32, r.angle.sin() as f32]),
                None => (0, [1.0, 0.0]),
            };
            let (lw, lh) = geom.layer;
            let (clw, clh) = chroma_size(lw, lh);
            let (pcw, pch) = chroma_size(geom.picture.0, geom.picture.1);
            let params = ComposeParams {
                opacity: [geom.opacity, 0.0, 0.0, 0.0],
                origin: [geom.origin.0, geom.origin.1],
                layer: [lw as i32, lh as i32],
                pic: [geom.picture.0 as i32, geom.picture.1 as i32],
                cpic: [pcw as i32, pch as i32],
                clayer: [clw as i32, clh as i32],
                rot,
                rotate,
                pad: [0; 3],
            };
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("compose params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("compose"),
                layout: &self.compose_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&py.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&pu.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&pv.view),
                    },
                ],
            });
            // Only the part of the canvas the layer can touch.
            let x0 = geom.origin.0.max(0) as i64;
            let y0 = geom.origin.1.max(0) as i64;
            let x1 = (i64::from(geom.origin.0) + i64::from(lw)).min(i64::from(ow));
            let y1 = (i64::from(geom.origin.1) + i64::from(lh)).min(i64::from(oh));
            if x1 <= x0 || y1 <= y0 {
                continue; // entirely off the canvas
            }
            self.pass(
                &mut enc,
                &self.compose,
                &canvas.view,
                &bind,
                None,
                Some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)),
            );
        }

        // The single YUV -> RGB conversion of the finished composite.
        let rgb = self.plane(
            "rgb",
            ow,
            oh,
            CANVAS_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let convert = ConvertParams {
            coef,
            coef2: [gv, 0.0, 0.0, 0.0],
        };
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("convert params"),
            contents: bytemuck::bytes_of(&convert),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("convert"),
            layout: &self.convert_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&canvas.view),
                },
            ],
        });
        self.pass(&mut enc, &self.convert, &rgb.view, &bind, Some(wgpu::Color::BLACK), None);

        self.read_back(enc, &rgb)
    }

    fn read_back(&self, mut enc: wgpu::CommandEncoder, canvas: &Plane) -> Result<RgbaFrame, GpuError> {
        let device = &self.gpu.device;
        let (w, h) = (canvas.w, canvas.h);
        // Rows in a texture-to-buffer copy are padded to 256 bytes.
        let pitch = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(pitch) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &canvas.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(pitch),
                    rows_per_image: Some(h),
                },
            },
            extent(w, h),
        );
        self.gpu.queue.submit(Some(enc.finish()));

        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            // The receiver outlives the poll below; a send can only fail if the
            // caller already gave up.
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|e| GpuError::Readback(e.to_string()))?
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let mapped = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for row in mapped.chunks(pitch as usize).take(h as usize) {
            data.extend_from_slice(&row[..(w * 4) as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(RgbaFrame {
            width: w,
            height: h,
            data,
        })
    }

    /// Decode every layer of `plan` through FFmpeg and composite them at `size`,
    /// timing the two halves apart.
    pub fn render_plan(&self, plan: &RenderPlan, size: (u32, u32)) -> Result<(RgbaFrame, RenderTimings), GpuError> {
        if !plan.gpu_supported() {
            return Err(GpuError::Unsupported(plan.unsupported_reasons().join("; ")));
        }
        let t0 = Instant::now();
        let frames = decode_layers(&plan.layers)?;
        let decode = t0.elapsed();
        let t1 = Instant::now();
        let frame = self.composite(plan, &frames, size)?;
        Ok((
            frame,
            RenderTimings {
                decode,
                composite: t1.elapsed(),
            },
        ))
    }
}

/// A scale stage as one plane sees it. The chroma planes are half the size
/// (rounded up) and are scaled on their own grid: the crop and Cover offsets are
/// even by construction, so they halve exactly.
fn plane_stage(stage: &ScaleStage, chroma: bool) -> (Rect, (u32, u32), Rect) {
    if !chroma {
        return (stage.src, stage.scaled, stage.keep);
    }
    let half = |v: u32| v.div_ceil(2);
    (
        Rect {
            x: stage.src.x / 2,
            y: stage.src.y / 2,
            w: half(stage.src.w),
            h: half(stage.src.h),
        },
        (half(stage.scaled.0), half(stage.scaled.1)),
        Rect {
            x: stage.keep.x / 2,
            y: stage.keep.y / 2,
            w: half(stage.keep.w),
            h: half(stage.keep.h),
        },
    )
}
