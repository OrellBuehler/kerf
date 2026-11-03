//! `prepare_placed_files` (a play session asks the files of the placed clips of its cut for their range): where the composite's range is
//! negotiated (9.0.2) a placed clip whose asset did not record its range is refused by the session's caps and by the render until its file has
//! said. The asks run four at a time and none is started after twenty seconds, so a cut with many placed files costs the session about that
//! and not one probe each. What this holds it to, with an `ffprobe` that sleeps (a wrapper script that counts every call of a file):
//!
//! * every distinct placed file is asked once, whatever the number of clips, and in parallel (not one after the other, not all at once);
//! * a file that is not placed is not asked (the old code marked every layer of a placed clip's plan as asked);
//! * past the budget no new probe starts, the call returns after the probes it started, and a file left unasked is **refused** by the session's
//!   caps and by the render, never drawn, until it is asked;
//! * a file whose probe failed or said nothing is refused too, not drawn.
//!
//! Each scenario is a child process of its own (the answers are kept for the life of the process, and the child's `KERF_FFPROBE` is the
//! wrapper). Where the composite's range is not negotiated (6.1.1) nothing is asked and the scenarios say so.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_b_m_files -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kerf_core::{
    Asset, Clip, CompositeColorPolicy, Delivery, Fit, Keyframe, OriginalMedia, PlayCut, Project, StreamKind, Timeline, Track,
    Unsupported,
};
use kerf_gpu::{Compositor, Gpu, GpuError, GpuOptions, YuvFrame};

const SCENARIO: &str = "KERF_FILES_SCENARIO";
const DIR: &str = "KERF_FILES_DIR";
const REAL_FFPROBE: &str = "KERF_FILES_REAL_FFPROBE";

/// Every call is logged (`$DIR/ffprobe.log`); once `$DIR/armed` exists a call about a file named `placed-*` sleeps `$DIR/sleep` seconds
/// (one named `broken-*` prints nothing and exits 0: a probe that says nothing usable; one named `hung-*` never answers) before the real `ffprobe` answers.
const FFPROBE_WRAPPER: &str = r#"#!/bin/bash
D="$KERF_FILES_DIR"; REAL="$KERF_FILES_REAL_FFPROBE"
echo "$*" >> "$D/ffprobe.log"
if [ -e "$D/armed" ]; then
  case "$*" in
    */broken-*) exit 0 ;;
    */hung-*) exec sleep 300 ;;
    */placed-*) sleep "$(cat "$D/sleep")" ;;
  esac
fi
exec "$REAL" "$@"
"#;

fn lines_with(log: &Path, needle: &str) -> usize {
    std::fs::read_to_string(log).map_or(0, |t| t.lines().filter(|l| l.contains(needle)).count())
}

