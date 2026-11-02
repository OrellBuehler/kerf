//! The gates of the moving still, attacked from outside: the still probe (`kerf_gpu::still_probe`) run against an FFmpeg whose chain
//! is perturbed for one kind of picture (the interpolation of its `perspective`, the scaler of its copy), made to fail, to print too
//! little, and to stall; and a moving still drawn when nothing was measured.
//!
//! Each scenario that changes what the process runs (`KERF_FFMPEG`: a wrapper that perturbs only the probe's own graphs, whose files
//! live in a `kerf-still-probe-<pid>-<kind>` directory, and counts every call) is a child process of its own, because the probes'
//! answers are kept for the life of the process. The test binary runs itself again with `--exact checker_t6_gates_child` and the
//! scenario in the environment. Needs `ffmpeg` and an adapter; `#[ignore]`d.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_t6_gates -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kerf_core::{
    Clip, CompositeColorPolicy, Delivery, ExportOptions, Fit, Keyframe, Project, RenderPlan, StillKinds, StreamKind, Timeline,
    Track,
};
use kerf_gpu::{Compositor, Gpu, GpuError, GpuOptions};

const SCENARIO: &str = "KERF_T6_SCENARIO";
const DIR: &str = "KERF_T6_DIR";
const MODE: &str = "KERF_T6_MODE";
const REAL_FFMPEG: &str = "KERF_T6_REAL_FFMPEG";

/// The `ffmpeg` wrapper. A call whose argv names the still probe's scratch directory (`kerf-still-probe-<pid>-rgb24` or
/// `-yuvj420p`) is logged (`ffmpeg.log`: the kind of every such call; `hit-<kind>.log`: one line for each run of a graph with a
/// `perspective` in it, the probe's export and still) and, when the mode in `$DIR/mode` (`<kind>:<what>`) names its kind, changed, cut short,
/// failed or stalled. Everything else is the real `ffmpeg`.
const FFMPEG_WRAPPER: &str = r#"#!/bin/bash
D="$KERF_T6_DIR"; REAL="$KERF_T6_REAL_FFMPEG"
args=("$@")
all="${args[*]}"
kind=none
case "$all" in
  *kerf-still-probe-*-rgb24*) kind=rgb ;;
  *kerf-still-probe-*-yuvj420p*) kind=jpeg ;;
esac
persp=0; case "$all" in *perspective*) persp=1;; esac
copy=0; case "$all" in *" -vf "*) copy=1;; esac
echo "$kind" >> "$D/ffmpeg.log"
if [ "$kind" != none ] && [ "$persp" = 1 ]; then echo x >> "$D/hit-$kind.log"; fi
if [ "$kind" != none ]; then
  mode="$(cat "$D/mode" 2>/dev/null)"
  target="${mode%%:*}"; what="${mode#*:}"
  if [ "$target" = "$kind" ] || [ "$target" = all ]; then
    edit() { for i in "${!args[@]}"; do args[$i]=$(printf '%s' "${args[$i]}" | sed "$1"); done; }
    case "$what" in
      cubic) [ "$persp" = 1 ] && edit 's/interpolation=linear/interpolation=cubic/g' ;;
      copy) [ "$copy" = 1 ] && edit 's/flags=lanczos/flags=bicubic/g' ;;
      fail) [ "$persp" = 1 ] && exit 1 ;;
      short) if [ "$persp" = 1 ]; then "$REAL" "${args[@]}" | head -c 17; exit 0; fi ;;
      stall) if [ "$persp" = 1 ]; then sleep 36; exit 1; fi ;;
    esac
  fi
fi
exec "$REAL" "${args[@]}"
"#;

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write a wrapper");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
}

