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
        }],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
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
        markers: Vec::new(),
        format: None,
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
            let plan = RenderPlan::at(&tl, assets, &opts, 0.5).expect("plan");
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
