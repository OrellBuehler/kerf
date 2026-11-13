//! The CPU model of a 10-bit export (`kerf_gpu::delivery10`) against the export's own frames, plane for plane, sample for sample: no tolerance.
//! Needs `ffmpeg`; no adapter.
//!
//! The graph of an export delivered in `yuv420p10le` / `yuv422p10le` is not the 8-bit graph with a conversion at the end: the fit `scale` writes
//! 10 bits, `fade` runs on them, an ordered dither takes the picture down to the 8 bits `overlay` composes in, and a last `scale` takes the
//! composite back up (`delivery10`'s module documentation has the chain). This file holds the model of all of it to FFmpeg's, on the real argv
//! (`kerf_core::export_args`, its encoder swapped for the raw pipe), over many sizes, ratios, contents, fades and filter thread counts.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_delivery10_model -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(clippy::print_stderr)]

mod common;
#[path = "common/ten_bit.rs"]
mod ten_bit;

use std::path::PathBuf;

use kerf_core::{
    Clip, Container, Delivery, ExportOptions, Fit, OriginalMedia, PlanRequest, Planner, Project, RateControl, StreamKind,
    TenBitKinds, TenBitLayout, Timeline, Track, FADE_FULL,
};
use kerf_gpu::delivery10::{self, LayerChain};
use kerf_gpu::scaler::{self, Kernels};
use kerf_gpu::sws::Scaler;

/// The contents: full-range noise (below black and above white, so a fade's clip and the dither's carry are looked at), hard steps (the
/// scaler's overshoot, clipped), and a smooth ramp (every table entry).
const CONTENTS: [(&str, &str); 3] = [
    (
        "noise",
        "lum='mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29,256)':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17,256)'",
    ),
    (
        "steps",
        "lum='if(gt(mod(floor(X/5)+floor(Y/7),2),0.5),0,255)':cb='if(gt(mod(floor(X/9),2),0.5),0,255)':cr='if(gt(mod(floor(Y/11),2),0.5),255,0)'",
    ),
    (
        "ramp",
        "lum='clip(X*255/W+Y/H*20,0,255)':cb='128+100*sin(X/13)':cr='128+100*cos(Y/9)'",
    ),
];

/// (source, canvas): an identity, enlargements (1.5x as in a 720p clip in a 1080p frame), shrinks, steep ones, a Cover crop, odd-ratio ones.
const SIZES: [((u32, u32), (u32, u32)); 18] = [
    ((64, 36), (64, 36)),
    ((64, 36), (96, 54)),
    ((64, 36), (32, 18)),
    ((96, 64), (96, 64)),
    ((160, 90), (256, 144)),
    ((160, 90), (64, 36)),
    ((320, 180), (160, 90)),
    ((320, 180), (100, 56)),
    ((128, 96), (192, 108)),
    ((180, 104), (90, 52)),
    ((322, 182), (240, 136)),
    ((400, 300), (320, 180)),
    ((200, 150), (120, 68)),
    ((250, 150), (180, 100)),
    ((640, 360), (480, 270)),
    ((640, 360), (800, 450)),
    ((426, 240), (320, 180)),
    ((480, 270), (720, 406)),
];

const THREADS: [u32; 8] = [1, 2, 3, 4, 5, 7, 8, 16];
const FPS: f64 = 30.0;
const FRAMES: u32 = 8;

fn caps() -> kerf_core::GpuCaps {
    let mut caps = kerf_core::GpuCaps::A0;
    caps.motion = true;
    caps.fades = true;
    caps.exact_scaler = true;
    caps.ten_bit = TenBitKinds::P420.union(TenBitKinds::P422);
    caps
}

fn kernel() -> scaler::Kernel {
    match scaler::kernels() {
        Kernels::Match(model) => model.kernel,
        other => panic!("this FFmpeg's scaler is not the model: {other:?}"),
    }
}

fn source(dir: &std::path::Path, name: &str, expr: &str, (w, h): (u32, u32)) -> PathBuf {
    let path = dir.join(format!("{name}-{w}x{h}.mkv"));
    if !path.exists() {
        let graph = format!("nullsrc=size={w}x{h}:rate={FPS}:duration=1,format=yuv420p,geq={expr}");
        common::ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &graph,
            "-frames:v",
            &(FRAMES + 2).to_string(),
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv420p",
            path.to_str().unwrap(),
        ]);
    }
    path
}

