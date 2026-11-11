//! The passes of a translucent layer whose picture has an odd side (`roundtrip.wgsl`; the arithmetic and what it was held to are in
//! [`crate::roundtrip`]): swscale takes `yuva420p -> argb` for such a picture by another route than the even one's, and the way back
//! reads or scales its last chroma column by the build's, which [`crate::roundtrip_probe::odd_translucency`] measured.

use super::*;
use crate::roundtrip::{FullChroma, OddWidth};

/// The pipelines of an odd picture's round trip.
pub(super) struct OddPasses {
    /// The chroma of every pixel scaled to the chroma plane's width (9.0, an odd width).
    chroma_h_scaled: wgpu::RenderPipeline,
    /// A chroma plane enlarged to the picture's height (an odd height), a byte.
    chroma_up: wgpu::RenderPipeline,
    /// ... to the picture's width, then height, for the full-chroma functions of an odd height and width.
    wide: wgpu::RenderPipeline,
    tall: wgpu::RenderPipeline,
    rgb_full: wgpu::RenderPipeline,
}

impl OddPasses {
    pub(super) fn new(
        gpu: &Gpu,
        rt_shader: &wgpu::ShaderModule,
        compose_layout: &wgpu::BindGroupLayout,
        rt_v_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let device = &gpu.device;
        let pipeline = |label: &str, layout: &wgpu::BindGroupLayout, fs: &str, format| {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module: rt_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: rt_shader,
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
        Self {
            chroma_h_scaled: pipeline("rt chroma h scaled", rt_v_layout, "fs_chroma_h_scaled", CHROMA_FORMAT),
            chroma_up: pipeline("rt chroma up", rt_v_layout, "fs_chroma_up", PLANE_FORMAT),
            wide: pipeline("rt wide", rt_v_layout, "fs_wide", MID_FORMAT),
            tall: pipeline("rt tall", rt_v_layout, "fs_tall", MID_FORMAT),
            rgb_full: pipeline("rt rgb full", compose_layout, "fs_rgb_full", CANVAS_FORMAT),
        }
    }
}

impl Compositor {
    /// `yuva420p -> argb` of an odd height (`roundtrip::yuv_to_rgb`): the chroma planes enlarged by the scaler to the picture's height
    /// (and, for an odd width too, to its width) and the colour from them. `params` is the round trip's uniform for the layer.
    pub(super) fn odd_height_rgb(
        &self,
        enc: &mut wgpu::CommandEncoder,
        input: &[Plane; 3],
        forward: YuvMatrix,
        params: &RoundTripParams,
    ) -> Result<Plane, GpuError> {
        let [py, pu, pv] = input;
        let (w, h) = (py.w, py.h);
        let (cw, ch) = (pu.w, pu.h);
        let too_far = || GpuError::Unsupported("a picture too tall or wide for the chroma filters".into());
        let vertical = sws::bicubic(ch, h, sws::ONE_VERTICAL).ok_or_else(too_far)?;
        let v_taps = self.filter_texture(&vertical);
        let rgb = self.plane("rt rgb", w, h, CANVAS_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
        if w.is_multiple_of(2) {
            let mut up = |plane: &Plane| {
                let out = self.plane("rt chroma up", cw, h, PLANE_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
                let p = RoundTripParams {
                    taps: vertical.size as i32,
                    ..*params
                };
                let bind = self.rt_bind(&self.rt_v_layout, &p, &[&plane.view, &v_taps]);
                self.pass(enc, &self.odd.chroma_up, &out.view, &bind, None, None);
                out
            };
            let (u, v) = (up(pu), up(pv));
            let p = RoundTripParams {
                pad: [0, 1, 0],
                ..*params
            };
            let bind = self.rt_bind(&self.compose_layout, &p, &[&py.view, &u.view, &v.view]);
            self.pass(enc, &self.rt_rgb, &rgb.view, &bind, None, None);
            return Ok(rgb);
        }
        let horizontal = sws::bicubic(cw, w, sws::ONE_HORIZONTAL).ok_or_else(too_far)?;
        let h_taps = self.filter_texture(&horizontal);
        let mut wide_and_tall = |plane: &Plane| {
            let wide = self.plane("rt wide", w, ch, MID_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
            let p = RoundTripParams {
                taps: horizontal.size as i32,
                ..*params
            };
            let bind = self.rt_bind(&self.rt_v_layout, &p, &[&plane.view, &h_taps]);
            self.pass(enc, &self.odd.wide, &wide.view, &bind, None, None);
            let tall = self.plane("rt tall", w, h, MID_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
            let p = RoundTripParams {
                taps: vertical.size as i32,
                csize: [w as i32, ch as i32],
                ..*params
            };
            let bind = self.rt_bind(&self.rt_v_layout, &p, &[&wide.view, &v_taps]);
            self.pass(enc, &self.odd.tall, &tall.view, &bind, None, None);
            tall
        };
        let (u, v) = (wide_and_tall(pu), wide_and_tall(pv));
        let full = FullChroma::new(forward);
        let p = RoundTripParams {
            full: [full.y_coeff, full.y_offset, full.v2r, full.v2g, full.u2g, full.u2b, 0, 0],
            ..*params
        };
        let bind = self.rt_bind(&self.compose_layout, &p, &[&py.view, &u.view, &v.view]);
        self.pass(enc, &self.odd.rgb_full, &rgb.view, &bind, None, None);
        Ok(rgb)
    }

    /// The chroma of the way back for an odd width on a build that scales the row (`OddWidth::Scaled`): the chroma of every pixel and the
    /// horizontal bicubic to the chroma plane's width, into the 15-bit intermediate the vertical pass takes.
    pub(super) fn scaled_chroma_h(
        &self,
        enc: &mut wgpu::CommandEncoder,
        rgb: &Plane,
        params: &RoundTripParams,
    ) -> Result<Plane, GpuError> {
        let (w, h) = (rgb.w, rgb.h);
        let cw = w.div_ceil(2);
        let horizontal = sws::bicubic(w, cw, sws::ONE_HORIZONTAL)
            .ok_or_else(|| GpuError::Unsupported("a picture too wide for the chroma filter".into()))?;
        let taps = self.filter_texture(&horizontal);
        let inter = self.plane(
            "rt chroma scaled",
            cw,
            h,
            CHROMA_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let p = RoundTripParams {
            taps: horizontal.size as i32,
            ..*params
        };
        let bind = self.rt_bind(&self.rt_v_layout, &p, &[&rgb.view, &taps]);
        self.pass(enc, &self.odd.chroma_h_scaled, &inter.view, &bind, None, None);
        Ok(inter)
    }
}

/// The model an odd picture is drawn by, if this FFmpeg's odd sizes were measured to follow one.
pub(super) fn odd_model(measured: &crate::roundtrip_probe::OddTranslucency) -> Result<OddWidth, GpuError> {
    measured.model().ok_or_else(|| {
        GpuError::Unsupported(format!(
            "a translucent layer of odd size: this FFmpeg's RGB round trip of odd sides is not measured to be the compositor's ({measured:?})"
        ))
    })
}
