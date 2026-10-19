//! Time per still: the GPU compositor against FFmpeg's own still of the same
//! timeline, at 1080p and 4K, with 1, 3 and 6 layers.
//!
//! `#[ignore]`d *and* gated on `KERF_BENCH=1` (a 4K, six-layer still on a
//! software adapter takes seconds, and CI's parity job runs every ignored test):
//!
//! ```text
//! KERF_BENCH=1 cargo test -p kerf-gpu --no-default-features --release -- \
//!     --ignored --nocapture bench
//! ```
//!
//! Set `KERF_GPU_ADAPTER=hardware` to run on the machine's own GPU. What is
//! timed:
//!
//! * **ffmpeg** — `export_still` to JPEG (quality 4, the preview's setting), i.e.
//!   decode + filter graph + encode in one process, which is what a preview or an
//!   agent's `preview_timeline` costs today;
//! * **gpu decode** — one `ffmpeg` spawn per layer returning raw `yuv420p`, the
//!   layers in parallel (v0 has no long-lived decoder and no cache);
//! * **gpu composite** — upload, passes and readback of RGBA. No encode: a
//!   preview that shows the pixels directly never pays one.
//!
//! The median of five runs after a warm-up. Numbers go to stderr and
//! `target/parity/bench.txt`.

#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::Utc;
use kerf_core::{
    export_still, Asset, Clip, ExportOptions, ImageFormat, RenderPlan, StreamInfo, StreamKind, Timeline, Track, Transform,
};
use kerf_gpu::{Compositor, Gpu, GpuOptions};
use uuid::Uuid;

fn source(dir: &Path, w: u32, h: u32) -> Asset {
    let path: PathBuf = dir.join(format!("bench-{w}x{h}.mp4"));
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={w}x{h}:rate=10:duration=1"))
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "18", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    Asset {
        id: Uuid::new_v4(),
        path: path.to_string_lossy().into_owned(),
        name: format!("bench {w}x{h}"),
        duration: 1.0,
        streams: vec![StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(w),
            height: Some(h),
            fps: Some(10.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            // As a real import records it (without it every decode is followed by a
            // second one that looks for transparency).
            pix_fmt: Some("yuv420p".into()),
            color_space: None,
            lead: 0.0,
        }],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
    }
}

