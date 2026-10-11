//! What the scrub cache and its window buy, measured (`#[ignore]`d: it drives the real `ffmpeg`
//! and `ffprobe`). One layer and three layers from 1280x720 all-intra proxies (the shape the
//! preview reads), previewed at 960 px:
//!
//! * **before**: the one-shot still per frame, `Project::composite_timeline_frame`, which is what
//!   the Preview asked for on every step;
//! * **first frame**: a cold miss on `ScrubFrames` (a window is started for it);
//! * **marginal frame**: the next frames of a scrub, asked for at the 60 Hz a dragged playhead moves
//!   at, and again back to back once drawn (a pure hit).
//!
//! ```text
//! KERF_BENCH_OUT=/path/to/dir cargo test -p kerf-core --no-default-features --release \
//!     --test scrub_bench -- --ignored --nocapture
//! ```
//!
//! `KERF_FFMPEG` / `KERF_FFPROBE` pick the build; the numbers go to
//! `<out>/scrub-<version>.json` and are printed.

#![allow(clippy::print_stderr)] // the numbers, for whoever runs the suite

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kerf_core::{Asset, Clip, Project, ScrubConfig, ScrubFrames, StreamKind, Timeline, Track};

const WIDTH: u32 = 960;
const QUALITY: u8 = 4;
const FPS: f64 = 30.0;

fn ffmpeg() -> String {
    kerf_core::ffmpeg_path()
}

fn version() -> String {
    let out = Command::new(ffmpeg()).arg("-version").output().expect("ffmpeg -version");
    let first = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    first.split_whitespace().nth(2).unwrap_or("unknown").to_string()
}

/// A proxy-shaped clip: 1280x720, 30 fps, all-intra x264, under `.../kerf/proxies/<16 hex>.mp4` (the
/// name the engine reads a preview proxy by).
fn proxy(dir: &Path, hash: &str, pattern: &str) -> Asset {
    let folder = dir.join("kerf").join("proxies");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join(format!("{hash}.mp4"));
    if !path.exists() {
        let made = Command::new(ffmpeg())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("{pattern}=size=1280x720:rate=30:duration=20"))
            .args([
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "24", "-g", "1", "-pix_fmt", "yuv420p",
            ])
            .arg(&path)
            .stdin(Stdio::null())
            .status()
            .expect("run ffmpeg");
        assert!(made.success());
    }
    Project::probe_asset(&path).expect("probe")
}

fn timeline(tracks: Vec<Vec<Clip>>) -> Timeline {
    Timeline {
        tracks: tracks
            .into_iter()
            .enumerate()
            .map(|(i, clips)| Track {
                clips,
                ..Track::new(StreamKind::Video, format!("V{}", i + 1))
            })
            .collect(),
        overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    }
}

struct Case {
    name: &'static str,
    timeline: Timeline,
    assets: Vec<Asset>,
}

