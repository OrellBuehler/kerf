//! A clip in a 10-bit delivery: the passes that take its decoded 8-bit picture through the chain the export builds it with
//! ([`crate::delivery10`] has the chain and the CPU model these are held to) and hand the compose pass the 8-bit planes `overlay` blends.
//!
//! Per plane: the horizontal pass of the resampler (15 bits, as for any scale) and the vertical pass that writes 10 bits (`fs_v10`);
//! `fade` on those (`fs_fade10`); then the way down to 8 bits, the ordered 2x2 dither of the unscaled conversion (`fs_dither10`) or,
//! for the chroma of a 4:2:2 delivery, the generic scaler's 2:1 shrink with its 8x8 dither (`fs_down422`). A plane the scale leaves
//! alone and no fade touches is the decoded plane itself (`v << 2` and the dither's at most 3 are the identity on 8 bits).

use super::*;
use crate::delivery10::{chroma_stage, layout_stage, whole_stage};
use kerf_core::TenBitLayout;

/// The pipelines of the 10-bit chain (`composite.wgsl`: `fs_v10`, `fs_fade10`, `fs_dither10`, `fs_down422`), all over the resampler's
/// bind group layout and its uniform.
pub(super) struct TenBitPasses {
    pub(super) v10: wgpu::RenderPipeline,
    pub(super) fade: wgpu::RenderPipeline,
    pub(super) dither: wgpu::RenderPipeline,
    pub(super) down: wgpu::RenderPipeline,
}

/// What [`Compositor::ten_draw`] judges and draws a layer from.
pub(super) struct TenInput<'a> {
    pub(super) plan: &'a RenderPlan,
    pub(super) layer: &'a kerf_core::PlanLayer,
    pub(super) geom: &'a LayerGeometry,
    pub(super) fades: &'a LayerFades,
    pub(super) frame: &'a YuvFrame,
    pub(super) caps: &'a GpuCaps,
    pub(super) size: (u32, u32),
    pub(super) n: usize,
}

/// One plane of a layer's 10-bit chain.
pub(super) struct TenPlane {
    /// The plane's stage (the plane of a 4:2:0 source at its own size).
    stage: ScaleStage,
    /// Its tables, and whether the scale changes nothing (the plane is `v << 2`: the decoded plane stands for it).
    tables: Option<(Filter, Filter)>,
    /// The 4:2:2 chroma plane's way down to 4:2:0 rows: the table of the 2:1 shrink, whether x86's kernel runs on all but the last row.
    down: Option<(Filter, bool)>,
}

/// What a layer of a 10-bit delivery needs drawn: the chain of [`crate::delivery10::LayerChain`], as passes.
pub(super) struct TenDraw {
    planes: [TenPlane; 3],
    /// The clip's fades: the factors of the two `fade` filters ([`FADE_FULL`]: not running).
    fade_in: u32,
    fade_out: u32,
    /// The rows of a chroma slice where the 4:2:0 dither restarts ([`kerf_core::ten_bit_dither_rows`]), else 0.
    slice_rows: u32,
}

impl TenDraw {
    pub(super) fn fades_run(&self) -> bool {
        self.fade_in < FADE_FULL || self.fade_out < FADE_FULL
    }
}

