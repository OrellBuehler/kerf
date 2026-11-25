//! A picture that swscale scales to a smaller size and takes from one colour matrix to another in the one `scale`
//! (`scaled_convert.wgsl`; the arithmetic and what it was held to are in [`crate::convert`], `scaled_to_rgb24`).
//!
//! FFmpeg 9's `scale` with different matrices in and out runs swscale's cascade: `yuv420p -> bgr24` **at the destination's size**
//! (the smaller of the two) in one scaler with the source's coefficients, then `bgr24 -> yuv420p` with the destination's by
//! `ff_rgb24toyv12`. A still's last `scale` and a dissolving clip's fit `scale` are that. The passes: the horizontal pass of the
//! resampler for the luma and for each chroma plane (15 bits), the vertical pass into RGB, and the conversion back that
//! [`Compositor::convert_matrix`] ends in.

use super::*;
use crate::grade::X86Kernel;
use crate::roundtrip::Yuv2Rgb;

/// `scaled_convert.wgsl`'s uniform.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ScaledParams {
    /// `yCoeff`, `vrCoeff`, `ubCoeff`, `vgCoeff`.
    k: [i32; 4],
    /// `ugCoeff`, `yOffset`, zero, zero.
    k2: [i32; 4],
    /// The C tables' `crv`, `cbu`, `cgu`, `cgv`.
    c: [i32; 4],
    /// The C tables' `cy`, `oy`, zero, zero.
    c2: [i32; 4],
    /// The luma taps, the chroma taps, the luma rows of the source, the rows the MMX kernel does (all but the last two).
    sizes: [i32; 4],
    /// The chroma rows of the source, zeros.
    chroma_rows: [i32; 4],
}

/// The pipeline of the vertical pass into RGB.
pub(super) struct ScaledConvert {
    layout: wgpu::BindGroupLayout,
    rgb: wgpu::RenderPipeline,
}

impl ScaledConvert {
    pub(super) fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kerf scaled convert"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/scaled_convert.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scaled convert"),
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
            label: Some("scaled convert"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let rgb = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scaled convert rgb"),
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
                entry_point: Some("fs_scaled_rgb"),
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
        Self { layout, rgb }
    }
}

