//! The blurred fill's up-scale when FFmpeg 9 makes it convert the picture's matrix as well (`fill_convert.wgsl`; the arithmetic and what
//! it was held to are in [`crate::convert`], `rgb_scale_up`).
//!
//! A dissolving clip's or a still's scales are tied to the composite's colour space, the fill's up-scale (`scale=W:H:flags=bilinear`
//! from the blurred thumbnail) among them. `sws_setColorspaceDetails` builds its two contexts around a `bgr24` picture of the smaller of
//! the two sizes, which for an enlargement is the thumbnail's: `yuv420p -> bgr24` unscaled with the source's coefficients (the x86 kernel
//! the conversion's way in is), then `bgr24 -> yuv420p` **scaled** to the frame with the destination's, by the generic scaler (the colour
//! of every pixel as 14 bits, a horizontal and a vertical pass). The passes here: the way in, a horizontal pass per plane
//! (`fs_h` over the RGB) and the resampler's vertical pass.

use super::*;
use crate::convert::EnlargeTables;

/// `fill_convert.wgsl`'s uniform.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct HParams {
    ky: [i32; 4],
    ku: [i32; 4],
    kv: [i32; 4],
}

/// The pipeline of the horizontal pass over an RGB picture.
pub(super) struct FillConvert {
    layout: wgpu::BindGroupLayout,
    horizontal: wgpu::RenderPipeline,
}

impl FillConvert {
    pub(super) fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kerf fill convert"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/fill_convert.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fill convert"),
            entries: &[uniform_entry(0), texture_entry(1), texture_entry(2)],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fill convert"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let horizontal = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fill convert horizontal"),
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
                entry_point: Some("fs_h"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: MID_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self { layout, horizontal }
    }
}

impl Compositor {
    /// `input` (the blurred thumbnail's `yuv420p` planes, even sides) enlarged to `frame` and taken from the matrix `from` to `to` as
    /// swscale's cascade does it ([`crate::convert::enlarge_and_change_matrix`]). What the model does not follow
    /// ([`EnlargeTables::of`]) is `GpuError::Unsupported`.
    pub(super) fn enlarge_convert_planes(
        &self,
        enc: &mut wgpu::CommandEncoder,
        input: &[Plane; 3],
        frame: (u32, u32),
        from: YuvMatrix,
        to: YuvMatrix,
    ) -> Result<[Plane; 3], GpuError> {
        let [py, pu, _] = input;
        let (tw, th) = (py.w, py.h);
        let (fw, fh) = frame;
        if (pu.w, pu.h) != (tw / 2, th / 2) {
            return Err(GpuError::Unsupported(format!(
                "a {tw}x{th} thumbnail whose chroma planes are not half its size"
            )));
        }
        let Some(EnlargeTables { hl, hc, vl, vc }) = EnlargeTables::of((tw as usize, th as usize), (fw as usize, fh as usize))
        else {
            return Err(GpuError::Unsupported(format!(
                "a {tw}x{th} thumbnail enlarged to {fw}x{fh} as the matrix is converted (not a size the cascade's model follows)"
            )));
        };
        // The way in: `yuv420p -> bgr24` at the thumbnail's size, with the source's coefficients.
        let rgb = self.rgb_of(enc, input, &Self::kernel_params(from, 0));
        let k = Rgb2Yuv::new(to);
        // (The tables are an x86 build's and the vertical pass is its kernel's, as in the cascade's other half: `convert::scaled` measures
        // that this FFmpeg runs them, and a machine whose scaler is another is `Differs` there.)
        let device = &self.gpu.device;
        let mut plane = |channel: i32, horizontal: &Filter, vertical: &Filter, out: (u32, u32), c_rows: u32| -> Plane {
            let params = HParams {
                ky: [k.ry, k.gy, k.by, horizontal.size as i32],
                ku: [k.ru, k.gu, k.bu, channel],
                kv: [k.rv, k.gv, k.bv, tw as i32],
            };
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("fill convert params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let h_taps = self.filter_texture(horizontal);
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("fill convert"),
                layout: &self.fill_convert.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&rgb.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&h_taps),
                    },
                ],
            });
            // The horizontal pass: the plane's width, a row for each row of the thumbnail, 15 bits.
            let mid = self.plane(
                "fill convert mid",
                out.0,
                th,
                MID_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            );
            self.pass(
                enc,
                &self.fill_convert.horizontal,
                &mid.view,
                &bind,
                Some(wgpu::Color::BLACK),
                None,
            );
            // The vertical pass: the resampler's, in the x86 kernel's arithmetic on every row but the last ones.
            let out_plane = self.plane(
                "fill convert plane",
                out.0,
                out.1,
                PLANE_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            );
            let params = ResampleParams {
                src_off: [0, 0],
                src_size: [out.0 as i32, th as i32],
                dst_off: [0, 0],
                out_size: [out.0 as i32, out.1 as i32],
                apply_lut: 0,
                plane: 0,
                taps: vertical.size as i32,
                x86: if vertical.size > 1 {
                    crate::scaler::vertical_start(vertical.size) as u32
                } else {
                    0
                },
                c_from: out.1 as i32 - c_rows as i32,
                pad: [0; 3],
            };
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("resample params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let v_taps = self.filter_texture(vertical);
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.resample_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&mid.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&self.identity_lut),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&v_taps),
                    },
                ],
            });
            self.pass(enc, &self.resample_v, &out_plane.view, &bind, Some(wgpu::Color::BLACK), None);
            out_plane
        };
        Ok([
            plane(0, &hl, &vl, (fw, fh), 2),
            plane(1, &hc, &vc, (fw / 2, fh / 2), 1),
            plane(2, &hc, &vc, (fw / 2, fh / 2), 1),
        ])
    }

    /// `frame` (a thumbnail) enlarged to `size` and taken from `from` to `to` on the GPU, read back: what the compositor draws of a blurred
    /// fill's up-scale that converts the matrix too, for the plane-level comparison with [`crate::convert::enlarge_and_change_matrix`]
    /// (`tests/convert.rs`).
    pub fn enlarge_convert_yuv(
        &self,
        frame: &YuvFrame,
        size: (u32, u32),
        from: YuvMatrix,
        to: YuvMatrix,
    ) -> Result<YuvFrame, GpuError> {
        let (w, h) = (frame.width, frame.height);
        if !frame.is_consistent() {
            return Err(GpuError::Unsupported(format!("an enlargement of a {w}x{h} frame")));
        }
        let (cw, ch) = chroma_size(w, h);
        let planes = [
            self.upload("enlarge convert y", w, h, &frame.y),
            self.upload("enlarge convert u", cw, ch, &frame.u),
            self.upload("enlarge convert v", cw, ch, &frame.v),
        ];
        let (ow, oh) = size;
        let (ocw, och) = chroma_size(ow, oh);
        let gpu = &self.gpu;
        gpu.guarded("enlarging and converting a picture", || {
            let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("enlarge convert"),
            });
            let [y, u, v] = self.enlarge_convert_planes(&mut enc, &planes, size, from, to)?;
            let luma = read_texels(gpu, enc, &y.texture, ow, oh, 1)?.data;
            let idle = || {
                gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("enlarge convert"),
                })
            };
            let u = read_texels(gpu, idle(), &u.texture, ocw, och, 1)?.data;
            let v = read_texels(gpu, idle(), &v.texture, ocw, och, 1)?.data;
            Ok(YuvFrame {
                width: ow,
                height: oh,
                y: luma,
                u,
                v,
            })
        })
    }
}