impl Compositor {
    /// The chain of one layer of a 10-bit delivery: its tables under the scaler this FFmpeg was measured to have, the clip's fades and
    /// the dither's rows. Refuses what the plan refuses (the plan is asked first; these are the render's own guards).
    pub(super) fn ten_draw(&self, input: TenInput) -> Result<TenDraw, GpuError> {
        let TenInput {
            plan,
            layer,
            geom,
            fades,
            frame,
            caps,
            size,
            n,
        } = input;
        let refuse = |why: &str| GpuError::Unsupported(format!("layer {n}: {why}"));
        let Some(layout) = plan.ten_bit_layout() else {
            return Err(refuse("not a 10-bit delivery"));
        };
        if !caps.ten_bit.contains(layout.kind()) {
            return Err(refuse(
                "a 10-bit delivery this FFmpeg's export was not measured to be the model for",
            ));
        }
        let scaler_kernels = self.scaler_kernels.unwrap_or_else(crate::scaler::measured);
        if !scaler_kernels.exact() {
            return Err(refuse("a 10-bit delivery over a scaler that is not measured to be swscale's"));
        }
        let model = scaler_kernels.model();
        let named = plan.canvas.scaler.as_deref().and_then(crate::scaler::named);
        if named.is_some_and(|s| s != Scaler::Bicubic) {
            return Err(refuse("a 10-bit delivery that names a scaler besides bicubic"));
        }
        let scaler = model.scaler(named);
        let (align_h, align_v) = model.kernel.alignment();
        if fades.ramp < FADE_FULL || fades.white_round_trip || fades.white_in < FADE_FULL || fades.white_out < FADE_FULL {
            return Err(refuse("a 10-bit delivery's fade that is not to black"));
        }
        if geom.stages.len() > 1
            || geom.matte
            || geom.rotation.is_some()
            || geom.translucent
            || layer.keyed_alpha.is_some()
            || geom.origin != (0, 0)
            || geom.layer != size
        {
            return Err(refuse("a 10-bit delivery's layer that is more than the plain fit"));
        }
        // (The plan says the rest of this, and the render says it again for a caller that did not ask: the chain was measured for a picture
        // of 8-bit 4:2:0 and even sides, of the composite's matrix, in a cut of one matrix, over no title the 10-bit chain was not held to.)
        if layer.stream.pix_fmt.as_deref() != Some("yuv420p")
            || layer.is_image
            || layer.stream.matrix() != Some(plan.canvas.matrix)
        {
            return Err(refuse(
                "a 10-bit delivery's picture that is not 8-bit 4:2:0 of the composite's matrix",
            ));
        }
        if plan.canvas.mixed_matrices_cut || !plan.svg.is_empty() || plan.canvas.scaler.as_deref().is_some_and(|s| s != "bicubic")
        {
            return Err(refuse(
                "a 10-bit delivery over a cut of mixed matrices, an SVG title or a named scaler",
            ));
        }
        if let Some(stage) = geom.stages.first() {
            let (src, keep) = (stage.src, stage.keep);
            if ![src.w, src.h, stage.scaled.0, stage.scaled.1, keep.w, keep.h]
                .into_iter()
                .all(|v| v % 2 == 0)
            {
                return Err(refuse("a 10-bit delivery's picture, window or result of odd size"));
            }
        } else if frame.width % 2 != 0 || frame.height % 2 != 0 {
            return Err(refuse("a 10-bit delivery's picture of odd size"));
        }
        let slice_rows = if layout == TenBitLayout::P420 {
            plan.ten_bit_dither_rows(caps, size.1)
                .ok_or_else(|| refuse("a 10-bit delivery whose dither depends on a thread count that is not known"))?
        } else {
            0
        };
        let too_far = || refuse("a scale ratio swscale needs two passes for");
        let luma = layout_stage(
            &geom
                .stages
                .first()
                .copied()
                .unwrap_or_else(|| whole_stage(frame.width, frame.height)),
            layout,
        );
        let (cw, ch) = frame.chroma_size();
        let plane = |stage: ScaleStage, (pw, ph): (u32, u32), chroma: bool| -> Result<TenPlane, GpuError> {
            let identity = stage.src == Rect::whole(pw, ph)
                && stage.keep == Rect::whole(stage.src.w, stage.src.h)
                && stage.scaled == (stage.src.w, stage.src.h);
            let tables = if identity {
                None
            } else {
                let h =
                    sws::table_aligned(scaler, stage.src.w, stage.scaled.0, sws::ONE_HORIZONTAL, align_h).ok_or_else(too_far)?;
                let v =
                    sws::table_aligned(scaler, stage.src.h, stage.scaled.1, sws::ONE_VERTICAL, align_v).ok_or_else(too_far)?;
                Some((h, v))
            };
            let down = if chroma && layout == TenBitLayout::P422 {
                let rows = stage.keep.h;
                let table =
                    sws::table_aligned(Scaler::Bicubic, rows, rows / 2, sws::ONE_VERTICAL, align_v).ok_or_else(too_far)?;
                let simd = model.kernel.simd_vertical() && table.size > 1;
                Some((table, simd))
            } else {
                None
            };
            Ok(TenPlane { stage, tables, down })
        };
        let cstage = chroma_stage(&luma, layout);
        Ok(TenDraw {
            planes: [
                plane(luma, (frame.width, frame.height), false)?,
                plane(cstage, (cw, ch), true)?,
                plane(cstage, (cw, ch), true)?,
            ],
            fade_in: fades.fade_in,
            fade_out: fades.fade_out,
            slice_rows,
        })
    }

