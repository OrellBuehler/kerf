//! T6 fix round 2, the gates attacked from outside, one child process a scenario (the probes keep their answers for the life of the process):
//!
//! * `chains`: an `ffmpeg` wrapper changes only the standalone copy of **one chain's** toy (the engine's fit behind a crop, the staged fit,
//!   either with the scaler named, the move inside the frame), so that chain's probe finds a difference and no other does. The plans of every
//!   chain are then prepared and drawn: the perturbed chain's plan is refused by the caps and by the render, no copy is made for it, and the
//!   chains beside it are drawn; only the chains a plan takes are measured.
//! * `probe_stall`: an `ffprobe` wrapper that never answers for the probe's own pictures; the still probe has to give up at its deadline
//!   (`Unknown`), kill the child and not wait the engine's five minutes.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_t6_round2_gates -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kerf_core::{
    Clip, CompositeColorPolicy, Delivery, ExportOptions, Fit, FitShape, Keyframe, PlanRequest, Planner, Project, RenderPlan,
    StillChain, StillKinds, StreamKind, Timeline, Track,
};
use kerf_gpu::{Compositor, Gpu, GpuError, GpuOptions};

const SCENARIO: &str = "KERF_R2_SCENARIO";
const DIR: &str = "KERF_R2_DIR";
const MODE: &str = "KERF_R2_MODE";
const REAL_FFMPEG: &str = "KERF_R2_REAL_FFMPEG";
const REAL_FFPROBE: &str = "KERF_R2_REAL_FFPROBE";

/// The `ffmpeg` wrapper: a standalone copy of the still probe's (an argv naming `kerf-still-probe-<pid>-<kind>` with `-vf`) is classified by the
/// text of its filters into a chain, and the chain named in `$DIR/mode` (`<kind>:<chain>`) has its scaler changed, so that the copy differs from the
/// export's picture. Everything else is the real `ffmpeg`.
const FFMPEG_WRAPPER: &str = r#"#!/bin/bash
D="$KERF_R2_DIR"; REAL="$KERF_R2_REAL_FFMPEG"
args=("$@")
all="${args[*]}"
kind=none
case "$all" in
  *kerf-still-probe-*-rgb24*) kind=rgb ;;
  *kerf-still-probe-*-yuvj420p*) kind=jpeg ;;
esac
echo "$kind" >> "$D/ffmpeg.log"
echo "$all" >> "$D/argv.log"
if [ "$kind" != none ]; then
  vf=""
  for i in "${!args[@]}"; do
    if [ "${args[$i]}" = "-vf" ]; then vf="${args[$((i+1))]}"; vi=$((i+1)); fi
  done
  if [ -n "$vf" ]; then
    shape=inside
    case "$vf" in
      *"crop=w=iw*"*) shape=engine_crop ;;
      *"exact=1"*) shape=staged ;;
      *force_original_aspect_ratio*) shape=engine ;;
    esac
    bic=""; case "$vf" in *"flags=bicubic"*) bic=bicubic_ ;; esac
    chain="$bic$shape"
    echo "$kind $chain" >> "$D/copies.log"
    mode="$(cat "$D/mode" 2>/dev/null)"
    target="${mode%%:*}"; what="${mode#*:}"
    if { [ "$target" = "$kind" ] || [ "$target" = all ]; } && [ "$what" = "$chain" ]; then
      if [ -n "$bic" ]; then
        args[$vi]=$(printf '%s' "$vf" | sed 's/flags=bicubic/flags=neighbor/g')
      elif [ "$shape" = inside ]; then
        args[$vi]=$(printf '%s' "$vf" | sed 's/flags=lanczos/flags=neighbor/g')
      else
        args[$vi]=$(printf '%s' "$vf" | sed -E 's/(scale=[0-9]+:[0-9]+(:force_original_aspect_ratio=increase)?)/\1:flags=neighbor/g')
      fi
    fi
  fi
fi
exec "$REAL" "${args[@]}"
"#;

/// The `ffprobe` wrapper: the probe's own pictures never get an answer (the process is the sleeper itself, so a kill ends it); every other
/// read is the real `ffprobe`.
const FFPROBE_WRAPPER: &str = r#"#!/bin/bash
D="$KERF_R2_DIR"; REAL="$KERF_R2_REAL_FFPROBE"
case "$*" in
  *kerf-still-probe-*)
    echo $$ >> "$D/ffprobe.pids"
    exec sleep 400 ;;