fn cases(dir: &Path) -> Vec<Case> {
    let a = proxy(dir, "00000000000000a1", "testsrc2");
    let b = proxy(dir, "00000000000000b2", "smptebars");
    let c = proxy(dir, "00000000000000c3", "testsrc");
    let one = timeline(vec![vec![Clip::new(a.id, 0.0, 18.0, 0.0)]]);
    let mut pip1 = Clip::new(b.id, 0.0, 18.0, 0.0);
    pip1.transform.scale = 0.4;
    pip1.transform.pos_x = 0.25;
    pip1.transform.pos_y = -0.25;
    let mut pip2 = Clip::new(c.id, 0.0, 18.0, 0.0);
    pip2.transform.scale = 0.3;
    pip2.transform.pos_x = -0.3;
    pip2.transform.pos_y = 0.3;
    let three = timeline(vec![vec![Clip::new(a.id, 0.0, 18.0, 0.0)], vec![pip1], vec![pip2]]);
    vec![
        Case {
            name: "1 layer",
            timeline: one,
            assets: vec![a.clone()],
        },
        Case {
            name: "3 layers",
            timeline: three,
            assets: vec![a, b, c],
        },
    ]
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

#[derive(Default, Clone, Copy)]
struct Stat {
    min: f64,
    median: f64,
    p95: f64,
    n: usize,
}

fn stat(mut ms: Vec<f64>) -> Stat {
    ms.sort_by(f64::total_cmp);
    let at = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
    Stat {
        min: ms[0],
        median: at(0.5),
        p95: at(0.95),
        n: ms.len(),
    }
}

fn time_of(k: u64) -> f64 {
    k as f64 / FPS
}

fn json(s: Stat) -> String {
    format!(
        "{{\"n\":{},\"min_ms\":{:.2},\"median_ms\":{:.2},\"p95_ms\":{:.2}}}",
        s.n, s.min, s.median, s.p95
    )
}

#[test]
#[ignore = "drives the real ffmpeg; run with --release --nocapture"]
fn what_the_scrub_cache_buys() {
    let dir = Scratch(std::env::temp_dir().join(format!("kerf-scrub-bench-{}", std::process::id())));
    let version = version();
    let mut report = Vec::new();
    for case in cases(&dir.0) {
        let assets = &case.assets;
        // Warm the page cache and the process-wide probes once, off the clock.
        let _ = Project::composite_timeline_frame(&case.timeline, assets, 1.0, WIDTH, QUALITY).unwrap();

        // Before: one process per frame, at 40 different frames.
        let mut single = Vec::new();
        for i in 0..40u64 {
            let t = time_of(30 + i * 7);
            let t0 = Instant::now();
            let jpeg = Project::composite_timeline_frame(&case.timeline, assets, t, WIDTH, QUALITY).unwrap();
            single.push(secs(t0.elapsed()));
            assert!(jpeg.len() > 1000);
        }

        // First frame: cold, a window started for it. Fresh cache and process each time.
        let mut first = Vec::new();
        for i in 0..12u64 {
            let scrub = ScrubFrames::new(ScrubConfig::default());
            let k = 40 + i * 31;
            let t0 = Instant::now();
            let jpeg = scrub.frame(&case.timeline, assets, time_of(k), WIDTH, QUALITY).unwrap();
            first.push(secs(t0.elapsed()));
            assert!(jpeg.len() > 1000);
        }

        // Marginal frame, paced like a drag: one step every 1/60 s from a cold start.
        let mut paced = Vec::new();
        let mut paced_total = Vec::new();
        for run in 0..8u64 {
            let scrub = ScrubFrames::new(ScrubConfig::default());
            let k0 = 60 + run * 53;
            let started = Instant::now();
            for step in 0..20u64 {
                let due = Duration::from_secs_f64(step as f64 / 60.0);
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    std::thread::sleep(wait);
                }
                let t0 = Instant::now();
                let _ = scrub
                    .frame(&case.timeline, assets, time_of(k0 + step), WIDTH, QUALITY)
                    .unwrap();
                if step > 0 {
                    paced.push(secs(t0.elapsed()));
                }
            }
            paced_total.push(secs(started.elapsed()));
        }

        // A pure hit: a window already drawn, every frame of it asked for back to back.
        let mut hits = Vec::new();
        let scrub = ScrubFrames::new(ScrubConfig::default());
        let _ = scrub.frame(&case.timeline, assets, time_of(300), WIDTH, QUALITY).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while scrub.stats().drawn < 24 {
            assert!(Instant::now() < until, "the window never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(100));
        for _ in 0..20 {
            for k in 298..322u64 {
                let t0 = Instant::now();
                let _ = scrub.frame(&case.timeline, assets, time_of(k), WIDTH, QUALITY).unwrap();
                hits.push(secs(t0.elapsed()));
            }
        }

        let (before, first, paced, hit) = (stat(single), stat(first), stat(paced), stat(hits));
        let drag_before = before.median * 20.0;
        let drag_after = stat(paced_total).median;
        eprintln!(
            "ffmpeg {version} | {:>8} | per-frame spawn: median {:>6.1} ms (p95 {:>6.1}) | first frame: median {:>6.1} (p95 {:>6.1}) | marginal while dragging at 60 Hz: median {:>5.1} (p95 {:>5.1}) | pure hit: median {:>5.2} (p95 {:>5.2}) | 20-step drag: {:>6.0} ms -> {:>6.0} ms",
            case.name, before.median, before.p95, first.median, first.p95, paced.median, paced.p95, hit.median, hit.p95, drag_before, drag_after
        );
        report.push(format!(
            "{{\"case\":\"{}\",\"per_frame_spawn\":{},\"first_frame\":{},\"marginal_dragging_60hz\":{},\"pure_hit\":{},\"drag_20_steps_before_ms\":{:.1},\"drag_20_steps_after_ms\":{:.1}}}",
            case.name,
            json(before),
            json(first),
            json(paced),
            json(hit),
            drag_before,
            drag_after
        ));
    }
    let body = format!(
        "{{\"ffmpeg\":\"{version}\",\"source\":\"1280x720 30 fps all-intra x264 (proxy shape)\",\"preview_width\":{WIDTH},\"cases\":[{}]}}\n",
        report.join(",")
    );
    if let Some(out) = std::env::var_os("KERF_BENCH_OUT").map(PathBuf::from) {
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join(format!("scrub-{version}.json")), body).unwrap();
    }
}
