//! Compositor v0: draw a [`RenderPlan`] onto a black canvas on the GPU and read
//! it back as RGBA.
//!
//! Per layer, the passes mirror what the FFmpeg still graph does to the same
//! pictures, in the same order and at the same precision — that is what makes a
//! pixel comparison against FFmpeg meaningful rather than aspirational:
//!
//! 1. **Scale each plane** (`resample_h` + `resample_v` in `composite.wgsl`) with
//!    swscale's bicubic: its own filter tables ([`crate::sws`]) and its integer
//!    arithmetic. Luma and the two chroma planes are scaled *independently at
//!    their own size*, like swscale does, and the result is an 8-bit plane. One
//!    stage for the fit, a second when the clip's transform has its own scale —
//!    FFmpeg runs those as two scalers too. A stage that changes nothing is
//!    skipped, as FFmpeg's `scale` skips it.
//! 2. **`eq` tables** (colour correction) are applied to the last stage's 8-bit
//!    planes — in YUV, before any RGB exists, as the filter does.
//! 3. **The RGB round trip** of a translucent layer (`roundtrip.wgsl`,
//!    [`crate::roundtrip`]): FFmpeg's `colorchannelmixer` only takes RGB, so a clip
//!    below full opacity is converted `yuva420p -> argb -> yuva420p` before it is
//!    overlaid, clipping out-of-gamut colour and rebuilding the chroma.
//! 4. **Compose**: rotate (bilinear, as `rotate`), place at FFmpeg's integer
//!    position and blend onto the canvas — which holds **Y, U, V and alpha**, not
//!    RGB, because `overlay` blends the encoded YUV planes. (Converting each layer
//!    to RGB first would clamp it before the blend, and legal-but-out-of-gamut
//!    footage then composites differently.) A letterboxed layer is the whole frame
//!    with black bars (`pad`'s output), and an odd layer's last chroma block
//!    reaches the pixel past it — both reproduced.
//! 5. **Convert once**: the finished composite goes YUV -> RGB with the plan's
//!    matrix (limited range, chroma replicated 2x2) and is read back.
//!
//! Every GPU call runs in error scopes ([`Gpu::guarded`]): a wgpu failure comes
//! back as a [`GpuError`], never a panic.
//!
//! Everything is 8-bit between stages, in encoded gamma — FFmpeg's own working
//! space — so there is no linear-light round trip to disagree about.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use kerf_core::{RenderPlan, YuvMatrix};
use wgpu::util::DeviceExt;

use crate::eq;
use crate::gpu::{Gpu, GpuError};
use crate::roundtrip::{Rgb2Yuv, Yuv2Rgb};
use crate::source::{chroma_size, decode_layers, YuvFrame};
use crate::sws::{self, Filter};
use kerf_core::layer_geometry::{LayerGeometry, Rect, ScaleStage};

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
    dst_off: [i32; 2],
    out_size: [i32; 2],
    apply_lut: u32,
    plane: u32,
    taps: i32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ComposeParams {
    opacity: [f32; 4],
    matte: [f32; 4],
    origin: [i32; 2],
    layer: [i32; 2],
    pic: [i32; 2],
    cpic: [i32; 2],
    clayer: [i32; 2],
    rot: [f32; 2],
    pic_at: [i32; 2],
    pic_shows: [i32; 2],
    rotate: u32,
    tail: u32,
    pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ConvertParams {
    coef: [f32; 4],
    coef2: [f32; 4],
}

/// `roundtrip.wgsl`'s uniform.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RoundTripParams {
    k: [i32; 4],
    m: [i32; 4],
    ky: [i32; 4],
    ku: [i32; 4],
    kv: [i32; 4],
    size: [i32; 2],
    csize: [i32; 2],
    channel: i32,
    taps: i32,
    pad: [i32; 2],
}

/// One plane on the GPU. A handle: cloning shares the texture.
#[derive(Clone)]
struct Plane {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    w: u32,
    h: u32,
}

/// How long a frame may take to come back from the GPU before the render gives up.
const READBACK_TIMEOUT: Duration = Duration::from_secs(30);