impl Compositor {
    /// The horizontal pass of the resampler over one plane: the window `src` of `input`, scaled to `out_w` columns by `table`,
    /// into a 15-bit intermediate of `src.h` rows.
    fn horizontal_15(&self, enc: &mut wgpu::CommandEncoder, input: &Plane, src: Rect, out_w: u32, table: &Filter) -> Plane {
        let device = &self.gpu.device;
        let mid = self.plane("scaled mid", out_w, src.h, MID_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let params = ResampleParams {
            src_off: [src.x as i32, src.y as i32],
            src_size: [src.w as i32, src.h as i32],
            dst_off: [0, 0],
            out_size: [out_w as i32, src.h as i32],
            apply_lut: 0,
            plane: 0,
            taps: table.size as i32,
            x86: 0,
            c_from: 0,
            pad: [0; 3],
        };
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("resample params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let taps = self.filter_texture(table);
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.resample_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input.view),
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
                    resource: wgpu::BindingResource::TextureView(&taps),
                },
            ],
        });
        self.pass(enc, &self.resample_h, &mid.view, &bind, Some(wgpu::Color::BLACK), None);
        mid
    }

    /// One scale `stage` of `input`'s planes (a `yuv420p` picture, even sides) that also takes the picture from the matrix `from`
    /// to `to`, as swscale's cascade does ([`crate::convert::scaled_to_rgb24`]). What is not the cascade's
    /// ([`ScaleStage::cascade_refusal`]) is `GpuError::Unsupported`.
    pub(super) fn scale_convert_planes(
        &self,
        enc: &mut wgpu::CommandEncoder,
        input: &[Plane; 3],
        stage: &ScaleStage,
        from: YuvMatrix,
        to: YuvMatrix,
    ) -> Result<[Plane; 3], GpuError> {
        if let Some(why) = stage.cascade_refusal() {
            return Err(GpuError::Unsupported(why.to_string()));
        }
        let too_far =
            || GpuError::Unsupported("a scale that converts the matrix, at a ratio swscale needs two passes for".into());
        let (sw, sh) = (stage.src.w, stage.src.h);
        let (dw, dh) = stage.scaled;
        let (scw, sch, dcw) = (sw / 2, sh / 2, dw / 2);
        let (ah, av) = (sws::SIMD_ALIGN_HORIZONTAL, sws::SIMD_ALIGN_VERTICAL);
        let table = |src: u32, dst: u32, one: i32, align: usize| {
            sws::table_aligned(Scaler::Bicubic, src, dst, one, align).ok_or_else(too_far)
        };
        let hl = table(sw, dw, sws::ONE_HORIZONTAL, ah)?;
        let hc = table(scw, dcw, sws::ONE_HORIZONTAL, ah)?;
        let vl = table(sh, dh, sws::ONE_VERTICAL, av)?;
        let vc = table(sch, dh, sws::ONE_VERTICAL, av)?;
        if vl.size <= 2 {
            return Err(GpuError::Unsupported(
                "a scale that converts the matrix, with a luma vertical filter of one or two taps".into(),
            ));
        }
        let window = Rect {
            x: stage.src.x,
            y: stage.src.y,
            w: sw,
            h: sh,
        };
        let chroma_window = Rect {
            x: stage.src.x / 2,
            y: stage.src.y / 2,
            w: scw,
            h: sch,
        };
        let mid_y = self.horizontal_15(enc, &input[0], window, dw, &hl);
        let mid_u = self.horizontal_15(enc, &input[1], chroma_window, dcw, &hc);
        let mid_v = self.horizontal_15(enc, &input[2], chroma_window, dcw, &hc);

        let kernel = X86Kernel::new(from);
        let tables = Yuv2Rgb::new(from);
        let params = ScaledParams {
            k: [kernel.y, kernel.vr, kernel.ub, kernel.vg],
            k2: [kernel.ug, kernel.y_offset, 0, 0],
            c: [tables.crv, tables.cbu, tables.cgu, tables.cgv],
            c2: [tables.cy, tables.oy, 0, 0],
            sizes: [vl.size as i32, vc.size as i32, sh as i32, dh.saturating_sub(2) as i32],
            chroma_rows: [sch as i32, 0, 0, 0],
        };
        let buf = self.gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("scaled convert params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let (lf, cf) = (self.filter_texture(&vl), self.filter_texture(&vc));
        let bind = self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scaled convert"),
            layout: &self.scaled.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&mid_y.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&mid_u.view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&mid_v.view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&lf),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&cf),
                },
            ],
        });
        let rgb = self.plane(
            "scaled convert rgb",
            dw,
            dh,
            CANVAS_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        self.pass(enc, &self.scaled.rgb, &rgb.view, &bind, Some(wgpu::Color::BLACK), None);
        self.rgb_to_planes(enc, &rgb, to)
    }
}

impl Compositor {
    /// `frame` scaled to `size` and taken from `from` to `to` on the GPU, read back: what the compositor draws of a layer FFmpeg 9's
    /// `scale` resizes and converts in one call, for the plane-level comparison with [`crate::convert::scale_and_change_matrix`]
    /// (`tests/convert.rs`).
    pub fn scale_convert_yuv(
        &self,
        frame: &YuvFrame,
        size: (u32, u32),
        from: YuvMatrix,
        to: YuvMatrix,
    ) -> Result<YuvFrame, GpuError> {
        let (w, h) = (frame.width, frame.height);
        if !frame.is_consistent() {
            return Err(GpuError::Unsupported(format!("a conversion of a {w}x{h} frame")));
        }
        let (cw, ch) = chroma_size(w, h);
        let stage = ScaleStage {
            src: Rect::whole(w, h),
            scaled: size,
            keep: Rect::whole(size.0, size.1),
        };
        let (ow, oh) = size;
        let (ocw, och) = chroma_size(ow, oh);
        let planes = [
            self.upload("scale convert y", w, h, &frame.y),
            self.upload("scale convert u", cw, ch, &frame.u),
            self.upload("scale convert v", cw, ch, &frame.v),
        ];
        let gpu = &self.gpu;
        gpu.guarded("scaling and converting a picture", || {
            let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("scale convert"),
            });
            let [y, u, v] = self.scale_convert_planes(&mut enc, &planes, &stage, from, to)?;
            let luma = read_texels(gpu, enc, &y.texture, ow, oh, 1)?.data;
            let idle = || {
                gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("scale convert"),
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
