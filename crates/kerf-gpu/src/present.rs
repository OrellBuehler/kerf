//! Showing a rendered frame in a window: [`Presenter`] owns a surface and draws a
//! [`RenderedFrame`] into a rectangle of it.
//!
//! The compositor's output is the finished picture as encoded RGB — the values a screen is to
//! show, the same ones FFmpeg's still writes. The presenter does nothing to them: it picks a
//! **non-sRGB** surface format where the adapter offers one (so the hardware does not encode
//! them a second time), and decodes them in the shader first where it does not. The frame is
//! drawn 1:1 when the rectangle is its size (a texel centre per pixel, exact) and bilinear
//! otherwise; the rest of the surface is a matte colour the caller chooses (the app's own
//! background, so a gap between panels that a transparent webview lets through looks like the
//! panel).
//!
//! Like every GPU call here, nothing in this module panics on a GPU failure: the work runs in
//! [`Gpu::guarded`]'s error scopes and a surface that cannot be used is a [`GpuError::Surface`]
//! (an occluded or timed-out acquire is one too: the caller skips the frame). A surface that
//! reports `Outdated` or `Lost` is reconfigured once and tried again.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::compositor::RenderedFrame;
use crate::gpu::{Gpu, GpuError};

/// A rectangle of target pixels (top-left origin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    /// The part of the rectangle inside a `size` target, or `None` when nothing is.
    pub fn clamped(self, size: (u32, u32)) -> Option<PixelRect> {
        let x1 = self.x.saturating_add(self.width).min(size.0);
        let y1 = self.y.saturating_add(self.height).min(size.1);
        (self.x < x1 && self.y < y1).then(|| PixelRect {
            x: self.x,
            y: self.y,
            width: x1 - self.x,
            height: y1 - self.y,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlitParams {
    dest: [f32; 4],
    matte: [f32; 4],
    flags: [u32; 4],
}

/// The pass that draws a frame into a target: a pipeline for one target format.
pub(crate) struct Blit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    srgb: bool,
}

impl Blit {
    pub(crate) fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kerf present"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/present.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("present"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("present"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("present"),
            layout: Some(&pipeline_layout),
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
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("present"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            layout,
            sampler,
            srgb: format.is_srgb(),
        }
    }

    /// Record and submit one pass: `frame` into `dest` of `target`, the rest `matte`.
    /// `dest` is `None` for a pass that is all matte.
    pub(crate) fn draw(
        &self,
        gpu: &Gpu,
        frame: Option<&RenderedFrame>,
        target: &wgpu::TextureView,
        dest: Option<PixelRect>,
        matte: [u8; 3],
    ) {
        let device = &gpu.device;
        let encoded = |c: u8| f32::from(c) / 255.0;
        // No rectangle is an empty one: no pixel is ever inside it.
        let rect = dest.unwrap_or(PixelRect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        });
        let params = BlitParams {
            dest: [rect.x as f32, rect.y as f32, rect.width as f32, rect.height as f32],
            matte: [encoded(matte[0]), encoded(matte[1]), encoded(matte[2]), 1.0],
            flags: [u32::from(self.srgb), 0, 0, 0],
        };
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("present params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        // A pass with no frame still binds a texture; the shader reads it only inside the
        // rectangle, and a matte-only pass has none.
        let placeholder;
        let view = match frame.filter(|_| dest.is_some()) {
            Some(f) => f.texture.create_view(&wgpu::TextureViewDescriptor::default()),
            None => {
                placeholder = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("present placeholder"),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                placeholder.create_view(&wgpu::TextureViewDescriptor::default())
            }
        };
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("present"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("kerf present"),
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        gpu.queue.submit(Some(enc.finish()));
    }
}

/// A window surface and what it takes to draw frames into it.
pub struct Presenter {
    gpu: Arc<Gpu>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    blit: Blit,
    size: (u32, u32),
}

impl Presenter {
    /// Configure `surface` (from [`Gpu::new_for_surface`] or [`Gpu::create_surface`]) at
    /// `size` physical pixels. A surface the adapter cannot present to is a
    /// [`GpuError::Surface`], and the caller keeps its FFmpeg preview.
    pub fn new(gpu: Arc<Gpu>, surface: wgpu::Surface<'static>, size: (u32, u32)) -> Result<Self, GpuError> {
        let size = (size.0.max(1), size.1.max(1));
        let caps = surface.get_capabilities(&gpu.adapter);
        if caps.formats.is_empty() {
            return Err(GpuError::Surface(
                "the adapter cannot present to this surface (no format in common)".into(),
            ));
        }
        // Encoded values go out as written: not an sRGB format if the adapter has another.
        let format = [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm]
            .into_iter()
            .find(|f| caps.formats.contains(f))
            .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
            .unwrap_or(caps.formats[0]);
        // The webview is what is transparent; the surface itself never is.
        let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            caps.alpha_modes.first().copied().unwrap_or(wgpu::CompositeAlphaMode::Auto)
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.0,
            height: size.1,
            desired_maximum_frame_latency: 2,
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode,
            view_formats: vec![],
        };
        let blit = Blit::new(&gpu, format);
        let this = Self {
            gpu,
            surface,
            config,
            blit,
            size,
        };
        this.configure()?;
        tracing::info!(
            ?format,
            ?alpha_mode,
            width = size.0,
            height = size.1,
            "kerf-gpu presenter ready"
        );
        Ok(this)
    }

    fn configure(&self) -> Result<(), GpuError> {
        self.gpu.guarded("configuring the surface", || {
            self.surface.configure(&self.gpu.device, &self.config);
            Ok(())
        })
    }

    /// The surface's size in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The format the surface was configured with.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Follow the window's new size (a no-op when it is the one configured).
    pub fn resize(&mut self, size: (u32, u32)) -> Result<(), GpuError> {
        let size = (size.0.max(1), size.1.max(1));
        if size == self.size {
            return Ok(());
        }
        self.config.width = size.0;
        self.config.height = size.1;
        self.configure()?;
        self.size = size;
        Ok(())
    }

    /// Draw `frame` into `dest` (clamped to the surface) and the rest of the surface in `matte`,
    /// then present. An empty rectangle is a surface of matte alone.
    pub fn present(&mut self, frame: &RenderedFrame, dest: PixelRect, matte: [u8; 3]) -> Result<(), GpuError> {
        self.draw(Some(frame), dest.clamped(self.size), matte)
    }

    /// The whole surface in `matte`.
    pub fn clear(&mut self, matte: [u8; 3]) -> Result<(), GpuError> {
        self.draw(None, None, matte)
    }

    fn draw(&mut self, frame: Option<&RenderedFrame>, dest: Option<PixelRect>, matte: [u8; 3]) -> Result<(), GpuError> {
        let gpu = Arc::clone(&self.gpu);
        gpu.guarded("presenting a frame", || {
            let output = self.acquire()?;
            let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.blit.draw(&self.gpu, frame, &view, dest, matte);
            drop(view);
            self.gpu.queue.present(output);
            Ok(())
        })
    }

    /// The next texture to draw to, reconfiguring once when the surface says it is stale.
    fn acquire(&self) -> Result<wgpu::SurfaceTexture, GpuError> {
        use wgpu::CurrentSurfaceTexture as Current;
        for attempt in 0..2 {
            match self.surface.get_current_texture() {
                Current::Success(t) => return Ok(t),
                // Usable, and the next acquire is better after a reconfigure: do that now,
                // while no texture is out.
                Current::Suboptimal(t) => return Ok(t),
                Current::Outdated | Current::Lost if attempt == 0 => {
                    self.configure()?;
                }
                Current::Outdated => return Err(GpuError::Surface("the surface stayed outdated after a reconfigure".into())),
                Current::Lost => return Err(GpuError::Surface("the surface was lost".into())),
                Current::Timeout => return Err(GpuError::Surface("acquiring the next surface texture timed out".into())),
                Current::Occluded => return Err(GpuError::Surface("the window is occluded".into())),
                Current::Validation => return Err(GpuError::Gpu("acquiring the surface texture failed validation".into())),
            }
        }
        Err(GpuError::Surface("no surface texture could be acquired".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::RenderedFrame;
    use crate::gpu::GpuOptions;

    #[test]
    fn a_rectangle_is_clamped_into_the_target_and_vanishes_outside_it() {
        let r = |x, y, width, height| PixelRect { x, y, width, height };
        assert_eq!(r(10, 20, 30, 40).clamped((100, 100)), Some(r(10, 20, 30, 40)));
        assert_eq!(r(90, 90, 30, 30).clamped((100, 100)), Some(r(90, 90, 10, 10)));
        assert_eq!(r(100, 0, 5, 5).clamped((100, 100)), None);
        assert_eq!(r(0, 0, 0, 5).clamped((100, 100)), None);
        assert_eq!(r(u32::MAX, 0, u32::MAX, 5).clamped((100, 100)), None);
    }

    fn texture(gpu: &Gpu, w: u32, h: u32, data: &[u8], format: wgpu::TextureFormat) -> wgpu::Texture {
        gpu.device.create_texture_with_data(
            &gpu.queue,
            &wgpu::TextureDescriptor {
                label: Some("test frame"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            data,
        )
    }

    /// Blit `frame` into `dest` of an empty `size` target of `format` and read it back (RGBA).
    fn blit_and_read(
        gpu: &Gpu,
        frame: &RenderedFrame,
        size: (u32, u32),
        dest: PixelRect,
        matte: [u8; 3],
        format: wgpu::TextureFormat,
    ) -> Vec<u8> {
        let blit = Blit::new(gpu, format);
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test target"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        blit.draw(
            gpu,
            Some(frame),
            &target.create_view(&wgpu::TextureViewDescriptor::default()),
            dest.clamped(size),
            matte,
        );
        let pitch = (size.0 * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test readback"),
            size: u64::from(pitch) * u64::from(size.1),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(pitch),
                    rows_per_image: Some(size.1),
                },
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit(Some(enc.finish()));
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the GPU finished");
        let mapped = buffer.slice(..).get_mapped_range().expect("mapped");
        let mut out = Vec::new();
        for row in mapped.chunks(pitch as usize).take(size.1 as usize) {
            out.extend_from_slice(&row[..(size.0 * 4) as usize]);
        }
        out
    }

    fn pixel(data: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * width + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    }

    /// A 4x2 frame whose pixels are all different.
    fn test_frame(gpu: &Gpu) -> RenderedFrame {
        let data: Vec<u8> = (0..8u8).flat_map(|i| [i * 30 + 5, 255 - i * 20, i * 7, 255]).collect();
        RenderedFrame {
            texture: texture(gpu, 4, 2, &data, wgpu::TextureFormat::Rgba8Unorm),
            width: 4,
            height: 2,
        }
    }

    #[test]
    #[ignore = "needs a GPU adapter (lavapipe is enough)"]
    fn a_frame_drawn_at_its_own_size_lands_exactly_and_the_rest_is_matte() {
        let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
        let frame = test_frame(&gpu);
        let size = (10, 6);
        let dest = PixelRect {
            x: 3,
            y: 2,
            width: 4,
            height: 2,
        };
        let matte = [10, 20, 30];
        let out = blit_and_read(&gpu, &frame, size, dest, matte, wgpu::TextureFormat::Rgba8Unorm);
        for y in 0..size.1 {
            for x in 0..size.0 {
                let got = pixel(&out, size.0, x, y);
                let inside = (3..7).contains(&x) && (2..4).contains(&y);
                let expect = if inside {
                    let i = (y - 2) * 4 + (x - 3);
                    let i = i as u8;
                    [i * 30 + 5, 255 - i * 20, i * 7, 255]
                } else {
                    [10, 20, 30, 255]
                };
                assert_eq!(got, expect, "pixel ({x}, {y})");
            }
        }
    }

    #[test]
    #[ignore = "needs a GPU adapter (lavapipe is enough)"]
    fn a_target_in_bgra_order_and_one_in_srgb_show_the_same_values() {
        let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
        let frame = test_frame(&gpu);
        let size = (4, 2);
        let whole = PixelRect {
            x: 0,
            y: 0,
            width: 4,
            height: 2,
        };
        let rgba = blit_and_read(&gpu, &frame, size, whole, [0; 3], wgpu::TextureFormat::Rgba8Unorm);
        let bgra = blit_and_read(&gpu, &frame, size, whole, [0; 3], wgpu::TextureFormat::Bgra8Unorm);
        let srgb = blit_and_read(&gpu, &frame, size, whole, [0; 3], wgpu::TextureFormat::Rgba8UnormSrgb);
        for i in 0..8 {
            let (r, b, s) = (
                pixel(&rgba, 4, i % 4, i / 4),
                pixel(&bgra, 4, i % 4, i / 4),
                pixel(&srgb, 4, i % 4, i / 4),
            );
            // The read-back of a BGRA texture is in its own channel order.
            assert_eq!([r[0], r[1], r[2]], [b[2], b[1], b[0]], "bgra pixel {i}");
            // The sRGB target decodes in the shader and the hardware encodes: the stored byte
            // is the one a non-sRGB target would hold, within the round trip's rounding.
            for c in 0..3 {
                assert!(r[c].abs_diff(s[c]) <= 1, "srgb pixel {i} channel {c}: {} vs {}", r[c], s[c]);
            }
        }
    }

    #[test]
    #[ignore = "needs a GPU adapter (lavapipe is enough)"]
    fn a_frame_drawn_smaller_is_smoothed_not_dropped() {
        let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
        // Black left half, white right half, drawn at half the width.
        let data: Vec<u8> = (0..4)
            .flat_map(|x| if x < 2 { [0, 0, 0, 255] } else { [255, 255, 255, 255] })
            .collect();
        let frame = RenderedFrame {
            texture: texture(&gpu, 4, 1, &data, wgpu::TextureFormat::Rgba8Unorm),
            width: 4,
            height: 1,
        };
        let dest = PixelRect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        let out = blit_and_read(&gpu, &frame, (2, 1), dest, [0; 3], wgpu::TextureFormat::Rgba8Unorm);
        // Each pixel centre falls on the boundary between two texels of one colour.
        assert_eq!(pixel(&out, 2, 0, 0)[0], 0);
        assert_eq!(pixel(&out, 2, 1, 0)[0], 255);
    }
}
