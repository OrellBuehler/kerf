//! The passes of a layer's grade (`grade.wgsl`, `roundtrip.wgsl`; the arithmetic and what it was held to are in
//! [`crate::grade`]): the picture goes `yuv420p -> rgb24` through swscale's x86 kernel, through the LUT, the
//! curves and the wheels, and back — to 4:2:0 at once for an untagged picture, through 4:4:4 and a 2:1 scale of
//! the chroma for a tagged one.

use super::*;
use crate::grade::{reciprocal_table, vertical_start, GradeStages, LutAxes, X86Kernel};
use kerf_core::BakedLut;

/// `grade.wgsl`'s uniform.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GradeParams {
    /// `yCoeff`, `vrCoeff`, `ubCoeff`, `vgCoeff`.
    k: [i32; 4],
    /// `ugCoeff`, `yOffset`, the LUT's size, zero (what `pin` ORs with).
    k2: [i32; 4],
    /// `colorbalance`'s numbers, a channel to a row: shadows, midtones, highlights, unused.
    bal: [[f32; 4]; 3],
}

/// A LUT on the GPU: the table as a 3D texture (red along x) and the axes' table.
pub(super) struct GpuLut {
    table: wgpu::TextureView,
    axes: wgpu::TextureView,
    size: i32,
}

/// What a layer's grade needs besides its stages: the GPU copy of its LUT.
pub(super) struct GradeLook {
    pub stages: GradeStages,
    lut: Option<Arc<GpuLut>>,
}

/// The pipelines of the grade.
pub(super) struct GradePasses {
    layout: wgpu::BindGroupLayout,
    lut_layout: wgpu::BindGroupLayout,
    rgb: wgpu::RenderPipeline,
    curves: wgpu::RenderPipeline,
    balance: wgpu::RenderPipeline,
    lut3d: wgpu::RenderPipeline,
    plane444: wgpu::RenderPipeline,
    chroma_v: wgpu::RenderPipeline,
    /// The LUTs uploaded, the newest last: a clip's frames are drawn through the same table.
    luts: Mutex<Vec<(Arc<BakedLut>, Arc<GpuLut>)>>,
    /// `src / 255.0f` for every byte, for `colorbalance`.
    reciprocals: wgpu::TextureView,
}

/// How many LUTs are kept on the GPU (each up to 65^3 texels of 16 bytes).
const LUTS_KEPT: usize = 4;

fn texture_3d_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D3,
            multisampled: false,
        },
        count: None,
    }
}

impl GradePasses {
    pub(super) fn new(
        gpu: &Gpu,
        rt_shader: &wgpu::ShaderModule,
        convert_layout: &wgpu::BindGroupLayout,
        rt_v_layout: &wgpu::BindGroupLayout,
    ) -> Self {
        let device = &gpu.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kerf grade"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/grade.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("grade"),
            entries: &[uniform_entry(0), texture_entry(1), texture_entry(2), texture_entry(3)],
        });
        let lut_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("grade lut"),
            entries: &[uniform_entry(0), texture_entry(1), texture_entry(2), texture_3d_entry(4)],
        });
        let pipeline = |label: &str, module: &wgpu::ShaderModule, layout: &wgpu::BindGroupLayout, fs: &str, format| {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
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
        let reciprocals = device
            .create_texture_with_data(
                &gpu.queue,
                &wgpu::TextureDescriptor {
                    label: Some("colorbalance reciprocals"),
                    size: extent(256, 1),
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: FILTER_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                bytemuck::cast_slice(reciprocal_table()),
            )
            .create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            rgb: pipeline("grade rgb", &shader, &layout, "fs_rgb", CANVAS_FORMAT),
            curves: pipeline("grade curves", &shader, &layout, "fs_curves", CANVAS_FORMAT),
            balance: pipeline("grade balance", &shader, &layout, "fs_balance", CANVAS_FORMAT),
            lut3d: pipeline("grade lut3d", &shader, &lut_layout, "fs_lut3d", CANVAS_FORMAT),
            plane444: pipeline("grade plane 444", rt_shader, convert_layout, "fs_plane444", PLANE_FORMAT),
            chroma_v: pipeline("grade chroma v", rt_shader, rt_v_layout, "fs_chroma_v_x86", PLANE_FORMAT),
            layout,
            lut_layout,
            luts: Mutex::new(Vec::new()),
            reciprocals,
        }
    }
}

