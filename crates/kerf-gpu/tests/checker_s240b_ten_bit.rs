//! `checker/s240b`, the 10-bit deliveries: a layer tagged SMPTE 240M in a `yuv420p10le` / `yuv422p10le` export is refused by the plan (the 10-bit chain
//! is held to FFmpeg's for the other matrices), and the render refuses it too for a caller that did not ask the plan. The refusal is judged by drawing
//! the frames anyway and comparing them with the export's own terminal planes: reported as exact or not (`KERF_CHECKER_ANYWAY=1` with the scratch patch
//! that lifts the plan's gate prints the figures), never asserted exact.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_s240b_ten_bit -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

mod common;
#[path = "common/s240b.rs"]
mod s240b;
#[path = "common/ten_bit.rs"]
mod ten_bit;

use std::sync::{Arc, OnceLock};

use kerf_core::{
    Asset, Clip, Container, Delivery, ExportOptions, Fit, PlanRequest, Planner, RateControl, StreamKind, TenBitKinds,
    TenBitLayout, Timeline, Track, Unsupported,
};
use kerf_gpu::delivery10;
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions, MotionFrames};

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| {
        let _ = kerf_gpu::scaler::kernels();
        let _ = kerf_gpu::text::model();
        let _ = delivery10::ten_bit();
        // (The cap for a 240M picture is the round trip's verdict: measured, so that the 10-bit chain is what refuses it.)
        let _ = kerf_gpu::roundtrip_probe::translucency();
        Compositor::new(gpu()).expect("a compositor").with_graph_threads(1)
    })
}

fn frame_source() -> Arc<FrameSource> {
    static S: OnceLock<Arc<FrameSource>> = OnceLock::new();
    S.get_or_init(|| FrameSource::new(FrameSourceConfig::default())).clone()
}

fn timeline(a: &Asset, canvas: (u32, u32), fit: Fit) -> Timeline {
    Timeline {
        tracks: vec![Track {
            clips: vec![Clip::new(a.id, 0.0, 0.5, 0.0)],
            ..Track::new(StreamKind::Video, "V1")
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Delivery::new(canvas.0, canvas.1, fit).with_fps(Some(30.0)).ok(),
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

fn options(layout: TenBitLayout) -> ExportOptions {
    ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        fps: Some(30.0),
        pix_fmt: Some(layout.pix_fmt().into()),
        ..ExportOptions::default()
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_s240b_ten_bit_a_240m_layer_in_a_10_bit_delivery_is_refused_and_judged_drawn_anyway() {
    let anyway = std::env::var_os("KERF_CHECKER_ANYWAY").is_some();
    let comp = compositor();
    let kinds = delivery10::measured_kinds();
    if !(kinds.contains(TenBitKinds::P420) && kinds.contains(TenBitKinds::P422)) {
        eprintln!(
            "S240B 10-bit: this FFmpeg's 10-bit export is not the model ({:?}), no frame is drawn at 10 bits and the case is vacuous",
            delivery10::details()
        );
        return;
    }
    let caps = comp.caps();
    let dir = common::media_dir("s240b-media");
    let mut report = Vec::new();
    for (tag, tag_name) in [
        ("none", "untagged (control)"),
        ("240", "240M"),
        ("240all", "240M with primaries and transfer"),
        ("709", "BT.709 (control)"),
    ] {
        let a = s240b::footage(&dir, (640, 360), "noise", tag, "yuv420p", "ffv1", (30, 1.0));
        for layout in [TenBitLayout::P420, TenBitLayout::P422] {
            let tl = timeline(&a, (640, 360), Fit::Cover);
            let opts = options(layout);
            let assets = std::slice::from_ref(&a);
            let policy = kerf_core::composite_color_policy();
            let planner = Planner::new(&tl, assets, &opts, PlanRequest::motion(policy).alone()).expect("planner");
            let first = planner.at_frame(0).expect("plan");
            let size = (first.canvas.width, first.canvas.height);
            let (w, h) = (size.0 as usize, size.1 as usize);
            let (cw, ch) = (w / 2, if layout == TenBitLayout::P422 { h } else { h / 2 });
            let exported = ten_bit::export_frames(&tl, assets, &opts, Some(1), 4, (w * h + 2 * cw * ch) * 2, &|k| k < 5);
            let mut motion = MotionFrames::new(frame_source());
            let (mut drawn, mut refused, mut exact, mut off, mut closed) = (0, 0, 0, 0, 0);
            for k in 0..5u64 {
                let plan = planner.at_frame(k).expect("plan");
                let why = plan.reasons(&caps, size);
                // (Under a fixed-matrix FFmpeg a 240M picture is "another colour matrix than the composite's" -- BT.601 -- before it is a 240M one.)
                let ten = why.iter().any(|u| matches!(u, Unsupported::DeliveryWith(..)));
                let ten_240m = why
                    .iter()
                    .any(|u| matches!(u, Unsupported::DeliveryWith(_, d) if d.contains("SMPTE 240M")));
                let is_240m = tag.starts_with("240");
                if !anyway {
                    if is_240m {
                        assert!(
                            ten,
                            "{tag_name} {}: frame {k}: the plan does not refuse a 240M layer in a 10-bit delivery: {why:?}",
                            layout.pix_fmt()
                        );
                    } else {
                        assert!(!ten_240m, "{tag_name} {}: frame {k}: {why:?}", layout.pix_fmt());
                    }
                }
                if why.is_empty() {
                    drawn += 1;
                } else {
                    refused += 1;
                }
                // Drawn anyway: the plan's word is not asked.
                let plan = plan.for_decode(&caps, size);
                let picture = motion.frames(&plan).expect("the layers' frames");
                match comp.composite_yuv10_counted(&plan, &picture, motion.counts(), size) {
                    Err(e) => {
                        closed += 1;
                        if k == 0 {
                            eprintln!(
                                "S240B 10-bit {tag_name} {}: the render refuses: {}",
                                layout.pix_fmt(),
                                e.to_string().chars().take(120).collect::<String>()
                            );
                        }
                    }
                    Ok(planes) => {
                        let want = ten_bit::samples(&exported[&k]);
                        let mut at = 0;
                        let mut apart = 0usize;
                        for plane in [&planes.y, &planes.u, &planes.v] {
                            apart += plane
                                .data
                                .iter()
                                .zip(&want[at..at + plane.data.len()])
                                .filter(|(a, b)| a != b)
                                .count();
                            at += plane.data.len();
                        }
                        if apart == 0 {
                            exact += 1;
                        } else {
                            off += 1;
                        }
                    }
                }
            }
            let line = format!(
                "{tag_name} {}: 5 frames, the plan draws {drawn} and refuses {refused}; drawn anyway: {exact} exact, {off} not exact, {closed} refused by the render",
                layout.pix_fmt()
            );
            eprintln!("S240B 10-bit {line}");
            report.push(line.clone());
            if !anyway && tag.starts_with("240") {
                assert_eq!(
                    closed, 5,
                    "{line}: the render draws a 240M layer at 10 bits for a caller that did not ask the plan"
                );
            }
            if !anyway && !tag.starts_with("240") {
                assert!(
                    exact == 5 || refused > 0,
                    "{line}: the control is neither drawn exactly nor refused"
                );
                assert_eq!(off, 0, "{line}: the control is drawn and not the export's");
            }
        }
    }
}