fn lines(log: &Path) -> usize {
    std::fs::read_to_string(log).map_or(0, |t| t.lines().count())
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

/// Run this test binary again as the child of `scenario` and fail unless it passes.
fn child(scenario: &str, mode: &str, limit: Duration) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("checker-t6-gates")
        .join(format!("{scenario}-{}", mode.replace(':', "-")));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let wrapper = dir.join("ffmpeg-wrapper.sh");
    executable(&wrapper, FFMPEG_WRAPPER);
    std::fs::write(dir.join("mode"), mode).expect("write the mode");
    let (out, err) = (dir.join("child.out"), dir.join("child.err"));
    let mut process = Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--ignored", "--exact", "--nocapture", "checker_t6_gates_child"])
        .env(SCENARIO, scenario)
        .env(MODE, mode)
        .env(DIR, &dir)
        .env(REAL_FFMPEG, kerf_core::ffmpeg_path())
        .env("KERF_FFMPEG", &wrapper)
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
    eprintln!("== {scenario} / {mode} ({:.1?}) ==\n{say}", started.elapsed());
    let passed = status.is_some_and(|s| s.success());
    if passed {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(
        passed,
        "{scenario} / {mode}: {} (see {})",
        status.map_or("did not finish in time".to_string(), |s| s.to_string()),
        err.display()
    );
}

/// A 640x426 photograph of the kind, probed as an import does.
fn photo(dir: &Path, name: &str, flags: &[&str]) -> kerf_core::Asset {
    let path = dir.join(name);
    let mut args = vec![
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x426:rate=1:duration=1",
        "-frames:v",
        "1",
    ];
    args.extend_from_slice(flags);
    args.push(path.to_str().expect("a utf-8 path"));
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Project::probe_asset(&path).expect("probe the photograph")
}

fn key(time: f64, scale: f64, x: f64, y: f64) -> Keyframe {
    Keyframe {
        time,
        scale,
        pos_x: x,
        pos_y: y,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    }
}

/// A still plan of a photograph on the slideshow's move (in from 1.2x towards the top left).
fn moving(asset: &kerf_core::Asset, policy: CompositeColorPolicy) -> RenderPlan {
    let mut c = Clip::new(asset.id, 0.0, 2.0, 0.0);
    c.keyframes = vec![key(0.0, 1.2, 0.04, 0.03), key(2.0, 1.0, 0.0, 0.0)];
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![c],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Delivery::new(320, 180, Fit::Cover).with_fps(Some(30.0)).ok(),
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    RenderPlan::at(&tl, std::slice::from_ref(asset), &ExportOptions::default(), 1.0, policy).expect("a plan")
}

/// `Some(why)` for a frame the compositor refuses, `None` for one it drew. Any other error is a failure of the test.
fn refusal<T>(result: Result<T, GpuError>) -> Option<String> {
    match result {
        Ok(_) => None,
        Err(GpuError::Unsupported(why)) => Some(why),
        Err(other) => panic!("not a refusal: {other}"),
    }
}

/// A verdict as its name (`Match`, `Differs(..)`, `Unknown`): the type is the crate's own.
fn named(verdict: &impl std::fmt::Debug) -> String {
    let text = format!("{verdict:?}");
    text.split('(').next().unwrap_or_default().to_string()
}

fn drawn(compositor: &Compositor, plan: &RenderPlan) -> Option<String> {
    let size = plan.size(u32::MAX);
    refusal(compositor.render_plan(&plan.clone().for_render(size), size))
}

#[test]
#[ignore = "this binary's own child: runs the scenario named in the environment"]
fn checker_t6_gates_child() {
    let Ok(scenario) = std::env::var(SCENARIO) else {
        return;
    };
    let dir = PathBuf::from(env(DIR));
    let mode = env(MODE);
    match scenario.as_str() {
        "stills" => stills_child(&dir, &mode),
        other => panic!("no scenario {other}"),
    }
}

fn stills_child(dir: &Path, mode: &str) {
    let (hits_rgb, hits_jpeg, all) = (dir.join("hit-rgb.log"), dir.join("hit-jpeg.log"), dir.join("ffmpeg.log"));
    let hits = || lines(&hits_rgb) + lines(&hits_jpeg);
    let (target, what) = mode.split_once(':').unwrap_or(("none", mode));
    let policy = kerf_core::composite_color_policy();
    assert_ne!(
        policy,
        CompositeColorPolicy::Unknown,
        "the composite colour probe could not read this FFmpeg"
    );
    let jpeg = photo(dir, "p.jpg", &["-q:v", "3", "-pix_fmt", "yuvj420p"]);
    let png = photo(dir, "p.png", &["-pix_fmt", "rgb24"]);
    let (jpeg_plan, png_plan) = (moving(&jpeg, policy), moving(&png, policy));
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    let compositor = Compositor::new(gpu).expect("a compositor");

    // Cold: nothing measured, so a moving still is refused, at once, with no process spawned by the check of the plan.
    let calls = lines(&all);
    assert_eq!(compositor.caps().still_motion, StillKinds::NONE, "cold caps");
    for (name, plan) in [("jpeg", &jpeg_plan), ("png", &png_plan)] {
        let size = plan.size(u32::MAX);
        let why = plan.clone().for_render(size).reasons(&compositor.caps(), size);
        assert!(
            why.iter().any(|w| w.to_string().contains("a still whose picture moves")),
            "{name}: a cold moving still was not refused as a moving still: {why:?}"
        );
    }
    assert_eq!(lines(&all), calls, "the check of a plan spawned a process");

    let started = Instant::now();
    kerf_gpu::prepare(&png_plan);
    kerf_gpu::prepare(&jpeg_plan);
    let took = started.elapsed();
    let (vj, vr) = (named(&kerf_gpu::still_probe::jpeg()), named(&kerf_gpu::still_probe::rgb()));
    eprintln!(
        "mode {mode}: jpeg {:?}, rgb {:?} after {took:.1?}, {} probe graph runs",
        kerf_gpu::still_probe::jpeg(),
        kerf_gpu::still_probe::rgb(),
        hits()
    );
    let kinds = kerf_gpu::still_probe::measured();
    let jpeg_would_match = policy == CompositeColorPolicy::FixedBt601;
    // What each kind's verdict has to be: the perturbed one is not a match; the other is what an FFmpeg left alone gives.
    let perturbed = |kind: &str| target == kind || target == "all";
    if what == "pass" || target == "none" {
        assert_eq!(
            vr, "Match",
            "the control: an unperturbed FFmpeg draws a PNG's chain as the model does"
        );
        assert!(kinds.contains(StillKinds::RGB24));
    }
    if perturbed("rgb") {
        assert_ne!(vr, "Match", "{mode}: a perturbed chain was found to match");
        assert!(
            !kinds.contains(StillKinds::RGB24),
            "{mode}: a kind that did not match is in the caps"
        );
        assert!(
            !compositor.caps().still_motion.contains(StillKinds::RGB24),
            "{mode}: the compositor's caps hold the kind"
        );
        if matches!(what, "cubic" | "copy") {
            assert_eq!(vr, "Differs", "{mode}: a perturbed chain was not found to differ");
            eprintln!("  first case: {:?}", kerf_gpu::still_probe::rgb());
        } else {
            assert_eq!(vr, "Unknown", "{mode}: a probe that could not tell must say so");
        }
    } else {
        assert_eq!(vr, "Match", "{mode}: the other kind is not touched");
    }
    if perturbed("jpeg") {
        assert_ne!(vj, "Match", "{mode}: a perturbed JPEG chain was found to match");
        assert!(!kinds.contains(StillKinds::YUVJ420P));
    } else if jpeg_would_match {
        assert_eq!(vj, "Match", "{mode}: the JPEG chain is not touched");
    }
    // What is drawn follows the caps: a PNG's moving still only where its chain matched, a JPEG's only where it matched and the
    // composite's range is fixed.
    let png_drawn = drawn(&compositor, &png_plan);
    let jpeg_drawn = drawn(&compositor, &jpeg_plan);
    assert_eq!(
        png_drawn.is_none(),
        !perturbed("rgb"),
        "{mode}: the PNG's frame: {png_drawn:?}"
    );
    if perturbed("jpeg") || !jpeg_would_match {
        assert!(jpeg_drawn.is_some(), "{mode}: a JPEG's frame was drawn");
    } else {
        assert_eq!(jpeg_drawn, None, "{mode}: the JPEG's frame was refused");
    }
    if let Some(why) = png_drawn.as_deref().filter(|_| perturbed("rgb")) {
        assert!(why.contains("a still whose picture moves"), "{why}");
    }

    match (target, what) {
        (_, "pass") | ("none", _) => {
            // Measured once: a match is kept without another run.
            let runs = hits();
            kerf_gpu::prepare(&png_plan);
            kerf_gpu::prepare(&jpeg_plan);
            assert_eq!(hits(), runs, "a match was measured twice");
        }
        (_, "cubic" | "copy") => {
            // A difference is for good: not asked again, not softened by time.
            let runs = hits();
            kerf_gpu::prepare(&png_plan);
            kerf_gpu::prepare(&jpeg_plan);
            assert_eq!(named(&kerf_gpu::still_probe::rgb()), vr);
            assert_eq!(hits(), runs, "a difference was measured again");
        }
        (_, "fail" | "short" | "stall") => {
            // Unknown is not believed and not asked again inside the backoff.
            let (runs, again) = (hits(), Instant::now());
            kerf_gpu::prepare(&png_plan);
            kerf_gpu::prepare(&jpeg_plan);
            assert_eq!(named(&kerf_gpu::still_probe::rgb()), "Unknown");
            assert_eq!(hits(), runs, "an unknown was asked again inside the backoff");
            assert!(again.elapsed() < Duration::from_secs(4), "{:?}", again.elapsed());
            assert!(drawn(&compositor, &png_plan).is_some(), "drawn inside the backoff");
            if what == "stall" {
                // Killed at its 20 s limit and not waited for: the wrapper's `sleep 36` holds the pipes for 16 s more.
                assert!(
                    took >= Duration::from_secs(19) && took < Duration::from_secs(33),
                    "the stalled probe took {took:?}"
                );
                assert_eq!(lines(&hits_rgb), 1, "one run, and the first one decides");
                // After the backoff, with the stall gone, the chain is measured and drawn.
                std::fs::write(dir.join("mode"), "none:pass").expect("clear the stall");
                std::thread::sleep(Duration::from_secs(31).saturating_sub(started.elapsed().saturating_sub(took)));
                kerf_gpu::prepare(&png_plan);
                assert_eq!(named(&kerf_gpu::still_probe::rgb()), "Match", "recovered after the backoff");
                assert_eq!(drawn(&compositor, &png_plan), None);
            }
        }
        other => panic!("no mode {other:?}"),
    }
}

#[test]
#[ignore = "needs ffmpeg and an adapter (lavapipe is enough)"]
fn checker_t6_gates_an_unperturbed_ffmpeg_draws_the_kinds_it_matches() {
    child("stills", "none:pass", Duration::from_secs(180));
}

#[test]
#[ignore = "needs ffmpeg and an adapter (lavapipe is enough)"]
fn checker_t6_gates_a_chain_that_differs_is_refused_for_its_kind_alone() {
    child("stills", "rgb:cubic", Duration::from_secs(180));
    child("stills", "jpeg:cubic", Duration::from_secs(180));
    child("stills", "rgb:copy", Duration::from_secs(180));
    child("stills", "jpeg:copy", Duration::from_secs(180));
    child("stills", "all:cubic", Duration::from_secs(180));
}

#[test]
#[ignore = "needs ffmpeg and an adapter (lavapipe is enough)"]
fn checker_t6_gates_a_probe_that_fails_or_prints_too_little_leaves_the_kind_out() {
    child("stills", "rgb:fail", Duration::from_secs(180));
    child("stills", "rgb:short", Duration::from_secs(180));
}

#[test]
#[ignore = "needs ffmpeg and an adapter (lavapipe is enough)"]
fn checker_t6_gates_a_probe_that_stalls_is_killed_refused_and_asked_again_after_the_backoff() {
    child("stills", "rgb:stall", Duration::from_secs(240));
}