/// Why a layer whose grade is drawn cannot be drawn at this geometry, or `None`: the grade runs in RGB on the picture the
/// chain has made, at its own size, and what is reproduced is FFmpeg's round trip for a picture the scaler did not
/// resize, with nothing after the grade that sees the RGB (or, for a tagged picture, 4:4:4) frame. The plan refuses
/// all of these first (`RenderPlan::reasons`: `Matte`, `GradeWith`, `ZoomBehind`); this is the render's own check,
/// for a caller that did not ask.
pub(super) fn grade_geometry_refusal(layer: &kerf_core::PlanLayer, geom: &LayerGeometry) -> Option<String> {
    let (zoom_last, (pw, ph)) = effect_picture(layer, geom);
    if geom.matte {
        Some("a grade on a letterboxed picture".into())
    } else if zoom_last {
        Some("a grade before a moving zoom".into())
    } else if geom.stages.iter().any(|s| s.scaled != (s.src.w, s.src.h)) {
        Some("a grade on a layer the scaler resizes".into())
    } else if pw % 2 != 0 || ph % 2 != 0 {
        Some(format!("a grade on a {pw}x{ph} picture (odd sides)"))
    } else if layer.effects.iter().any(|e| !matches!(e, VideoEffect::ChromaKey { .. })) {
        Some("video effects behind a grade".into())
    } else {
        None
    }
}

impl Compositor {
    /// What a layer's grade needs on the GPU (its LUT, uploaded once for the frames that follow), or `None` when
    /// nothing of the grade is left to draw: FFmpeg takes the picture through RGB only for the filters it has.
    pub(super) fn grade_look(&self, layer: &kerf_core::PlanLayer) -> Result<Option<GradeLook>, GpuError> {
        if layer.grade.is_identity() {
            return Ok(None);
        }
        let unsupported = |what: &str| GpuError::Unsupported(format!("layer {}: a grade {what}", layer.name));
        // The conversions are the model's only where this FFmpeg's kernels are the x86 ones it follows.
        match self.grade_kernels.unwrap_or_else(crate::grade::kernels) {
            crate::grade::Kernels::Match => {}
            crate::grade::Kernels::Differs(why) => {
                return Err(unsupported(&format!(
                    "because this FFmpeg's conversions are not the ones the compositor follows ({why})"
                )))
            }
            crate::grade::Kernels::Unknown => {
                return Err(unsupported("until this FFmpeg's conversions to and from RGB can be measured"))
            }
        }
        if layer.stream.pix_fmt.as_deref() != Some("yuv420p") {
            return Err(unsupported("on a picture that is not 8-bit 4:2:0 yuv420p"));
        }
        let route = layer
            .grade_route()
            .ok_or_else(|| unsupported("on a picture whose colour matrix has no measured round trip"))?;
        let Some(stages) = GradeStages::of(&layer.grade, route) else {
            return Ok(None);
        };
        let lut = match &stages.lut {
            Some(table) => Some(self.gpu_lut(table)?),
            None => None,
        };
        Ok(Some(GradeLook { stages, lut }))
    }