esac
exec "$REAL" "$@"
"#;

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write a wrapper");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
}

fn lines(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .map(|t| t.lines().map(String::from).collect())
        .unwrap_or_default()
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

fn child(scenario: &str, mode: &str, limit: Duration) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("checker-t6-round2-gates")
        .join(format!("{scenario}-{}", mode.replace(':', "-")));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let (ffmpeg, ffprobe) = (dir.join("ffmpeg-wrapper.sh"), dir.join("ffprobe-wrapper.sh"));
    executable(&ffmpeg, FFMPEG_WRAPPER);
    executable(&ffprobe, FFPROBE_WRAPPER);
    std::fs::write(dir.join("mode"), mode).expect("write the mode");
    let (out, err) = (dir.join("child.out"), dir.join("child.err"));
    let real_ffprobe = std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string());
    let mut cmd = Command::new(std::env::current_exe().expect("this test binary"));
    cmd.args(["--ignored", "--exact", "--nocapture", "checker_t6_round2_gates_child"])
        .env(SCENARIO, scenario)
        .env(MODE, mode)
        .env(DIR, &dir)
        .env(REAL_FFMPEG, kerf_core::ffmpeg_path())
        .env(REAL_FFPROBE, &real_ffprobe)
        .env("KERF_FFMPEG", &ffmpeg);
    if scenario == "probe_stall" {
        cmd.env("KERF_FFPROBE", &ffprobe);
    }
    let mut process = cmd
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
        easing: Default::default(),
        time,
        scale,
        pos_x: x,
        pos_y: y,
        rotation: 0.0,
        opacity: 1.0,
    }
}

/// The export frame (the delivery's frame) of a still on the chain `chain`: a move inside the frame, panned off its edge (the engine's fit),
/// behind a static crop, or on a Contain frame (the geometry's stages); with the delivery's scaler named or not.
fn plan_of(asset: &kerf_core::Asset, shape: Option<FitShape>, bicubic: bool, policy: CompositeColorPolicy) -> RenderPlan {
    plan_with(asset, shape, bicubic, policy, 0.0)
}

/// [`plan_of`] with a static brightness on the clip (the plan refuses the layer for it whatever the probe says).
fn plan_with(
    asset: &kerf_core::Asset,
    shape: Option<FitShape>,
    bicubic: bool,
    policy: CompositeColorPolicy,
    brightness: f64,
) -> RenderPlan {
    let mut c = Clip::new(asset.id, 0.0, 2.0, 0.0);
    c.color.brightness = brightness;
    c.keyframes = match shape {
        None => vec![key(0.0, 1.2, 0.04, 0.03), key(2.0, 1.0, 0.0, 0.0)],
        Some(_) => vec![key(0.0, 1.0, -0.03, 0.0), key(2.0, 1.18, 0.03, -0.02)],
    };
    if shape == Some(FitShape::EngineCrop) {
        c.transform.crop_left = 0.1;
        c.transform.crop_top = 0.05;
    }
    let fit = if shape == Some(FitShape::Staged) {
        Fit::Contain
    } else {
        Fit::Cover
    };
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![c],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Delivery::new(320, 180, fit).with_fps(Some(30.0)).ok(),
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let opts = ExportOptions {
        fps: Some(30.0),
        scaler: bicubic.then(|| "bicubic".to_string()),
        ..ExportOptions::default()
    };
    Planner::new(&tl, std::slice::from_ref(asset), &opts, PlanRequest::motion(policy))
        .expect("a planner")
        .at_frame(10)
        .expect("a plan")
}

fn refusal<T>(result: Result<T, GpuError>) -> Option<String> {
    match result {
        Ok(_) => None,
        Err(GpuError::Unsupported(why)) => Some(why),
        Err(other) => panic!("not a refusal: {other}"),
    }
}

fn drawn(compositor: &Compositor, plan: &RenderPlan) -> Option<String> {
    let size = (plan.canvas.width, plan.canvas.height);
    refusal(compositor.render_plan(&plan.clone().for_render(size), size))
}

fn refused_as_moving(compositor: &Compositor, plan: &RenderPlan) -> bool {
    let size = (plan.canvas.width, plan.canvas.height);
    plan.reasons(&compositor.caps(), size)
        .iter()
        .any(|u| matches!(u, kerf_core::Unsupported::StillMotion(..)))
}