    /// A bind group for one of the resampler's passes: the input, the uniform and the two tables.
    fn ten_bind(&self, params: &ResampleParams, input: &wgpu::TextureView, taps: &wgpu::TextureView) -> wgpu::BindGroup {
        let device = &self.gpu.device;
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ten-bit params"),
            contents: bytemuck::bytes_of(params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.resample_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input),
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
                    resource: wgpu::BindingResource::TextureView(taps),
                },
            ],
        })
    }

    /// The three planes of a layer of a 10-bit delivery, as the 8-bit planes `overlay` blends.
    pub(super) fn ten_bit_planes(&self, enc: &mut wgpu::CommandEncoder, input: &[Plane; 3], ten: &TenDraw) -> [Plane; 3] {
        std::array::from_fn(|pi| self.ten_bit_plane(enc, &input[pi], &ten.planes[pi], pi, ten))
    }

    fn ten_bit_plane(&self, enc: &mut wgpu::CommandEncoder, input: &Plane, tp: &TenPlane, pi: usize, ten: &TenDraw) -> Plane {
        let chroma = pi > 0;
        let device_zero = ResampleParams {
            src_off: [0; 2],
            src_size: [0; 2],
            dst_off: [0; 2],
            out_size: [0; 2],
            apply_lut: 0,
            plane: u32::from(chroma),
            taps: 0,
            x86: 0,
            c_from: 0,
            pad: [0; 3],
        };
        // 1. The scale, into 10 bits (the decoded 8-bit plane stands for `v << 2` where it changes nothing).
        let (mut current, mut eight) = match &tp.tables {
            None => (input.clone(), true),
            Some((h, v)) => {
                let ScaleStage { src, keep, .. } = tp.stage;
                let mid = self.plane("ten mid", keep.w, src.h, MID_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
                let out = self.plane(
                    "ten plane",
                    keep.w,
                    keep.h,
                    MID_FORMAT,
                    wgpu::TextureUsages::RENDER_ATTACHMENT,
                );
                let base = ResampleParams {
                    src_off: [src.x as i32, src.y as i32],
                    src_size: [src.w as i32, src.h as i32],
                    dst_off: [keep.x as i32, keep.y as i32],
                    out_size: [keep.w as i32, keep.h as i32],
                    taps: h.size as i32,
                    ..device_zero
                };
                let h_taps = self.filter_texture(h);
                let v_taps = self.filter_texture(v);
                let bind = self.ten_bind(&base, &input.view, &h_taps);
                self.pass(enc, &self.resample_h, &mid.view, &bind, Some(wgpu::Color::BLACK), None);
                let bind = self.ten_bind(
                    &ResampleParams {
                        taps: v.size as i32,
                        ..base
                    },
                    &mid.view,
                    &v_taps,
                );
                self.pass(enc, &self.ten.v10, &out.view, &bind, Some(wgpu::Color::BLACK), None);
                (out, false)
            }
        };
        // 2. `fade`, on those 10 bits.
        if ten.fades_run() {
            let out = self.plane(
                "ten fade",
                current.w,
                current.h,
                MID_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            );
            let params = ResampleParams {
                apply_lut: u32::from(eight),
                pad: [ten.fade_in as i32, ten.fade_out as i32, 0],
                ..device_zero
            };
            let bind = self.ten_bind(&params, &current.view, &self.identity_lut);
            self.pass(enc, &self.ten.fade, &out.view, &bind, Some(wgpu::Color::BLACK), None);
            current = out;
            eight = false;
        }
        // 3. Down to the 8 bits `overlay` composes in.
        match &tp.down {
            Some((table, simd)) => {
                debug_assert!(!eight, "a 4:2:2 chroma plane is always resampled");
                let rows = current.h / 2;
                let out = self.plane(
                    "ten down",
                    current.w,
                    rows,
                    PLANE_FORMAT,
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
                );
                let params = ResampleParams {
                    src_size: [current.w as i32, current.h as i32],
                    out_size: [current.w as i32, rows as i32],
                    // (V's dither is the table's columns three along.)
                    plane: u32::from(pi == 2),
                    taps: table.size as i32,
                    x86: u32::from(*simd),
                    c_from: rows as i32 - 1,
                    ..device_zero
                };
                let taps = self.filter_texture(table);
                let bind = self.ten_bind(&params, &current.view, &taps);
                self.pass(enc, &self.ten.down, &out.view, &bind, Some(wgpu::Color::BLACK), None);
                out
            }
            None if eight => current,
            None => {
                let out = self.plane(
                    "ten dither",
                    current.w,
                    current.h,
                    PLANE_FORMAT,
                    wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
                );
                // (Luma's slices start on even rows: its pattern is the picture's.)
                let rows = if chroma { ten.slice_rows } else { 0 };
                let params = ResampleParams {
                    pad: [rows as i32, 0, 0],
                    ..device_zero
                };
                let bind = self.ten_bind(&params, &current.view, &self.identity_lut);
                self.pass(enc, &self.ten.dither, &out.view, &bind, Some(wgpu::Color::BLACK), None);
                out
            }
        }
    }
}