    /// The table on the GPU, uploaded the first time it is drawn with.
    fn gpu_lut(&self, table: &Arc<BakedLut>) -> Result<Arc<GpuLut>, GpuError> {
        let limit = self.gpu.device.limits().max_texture_dimension_3d as usize;
        if table.size > limit {
            return Err(GpuError::Unsupported(format!(
                "a {0}x{0}x{0} LUT is over this device's {limit}px 3D texture limit",
                table.size
            )));
        }
        let mut luts = self.grade.luts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = luts.iter().position(|(t, _)| Arc::ptr_eq(t, table) || **t == **table) {
            let hit = luts.remove(at);
            let gpu = hit.1.clone();
            luts.push(hit);
            return Ok(gpu);
        }
        let n = table.size as u32;
        let texels: Vec<f32> = table.data.iter().flat_map(|c| [c[0], c[1], c[2], 1.0]).collect();
        let tex = self.gpu.device.create_texture_with_data(
            &self.gpu.queue,
            &wgpu::TextureDescriptor {
                label: Some("lut3d"),
                size: wgpu::Extent3d {
                    width: n,
                    height: n,
                    depth_or_array_layers: n,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D3,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&texels),
        );
        let axes = self.gpu.device.create_texture_with_data(
            &self.gpu.queue,
            &wgpu::TextureDescriptor {
                label: Some("lut3d axes"),
                size: extent(256, 3),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FILTER_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&LutAxes::new(table.size).texels()),
        );
        let gpu = Arc::new(GpuLut {
            table: tex.create_view(&wgpu::TextureViewDescriptor::default()),
            axes: axes.create_view(&wgpu::TextureViewDescriptor::default()),
            size: table.size as i32,
        });
        if luts.len() >= LUTS_KEPT {
            luts.remove(0);
        }
        luts.push((table.clone(), gpu.clone()));
        Ok(gpu)
    }

    fn grade_bind(
        &self,
        layout: &wgpu::BindGroupLayout,
        params: &GradeParams,
        views: &[(u32, &wgpu::TextureView)],
    ) -> wgpu::BindGroup {
        let buf = self.gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("grade params"),
            contents: bytemuck::bytes_of(params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mut entries = vec![wgpu::BindGroupEntry {
            binding: 0,
            resource: buf.as_entire_binding(),
        }];
        entries.extend(views.iter().map(|(binding, view)| wgpu::BindGroupEntry {
            binding: *binding,
            resource: wgpu::BindingResource::TextureView(view),
        }));
        self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("grade"),
            layout,
            entries: &entries,
        })
    }

