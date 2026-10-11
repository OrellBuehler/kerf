//! `Hint::Prefetch` against the real `ffmpeg`: a run is waiting for the first frame of the clip a
//! cut is about to bring, the request that follows finds it, and nothing about the call waits for a
//! frame, decodes one on its own or takes a run from anybody. Also the number it exists for: how
//! long the first frame past a cut takes with and without it (`KERF_BENCH=1`).
//!
//! Needs `ffmpeg` and `ffprobe` on `PATH` (or `KERF_FFMPEG` / `KERF_FFPROBE`); no GPU. `#[ignore]`d;
//! run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test prefetch -- --ignored --nocapture
//! KERF_BENCH=1 KERF_BENCH_OUT=<dir> cargo test -p kerf-gpu --no-default-features --release \
//!     --test prefetch -- --ignored --nocapture a_cut_costs
//! ```

#![allow(clippy::print_stderr)] // the numbers, for whoever runs the suite

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use kerf_core::{
    Asset, Clip, CompositeColorPolicy, ExportOptions, Pick, PlanLayer, Project, RenderPlan, StreamKind, Timeline, Track,
};
use kerf_gpu::{FrameSource, FrameSourceConfig, Hint};

fn ffmpeg(args: &[&str]) {
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("prefetch-media");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    })
}

/// `name`: 1280x720, 30 fps, `secs` long, every frame a keyframe (what a preview proxy is).
fn proxy(name: &str, secs: u32) -> Asset {
    static MAKING: Mutex<()> = Mutex::new(());
    let path = dir().join(name);
    let making = MAKING.lock().unwrap_or_else(|e| e.into_inner());
    if !path.exists() {
        let part = dir().join(format!("part-{name}"));
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size=1280x720:rate=30:duration={secs}"),
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "24",
            "-g",
            "1",
            "-pix_fmt",
            "yuv420p",
            part.to_str().unwrap(),
        ]);
        std::fs::rename(&part, &path).unwrap();
    }
    drop(making);
    Project::probe_asset(&path).expect("probe")
}

/// The layer asking for source time `t` of `a`, read as a proxy is.
fn at(a: &Asset, t: f64) -> PlanLayer {
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![Clip::new(a.id, 0.0, a.duration, 0.0)],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    };
    let mut layer = RenderPlan::at(
        &tl,
        std::slice::from_ref(a),
        &ExportOptions::default(),
        0.0,
        CompositeColorPolicy::FixedBt601,
    )
    .expect("plan")
    .layers
    .remove(0);
    layer.source_time = t;
    layer.pick = Pick::AtOrAfter(t);
    layer.source.proxy = true;
    layer
}

fn source() -> Arc<FrameSource> {
    FrameSource::new(FrameSourceConfig::default())
}

/// A copy of `a` under another name: a file the process has not probed.
fn copy_of(a: &Asset, name: &str) -> Asset {
    let to = dir().join(name);
    std::fs::copy(&a.path, &to).unwrap();
    Project::probe_asset(&to).expect("probe")
}