const PLANE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
const MID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const FILTER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
/// U and V together, as 15-bit integers: the round trip's chroma intermediate.
const CHROMA_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Float;
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
    /// The same shader writing only U and V: the chroma an odd layer's last
    /// half block puts on the pixel just past it.
    compose_chroma: wgpu::RenderPipeline,
    convert: wgpu::RenderPipeline,
    /// The RGB round trip of a translucent layer (`roundtrip.wgsl`).
    rt_rgb: wgpu::RenderPipeline,
    rt_luma: wgpu::RenderPipeline,
    rt_chroma_h: wgpu::RenderPipeline,
    rt_chroma_v: wgpu::RenderPipeline,
    rt_v_layout: wgpu::BindGroupLayout,
    /// A 256x3 table that changes nothing, bound when a layer has no `eq`.
    identity_lut: wgpu::TextureView,
}

/// Limited-range YUV -> full-range RGB coefficients for a matrix, `([ky, rv, bu,
/// gu], gv)`, in the 0..255 domain the final conversion works in.
fn matrix_coefs(matrix: YuvMatrix) -> ([f32; 4], f32) {
    let (kr, kb) = matrix.weights();
    let kg = 1.0 - kr - kb;
    let chroma_gain = 255.0 / 224.0;
    let coef = [
        (255.0 / 219.0) as f32,
        (2.0 * (1.0 - kr) * chroma_gain) as f32,
        (2.0 * (1.0 - kb) * chroma_gain) as f32,
        (-(2.0 * (1.0 - kb) * kb / kg) * chroma_gain) as f32,
    ];
    let gv = (-(2.0 * (1.0 - kr) * kr / kg) * chroma_gain) as f32;
    (coef, gv)
}

/// What a composite hands back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Output {
    /// RGB, converted once at the end — what a preview or export shows.
    Rgb,
    /// The canvas planes before that conversion.
    Yuv,
}