/// `layers` full-length clips, one per track: a full-frame base and then
/// picture-in-pictures, some translucent, one rotated — the work a real cut
/// asks of a compositor, not `n` copies of nothing.
fn timeline(asset: &Asset, layers: usize) -> Timeline {
    let tracks = (0..layers)
        .map(|i| {
            let mut c = Clip::new(asset.id, 0.0, 1.0, 0.0);
            if i > 0 {
                c.transform = Transform {
                    scale: 0.4,
                    pos_x: -0.3 + 0.2 * (i % 4) as f64,
                    pos_y: -0.25 + 0.25 * (i / 4) as f64,
                    opacity: if i % 2 == 0 { 0.8 } else { 1.0 },
                    rotation: if i == 3 { 12.0 } else { 0.0 },
                    ..Transform::default()
                };
            }
            Track {
                clips: vec![c],
                ..Track::new(StreamKind::Video, format!("V{}", i + 1))
            }
        })
        .collect();
    Timeline {
        tracks,
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    }
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

#[test]
#[ignore = "benchmark: set KERF_BENCH=1; needs ffmpeg and a GPU adapter"]
fn bench_time_per_still_gpu_vs_ffmpeg() {
    if std::env::var_os("KERF_BENCH").is_none() {
        eprintln!("skipped: set KERF_BENCH=1 to run the benchmark");
        return;
    }
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let info = gpu.adapter_info().clone();
    let comp = Compositor::new(gpu).expect("compositor");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let scratch = dir.join(format!("bench-out-{}.jpg", std::process::id()));
    let opts = ExportOptions::default();
    let mut report = format!(
        "adapter: {} ({:?}, {:?}); ffmpeg: {}\n{:<7} {:<7} {:>10} {:>12} {:>14} {:>11}\n",
        info.name,
        info.backend,
        info.device_type,
        kerf_core::ffmpeg_path(),
        "canvas",
        "layers",
        "ffmpeg ms",
        "gpu decode ms",
        "gpu composite ms",
        "gpu total ms"
    );
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        let asset = source(dir, w, h);
        for layers in [1usize, 3, 6] {
            let tl = timeline(&asset, layers);
            let assets = std::slice::from_ref(&asset);
            let plan = RenderPlan::at(&tl, assets, &opts, 0.5, kerf_core::composite_color_policy()).expect("plan");
            assert!(plan.gpu_supported(), "{:?}", plan.unsupported_reasons());
            let size = plan.size(u32::MAX);

            let (mut ff, mut dec, mut comp_t) = (Vec::new(), Vec::new(), Vec::new());
            for run in 0..6 {
                let t0 = Instant::now();
                export_still(&tl, assets, &opts, 0.5, &scratch, ImageFormat::Jpeg, 4).expect("ffmpeg still");
                let ffmpeg_time = t0.elapsed();
                let (_, timings) = comp.render_plan(&plan, size).expect("gpu render");
                if run > 0 {
                    ff.push(ffmpeg_time);
                    dec.push(timings.decode);
                    comp_t.push(timings.composite);
                }
            }
            let (ff, dec, comp_t) = (median(ff), median(dec), median(comp_t));
            let line = format!(
                "{:<7} {:<7} {:>10.0} {:>12.0} {:>14.0} {:>11.0}\n",
                format!("{h}p"),
                layers,
                ms(ff),
                ms(dec),
                ms(comp_t),
                ms(dec + comp_t)
            );
            eprint!("{line}");
            report.push_str(&line);
        }
    }
    let _ = std::fs::remove_file(&scratch);
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap().join("parity");
    let _ = std::fs::create_dir_all(&out);
    let _ = std::fs::write(out.join("bench.txt"), &report);
    eprintln!("{report}");
}

/// A 30 fps source of `seconds` for the decode benchmark (the still's source is 10 frames).
fn playback_source(dir: &Path, w: u32, h: u32, seconds: u32) -> Asset {
    let path: PathBuf = dir.join(format!("bench-play-{w}x{h}.mp4"));
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={w}x{h}:rate=30:duration={seconds}"))
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "18",
            "-g",
            "60",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    kerf_core::Project::probe_asset(&path).expect("probe")
}