/// One cut: the clip fills the frame, with fades of `fade_in` and `fade_out` seconds.
fn cut(asset: &kerf_core::Asset, canvas: (u32, u32), fade_in: f64, fade_out: f64) -> Timeline {
    let mut clip = Clip::new(asset.id, 0.0, f64::from(FRAMES) / FPS, 0.0);
    clip.fade_in = fade_in;
    clip.fade_out = fade_out;
    Timeline {
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Delivery::new(canvas.0, canvas.1, Fit::Cover).with_fps(Some(FPS)).ok(),
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_delivery10_model_every_size_content_fade_and_thread_count() {
    let kernel = kernel();
    let policy = kerf_core::composite_color_policy();
    let verdict = delivery10::ten_bit();
    let kinds = delivery10::measured_kinds();
    eprintln!("probe: {verdict:?} {kinds:?}");
    let per_slice = kinds.contains(TenBitKinds::PER_SLICE);
    let dir = common::media_dir("delivery10-model");
    std::fs::create_dir_all(&dir).unwrap();
    let (mut runs, mut samples_compared, mut skipped) = (0usize, 0usize, 0usize);
    for (cname, expr) in CONTENTS {
        for (source_size, canvas) in SIZES {
            let file = source(&dir, cname, expr, source_size);
            let asset = Project::probe_asset(&file).expect("probe");
            for (fade_in, fade_out) in [(0.1, 0.1), (0.2, 0.0), (0.0, 0.0)] {
                let tl = cut(&asset, canvas, fade_in, fade_out);
                let assets = std::slice::from_ref(&asset);
                for layout in [TenBitLayout::P420, TenBitLayout::P422] {
                    let opts = ExportOptions {
                        container: Container::Mkv,
                        video_codec: Some("libx264".into()),
                        rate_control: RateControl::Lossless,
                        fps: Some(FPS),
                        pix_fmt: Some(layout.pix_fmt().into()),
                        ..ExportOptions::default()
                    };
                    let planner = Planner::new(
                        &tl,
                        assets,
                        &opts,
                        PlanRequest::motion(policy).alone().with_media(&OriginalMedia),
                    )
                    .unwrap();
                    // The picture each frame is made of (static content: every frame is the file's first).
                    let decoded = common::ffmpeg(&[
                        "-i",
                        file.to_str().unwrap(),
                        "-frames:v",
                        "1",
                        "-f",
                        "rawvideo",
                        "-pix_fmt",
                        "yuv420p",
                        "pipe:1",
                    ]);
                    let (sw, sh) = (source_size.0 as usize, source_size.1 as usize);
                    let (yp, rest) = decoded.split_at(sw * sh);
                    let (up, vp) = rest.split_at(sw * sh / 4);
                    let plan0 = planner.at_frame(0).unwrap();
                    let canvas_size = (plan0.canvas.width, plan0.canvas.height);
                    let refused = plan0
                        .reasons(&caps(), canvas_size)
                        .into_iter()
                        .find(|u| matches!(u, kerf_core::Unsupported::DeliveryWith(..)));
                    if let Some(why) = refused {
                        eprintln!("skipped {cname} {source_size:?} -> {canvas:?} {}: {why}", layout.pix_fmt());
                        skipped += 1;
                        continue;
                    }
                    let frame_samples = canvas_size.0 as usize * canvas_size.1 as usize
                        + 2 * (canvas_size.0 as usize / 2)
                            * if layout == TenBitLayout::P422 {
                                canvas_size.1 as usize
                            } else {
                                canvas_size.1 as usize / 2
                            };
                    for threads in THREADS {
                        let raw = ten_bit::export_planes(&tl, assets, &opts, Some(threads), Some(FRAMES));
                        let got = ten_bit::samples(&raw);
                        assert_eq!(
                            got.len(),
                            frame_samples * FRAMES as usize,
                            "{cname} {source_size:?} -> {canvas:?} {}",
                            layout.pix_fmt()
                        );
                        for k in 0..u64::from(FRAMES) {
                            let plan = planner.at_frame(k).unwrap();
                            let layer = &plan.layers[0];
                            let geom = plan.layer_geometry(layer, canvas_size).unwrap();
                            let stage = geom
                                .stages
                                .first()
                                .copied()
                                .unwrap_or_else(|| delivery10::whole_stage(source_size.0, source_size.1));
                            let chain = LayerChain {
                                layout,
                                stage,
                                fades: plan
                                    .fade_ops(layer)
                                    .into_iter()
                                    .map(|op| op.factor)
                                    .filter(|f| *f < FADE_FULL)
                                    .collect(),
                                slice_rows: if per_slice && layout == TenBitLayout::P420 {
                                    kerf_core::ten_bit_dither_rows(threads, canvas_size.1)
                                } else {
                                    0
                                },
                                scaler: Scaler::Bicubic,
                                kernel,
                            };
                            let layer8 = delivery10::layer_planes([yp, up, vp], (sw, sh), &chain).expect("a table");
                            let want = delivery10::to_ten_bit(
                                [&layer8[0], &layer8[1], &layer8[2]],
                                (canvas_size.0 as usize, canvas_size.1 as usize),
                                layout,
                                kernel,
                            )
                            .expect("a table");
                            let frame = &got[k as usize * frame_samples..(k as usize + 1) * frame_samples];
                            let mut at = 0;
                            for (name, plane) in [("Y", &want.y), ("U", &want.u), ("V", &want.v)] {
                                let apart = plane.data.iter().zip(&frame[at..]).filter(|(a, b)| a != b).count();
                                assert_eq!(
                                    apart,
                                    0,
                                    "{cname} {source_size:?} -> {canvas:?} {} fades ({fade_in}, {fade_out}) on {threads} threads, frame {k}: {name} differs in {apart} of {} samples",
                                    layout.pix_fmt(),
                                    plane.data.len()
                                );
                                at += plane.data.len();
                                samples_compared += plane.data.len();
                            }
                        }
                        runs += 1;
                    }
                }
            }
        }
    }
    eprintln!("checker_delivery10_model: {runs} exports ({skipped} cuts the plan refuses skipped), {samples_compared} samples compared, 0 differ");
    assert!(runs > 0);
}