#[test]
#[ignore = "this binary's own child: runs the scenario named in the environment"]
fn checker_t6_round2_gates_child() {
    let Ok(scenario) = std::env::var(SCENARIO) else {
        return;
    };
    let dir = PathBuf::from(env(DIR));
    let mode = env(MODE);
    match scenario.as_str() {
        "chains" => chains_child(&dir, &mode),
        "probe_stall" => probe_stall_child(&dir),
        "prefetch" => prefetch_child(&dir),
        other => panic!("no scenario {other}"),
    }
}

const NAMES: [(&str, Option<FitShape>, bool); 8] = [
    ("inside", None, false),
    ("engine", Some(FitShape::Engine), false),
    ("engine_crop", Some(FitShape::EngineCrop), false),
    ("staged", Some(FitShape::Staged), false),
    ("bicubic_inside", None, true),
    ("bicubic_engine", Some(FitShape::Engine), true),
    ("bicubic_engine_crop", Some(FitShape::EngineCrop), true),
    ("bicubic_staged", Some(FitShape::Staged), true),
];

fn chains_child(dir: &Path, mode: &str) {
    let (target, what) = mode.split_once(':').unwrap_or(("none", mode));
    let policy = kerf_core::composite_color_policy();
    assert_ne!(
        policy,
        CompositeColorPolicy::Unknown,
        "the composite colour probe could not read this FFmpeg"
    );
    let png = photo(dir, "p.png", &["-pix_fmt", "rgb24"]);
    let plans: Vec<(&str, RenderPlan)> = NAMES
        .iter()
        .map(|(name, shape, bicubic)| (*name, plan_of(&png, *shape, *bicubic, policy)))
        .collect();
    // The plans take the chains they stand for.
    for ((name, shape, bicubic), (_, plan)) in NAMES.iter().zip(&plans) {
        let said = plan.still_chain(&plan.layers[0], (320, 180));
        let want = StillChain {
            fit: *shape,
            bicubic: *bicubic,
        };
        assert_eq!(said, Some(want), "{name}");
    }
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    let compositor = Compositor::new(gpu).expect("a compositor");
    // A layer the plan refuses whatever the probe says (colour on a moving still) is neither measured for nor made a copy of, cold.
    {
        let held = kerf_gpu::copies::held().0;
        for shape in [None, Some(FitShape::Engine)] {
            let plan = plan_with(&png, shape, false, policy, 0.1);
            kerf_gpu::prepare(&plan);
        }
        assert_eq!(
            kerf_gpu::still_probe::measured(),
            StillKinds::NONE,
            "a probe ran for a layer the plan refuses whatever it says"
        );
        assert_eq!(
            kerf_gpu::copies::held().0,
            held,
            "a copy was made for a layer the plan refuses whatever the probe says"
        );
        assert!(
            lines(&dir.join("ffmpeg.log")).iter().all(|k| k == "none"),
            "a probe child ran"
        );
    }
    // Cold: nothing is measured, so every one of them is refused.
    assert_eq!(compositor.caps().still_motion, StillKinds::NONE, "cold caps");
    for (name, plan) in &plans {
        assert!(refused_as_moving(&compositor, plan), "{name}: refused cold");
    }
    // The perturbed chain's slot differs: for a base chain, both base chains (they are measured together).
    let rgb = StillKinds::RGB24;
    let perturbed: Vec<&str> = match (target, what) {
        ("none", _) => vec![],
        (_, "inside" | "engine") => vec!["inside", "engine"],
        (_, other) => vec![NAMES.iter().find(|(n, ..)| *n == other).map(|(n, ..)| *n).expect("a chain")],
    };
    let held = || kerf_gpu::copies::held().0;
    let mut expected_measured = StillKinds::NONE;
    // A kind whose base chains differ is not in the caps at all, whatever else matched: every plan of the kind is refused.
    let base_differs = perturbed.contains(&"inside");
    for (name, plan) in &plans {
        let is_base = matches!(*name, "inside" | "engine");
        let before = held();
        let log_before = lines(&dir.join("copies.log")).len();
        kerf_gpu::prepare(plan);
        let measured = kerf_gpu::still_probe::measured();
        let after_prepare = held();
        let chain = plan.still_chain(&plan.layers[0], (320, 180)).expect("a chain");
        let must_differ = perturbed.contains(name);
        // What the caps hold now: the base chains with the first plan (any), then each chain a plan takes, but never the perturbed ones.
        if is_base {
            if !base_differs {
                expected_measured = expected_measured.union(rgb);
            }
        } else if !must_differ {
            expected_measured = expected_measured.union(rgb.chain(chain));
        }
        assert_eq!(measured, expected_measured, "{name}: the caps after preparing its plan");
        assert_eq!(
            measured.contains_chain(rgb, chain),
            !must_differ,
            "{name}: its chain in the caps"
        );
        if must_differ || base_differs {
            assert!(
                refused_as_moving(&compositor, plan),
                "{name}: the plan of a chain that is not drawn is not refused"
            );
            let why = drawn(&compositor, plan);
            assert!(why.is_some(), "{name}: drawn although its chain (or its kind's base) differs");
            if must_differ {
                assert_eq!(
                    after_prepare, before,
                    "{name}: a copy was made for a layer whose chain differs"
                );
            } else if after_prepare != before {
                eprintln!("R2 WASTE {name}: prepare made a copy for a layer whose chain matched but whose kind is refused (its base chains differ)");
            }
        } else {
            assert_eq!(
                after_prepare,
                before + 1,
                "{name}: the copy of a drawn layer was not made ahead"
            );
            assert!(
                !refused_as_moving(&compositor, plan),
                "{name}: refused though its chain matched"
            );
            assert_eq!(drawn(&compositor, plan), None, "{name}: not drawn though its chain matched");
        }
        let copies = lines(&dir.join("copies.log"));
        eprintln!(
            "{name}: caps {measured:?}, {} probe copies since the last plan",
            copies.len() - log_before
        );
    }
    // Every chain of the perturbed slot was found different, none of the others.
    for (name, _) in &plans {
        let chain = NAMES
            .iter()
            .find(|(n, ..)| n == name)
            .map(|(_, s, b)| StillChain { fit: *s, bicubic: *b })
            .unwrap();
        let verdict = format!("{:?}", kerf_gpu::still_probe::verdict(rgb, chain));
        let differs = perturbed.contains(name);
        assert_eq!(verdict.starts_with("Differs"), differs, "{name}: {verdict}");
        assert!(differs || verdict == "Match", "{name}: {verdict}");
    }
    // Preparing again measures nothing again.
    let runs = lines(&dir.join("ffmpeg.log")).len();
    for (_, plan) in &plans {
        kerf_gpu::prepare(plan);
    }
    assert_eq!(lines(&dir.join("ffmpeg.log")).len(), runs, "a verdict was measured twice");
}