/// Wait until the frames the source holds stop changing (a run that read ahead is parked), and
/// say how many frames it has taken in.
fn settle(src: &FrameSource) -> u64 {
    let mut last = src.stats().cache.inserts;
    loop {
        std::thread::sleep(Duration::from_millis(120));
        let now = src.stats().cache.inserts;
        if now == last {
            return now;
        }
        last = now;
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn a_prefetch_has_a_run_waiting_and_the_request_that_follows_starts_nothing() {
    let (a, b) = (proxy("pf-a.mp4", 6), proxy("pf-b.mp4", 6));
    let src = source();
    // Playing clip A.
    for k in 0..10 {
        let _ = src
            .frame(&at(&a, 1.0 + f64::from(k) / 30.0), Hint::Forward { fps: 30.0 })
            .unwrap()
            .unwrap();
    }
    assert_eq!(src.stats().spawned, 1);
    let taken = settle(&src);

    // The cut to B is coming.
    let began = Instant::now();
    let nothing = src.frame(&at(&b, 2.0), Hint::Prefetch).expect("a prefetch");
    assert!(nothing.is_none(), "a prefetch draws nothing");
    assert!(began.elapsed() < Duration::from_secs(5), "and waits for no frame");
    assert_eq!(src.stats().spawned, 2, "it started B's run");
    assert_eq!(src.stats().prefetched, 1);
    wait_for("the run to draw its first frame", || src.stats().cache.inserts > taken);

    // The cut: B's first frame is already there.
    let at_the_cut = Instant::now();
    let got = src.frame(&at(&b, 2.0), Hint::Forward { fps: 30.0 }).unwrap();
    assert!(got.is_some());
    assert!(
        at_the_cut.elapsed() < Duration::from_millis(100),
        "{:?}",
        at_the_cut.elapsed()
    );
    let stats = src.stats();
    assert_eq!((stats.spawned, stats.replaced, stats.oneshots), (2, 0, 0), "{stats:?}");

    // And playing on reads that run, not a third.
    for k in 1..20 {
        let _ = src
            .frame(&at(&b, 2.0 + f64::from(k) / 30.0), Hint::Forward { fps: 30.0 })
            .unwrap();
    }
    assert_eq!(src.stats().spawned, 2);
}

#[test]
#[ignore = "needs ffmpeg"]
fn a_prefetch_of_a_frame_already_held_or_heading_for_starts_nothing() {
    let a = proxy("pf-a.mp4", 6);
    let src = source();
    let _ = src.frame(&at(&a, 1.0), Hint::Scrub).unwrap().unwrap();
    let spawned = src.stats().spawned;
    // Held: the cache has it.
    src.frame(&at(&a, 1.0), Hint::Prefetch).unwrap();
    // A frame ahead of the run, within what it reads forward to serve a request.
    src.frame(&at(&a, 1.3), Hint::Prefetch).unwrap();
    // Asked twice.
    let b = copy_of(&a, "pf-copy-1.mp4");
    src.frame(&at(&b, 2.0), Hint::Prefetch).unwrap();
    src.frame(&at(&b, 2.0), Hint::Prefetch).unwrap();
    let stats = src.stats();
    assert_eq!(stats.spawned, spawned + 1, "only the unheld file's run: {stats:?}");
    assert_eq!(stats.prefetched, 1);
}

#[test]
#[ignore = "needs ffmpeg"]
fn a_prefetch_only_takes_a_spare_slot() {
    let a = proxy("pf-a.mp4", 6);
    let src = source();
    // Three runs of one file, each far enough from the others (more than 24 frames) to be its own:
    // the file's cap.
    for t in [0.2, 2.0, 4.0] {
        let _ = src.frame(&at(&a, t), Hint::Scrub).unwrap().unwrap();
    }
    let before = src.stats();
    assert_eq!((before.spawned, before.runs), (3, 3), "{before:?}");
    src.frame(&at(&a, 5.0), Hint::Prefetch).unwrap();
    let after = src.stats();
    assert_eq!(
        (after.spawned, after.replaced, after.runs),
        (3, 0, 3),
        "no slot, no run, no eviction: {after:?}"
    );
}

#[test]
#[ignore = "needs ffmpeg"]
fn a_prefetch_decodes_nothing_for_a_file_runs_are_not_trusted_for() {
    let a = proxy("pf-a.mp4", 6);
    // The same picture as a transport stream: a container runs are not proved equal on.
    let ts = dir().join("pf-a.ts");
    if !ts.exists() {
        ffmpeg(&["-i", &a.path, "-c", "copy", ts.to_str().unwrap()]);
    }
    let ts = Project::probe_asset(&ts).expect("probe");
    let src = source();
    let layers = [at(&ts, 1.0)];
    let got = src.frames(&layers, Hint::Prefetch).expect("a prefetch");
    assert_eq!(got.len(), 1);
    assert!(got[0].is_none());
    let stats = src.stats();
    assert_eq!(
        (stats.spawned, stats.oneshots),
        (0, 0),
        "nothing was started or decoded: {stats:?}"
    );
}

#[test]
#[ignore = "needs ffmpeg"]
fn a_prefetch_returns_nothing_for_every_layer() {
    let (a, b) = (proxy("pf-a.mp4", 6), proxy("pf-b.mp4", 6));
    let src = source();
    let got = src.frames(&[at(&a, 1.0), at(&b, 1.0)], Hint::Prefetch).expect("a prefetch");
    assert!(got.iter().all(Option::is_none), "{} frames", got.len());
    assert_eq!(src.stats().spawned, 2, "a run for each");
}

#[test]
#[ignore = "needs ffmpeg"]
fn dropping_the_source_kills_a_prefetched_run() {
    let a = proxy("pf-a.mp4", 6);
    let before = ffmpeg_children();
    {
        let src = source();
        src.frame(&at(&a, 1.0), Hint::Prefetch).unwrap();
        wait_for("its run", || src.stats().runs == 1);
        wait_for("its process", || ffmpeg_children() > before);
    }
    wait_for("the run's process to be gone", || ffmpeg_children() <= before);
}

/// Processes named after the ffmpeg this suite runs that are children of this one (Linux `/proc`).
fn ffmpeg_children() -> usize {
    let me = std::process::id().to_string();
    let Ok(procs) = std::fs::read_dir("/proc") else { return 0 };
    procs
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path().join("stat")).ok())
        .filter(|stat| {
            let after_name = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
            let mut fields = after_name.split_whitespace();
            let (state, ppid) = (fields.next().unwrap_or(""), fields.next().unwrap_or(""));
            stat.contains("(ffmpeg") && ppid == me && state != "Z"
        })
        .count()
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// The first frame past a cut, with and without a prefetch: clip A is playing, the cut brings B (a
/// file nobody has read), and the time B's first frame takes to come back is measured. Each trial
/// has a file of its own, so the probes (container, seek points) are cold every time, as the first
/// time a clip is reached is; a third kind of trial has the probes warm (a clip reached before) and
/// only the run to start, which is the stall the review measured.
#[test]
#[ignore = "needs ffmpeg; KERF_BENCH=1"]
fn a_cut_costs_a_process_start_without_a_prefetch_and_nothing_with_one() {
    if std::env::var_os("KERF_BENCH").is_none() {
        eprintln!("set KERF_BENCH=1 to time it");
        return;
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        Cold,
        ProbesWarm,
        Prefetched,
    }
    let a = proxy("pf-a.mp4", 6);
    let b = proxy("pf-b.mp4", 6);
    const TRIALS: usize = 12;
    let (mut cold, mut warm, mut prefetched) = (Vec::new(), Vec::new(), Vec::new());
    for trial in 0..TRIALS {
        for kind in [Kind::Cold, Kind::ProbesWarm, Kind::Prefetched] {
            let next = copy_of(&b, &format!("pf-trial-{trial}-{}.mp4", kind as u8));
            let src = source();
            // Playing A, forward, to the cut.
            for k in 0..30 {
                let _ = src
                    .frame(&at(&a, 1.0 + f64::from(k) / 30.0), Hint::Forward { fps: 30.0 })
                    .unwrap()
                    .unwrap();
            }
            match kind {
                Kind::Cold => {}
                Kind::ProbesWarm => {
                    let path = std::path::Path::new(&next.path);
                    assert!(kerf_core::source_is_indexed_container(path));
                    assert!(kerf_core::source_seek_points_are_keyframes(path));
                }
                Kind::Prefetched => {
                    let taken = settle(&src);
                    src.frame(&at(&next, 0.5), Hint::Prefetch).unwrap();
                    // The time the playing before the cut gives it.
                    wait_for("the prefetched frame", || src.stats().cache.inserts > taken);
                }
            }
            let began = Instant::now();
            let got = src.frame(&at(&next, 0.5), Hint::Forward { fps: 30.0 }).unwrap();
            let ms = began.elapsed().as_secs_f64() * 1e3;
            assert!(got.is_some());
            match kind {
                Kind::Cold => &mut cold,
                Kind::ProbesWarm => &mut warm,
                Kind::Prefetched => &mut prefetched,
            }
            .push(ms);
            drop(src);
            let _ = std::fs::remove_file(&next.path);
        }
    }
    let version = {
        let out = Command::new(kerf_core::ffmpeg_path()).arg("-version").output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(2).map(str::to_string))
            .unwrap_or_default()
    };
    let (cold_med, warm_med, pre_med) = (median(cold.clone()), median(warm.clone()), median(prefetched.clone()));
    let max = |v: &[f64]| v.iter().copied().fold(0.0, f64::max);
    eprintln!(
        "ffmpeg {version} | first frame of the clip past a cut, 1280x720 all-intra: a clip never read before, no prefetch: median {cold_med:.1} ms (max {:.1}); its probes already done, no prefetch: median {warm_med:.1} ms (max {:.1}); with a prefetch: median {pre_med:.2} ms (max {:.2})",
        max(&cold),
        max(&warm),
        max(&prefetched)
    );
    if let Some(out) = std::env::var_os("KERF_BENCH_OUT").map(PathBuf::from) {
        std::fs::create_dir_all(&out).unwrap();
        let list = |v: &[f64]| v.iter().map(|x| format!("{x:.2}")).collect::<Vec<_>>().join(",");
        std::fs::write(
            out.join(format!("cut-stall-{version}.json")),
            format!(
                "{{\"ffmpeg\":\"{version}\",\"source\":\"1280x720 30 fps all-intra x264 (proxy shape)\",\"trials\":{TRIALS},\"cold_no_prefetch_ms\":[{}],\"probes_warm_no_prefetch_ms\":[{}],\"with_prefetch_ms\":[{}],\"cold_no_prefetch_median_ms\":{cold_med:.2},\"probes_warm_no_prefetch_median_ms\":{warm_med:.2},\"with_prefetch_median_ms\":{pre_med:.2}}}\n",
                list(&cold),
                list(&warm),
                list(&prefetched)
            ),
        )
        .unwrap();
    }
    assert!(
        pre_med < warm_med && warm_med < cold_med,
        "a prefetch made the cut no cheaper"
    );
}