    /// A layer's planes after its grade. `input` is the picture the chain has made (an even size, not resized:
    /// the plan sees to both), `output` the planes the next filter of the chain gets.
    pub(super) fn grade_planes(
        &self,
        enc: &mut wgpu::CommandEncoder,
        input: &[Plane; 3],
        grade: &GradeLook,
    ) -> Result<[Plane; 3], GpuError> {
        let [py, pu, pv] = input;
        let (w, h) = (py.w, py.h);
        let (cw, ch) = (pu.w, pu.h);
        if w % 2 != 0 || h % 2 != 0 || (cw, ch) != (w / 2, h / 2) {
            return Err(GpuError::Unsupported(format!(
                "a grade on a {w}x{h} picture (the conversions need even sides)"
            )));
        }
        let stages = &grade.stages;
        let passes = &self.grade;
        let kernel = X86Kernel::new(stages.route.matrix);
        let mut params = GradeParams {
            k: [kernel.y, kernel.vr, kernel.ub, kernel.vg],
            k2: [kernel.ug, kernel.y_offset, grade.lut.as_ref().map_or(0, |l| l.size), 0],
            bal: [[0.0; 4]; 3],
        };
        if let Some(b) = &stages.balance {
            for (c, row) in params.bal.iter_mut().enumerate() {
                *row = [b.params[c * 3], b.params[c * 3 + 1], b.params[c * 3 + 2], 0.0];
            }
        }
        let rgb_target = |label: &str| self.plane(label, w, h, CANVAS_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let mut rgb = rgb_target("grade rgb");
        let bind = self.grade_bind(&passes.layout, &params, &[(1, &py.view), (2, &pu.view), (3, &pv.view)]);
        self.pass(enc, &passes.rgb, &rgb.view, &bind, None, None);

        if let (Some(table), Some(lut)) = (&stages.lut, &grade.lut) {
            debug_assert_eq!(table.size as i32, lut.size);
            let out = rgb_target("grade lut3d");
            let bind = self.grade_bind(
                &passes.lut_layout,
                &params,
                &[(1, &rgb.view), (2, &lut.axes), (4, &lut.table)],
            );
            self.pass(enc, &passes.lut3d, &out.view, &bind, None, None);
            rgb = out;
        }
        if let Some(tables) = &stages.curves {
            let flat: Vec<u8> = tables.iter().flatten().copied().collect();
            let view = Self::lut_texture(&self.gpu, &flat);
            let out = rgb_target("grade curves");
            let bind = self.grade_bind(&passes.layout, &params, &[(1, &rgb.view), (2, &view), (3, &view)]);
            self.pass(enc, &passes.curves, &out.view, &bind, None, None);
            rgb = out;
        }
        if stages.balance.is_some() {
            let out = rgb_target("grade balance");
            let bind = self.grade_bind(
                &passes.layout,
                &params,
                &[(1, &rgb.view), (2, &passes.reciprocals), (3, &passes.reciprocals)],
            );
            self.pass(enc, &passes.balance, &out.view, &bind, None, None);
            rgb = out;
        }

        // The way back.
        let back = Rgb2Yuv::new(stages.route.matrix);
        let rt = RoundTripParams {
            k: [0; 4],
            m: [0; 4],
            ky: [back.ry, back.gy, back.by, 0],
            ku: [back.ru, back.gu, back.bu, 0],
            kv: [back.rv, back.gv, back.bv, 0],
            size: [w as i32, h as i32],
            csize: [cw as i32, ch as i32],
            channel: 0,
            taps: 0,
            white: [0; 2],
            start: 0,
            pad: [0; 3],
        };
        let plane_out = |label: &str, pw: u32, ph: u32| {
            self.plane(
                label,
                pw,
                ph,
                PLANE_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            )
        };
        if !stages.route.via_444 {
            let luma = plane_out("grade y", w, h);
            let bind = self.rt_bind(&self.convert_layout, &rt, &[&rgb.view]);
            self.pass(enc, &self.rt_luma, &luma.view, &bind, None, None);
            let inter = self.plane("grade chroma", cw, h, CHROMA_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
            self.pass(enc, &self.rt_chroma_h, &inter.view, &bind, None, None);
            let filter = sws::bicubic(h, ch, sws::ONE_VERTICAL)
                .ok_or_else(|| GpuError::Unsupported("a picture too tall for the chroma filter".into()))?;
            let taps = self.filter_texture(&filter);
            let start = vertical_start(&filter);
            let mut chroma = Vec::with_capacity(2);
            for channel in 0..2 {
                let p = RoundTripParams {
                    channel,
                    taps: filter.size as i32,
                    start,
                    ..rt
                };
                let plane = plane_out("grade chroma plane", cw, ch);
                let bind = self.rt_bind(&self.rt_v_layout, &p, &[&inter.view, &taps]);
                self.pass(enc, &passes.chroma_v, &plane.view, &bind, None, None);
                chroma.push(plane);
            }
            let (Some(v), Some(u)) = (chroma.pop(), chroma.pop()) else {
                unreachable!("two chroma planes")
            };
            return Ok([luma, u, v]);
        }

        // Tagged: 4:4:4 first, by the grade's own `scale`, then the next filter's 2:1 scale of the chroma.
        let full: Vec<Plane> = (0..3)
            .map(|channel| {
                let plane = plane_out("grade 444", w, h);
                let p = RoundTripParams { channel, ..rt };
                let bind = self.rt_bind(&self.convert_layout, &p, &[&rgb.view]);
                self.pass(enc, &passes.plane444, &plane.view, &bind, None, None);
                plane
            })
            .collect();
        let whole = |w: u32, h: u32| Rect::whole(w, h);
        let stage = PlaneStage {
            src: whole(w, h),
            scaled: (cw, ch),
            keep: whole(cw, ch),
        };
        let too_far = || GpuError::Unsupported("a picture too large for the chroma filter".into());
        let v = sws::bicubic(h, ch, sws::ONE_VERTICAL).ok_or_else(too_far)?;
        let filters = PlaneFilters {
            h: sws::bicubic(w, cw, sws::ONE_HORIZONTAL).ok_or_else(too_far)?,
            x86_start: vertical_start(&v) as u32,
            v,
        };
        let u = self.scale_plane(enc, &full[1], stage, &filters, None);
        let v = self.scale_plane(enc, &full[2], stage, &filters, None);
        let mut full = full;
        let luma = full.swap_remove(0);
        Ok([luma, u, v])
    }
}

impl Compositor {
    /// A picture's planes through a grade on the GPU, read back: what the compositor draws of a layer that is
    /// not resized, before anything else of its chain — for the plane-level comparison with
    /// [`crate::grade::reference`] and with FFmpeg (`tests/grade.rs`).
    pub fn grade_yuv(&self, frame: &YuvFrame, stages: &GradeStages) -> Result<YuvFrame, GpuError> {
        let (w, h) = (frame.width, frame.height);
        if !frame.is_consistent() || w % 2 != 0 || h % 2 != 0 {
            return Err(GpuError::Unsupported(format!("a grade of a {w}x{h} frame")));
        }
        let (cw, ch) = chroma_size(w, h);
        let lut = match &stages.lut {
            Some(table) => Some(self.gpu_lut(table)?),
            None => None,
        };
        let look = GradeLook {
            stages: stages.clone(),
            lut,
        };
        let planes = [
            self.upload("grade y", w, h, &frame.y),
            self.upload("grade u", cw, ch, &frame.u),
            self.upload("grade v", cw, ch, &frame.v),
        ];
        let gpu = &self.gpu;
        gpu.guarded("grading a picture", || {
            let mut enc = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("grade") });
            let [y, u, v] = self.grade_planes(&mut enc, &planes, &look)?;
            let luma = read_texels(gpu, enc, &y.texture, w, h, 1)?.data;
            let idle = || {
                gpu.device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("grade") })
            };
            let u = read_texels(gpu, idle(), &u.texture, cw, ch, 1)?.data;
            let v = read_texels(gpu, idle(), &v.texture, cw, ch, 1)?.data;
            Ok(YuvFrame {
                width: w,
                height: h,
                y: luma,
                u,
                v,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerf_core::{
        Asset, Clip, CompositeColorPolicy, Delivery, ExportOptions, Fit, Grade, Keyframe, PlanRequest, Planner, RenderPlan,
        StreamInfo, StreamKind, Timeline, Track, Wheel, Wheels,
    };

    fn stream(w: u32, h: u32) -> StreamInfo {
        StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(w),
            height: Some(h),
            fps: Some(30.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: Some("yuv420p".into()),
            color_space: None,
            color_range: None,
            lead: 0.0,
        }
    }

    fn wheels() -> Grade {
        Grade {
            wheels: Some(Wheels {
                midtones: Wheel {
                    x: 0.2,
                    y: 0.1,
                    lightness: 0.05,
                },
                ..Wheels::default()
            }),
            ..Grade::default()
        }
    }

    /// The Motion plan of one graded clip of `w`x`h` footage at its fourth frame, on a canvas of `delivery` (the
    /// footage's own size without one), `edit` applied to the clip.
    fn plan(w: u32, h: u32, delivery: Option<Delivery>, edit: impl FnOnce(&mut Clip)) -> RenderPlan {
        let asset = Asset {
            id: uuid::Uuid::new_v4(),
            path: "/media/a.mp4".into(),
            name: "a".into(),
            duration: 20.0,
            streams: vec![stream(w, h)],
            imported_at: chrono::Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
            music_score: None,
        };
        let mut clip = Clip::new(asset.id, 0.0, 10.0, 0.0);
        clip.grade = wheels();
        edit(&mut clip);
        let timeline = Timeline {
            tracks: vec![Track {
                clips: vec![clip],
                ..Track::new(StreamKind::Video, "V1")
            }],
            overlays: Vec::new(),
            svg_overlays: Vec::new(),
            markers: Vec::new(),
            format: delivery,
            master: Default::default(),
        };
        let opts = ExportOptions {
            fps: Some(30.0),
            ..ExportOptions::default()
        };
        Planner::new(
            &timeline,
            std::slice::from_ref(&asset),
            &opts,
            PlanRequest::motion(CompositeColorPolicy::FixedBt601),
        )
        .expect("a planner")
        .at_frame(3)
        .expect("a plan")
    }

    fn geometry(plan: &RenderPlan) -> LayerGeometry {
        plan.layer_geometry(&plan.layers[0], plan.size(u32::MAX)).expect("a geometry")
    }

    fn zoom(time: f64, scale: f64) -> Keyframe {
        Keyframe {
            time,
            scale,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        }
    }

    type Edit = Box<dyn Fn(&mut Clip)>;

    /// A layer a grade is not drawn for: what it is, its footage's size, the canvas, the edit of its clip, and the word its refusal holds.
    type Refused = (&'static str, (u32, u32), Option<Delivery>, Edit, &'static str);

    /// Every layer a grade is not drawn for at its geometry, with the word its refusal holds: the plan refuses these
    /// first (`RenderPlan::reasons`), and the render says no too, for a caller that did not ask.
    fn refused() -> Vec<Refused> {
        vec![
            (
                "a letterbox",
                (360, 640),
                Some(Delivery::new(640, 360, Fit::Contain)),
                Box::new(|_| {}),
                "letterboxed",
            ),
            (
                "a picture-in-picture",
                (640, 360),
                None,
                Box::new(|c| c.transform.scale = 0.6),
                "resizes",
            ),
            (
                "an enlargement",
                (640, 360),
                Some(Delivery::new(960, 540, Fit::Contain)),
                Box::new(|_| {}),
                "resizes",
            ),
            (
                "a blur behind it",
                (640, 360),
                None,
                Box::new(|c| c.effects = vec![kerf_core::VideoEffect::Blur { sigma: 2.0 }]),
                "video effects",
            ),
            (
                "a moving zoom last",
                (640, 360),
                None,
                Box::new(|c| c.keyframes = vec![zoom(0.0, 1.0), zoom(4.0, 1.4)]),
                "moving zoom",
            ),
        ]
    }

    #[test]
    fn a_layer_at_its_own_size_has_nothing_to_refuse() {
        let p = plan(640, 360, None, |_| {});
        assert_eq!(grade_geometry_refusal(&p.layers[0], &geometry(&p)), None);
        // A chroma key is behind `format=yuva420p`, after the grade's conversion back.
        let p = plan(640, 360, None, |c| {
            c.effects = vec![kerf_core::VideoEffect::ChromaKey {
                color: "green".into(),
                similarity: 0.3,
                blend: 0.1,
            }];
        });
        assert_eq!(grade_geometry_refusal(&p.layers[0], &geometry(&p)), None);
    }

    #[test]
    fn a_letterbox_a_resize_an_effect_and_a_zoom_last_are_each_refused_at_the_render() {
        for (name, (w, h), delivery, edit, word) in refused() {
            let p = plan(w, h, delivery, |c| edit(c));
            let why = grade_geometry_refusal(&p.layers[0], &geometry(&p)).unwrap_or_else(|| panic!("{name}: drawn"));
            assert!(why.contains(word), "{name}: {why}");
        }
    }

    #[test]
    fn an_odd_side_is_refused_at_the_render() {
        let mut p = plan(640, 360, None, |_| {});
        let geom = geometry(&p);
        // (A picture is odd only on a canvas that is: the plan's canvas is what the render's size is made of.)
        p.layers[0].stream.width = 641;
        let geom = LayerGeometry {
            picture: (641, 360),
            ..geom
        };
        let why = grade_geometry_refusal(&p.layers[0], &geom).expect("an odd picture is refused");
        assert!(why.contains("641x360") && why.contains("odd"), "{why}");
        let geom = LayerGeometry {
            picture: (640, 361),
            ..geom
        };
        assert!(grade_geometry_refusal(&p.layers[0], &geom).is_some());
    }

    #[test]
    #[ignore = "needs an adapter (lavapipe is enough)"]
    fn look_of_refuses_them_itself_whatever_the_plan_said() {
        use crate::gpu::{Gpu, GpuOptions};
        use crate::grade::Kernels;
        let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
        let compositor = Compositor::new(gpu).expect("a compositor").with_grade_kernels(Kernels::Match);
        let own = plan(640, 360, None, |_| {});
        assert!(compositor.look_of(&own.layers[0], &geometry(&own)).is_ok());
        for (name, (w, h), delivery, edit, word) in refused() {
            let p = plan(w, h, delivery, |c| edit(c));
            let Err(GpuError::Unsupported(why)) = compositor.look_of(&p.layers[0], &geometry(&p)) else {
                panic!("{name}: drawn");
            };
            assert!(why.contains(word), "{name}: {why}");
        }
        // The kernels come first: a machine that converts otherwise draws no grade at all.
        let unmeasured = Compositor::new(Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter"))
            .expect("a compositor")
            .with_grade_kernels(Kernels::Unknown);
        let Err(GpuError::Unsupported(why)) = unmeasured.look_of(&own.layers[0], &geometry(&own)) else {
            panic!("drawn with unknown kernels");
        };
        assert!(why.contains("measured"), "{why}");
    }

    fn flat(w: u32, h: u32) -> YuvFrame {
        let (cw, ch) = crate::source::chroma_size(w, h);
        YuvFrame {
            width: w,
            height: h,
            y: vec![100; (w * h) as usize],
            u: vec![110; (cw * ch) as usize],
            v: vec![140; (cw * ch) as usize],
        }
    }

    /// The same refusals through `composite_yuv`, which is what a host that skipped the plan calls: each guard of
    /// `look_of` is an error out of the render (and no frame), and the layer at its own size is drawn. (An odd side
    /// has no plan to start from: the canvas is even and a crop of a 4:2:0 picture snaps to even, so
    /// `an_odd_side_is_refused_at_the_render` hands the guard the geometry itself.)
    #[test]
    #[ignore = "needs an adapter (lavapipe is enough)"]
    fn composite_yuv_refuses_each_of_them_and_draws_a_layer_at_its_own_size() {
        use crate::gpu::{Gpu, GpuOptions};
        use crate::grade::Kernels;
        let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
        let compositor = Compositor::new(gpu).expect("a compositor").with_grade_kernels(Kernels::Match);
        let own = plan(640, 360, None, |_| {});
        let drawn = compositor
            .composite_yuv(&own, &[Some(flat(640, 360))], own.size(u32::MAX))
            .expect("a graded layer at its own size is drawn");
        assert_eq!((drawn.width, drawn.height), (640, 360));
        for (name, (w, h), delivery, edit, word) in refused() {
            let p = plan(w, h, delivery, |c| edit(c));
            let Err(GpuError::Unsupported(why)) = compositor.composite_yuv(&p, &[Some(flat(w, h))], p.size(u32::MAX)) else {
                panic!("{name}: drawn");
            };
            assert!(why.contains(word), "{name}: {why}");
        }
        for kernels in [Kernels::Differs("a test"), Kernels::Unknown] {
            let pinned = Compositor::new(Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter"))
                .expect("a compositor")
                .with_grade_kernels(kernels);
            let Err(GpuError::Unsupported(_)) = pinned.composite_yuv(&own, &[Some(flat(640, 360))], own.size(u32::MAX)) else {
                panic!("{kernels:?}: drawn");
            };
        }
    }
}