/// A moving still's plan that is not worked out for a render size has no copy to make yet, and its prefetch does not decode the original beside the
/// copy it will be drawn from (nothing is run for it: the copy is made when the plan is worked out).
fn prefetch_child(dir: &Path) {
    use kerf_gpu::{FrameSource, FrameSourceConfig, Hint};
    let policy = kerf_core::composite_color_policy();
    assert_ne!(policy, CompositeColorPolicy::Unknown);
    let png = photo(dir, "big.png", &["-pix_fmt", "rgb24"]);
    let plan = plan_of(&png, None, false, policy);
    assert!(plan.layers.iter().all(|l| l.is_image));
    // A Still plan of the same cut, as a scrub asks for it (not worked out for a size).
    let mut c = Clip::new(png.id, 0.0, 2.0, 0.0);
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
    let still = RenderPlan::at(&tl, std::slice::from_ref(&png), &ExportOptions::default(), 1.0, policy).expect("a plan");
    let layer = &still.layers[0];
    assert!(
        kerf_gpu::source::copy_of(layer).is_err(),
        "the still plan's copy is worked out already"
    );
    let path = png.path;
    let mentions = || {
        lines(&dir.join("argv.log"))
            .iter()
            .filter(|l| l.contains(&path) && !l.contains("kerf-still-probe-") && !l.contains("-f lavfi"))
            .count()
    };
    let before = mentions();
    let source = FrameSource::new(FrameSourceConfig::default());
    let out = source.frames(&still.layers, Hint::Prefetch).expect("a prefetch");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(out.iter().all(Option::is_none));
    assert_eq!(
        mentions(),
        before,
        "the prefetch ran ffmpeg on the original of a moving still"
    );
    let stats = source.stats();
    eprintln!(
        "prefetch of a moving still not worked out for a size: {stats:?}, runs enabled {}",
        source.runs_enabled()
    );
    assert!(source.runs_enabled(), "the run path is off: the control proves nothing");
    assert_eq!(stats.cache.entries, 0, "{stats:?}");
    // (A one-shot decode of the original would be counted whether or not it got to spawn: `decode_layer` refuses a copy that is not worked
    // out before it runs ffmpeg, so the stat is what shows that the prefetch asked.)
    assert_eq!(
        stats.oneshots, 0,
        "the prefetch asked for a one-shot decode of the original: {stats:?}"
    );
    assert_eq!(stats.spawned, 0, "{stats:?}");
}