fn child(scenario: &str, limit: Duration) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("checker-b-m-files")
        .join(scenario);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let wrapper = dir.join("ffprobe-wrapper.sh");
    std::fs::write(&wrapper, FFPROBE_WRAPPER).expect("write the wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
    let real = std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string());
    let (out, err) = (dir.join("child.out"), dir.join("child.err"));
    let mut process = Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--ignored", "--exact", "--nocapture", "checker_b_m_files_child"])
        .env(SCENARIO, scenario)
        .env(DIR, &dir)
        .env(REAL_FFPROBE, real)
        .env("KERF_FFPROBE", &wrapper)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&out).expect("a stdout file"))
        .stderr(std::fs::File::create(&err).expect("a stderr file"))
        .spawn()
        .expect("run the scenario");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = process.try_wait().expect("wait") {
            break Some(status);
        }
        if started.elapsed() > limit {
            let _ = process.kill();
            let _ = process.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let say = std::fs::read_to_string(&err).unwrap_or_default();
    eprintln!("== {scenario} ({:.1?}) ==\n{say}", started.elapsed());
    let passed = status.is_some_and(|s| s.success());
    assert!(
        passed,
        "{scenario}: {} (see {})",
        status.map_or("did not finish in time".to_string(), |s| s.to_string()),
        err.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn make(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let out = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x36:rate=30:duration=1",
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    path
}

/// An asset of `path` whose range was not recorded (probed before the flag was kept).
fn unrecorded(path: &Path) -> Asset {
    let mut asset = Project::probe_asset(path).expect("probe the asset");
    for stream in &mut asset.streams {
        stream.color_range = None;
    }
    asset
}

fn key(time: f64, scale: f64) -> Keyframe {
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

struct Session {
    timeline: Timeline,
    assets: Vec<Asset>,
    /// The placed clips' file paths, in timeline order.
    placed: Vec<String>,
    /// The file of the one clip that is not placed.
    plain: String,
}

/// A base clip on V1 and `placed` clips one after the other on V2, each of its own file (`prefix-i.mkv`) and moving its zoom.
fn session(dir: &Path, prefix: &str, placed: usize) -> Session {
    let plain = make(dir, "plain-base.mkv");
    let base = unrecorded(&plain);
    let mut assets = vec![base.clone()];
    let mut clips = Vec::new();
    let mut paths = Vec::new();
    for i in 0..placed {
        let path = make(dir, &format!("{prefix}-{i}.mkv"));
        let asset = unrecorded(&path);
        let mut c = Clip::new(asset.id, 0.0, 0.4, 0.4 * i as f64);
        c.keyframes = vec![key(0.0, 1.0), key(0.4, 1.6)];
        clips.push(c);
        paths.push(path.to_string_lossy().into_owned());
        assets.push(asset);
    }
    let timeline = Timeline {
        tracks: vec![
            Track {
                clips: vec![Clip::new(base.id, 0.0, 0.4 * placed as f64, 0.0)],
                ..Track::new(StreamKind::Video, "V1".to_string())
            },
            Track {
                clips,
                ..Track::new(StreamKind::Video, "V2".to_string())
            },
        ],
        format: Some(Delivery::new(64, 36, Fit::Contain)),
        ..Timeline::new()
    };
    Session {
        timeline,
        assets,
        placed: paths,
        plain: plain.to_string_lossy().into_owned(),
    }
}

fn flat(width: u32, height: u32) -> Arc<YuvFrame> {
    Arc::new(YuvFrame {
        width,
        height,
        y: vec![90; (width * height) as usize],
        u: vec![120; (width.div_ceil(2) * height.div_ceil(2)) as usize],
        v: vec![130; (width.div_ceil(2) * height.div_ceil(2)) as usize],
    })
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

#[test]
#[ignore = "this binary's own child: runs the scenario named in the environment"]
fn checker_b_m_files_child() {
    let Ok(scenario) = std::env::var(SCENARIO) else {
        return;
    };
    let dir = PathBuf::from(env(DIR));
    match scenario.as_str() {
        "parallel" => parallel(&dir),
        "budget" => budget(&dir),
        "broken" => broken(&dir),
        "hung" => hung(&dir),
        other => panic!("no scenario {other}"),
    }
}

fn policy() -> CompositeColorPolicy {
    match kerf_core::composite_color_policy() {
        CompositeColorPolicy::Unknown => panic!("the composite colour probe could not read this FFmpeg"),
        p => p,
    }
}

/// The plan of the frame in the middle of the `i`th placed clip, over `cut`.
fn plan_of(cut: &PlayCut, i: usize) -> kerf_core::RenderPlan {
    cut.at_frame(cut.frame_at(0.4 * i as f64 + 0.2)).expect("a plan")
}

fn ranged(why: &[Unsupported]) -> bool {
    why.iter()
        .any(|u| matches!(u, Unsupported::PlacedFormat(_, d) if d.contains("has not said")))
}

fn parallel(dir: &Path) {
    let probes = dir.join("ffprobe.log");
    let s = session(dir, "placed", 8);
    let policy = policy();
    kerf_gpu::prepare_timeline(&s.timeline);
    let cut = PlayCut::new(&s.timeline, &s.assets, 0.0, policy, &OriginalMedia).expect("a cut");
    std::fs::write(dir.join("sleep"), "1.5").expect("the sleep");
    std::fs::write(dir.join("armed"), "").expect("arm the wrapper");
    let negotiated = policy == CompositeColorPolicy::BottomLayerTag;
    let before = lines_with(&probes, "placed-");
    let plain_before = lines_with(&probes, "plain-");
    let started = Instant::now();
    kerf_gpu::prepare_placed_files(&cut, &s.timeline);
    let took = started.elapsed();
    let calls = lines_with(&probes, "placed-") - before;
    eprintln!("8 placed files, 1.5 s each: {calls} probes, {took:.1?}");
    if !negotiated {
        assert_eq!(calls, 0, "the composite's range is not negotiated and files were asked");
        assert!(took < Duration::from_secs(1), "{took:?}");
        return;
    }
    assert_eq!(calls, 8, "every distinct placed file is asked once");
    assert!(
        took >= Duration::from_millis(2800) && took < Duration::from_secs(6),
        "8 probes of 1.5 s, four at a time, took {took:?} (two rounds is 3 s; one after the other 12 s; all at once 1.5 s)"
    );
    for (i, path) in s.placed.iter().enumerate() {
        assert_eq!(
            kerf_core::source_is_full_range_known(Path::new(path)),
            Some(false),
            "placed file {i}"
        );
    }
    assert_eq!(
        kerf_core::source_is_full_range_known(Path::new(&s.plain)),
        None,
        "a file that is not placed was asked"
    );
    assert_eq!(
        lines_with(&probes, "plain-"),
        plain_before,
        "ffprobe was run on a file that is not placed"
    );
    // Asked again (every clip's frame, the same files): nothing is probed twice.
    kerf_gpu::prepare_placed_files(&cut, &s.timeline);
    assert_eq!(lines_with(&probes, "placed-") - before, 8, "a known file was probed again");
    let caps = Compositor::session_caps_for(
        kerf_gpu::grade::Kernels::Match,
        kerf_gpu::scaler::Kernels::Match(kerf_gpu::scaler::Model::ASSUMED),
        kerf_gpu::convert::Kernels::Match,
    );
    for i in 0..8 {
        let plan = plan_of(&cut, i);
        let why = plan.reasons(&caps, (64, 36));
        assert!(!ranged(&why), "clip {i} is asked and still refused for its range: {why:?}");
    }
}

fn budget(dir: &Path) {
    let probes = dir.join("ffprobe.log");
    let s = session(dir, "placed", 12);
    let policy = policy();
    kerf_gpu::prepare_timeline(&s.timeline);
    let cut = PlayCut::new(&s.timeline, &s.assets, 0.0, policy, &OriginalMedia).expect("a cut");
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    let comp = Compositor::new(gpu)
        .expect("a compositor")
        .with_placement(kerf_gpu::placement_probe::Placement::Match)
        .with_scaler_kernels(kerf_gpu::scaler::Kernels::Match(kerf_gpu::scaler::Model::ASSUMED))
        .with_convert_kernels(kerf_gpu::convert::Kernels::Match)
        .with_grade_kernels(kerf_gpu::grade::Kernels::Match);
    std::fs::write(dir.join("sleep"), "12").expect("the sleep");
    std::fs::write(dir.join("armed"), "").expect("arm the wrapper");
    let negotiated = policy == CompositeColorPolicy::BottomLayerTag;
    let before = lines_with(&probes, "placed-");
    let started = Instant::now();
    kerf_gpu::prepare_placed_files(&cut, &s.timeline);
    let took = started.elapsed();
    let asked: Vec<bool> = s
        .placed
        .iter()
        .map(|p| kerf_core::source_is_full_range_known(Path::new(p)).is_some())
        .collect();
    eprintln!(
        "12 placed files, 12 s each: {} asked, {took:.1?}, {} probe runs",
        asked.iter().filter(|a| **a).count(),
        lines_with(&probes, "placed-") - before
    );
    if !negotiated {
        assert!(
            asked.iter().all(|a| !a) && took < Duration::from_secs(1),
            "{asked:?} {took:?}"
        );
        return;
    }
    // Four at a time, a second round started at 12 s (inside the 20 s), a third not: eight asked, four left, and the call over at about 24 s.
    assert!(
        took >= Duration::from_secs(22) && took < Duration::from_secs(40),
        "the call took {took:?}: no new probe may start after 20 s, and the ones started are waited for"
    );
    assert_eq!(asked.iter().filter(|a| **a).count(), 8, "{asked:?}");
    assert_eq!(
        lines_with(&probes, "placed-") - before,
        8,
        "a probe was started past the budget"
    );
    let caps = Compositor::session_caps_for(
        kerf_gpu::grade::Kernels::Match,
        kerf_gpu::scaler::Kernels::Match(kerf_gpu::scaler::Model::ASSUMED),
        kerf_gpu::convert::Kernels::Match,
    );
    let (mut refused, mut cleared) = (0, 0);
    for (i, asked) in asked.iter().enumerate() {
        let plan = plan_of(&cut, i);
        // The session's caps: a file left unasked is refused for its range; an asked one is not.
        let why = plan.reasons(&caps, (64, 36));
        assert_eq!(ranged(&why), !asked, "clip {i}, asked {asked}: {why:?}");
        // The render's own check (`composite_yuv_shared`, which never probes): a file left unasked is refused, not drawn.
        let frames: Vec<Option<Arc<YuvFrame>>> = plan
            .layers
            .iter()
            .map(|l| Some(flat(l.stream.width, l.stream.height)))
            .collect();
        let drawn = comp.composite_yuv_shared(&plan, &frames, (64, 36));
        if *asked {
            assert!(
                drawn.is_ok(),
                "clip {i} is asked and was refused by the render: {:?}",
                drawn.err()
            );
            cleared += 1;
        } else {
            assert!(
                matches!(drawn, Err(GpuError::Unsupported(_))),
                "clip {i} was never asked and the render drew it: {:?}",
                drawn.map(|f| (f.width, f.height))
            );
            refused += 1;
        }
    }
    assert_eq!((cleared, refused), (8, 4));
    // The horizon rescan asks what is left (the same asks, with no budget but their own timeout): the four are known afterwards.
    std::fs::write(dir.join("sleep"), "0").expect("no sleep");
    kerf_gpu::prepare_ranges_ahead(&cut, 0.0, 0.4 * 12.0);
    for (i, path) in s.placed.iter().enumerate() {
        assert_eq!(
            kerf_core::source_is_full_range_known(Path::new(path)),
            Some(false),
            "clip {i} after the rescan"
        );
    }
    assert_eq!(lines_with(&probes, "placed-") - before, 12, "each file is probed once in all");
}

fn unanswered(dir: &Path, prefix: &str, n: usize, hung: bool) {
    let probes = dir.join("ffprobe.log");
    // Clips of `broken-*` files: ffprobe prints nothing for them. Files of `placed-*` answer.
    let s = session(dir, prefix, n);
    let policy = policy();
    kerf_gpu::prepare_timeline(&s.timeline);
    let cut = PlayCut::new(&s.timeline, &s.assets, 0.0, policy, &OriginalMedia).expect("a cut");
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    let comp = Compositor::new(gpu)
        .expect("a compositor")
        .with_placement(kerf_gpu::placement_probe::Placement::Match)
        .with_scaler_kernels(kerf_gpu::scaler::Kernels::Match(kerf_gpu::scaler::Model::ASSUMED))
        .with_convert_kernels(kerf_gpu::convert::Kernels::Match)
        .with_grade_kernels(kerf_gpu::grade::Kernels::Match);
    std::fs::write(dir.join("armed"), "").expect("arm the wrapper");
    let negotiated = policy == CompositeColorPolicy::BottomLayerTag;
    let before = lines_with(&probes, &format!("{prefix}-"));
    let started = Instant::now();
    kerf_gpu::prepare_placed_files(&cut, &s.timeline);
    let took = started.elapsed();
    eprintln!(
        "{n} files ffprobe does not answer ({prefix}): {} runs, {took:.1?}",
        lines_with(&probes, &format!("{prefix}-")) - before
    );
    if !negotiated {
        assert_eq!(lines_with(&probes, &format!("{prefix}-")) - before, 0);
        return;
    }
    if hung {
        // Four at a time, each killed at its own 20 s: nothing new starts past the budget, so the call is over at about 20 s with four
        // asked (and left with no answer) and the rest never started.
        assert!(
            took >= Duration::from_secs(19) && took < Duration::from_secs(32),
            "the hung probes took {took:?} to give up"
        );
        assert_eq!(
            lines_with(&probes, &format!("{prefix}-")) - before,
            4,
            "probes started past the budget"
        );
    } else {
        assert!(took < Duration::from_secs(20), "{took:?}");
    }
    let caps = Compositor::session_caps_for(
        kerf_gpu::grade::Kernels::Match,
        kerf_gpu::scaler::Kernels::Match(kerf_gpu::scaler::Model::ASSUMED),
        kerf_gpu::convert::Kernels::Match,
    );
    for (i, path) in s.placed.iter().enumerate() {
        assert_eq!(
            kerf_core::source_is_full_range_known(Path::new(path)),
            None,
            "clip {i}: a probe that said nothing left an answer"
        );
        let plan = plan_of(&cut, i);
        let why = plan.reasons(&caps, (64, 36));
        assert!(
            ranged(&why),
            "clip {i}: a file that ffprobe says nothing of is not refused by the session's caps: {why:?}"
        );
        let frames: Vec<Option<Arc<YuvFrame>>> = plan
            .layers
            .iter()
            .map(|l| Some(flat(l.stream.width, l.stream.height)))
            .collect();
        let drawn = comp.composite_yuv_shared(&plan, &frames, (64, 36));
        assert!(
            matches!(drawn, Err(GpuError::Unsupported(_))),
            "clip {i}: a file that ffprobe says nothing of was drawn"
        );
    }
    if !hung {
        // Not asked again inside the failure's memory (a probe that said nothing is not respawned by every frame).
        let runs = lines_with(&probes, &format!("{prefix}-"));
        kerf_gpu::prepare_placed_files(&cut, &s.timeline);
        assert_eq!(
            lines_with(&probes, &format!("{prefix}-")),
            runs,
            "a failed probe was asked again at once"
        );
    }
}

fn broken(dir: &Path) {
    unanswered(dir, "broken", 3, false);
}

fn hung(dir: &Path) {
    unanswered(dir, "hung", 8, true);
}

#[test]
#[ignore = "needs ffmpeg, ffprobe and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_files_are_asked_once_in_parallel_and_only_the_placed_ones() {
    child("parallel", Duration::from_secs(120));
}

#[test]
#[ignore = "needs ffmpeg, ffprobe and a GPU adapter (lavapipe is enough); takes about half a minute"]
fn checker_b_m_files_past_the_budget_are_left_unasked_and_refused_not_drawn() {
    child("budget", Duration::from_secs(150));
}

#[test]
#[ignore = "needs ffmpeg, ffprobe and a GPU adapter (lavapipe is enough)"]
fn checker_b_m_files_ffprobe_says_nothing_of_are_refused_not_drawn() {
    child("broken", Duration::from_secs(120));
}

#[test]
#[ignore = "needs ffmpeg, ffprobe and a GPU adapter (lavapipe is enough); takes about half a minute"]
fn checker_b_m_files_a_hung_ffprobe_is_given_up_on_within_the_budget_and_the_files_refused() {
    child("hung", Duration::from_secs(150));
}