/// A layer that has a picture and a place to draw it.
struct Drawn<'a> {
    layer: &'a kerf_core::PlanLayer,
    frame: &'a YuvFrame,
    geom: LayerGeometry,
    /// Per stage, per plane (Y, U, V): the scaler's tables.
    filters: Vec<[PlaneFilters; 3]>,
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
    /// Build the pipelines on `gpu`. Fails with a [`GpuError`] — it never panics —
    /// when the device rejects the shader or a pipeline, or is already lost.
    pub fn new(gpu: Arc<Gpu>) -> Result<Self, GpuError> {
        let g = Arc::clone(&gpu);
        gpu.guarded("creating the compositor", move || {
            let device = &g.device;
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("kerf composite"),
                source: wgpu::ShaderSource::Wgsl(include_str!("shaders/composite.wgsl").into()),
            });
            let resample_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("resample"),
                entries: &[texture_entry(0), uniform_entry(1), texture_entry(2), texture_entry(3)],
            });
            let compose_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("compose"),
                entries: &[uniform_entry(0), texture_entry(1), texture_entry(2), texture_entry(3)],
            });
            let convert_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("convert"),
                entries: &[uniform_entry(0), texture_entry(1)],
            });
            let pipeline = |label: &str, layout: &wgpu::BindGroupLayout, fs: &str, format, blend, mask| {
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
                            write_mask: mask,
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
            let all = wgpu::ColorWrites::ALL;
            let resample_h = pipeline("resample_h", &resample_layout, "fs_h", MID_FORMAT, None, all);
            let resample_v = pipeline("resample_v", &resample_layout, "fs_v", PLANE_FORMAT, None, all);
            let compose = pipeline("compose", &compose_layout, "fs_compose", CANVAS_FORMAT, Some(over), all);
            let chroma_only = wgpu::ColorWrites::GREEN | wgpu::ColorWrites::BLUE;
            let compose_chroma = pipeline(
                "compose chroma",
                &compose_layout,
                "fs_compose",
                CANVAS_FORMAT,
                Some(over),
                chroma_only,
            );
            let convert = pipeline("convert", &convert_layout, "fs_convert", CANVAS_FORMAT, None, all);

            // The round trip: its own module, its own bindings.
            let rt_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("kerf roundtrip"),
                source: wgpu::ShaderSource::Wgsl(include_str!("shaders/roundtrip.wgsl").into()),
            });
            let rt_v_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("roundtrip vertical"),
                entries: &[uniform_entry(0), texture_entry(1), texture_entry(2)],
            });
            let rt_pipeline = |label: &str, layout: &wgpu::BindGroupLayout, fs: &str, format| {
                let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some(label),
                    bind_group_layouts: &[Some(layout)],
                    immediate_size: 0,
                });
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&pl),
                    vertex: wgpu::VertexState {
                        module: &rt_shader,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &rt_shader,
                        entry_point: Some(fs),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
            let rt_rgb = rt_pipeline("rt rgb", &compose_layout, "fs_rgb", CANVAS_FORMAT);
            let rt_luma = rt_pipeline("rt luma", &convert_layout, "fs_luma", PLANE_FORMAT);
            let rt_chroma_h = rt_pipeline("rt chroma h", &convert_layout, "fs_chroma_h", CHROMA_FORMAT);
            let rt_chroma_v = rt_pipeline("rt chroma v", &rt_v_layout, "fs_chroma_v", PLANE_FORMAT);

            let identity: Vec<u8> = (0..3).flat_map(|_| 0..=255u8).collect();
            let identity_lut = Self::lut_texture(&g, &identity);
            Ok(Self {
                gpu: Arc::clone(&g),
                resample_layout,
                compose_layout,
                convert_layout,
                resample_h,
                resample_v,
                compose,
                compose_chroma,
                convert,
                rt_rgb,
                rt_luma,
                rt_chroma_h,
                rt_chroma_v,
                rt_v_layout,
                identity_lut,
            })
        })
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

    /// One uniform buffer and a bind group over `views` for a round-trip pass.
    fn rt_bind(&self, layout: &wgpu::BindGroupLayout, params: &RoundTripParams, views: &[&wgpu::TextureView]) -> wgpu::BindGroup {
        let device = &self.gpu.device;
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("roundtrip params"),
            contents: bytemuck::bytes_of(params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mut entries = vec![wgpu::BindGroupEntry {
            binding: 0,
            resource: buf.as_entire_binding(),
        }];
        for (i, v) in views.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: i as u32 + 1,
                resource: wgpu::BindingResource::TextureView(v),
            });
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("roundtrip"),
            layout,
            entries: &entries,
        })
    }

    /// The layer's planes after FFmpeg's `yuva420p -> argb -> yuva420p` (see
    /// [`crate::roundtrip`]): out of YUV with the layer's matrix, then back with
    /// the composite's (`back`) and the chroma rebuilt from the RGB.
    fn translucent_planes(
        &self,
        enc: &mut wgpu::CommandEncoder,
        input: &[Plane; 3],
        forward: YuvMatrix,
        back: YuvMatrix,
    ) -> Result<[Plane; 3], GpuError> {
        let [py, pu, pv] = input;
        let (w, h) = (py.w, py.h);
        let (cw, ch) = (pu.w, pu.h);
        let t = Yuv2Rgb::new(forward);
        let b = Rgb2Yuv::new(back);
        let params = RoundTripParams {
            k: [t.crv, t.cbu, t.cgu, t.cgv],
            m: [t.cy, t.oy, 326 + 512, 0],
            ky: [b.ry, b.gy, b.by, 0],
            ku: [b.ru, b.gu, b.bu, 0],
            kv: [b.rv, b.gv, b.bv, 0],
            size: [w as i32, h as i32],
            csize: [cw as i32, ch as i32],
            channel: 0,
            taps: 0,
            pad: [0; 2],
        };
        let rgb = self.plane("rt rgb", w, h, CANVAS_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let bind = self.rt_bind(&self.compose_layout, &params, &[&py.view, &pu.view, &pv.view]);
        self.pass(enc, &self.rt_rgb, &rgb.view, &bind, None, None);

        let luma = self.plane(
            "rt y",
            w,
            h,
            PLANE_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let bind = self.rt_bind(&self.convert_layout, &params, &[&rgb.view]);
        self.pass(enc, &self.rt_luma, &luma.view, &bind, None, None);

        let inter = self.plane("rt chroma", cw, h, CHROMA_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
        self.pass(enc, &self.rt_chroma_h, &inter.view, &bind, None, None);

        let filter = sws::bicubic(h, ch, sws::ONE_VERTICAL)
            .ok_or_else(|| GpuError::Unsupported("a picture too tall for the chroma filter".into()))?;
        let taps = self.filter_texture(&filter);
        let mut out = Vec::with_capacity(2);
        for channel in 0..2 {
            let p = RoundTripParams {
                channel,
                taps: filter.size as i32,
                ..params
            };
            let plane = self.plane(
                "rt chroma plane",
                cw,
                ch,
                PLANE_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            );
            let bind = self.rt_bind(&self.rt_v_layout, &p, &[&inter.view, &taps]);
            self.pass(enc, &self.rt_chroma_v, &plane.view, &bind, None, None);
            out.push(plane);
        }
        let (Some(v), Some(u)) = (out.pop(), out.pop()) else {
            unreachable!("two chroma planes")
        };
        Ok([luma, u, v])
    }

    /// A swscale filter table as a texture: one row per output sample, the window
    /// start then the weights.
    fn filter_texture(&self, f: &Filter) -> wgpu::TextureView {
        let texels = f.texels();
        let tex = self.gpu.device.create_texture_with_data(
            &self.gpu.queue,
            &wgpu::TextureDescriptor {
                label: Some("scaler taps"),
                size: extent((f.size + 1) as u32, f.pos.len() as u32),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FILTER_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&texels),
        );
        tex.create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// One swscale stage over one plane: horizontal pass into a 15-bit
    /// intermediate, vertical pass into the 8-bit result.
    fn scale_plane(
        &self,
        enc: &mut wgpu::CommandEncoder,
        input: &Plane,
        stage: PlaneStage,
        filters: &PlaneFilters,
        lut: Option<(&wgpu::TextureView, u32)>,
    ) -> Plane {
        let device = &self.gpu.device;
        let PlaneStage { src, keep, .. } = stage;
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
            dst_off: [keep.x as i32, keep.y as i32],
            out_size: [keep.w as i32, keep.h as i32],
            apply_lut: 0,
            plane: 0,
            taps: filters.h.size as i32,
            pad: 0,
        };
        let (lut_view, lut_row) = lut.map_or((&self.identity_lut, 0), |(v, r)| (v, r));
        let bind = |params: ResampleParams, view: &wgpu::TextureView, taps: &wgpu::TextureView| {
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
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(taps),
                    },
                ],
            })
        };
        let h_taps = self.filter_texture(&filters.h);
        let v_taps = self.filter_texture(&filters.v);
        let h = bind(base, &input.view, &h_taps);
        self.pass(enc, &self.resample_h, &mid.view, &h, Some(wgpu::Color::BLACK), None);
        let v = bind(
            ResampleParams {
                apply_lut: u32::from(lut.is_some()),
                plane: lut_row,
                taps: filters.v.size as i32,
                ..base
            },
            &mid.view,
            &v_taps,
        );
        self.pass(enc, &self.resample_v, &out.view, &v, Some(wgpu::Color::BLACK), None);
        out
    }

    /// Composite `frames` (one decoded picture per plan layer; `None` is a layer
    /// whose source had no frame there, which FFmpeg draws nothing for) at `size`.
    ///
    /// Refuses a plan that is not [`RenderPlan::gpu_supported_at`] this size, a frame that does
    /// not match its layer's stream, or planes whose lengths do not match their
    /// size, so a caller that forgot to ask gets an error naming why instead of a
    /// wrong picture — or a wgpu panic.
    pub fn composite(&self, plan: &RenderPlan, frames: &[Option<YuvFrame>], size: (u32, u32)) -> Result<RgbaFrame, GpuError> {
        self.composite_as(plan, frames, size, Output::Rgb)
    }

    /// [`Compositor::composite`] stopping before the final YUV -> RGB conversion:
    /// the canvas as the 8-bit Y, U and V planes the layers were blended in (the
    /// chroma planes at half size). It exists so the scaler and the blend can be
    /// compared with FFmpeg's planes directly, without a conversion in between.
    pub fn composite_yuv(&self, plan: &RenderPlan, frames: &[Option<YuvFrame>], size: (u32, u32)) -> Result<YuvFrame, GpuError> {
        let canvas = self.composite_as(plan, frames, size, Output::Yuv)?;
        let (w, h) = (canvas.width as usize, canvas.height as usize);
        let at = |x: usize, y: usize, c: usize| canvas.data[(y * w + x) * 4 + c];
        let chroma = |c: usize| -> Vec<u8> {
            (0..h / 2)
                .flat_map(|y| (0..w / 2).map(move |x| (x, y)))
                .map(|(x, y)| at(x * 2, y * 2, c))
                .collect()
        };
        Ok(YuvFrame {
            width: canvas.width,
            height: canvas.height,
            y: (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| at(x, y, 0))
                .collect(),
            u: chroma(1),
            v: chroma(2),
        })
    }

    fn composite_as(
        &self,
        plan: &RenderPlan,
        frames: &[Option<YuvFrame>],
        size: (u32, u32),
        output: Output,
    ) -> Result<RgbaFrame, GpuError> {
        let reasons = plan.unsupported_reasons_at(size);
        if !reasons.is_empty() {
            return Err(GpuError::Unsupported(reasons.join("; ")));
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
        let limit = self.gpu.device.limits().max_texture_dimension_2d;
        if ow > limit || oh > limit {
            return Err(GpuError::Unsupported(format!(
                "a {ow}x{oh} canvas is over this device's {limit}px texture limit"
            )));
        }

        // Everything that can be refused is refused before the GPU is touched.
        let mut drawn = Vec::with_capacity(frames.len());
        for (n, (layer, frame)) in plan.layers.iter().zip(frames).enumerate() {
            let Some(frame) = frame else { continue };
            if !frame.is_consistent() {
                return Err(GpuError::Decode(format!(
                    "layer {n}: the planes do not add up to a {}x{} 4:2:0 picture",
                    frame.width, frame.height
                )));
            }
            if (frame.width, frame.height) != (layer.stream.width, layer.stream.height) {
                return Err(GpuError::Unsupported(format!(
                    "layer {n}: a {}x{} frame for a {}x{} stream",
                    frame.width, frame.height, layer.stream.width, layer.stream.height
                )));
            }
            let geom = LayerGeometry::resolve((frame.width, frame.height), size, plan.canvas.fit, &layer.transform)
                .map_err(|e| GpuError::Unsupported(format!("layer {n}: {e}")))?;
            // wgpu raises a validation error for a texture over the device's
            // limit — a 10x zoom of a 4K layer would be one. Refuse it instead,
            // so the frame goes to FFmpeg.
            let biggest = geom
                .stages
                .iter()
                .flat_map(|s| [s.src.w, s.src.h, s.keep.w, s.keep.h, s.scaled.0, s.scaled.1])
                .chain([frame.width, frame.height, geom.layer.0, geom.layer.1])
                .max()
                .unwrap_or(0);
            if biggest > limit {
                return Err(GpuError::Unsupported(format!(
                    "layer {n}: a {biggest}px texture is over this device's {limit}px limit"
                )));
            }
            // swscale's tables for every plane of every stage; a ratio it would
            // need two scalers for is FFmpeg's to render.
            let mut filters = Vec::with_capacity(geom.stages.len());
            for stage in &geom.stages {
                let table = |chroma: bool| -> Result<PlaneFilters, GpuError> {
                    let ps = plane_stage(stage, chroma);
                    let too_far = || GpuError::Unsupported(format!("layer {n}: a scale ratio swscale needs two passes for"));
                    Ok(PlaneFilters {
                        h: sws::bicubic(ps.src.w, ps.scaled.0, sws::ONE_HORIZONTAL).ok_or_else(too_far)?,
                        v: sws::bicubic(ps.src.h, ps.scaled.1, sws::ONE_VERTICAL).ok_or_else(too_far)?,
                    })
                };
                filters.push([table(false)?, table(true)?, table(true)?]);
            }
            drawn.push(Drawn {
                layer,
                frame,
                geom,
                filters,
            });
        }
        self.gpu
            .guarded("compositing a frame", || self.draw(plan, &drawn, size, output))
    }

    /// Record, submit and read back the passes for the layers that are drawn.
    fn draw(&self, plan: &RenderPlan, drawn: &[Drawn], size: (u32, u32), output: Output) -> Result<RgbaFrame, GpuError> {
        let (ow, oh) = size;
        let device = &self.gpu.device;
        // The canvas holds Y, U, V (and alpha) in an RGBA8 texture: FFmpeg's
        // `overlay` blends the encoded YUV planes, so this does too, and the one
        // conversion to RGB happens at the very end.
        let canvas = self.plane(
            "canvas",
            ow,
            oh,
            CANVAS_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
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

        let (coef, gv) = matrix_coefs(plan.canvas.matrix);

        for Drawn {
            layer,
            frame,
            geom,
            filters,
        } in drawn
        {
            let (cw, ch) = frame.chroma_size();
            let planes = [
                self.upload("y", frame.width, frame.height, &frame.y),
                self.upload("u", cw, ch, &frame.u),
                self.upload("v", cw, ch, &frame.v),
            ];

            // `eq` tables, applied on the final stage only.
            let eq_rows = eq::luts(&layer.color);
            let tables = eq_rows.as_ref().map(|rows| {
                let flat: Vec<u8> = rows.iter().flatten().copied().collect();
                Self::lut_texture(&self.gpu, &flat)
            });
            // `pad`'s black bars go through the same tables (they are part of the
            // frame `eq` sees): Y 16, U = V = 128 mapped through them.
            let matte = eq_rows.as_ref().map_or([16.0, 128.0, 128.0], |t| {
                [f32::from(t[0][16]), f32::from(t[1][128]), f32::from(t[2][128])]
            });

            let mut current = planes;
            for (si, stage) in geom.stages.iter().enumerate() {
                let last = si + 1 == geom.stages.len();
                let mut next: Vec<Plane> = Vec::with_capacity(3);
                for (pi, input) in current.iter().enumerate() {
                    let ps = plane_stage(stage, pi > 0);
                    let lut = last.then_some(tables.as_ref()).flatten().map(|t| (t, pi as u32));
                    // A stage that changes nothing — the whole plane, at its own
                    // size, no table to apply — is the plane itself. FFmpeg's
                    // `scale` is a passthrough in the same case, and it is by far
                    // the common one (footage already at the delivery size).
                    let identity = lut.is_none()
                        && ps.src == Rect::whole(input.w, input.h)
                        && ps.keep == Rect::whole(ps.src.w, ps.src.h)
                        && ps.scaled == (ps.src.w, ps.src.h);
                    next.push(if identity {
                        input.clone()
                    } else {
                        self.scale_plane(&mut enc, input, ps, &filters[si][pi], lut)
                    });
                }
                current = next.try_into().unwrap_or_else(|_| unreachable!("three planes in, three out"));
            }
            // Below full opacity FFmpeg takes the layer through RGB and back before
            // `overlay` sees it (see `roundtrip.rs`); that happens to the scaled
            // picture, ahead of the rotation. An opaque layer never leaves YUV.
            if geom.translucent {
                // Out of YCbCr with the layer's own matrix; back with the
                // composite's (BT.601 under a fixed policy, the negotiated one else).
                let forward = layer.stream.matrix().unwrap_or(YuvMatrix::Bt601);
                current = self.translucent_planes(&mut enc, &current, forward, plan.canvas.matrix)?;
            }
            let [py, pu, pv] = &current;

            let (rotate, rot) = match geom.rotation {
                Some(r) => (1, [r.angle.cos() as f32, r.angle.sin() as f32]),
                None => (0, [1.0, 0.0]),
            };
            let (lw, lh) = geom.layer;
            let (clw, clh) = chroma_size(lw, lh);
            let (pcw, pch) = chroma_size(geom.picture.0, geom.picture.1);
            let mut params = ComposeParams {
                opacity: [geom.opacity, 0.0, 0.0, 0.0],
                matte: [matte[0], matte[1], matte[2], 0.0],
                origin: [geom.origin.0, geom.origin.1],
                layer: [lw as i32, lh as i32],
                pic: [geom.picture.0 as i32, geom.picture.1 as i32],
                cpic: [pcw as i32, pch as i32],
                clayer: [clw as i32, clh as i32],
                rot,
                pic_at: [geom.picture_at.0 as i32, geom.picture_at.1 as i32],
                pic_shows: [geom.picture_shows.0 as i32, geom.picture_shows.1 as i32],
                rotate,
                tail: 0,
                pad: [0; 2],
            };
            // Only the part of the canvas the layer can touch.
            let x0 = i64::from(geom.origin.0).max(0);
            let y0 = i64::from(geom.origin.1).max(0);
            let x1 = (i64::from(geom.origin.0) + i64::from(lw)).min(i64::from(ow));
            let y1 = (i64::from(geom.origin.1) + i64::from(lh)).min(i64::from(oh));
            if x1 <= x0 || y1 <= y0 {
                continue; // entirely off the canvas
            }
            let views = [&py.view, &pu.view, &pv.view];
            let bind = self.compose_bind(&params, views);
            self.pass(
                &mut enc,
                &self.compose,
                &canvas.view,
                &bind,
                None,
                Some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)),
            );

            // `overlay` blends whole chroma samples: the half block an odd layer
            // ends in reaches one pixel past it, carrying the layer's chroma onto
            // the base's luma. Reproduced for a layer that is not rotated (a
            // rotated one has soft, partly transparent edges there).
            let odd = (lw % 2 == 1, lh % 2 == 1);
            let past_x = i64::from(geom.origin.0) + i64::from(lw);
            let past_y = i64::from(geom.origin.1) + i64::from(lh);
            if rotate == 0 && (odd.0 || odd.1) {
                params.tail = 1;
                let bind = self.compose_bind(&params, views);
                let tx1 = (past_x + i64::from(odd.0)).min(i64::from(ow));
                let ty1 = (past_y + i64::from(odd.1)).min(i64::from(oh));
                self.pass(
                    &mut enc,
                    &self.compose_chroma,
                    &canvas.view,
                    &bind,
                    None,
                    Some((x0 as u32, y0 as u32, (tx1 - x0) as u32, (ty1 - y0) as u32)),
                );
            }
        }

        if output == Output::Yuv {
            return self.read_back(enc, &canvas);
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

    fn compose_bind(&self, params: &ComposeParams, planes: [&wgpu::TextureView; 3]) -> wgpu::BindGroup {
        let device = &self.gpu.device;
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("compose params"),
            contents: bytemuck::bytes_of(params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compose"),
            layout: &self.compose_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(planes[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(planes[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(planes[2]),
                },
            ],
        })
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
            // The receiver outlives the wait below unless that gave up, in which
            // case nobody wants the result.
            let _ = tx.send(r);
        });
        // A wait that cannot hang the caller: a GPU that stops answering is an
        // error (the owner falls back to FFmpeg and may rebuild the device), not a
        // thread stuck in `poll` forever.
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(READBACK_TIMEOUT),
            })
            .map_err(|e| GpuError::Readback(format!("the GPU did not finish within {} s: {e}", READBACK_TIMEOUT.as_secs())))?;
        rx.recv_timeout(Duration::from_secs(1))
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
        let reasons = plan.unsupported_reasons_at(size);
        if !reasons.is_empty() {
            return Err(GpuError::Unsupported(reasons.join("; ")));
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

/// A scale stage as one plane sees it.
#[derive(Clone, Copy)]
struct PlaneStage {
    src: Rect,
    scaled: (u32, u32),
    keep: Rect,
}

/// The two tables of one plane's stage.
struct PlaneFilters {
    h: Filter,
    v: Filter,
}

/// A scale stage as one plane sees it. The chroma planes are half the size
/// (rounded up) and are scaled on their own grid: the crop and Cover offsets are
/// even by construction, so they halve exactly.
fn plane_stage(stage: &ScaleStage, chroma: bool) -> PlaneStage {
    if !chroma {
        return PlaneStage {
            src: stage.src,
            scaled: stage.scaled,
            keep: stage.keep,
        };
    }
    let half = |v: u32| v.div_ceil(2);
    PlaneStage {
        src: Rect {
            x: stage.src.x / 2,
            y: stage.src.y / 2,
            w: half(stage.src.w),
            h: half(stage.src.h),
        },
        scaled: (half(stage.scaled.0), half(stage.scaled.1)),
        keep: Rect {
            x: stage.keep.x / 2,
            y: stage.keep.y / 2,
            w: half(stage.keep.w),
            h: half(stage.keep.h),
        },
    }
}