fn probe_stall_child(dir: &Path) {
    let policy = kerf_core::composite_color_policy();
    assert_ne!(
        policy,
        CompositeColorPolicy::Unknown,
        "the composite colour probe could not read this FFmpeg"
    );
    let started = Instant::now();
    let verdict = kerf_gpu::still_probe::verdict(StillKinds::RGB24, StillChain::inside(false));
    let took = started.elapsed();
    eprintln!("the probe of a stalled ffprobe: {verdict:?} after {took:.1?}");
    assert_eq!(
        format!("{verdict:?}"),
        "Unknown",
        "a probe that could not read its picture must say so"
    );
    assert!(took >= Duration::from_secs(40), "gave up before its deadline: {took:?}");
    assert!(
        took < Duration::from_secs(70),
        "the probe sat out the engine's own ffprobe timeout: {took:?}"
    );
    let pids = lines(&dir.join("ffprobe.pids"));
    assert!(!pids.is_empty(), "the wrapper was never asked");
    std::thread::sleep(Duration::from_millis(500));
    for pid in pids {
        let alive = Path::new(&format!("/proc/{}", pid.trim())).exists()
            && std::fs::read_to_string(format!("/proc/{}/stat", pid.trim()))
                .map(|s| !s.contains(") Z"))
                .unwrap_or(false);
        assert!(!alive, "the ffprobe child {pid} was left running past the deadline");
    }
    assert_eq!(kerf_gpu::still_probe::measured(), StillKinds::NONE);
}

#[test]
#[ignore = "needs ffmpeg and an adapter (lavapipe is enough)"]
fn r2_gates_a_control_unperturbed_ffmpeg_measures_each_chain_a_plan_takes_and_no_other() {
    child("chains", "none:pass", Duration::from_secs(400));
}

#[test]
#[ignore = "needs ffmpeg and an adapter (lavapipe is enough)"]
fn r2_gates_one_chain_that_differs_is_refused_alone_and_no_copy_is_made_for_it() {
    for chain in [
        "engine_crop",
        "staged",
        "bicubic_inside",
        "bicubic_engine",
        "bicubic_engine_crop",
        "bicubic_staged",
        "inside",
    ] {
        child("chains", &format!("rgb:{chain}"), Duration::from_secs(400));
    }
}

#[test]
#[ignore = "needs ffmpeg and an adapter (lavapipe is enough)"]
fn r2_gates_prefetch_does_not_decode_the_original_of_a_moving_still_whose_copy_is_not_worked_out() {
    child("prefetch", "none:pass", Duration::from_secs(100));
}

#[test]
#[ignore = "needs ffmpeg and an adapter (lavapipe is enough)"]
fn r2_gates_the_probe_kills_an_ffprobe_that_does_not_answer_at_its_deadline() {
    child("probe_stall", "none:pass", Duration::from_secs(150));
}

/// What the docs say of the verdicts (gpu.md): 6.1.1 every chain of both kinds `Match`; 9.0.2 `rgb24` every chain `Match`, `yuvj420p` `Differs` on
/// the placement route and its move inside the frame alone `Match`es with the bicubic scaler named. Printed for every chain and held to the doc.
#[test]
#[ignore = "needs ffmpeg"]
fn r2_gates_the_probe_verdicts_are_what_the_docs_say() {
    let policy = kerf_core::composite_color_policy();
    assert_ne!(
        policy,
        CompositeColorPolicy::Unknown,
        "the composite colour probe could not read this FFmpeg"
    );
    let fixed = policy == CompositeColorPolicy::FixedBt601;
    let mut wrong = Vec::new();
    for (kname, kind) in [("rgb24", StillKinds::RGB24), ("yuvj420p", StillKinds::YUVJ420P)] {
        for chain in StillChain::ALL {
            let v = kerf_gpu::still_probe::verdict(kind, chain);
            let name = format!("{kname} {chain:?}");
            eprintln!("R2 VERDICT {} {name}: {v:?}", kerf_core::ffmpeg_path());
            let doc_says_match = kname == "rgb24" || fixed || (chain == StillChain::inside(true));
            let matches = format!("{v:?}") == "Match";
            if doc_says_match != matches {
                wrong.push(format!("{name}: {v:?} (doc says match: {doc_says_match})"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