/// Time per frame of a layer's decode: one `ffmpeg` per frame (A0) against a `FrameSource`
/// (A1b-2), for 1x playback (frame after frame) and for a scrub (jumps of a second or so).
/// No GPU. Same gate as the still benchmark.
#[test]
#[ignore = "benchmark: set KERF_BENCH=1; needs ffmpeg"]
fn bench_frame_source_decode_vs_one_shot() {
    if std::env::var_os("KERF_BENCH").is_none() {
        eprintln!("skipped: set KERF_BENCH=1 to run the benchmark");
        return;
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let opts = ExportOptions::default();
    let mut report = format!(
        "ffmpeg: {}\n{:<7} {:<9} {:>14} {:>16}\n",
        kerf_core::ffmpeg_path(),
        "source",
        "pattern",
        "one-shot ms",
        "frame source ms"
    );
    for (w, h) in [(1280u32, 720u32), (1920, 1080), (3840, 2160)] {
        let asset = playback_source(dir, w, h, 6);
        let tl = timeline(&asset, 1);
        let layer_at = |t: f64| {
            let tl = Timeline {
                tracks: vec![Track {
                    clips: vec![Clip::new(asset.id, 0.0, asset.duration, 0.0)],
                    ..tl.tracks[0].clone()
                }],
                ..tl.clone()
            };
            RenderPlan::at(
                &tl,
                std::slice::from_ref(&asset),
                &opts,
                t,
                kerf_core::CompositeColorPolicy::FixedBt601,
            )
            .expect("plan")
            .layers
            .remove(0)
        };
        let patterns: [(&str, Vec<f64>, kerf_gpu::Hint); 2] = [
            (
                "playback",
                (0..60).map(|k| 1.0 + f64::from(k) / 30.0).collect(),
                kerf_gpu::Hint::Forward { fps: 30.0 },
            ),
            (
                "scrub",
                (0..20).map(|k| f64::from((k * 7) % 20) * 0.27 + 0.1).collect(),
                kerf_gpu::Hint::Scrub,
            ),
        ];
        for (name, times, hint) in patterns {
            let layers: Vec<_> = times.iter().map(|&t| layer_at(t)).collect();
            let t0 = Instant::now();
            for l in &layers {
                kerf_gpu::decode_layer(l).expect("one-shot");
            }
            let oneshot = t0.elapsed() / layers.len() as u32;
            let src = kerf_gpu::FrameSource::new(kerf_gpu::FrameSourceConfig::default());
            let t0 = Instant::now();
            for l in &layers {
                src.frame(l, hint).expect("frame source");
            }
            let runs = t0.elapsed() / layers.len() as u32;
            let line = format!(
                "{:<7} {:<9} {:>14.1} {:>16.1}   {:?}\n",
                format!("{h}p"),
                name,
                ms(oneshot),
                ms(runs),
                src.stats()
            );
            eprint!("{line}");
            report.push_str(&line);
        }
    }
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap().join("parity");
    let _ = std::fs::create_dir_all(&out);
    let _ = std::fs::write(out.join("bench-frame-source.txt"), &report);
}

/// A captioned frame: the first sight of a caption (FFmpeg rasterises it: one process) against the same
/// caption again (the atlas) and against no caption, and FFmpeg's own still of the captioned frame, at a
/// 960 px preview and at the delivery size. Same gate as the still benchmark.
#[test]
#[ignore = "benchmark: set KERF_BENCH=1; needs ffmpeg and a GPU adapter"]
fn bench_a_captioned_frame_cold_and_warm() {
    if std::env::var_os("KERF_BENCH").is_none() {
        eprintln!("skipped: set KERF_BENCH=1 to run the benchmark");
        return;
    }
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let info = gpu.adapter_info().clone();
    let comp = Compositor::new(gpu).expect("compositor");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let scratch = dir.join(format!("bench-text-{}.jpg", std::process::id()));
    let asset = source(dir, 1920, 1080);
    let assets = std::slice::from_ref(&asset);
    let opts = ExportOptions::default();
    let policy = kerf_core::composite_color_policy();
    let captioned = |text: &str| {
        let mut tl = timeline(&asset, 1);
        let mut o = kerf_core::TextOverlay::new(text, 0.0, 1.0);
        o.bg = Some("black@0.5".into());
        o.size = 0.05;
        o.pos_y = 0.88;
        tl.overlays.push(o);
        tl
    };
    let mut report = format!(
        "adapter: {} ({:?}, {:?}); ffmpeg: {}\n{:<14} {:>12} {:>16} {:>16} {:>14} {:>16}\n",
        info.name,
        info.backend,
        info.device_type,
        kerf_core::ffmpeg_path(),
        "render",
        "ffmpeg ms",
        "gpu, no text ms",
        "gpu, new caption",
        "gpu, again ms",
        "text cost, cold"
    );
    for (name, width) in [("960 px", 960u32), ("1920 px", u32::MAX)] {
        let plain = RenderPlan::at(&timeline(&asset, 1), assets, &opts, 0.5, policy).expect("plan");
        let size = plain.size(width);
        let mut bare = Vec::new();
        let (mut cold, mut warm, mut ff) = (Vec::new(), Vec::new(), Vec::new());
        for run in 0..6 {
            let tl = captioned(&format!("a caption number {run} for this frame"));
            let plan = RenderPlan::at(&tl, assets, &opts, 0.5, policy).expect("plan");
            assert!(plan.reasons(&Compositor::CAPS, size).is_empty());
            let t0 = Instant::now();
            export_still(&tl, assets, &opts, 0.5, &scratch, ImageFormat::Jpeg, 4).expect("ffmpeg still");
            let ffmpeg_time = t0.elapsed();
            let (_, without) = comp.render_plan(&plain, size).expect("render without text");
            let (_, first) = comp.render_plan(&plan, size).expect("render with a new caption");
            let (_, again) = comp.render_plan(&plan, size).expect("render with it again");
            if run > 0 {
                bare.push(without.composite);
                cold.push(first.composite);
                warm.push(again.composite);
                ff.push(ffmpeg_time);
            }
        }
        let (bare, cold, warm, ff) = (median(bare), median(cold), median(warm), median(ff));
        let line = format!(
            "{:<14} {:>12.0} {:>16.0} {:>16.0} {:>14.0} {:>16.0}\n",
            name,
            ms(ff),
            ms(bare),
            ms(cold),
            ms(warm),
            ms(cold.saturating_sub(bare))
        );
        eprint!("{line}");
        report.push_str(&line);
    }
    let _ = std::fs::remove_file(&scratch);
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap().join("parity");
    let _ = std::fs::create_dir_all(&out);
    let _ = std::fs::write(out.join("bench-text.txt"), &report);
}

/// A frame inside a transition: FFmpeg's still (which draws it with `fade`, `overlay` and a second input)
/// against the compositor — the plan's decode (two files' frames) and the passes (the exact blend reads a
/// copy of the canvas) — at a 960 px preview and at 1080p, with the cost of a plain frame beside it, and an
/// export frame through one cursor per clip (`MotionFrames`, forward: the cost per frame of a playback).
/// Same gate as the still benchmark.
#[test]
#[ignore = "benchmark: set KERF_BENCH=1; needs ffmpeg and a GPU adapter"]
fn bench_a_frame_inside_a_transition() {
    if std::env::var_os("KERF_BENCH").is_none() {
        eprintln!("skipped: set KERF_BENCH=1 to run the benchmark");
        return;
    }
    use kerf_core::{Transition, TransitionKind};
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let info = gpu.adapter_info().clone();
    let comp = Compositor::new(gpu).expect("compositor");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let scratch = dir.join(format!("bench-transition-{}.jpg", std::process::id()));
    let asset = source(dir, 1920, 1080);
    let assets = std::slice::from_ref(&asset);
    let opts = ExportOptions::default();
    let policy = kerf_core::composite_color_policy();
    // Two clips of the one second of footage, the second entering through `kind`: the outgoing one keeps
    // playing on its tail, so the dissolve has two frames to decode.
    let cut = |kind: Option<(TransitionKind, f64)>| {
        let mut tl = timeline(&asset, 1);
        let mut second = Clip::new(asset.id, 0.4, 0.9, 0.4);
        second.transition_in = kind.map(|(kind, duration)| Transition { kind, duration });
        tl.tracks[0].clips = vec![Clip::new(asset.id, 0.0, 0.4, 0.0), second];
        tl
    };
    let mut report = format!(
        "adapter: {} ({:?}, {:?}); ffmpeg: {}; min of 7 runs after a warm-up (median in brackets)\n{:<22} {:<8} {:>14} {:>16} {:>19} {:>13}\n",
        info.name,
        info.backend,
        info.device_type,
        kerf_core::ffmpeg_path(),
        "frame",
        "render",
        "ffmpeg ms",
        "gpu decode ms",
        "gpu composite ms",
        "layers drawn"
    );
    type Kind = Option<(TransitionKind, f64)>;
    let frames: [(&str, Kind, f64); 5] = [
        ("plain cut", None, 0.5),
        ("dissolve, half way", Some((TransitionKind::Crossfade, 0.3)), 0.5),
        ("dip to black", Some((TransitionKind::DipToBlack, 0.3)), 0.45),
        ("slide left", Some((TransitionKind::SlideLeft, 0.3)), 0.5),
        ("dip to white", Some((TransitionKind::DipToWhite, 0.3)), 0.45),
    ];
    for (name, width) in [("960 px", 960u32), ("1080p", u32::MAX)] {
        for (label, kind, t) in frames {
            let tl = cut(kind);
            let plan = RenderPlan::at(&tl, assets, &opts, t, policy).expect("plan");
            let size = plan.size(width);
            assert!(plan.reasons(&Compositor::CAPS, size).is_empty(), "{label}");
            let (mut ff, mut dec, mut comp_t) = (Vec::new(), Vec::new(), Vec::new());
            for run in 0..8 {
                let t0 = Instant::now();
                export_still(&tl, assets, &opts, t, &scratch, ImageFormat::Jpeg, 4).expect("ffmpeg still");
                let ffmpeg_time = t0.elapsed();
                let (_, timings) = comp.render_plan(&plan, size).expect("gpu render");
                if run > 0 {
                    ff.push(ffmpeg_time);
                    dec.push(timings.decode);
                    comp_t.push(timings.composite);
                }
            }
            let both = |v: Vec<Duration>| {
                let min = v.iter().min().copied().unwrap_or_default();
                format!("{:>6.0} ({:.0})", ms(min), ms(median(v)))
            };
            let line = format!(
                "{:<22} {:<8} {:>14} {:>16} {:>19} {:>13}\n",
                label,
                name,
                both(ff),
                both(dec),
                both(comp_t),
                plan.layers.len()
            );
            eprint!("{line}");
            report.push_str(&line);
        }
    }
    // An export frame: the frames of a dissolve through one cursor per clip, composited one after the
    // other (forward, as a playback or an export reads them).
    let tl = cut(Some((TransitionKind::Crossfade, 0.3)));
    let planner = kerf_core::Planner::new(&tl, assets, &opts, kerf_core::PlanRequest::motion(policy)).expect("planner");
    let size = planner.at_frame(0).expect("plan").size(960);
    let source = kerf_gpu::FrameSource::new(kerf_gpu::FrameSourceConfig::default());
    let mut motion = kerf_gpu::MotionFrames::new(source);
    let t0 = Instant::now();
    let (mut decode, mut composite, mut count) = (Duration::ZERO, Duration::ZERO, 0u32);
    for k in 0..9 {
        let plan = planner.at_frame(k).expect("plan");
        let a = Instant::now();
        let frames = motion.frames(&plan).expect("cursor frames");
        let b = Instant::now();
        comp.composite_shared(&plan, &frames, size).expect("composite");
        decode += b - a;
        composite += b.elapsed();
        count += 1;
    }
    let line = format!(
        "export frames 0..9 of a dissolve at 960 px, forward: {:.1} ms a frame (decode {:.1}, composite {:.1}), first frame included\n",
        ms(t0.elapsed()) / f64::from(count),
        ms(decode) / f64::from(count),
        ms(composite) / f64::from(count)
    );
    eprint!("{line}");
    report.push_str(&line);
    let _ = std::fs::remove_file(&scratch);
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap().join("parity");
    let _ = std::fs::create_dir_all(&out);
    let _ = std::fs::write(out.join("bench-transitions.txt"), &report);
}

/// A moving zoom, a mask and the effects, frame by frame: FFmpeg's own still of the frame, and the
/// export graph's per-frame cost (what `geq` costs: 93 ms of CPU a 960x540 frame and 402 ms at 1080p
/// in the review), against the compositor — a still (decode, then the passes and the readback) and an
/// export frame through `MotionFrames`, forward, the mask plane already made (a mask does not move: it
/// is made once, timed on its own). Same gate as the still benchmark.
#[test]
#[ignore = "benchmark: set KERF_BENCH=1; needs ffmpeg and a GPU adapter"]
fn bench_a_mask_a_blur_and_a_moving_zoom() {
    if std::env::var_os("KERF_BENCH").is_none() {
        eprintln!("skipped: set KERF_BENCH=1 to run the benchmark");
        return;
    }
    use kerf_core::{Container, Delivery, Fit, Keyframe, Mask, MaskShape, RateControl, VideoEffect};
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let info = gpu.adapter_info().clone();
    let comp = Compositor::new(gpu).expect("compositor");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let scratch = dir.join(format!("bench-zoom-{}.jpg", std::process::id()));
    let asset = playback_source(dir, 1920, 1080, 2);
    let assets = std::slice::from_ref(&asset);
    let opts = ExportOptions::default();
    let policy = kerf_core::composite_color_policy();
    let key = |time: f64, scale: f64, pos_x: f64| Keyframe {
        time,
        scale,
        pos_x,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    };
    let mask = Mask {
        shape: MaskShape::Ellipse,
        x: 0.5,
        y: 0.5,
        width: 0.6,
        height: 0.6,
        feather: 0.15,
        inverted: false,
    };
    type Edit = Box<dyn Fn(&mut Clip)>;
    let frames: Vec<(&str, Edit)> = vec![
        ("plain cut", Box::new(|_| {})),
        ("ellipse mask, feathered", Box::new(move |c| c.mask = Some(mask))),
        (
            "blur sigma 6",
            Box::new(|c| c.effects = vec![VideoEffect::Blur { sigma: 6.0 }]),
        ),
        (
            "sharpen 0.5",
            Box::new(|c| c.effects = vec![VideoEffect::Sharpen { amount: 0.5 }]),
        ),
        (
            "moving zoom 1 -> 1.25",
            Box::new(move |c| c.keyframes = vec![key(0.0, 1.0, 0.0), key(2.0, 1.25, -0.05)]),
        ),
        (
            "zoom + mask + blur",
            Box::new(move |c| {
                c.keyframes = vec![key(0.0, 1.0, 0.0), key(2.0, 1.25, -0.05)];
                c.mask = Some(mask);
                c.effects = vec![VideoEffect::Blur { sigma: 6.0 }];
            }),
        ),
    ];
    let mut report = format!(
        "adapter: {} ({:?}, {:?}); ffmpeg: {}; a 1920x1080 30 fps source, min of 7 runs after a warm-up (median in brackets)\n",
        info.name,
        info.backend,
        info.device_type,
        kerf_core::ffmpeg_path(),
    );
    let both = |v: Vec<Duration>| {
        let min = v.iter().min().copied().unwrap_or_default();
        format!("{:>6.0} ({:.0})", ms(min), ms(median(v)))
    };
    // The mask plane, on its own: made once per (mask, size).
    for (w, h) in [(960u32, 540u32), (1920, 1080)] {
        let t0 = Instant::now();
        let plane = kerf_core::mask_alpha_plane(&mask, w, h);
        let line = format!(
            "mask plane {w}x{h}: {:.1} ms, once ({} bytes)\n",
            ms(t0.elapsed()),
            plane.len()
        );
        eprint!("{line}");
        report.push_str(&line);
    }
    report.push_str(&format!(
        "\n{:<26} {:<8} {:>14} {:>16} {:>19}\n",
        "still of", "render", "ffmpeg ms", "gpu decode ms", "gpu composite ms"
    ));
    for (name, width) in [("960 px", 960u32), ("1080p", u32::MAX)] {
        for (label, edit) in &frames {
            let mut tl = timeline(&asset, 1);
            edit(&mut tl.tracks[0].clips[0]);
            tl.tracks[0].clips[0].source_out = 2.0;
            let plan = RenderPlan::at(&tl, assets, &opts, 1.0, policy).expect("plan");
            let size = plan.size(width);
            assert!(plan.reasons(&Compositor::CAPS, size).is_empty(), "{label}");
            let (mut ff, mut dec, mut comp_t) = (Vec::new(), Vec::new(), Vec::new());
            for run in 0..8 {
                let t0 = Instant::now();
                export_still(&tl, assets, &opts, 1.0, &scratch, ImageFormat::Jpeg, 4).expect("ffmpeg still");
                let ffmpeg_time = t0.elapsed();
                let (_, timings) = comp.render_plan(&plan, size).expect("gpu render");
                if run > 0 {
                    ff.push(ffmpeg_time);
                    dec.push(timings.decode);
                    comp_t.push(timings.composite);
                }
            }
            let line = format!(
                "{:<26} {:<8} {:>14} {:>16} {:>19}\n",
                label,
                name,
                both(ff),
                both(dec),
                both(comp_t)
            );
            eprint!("{line}");
            report.push_str(&line);
        }
    }
    // An export frame: FFmpeg's export of the second (graph + a cheap x264, 30 frames, the cost of the
    // plain cut being the part that is not the effect) against the compositor's frames through one cursor
    // per clip, forward.
    report.push_str(&format!(
        "\n{:<26} {:>26} {:>26}\n",
        "export of a second, 960x540", "ffmpeg ms a frame", "gpu ms a frame (decode + composite)"
    ));
    let export_opts = ExportOptions {
        resolution: Some((960, 540)),
        fps: Some(30.0),
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..ExportOptions::default()
    };
    let out = dir.join(format!("bench-zoom-{}.mkv", std::process::id()));
    for (label, edit) in &frames {
        let mut tl = timeline(&asset, 1);
        edit(&mut tl.tracks[0].clips[0]);
        tl.tracks[0].clips[0].source_in = 0.5;
        tl.tracks[0].clips[0].source_out = 1.5;
        tl.format = Some(Delivery::new(960, 540, Fit::Cover));
        let planner =
            kerf_core::Planner::new(&tl, assets, &export_opts, kerf_core::PlanRequest::motion(policy)).expect("planner");
        let size = planner.at_frame(0).expect("plan").size(960);
        let (mut ff, mut gpu_t) = (Vec::new(), Vec::new());
        for run in 0..4 {
            let t0 = Instant::now();
            kerf_core::render_with(&tl, assets, &out, &export_opts).expect("ffmpeg export");
            let ffmpeg_time = t0.elapsed();
            let mut motion = kerf_gpu::MotionFrames::new(kerf_gpu::FrameSource::new(kerf_gpu::FrameSourceConfig::default()));
            let t0 = Instant::now();
            for k in 0..30 {
                let plan = planner.at_frame(k).expect("plan");
                assert!(plan.reasons(&Compositor::CAPS, size).is_empty(), "{label} frame {k}");
                let frames = motion.frames(&plan).expect("cursor frames");
                comp.composite_shared(&plan, &frames, size).expect("composite");
            }
            let gpu_time = t0.elapsed();
            if run > 0 {
                ff.push(ffmpeg_time / 30);
                gpu_t.push(gpu_time / 30);
            }
        }
        let line = format!("{:<26} {:>26} {:>26}\n", label, both(ff), both(gpu_t));
        eprint!("{line}");
        report.push_str(&line);
    }
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&scratch);
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap().join("parity");
    let _ = std::fs::create_dir_all(&out);
    let _ = std::fs::write(out.join("bench-zoom-mask-effects.txt"), &report);
}

/// A chroma key at 1080p (the picture at its own size: the key is drawn only where nothing scales ahead of
/// it), against the plain cut: FFmpeg's still and a second of its export, and the compositor's. Same gate.
#[test]
#[ignore = "benchmark: set KERF_BENCH=1; needs ffmpeg and a GPU adapter"]
fn bench_a_chroma_key() {
    if std::env::var_os("KERF_BENCH").is_none() {
        eprintln!("skipped: set KERF_BENCH=1 to run the benchmark");
        return;
    }
    use kerf_core::{Container, RateControl, VideoEffect};
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let info = gpu.adapter_info().clone();
    let comp = Compositor::new(gpu).expect("compositor");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let scratch = dir.join(format!("bench-key-{}.jpg", std::process::id()));
    let asset = playback_source(dir, 1920, 1080, 2);
    let assets = std::slice::from_ref(&asset);
    let opts = ExportOptions::default();
    let policy = kerf_core::composite_color_policy();
    let cases: [(&str, Vec<VideoEffect>); 2] = [
        ("plain cut", Vec::new()),
        (
            "chroma key",
            vec![VideoEffect::ChromaKey {
                color: "green".into(),
                similarity: 0.15,
                blend: 0.1,
            }],
        ),
    ];
    let both = |v: Vec<Duration>| {
        let min = v.iter().min().copied().unwrap_or_default();
        format!("{:>6.0} ({:.0})", ms(min), ms(median(v)))
    };
    let mut report = format!(
        "adapter: {} ({:?}, {:?}); ffmpeg: {}; a 1920x1080 30 fps source, {} graph threads, min of the runs after a warm-up (median in brackets)\n\n{:<12} {:>14} {:>16} {:>19}\n",
        info.name,
        info.backend,
        info.device_type,
        kerf_core::ffmpeg_path(),
        kerf_core::cpu_graph_threads(),
        "still 1080p",
        "ffmpeg ms",
        "gpu decode ms",
        "gpu composite ms"
    );
    for (label, effects) in &cases {
        let mut tl = timeline(&asset, 1);
        tl.tracks[0].clips[0].effects = effects.clone();
        tl.tracks[0].clips[0].source_out = 2.0;
        let plan = RenderPlan::at(&tl, assets, &opts, 1.0, policy).expect("plan");
        let size = plan.size(u32::MAX);
        assert!(plan.reasons(&Compositor::CAPS, size).is_empty(), "{label}");
        let (mut ff, mut dec, mut comp_t) = (Vec::new(), Vec::new(), Vec::new());
        for run in 0..8 {
            let t0 = Instant::now();
            export_still(&tl, assets, &opts, 1.0, &scratch, ImageFormat::Jpeg, 4).expect("ffmpeg still");
            let ffmpeg_time = t0.elapsed();
            let (_, timings) = comp.render_plan(&plan, size).expect("gpu render");
            if run > 0 {
                ff.push(ffmpeg_time);
                dec.push(timings.decode);
                comp_t.push(timings.composite);
            }
        }
        let line = format!("{:<12} {:>14} {:>16} {:>19}\n", label, both(ff), both(dec), both(comp_t));
        eprint!("{line}");
        report.push_str(&line);
    }
    report.push_str(&format!(
        "\n{:<12} {:>26} {:>26}\n",
        "export 1080p", "ffmpeg ms a frame", "gpu ms a frame (decode + composite)"
    ));
    let export_opts = ExportOptions {
        fps: Some(30.0),
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..ExportOptions::default()
    };
    let out = dir.join(format!("bench-key-{}.mkv", std::process::id()));
    for (label, effects) in &cases {
        let mut tl = timeline(&asset, 1);
        tl.tracks[0].clips[0].effects = effects.clone();
        tl.tracks[0].clips[0].source_in = 0.5;
        tl.tracks[0].clips[0].source_out = 1.5;
        let planner =
            kerf_core::Planner::new(&tl, assets, &export_opts, kerf_core::PlanRequest::motion(policy)).expect("planner");
        let size = planner.at_frame(0).expect("plan").size(u32::MAX);
        let (mut ff, mut gpu_t) = (Vec::new(), Vec::new());
        for run in 0..4 {
            let t0 = Instant::now();
            kerf_core::render_with(&tl, assets, &out, &export_opts).expect("ffmpeg export");
            let ffmpeg_time = t0.elapsed();
            let mut motion = kerf_gpu::MotionFrames::new(kerf_gpu::FrameSource::new(kerf_gpu::FrameSourceConfig::default()));
            let t0 = Instant::now();
            for k in 0..30 {
                let plan = planner.at_frame(k).expect("plan");
                assert!(plan.reasons(&Compositor::CAPS, size).is_empty(), "{label} frame {k}");
                let frames = motion.frames(&plan).expect("cursor frames");
                comp.composite_shared(&plan, &frames, size).expect("composite");
            }
            let gpu_time = t0.elapsed();
            if run > 0 {
                ff.push(ffmpeg_time / 30);
                gpu_t.push(gpu_time / 30);
            }
        }
        let line = format!("{:<12} {:>26} {:>26}\n", label, both(ff), both(gpu_t));
        eprint!("{line}");
        report.push_str(&line);
    }
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&scratch);
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap().join("parity");
    let _ = std::fs::create_dir_all(&out);
    let _ = std::fs::write(out.join("bench-chroma-key.txt"), &report);
}
