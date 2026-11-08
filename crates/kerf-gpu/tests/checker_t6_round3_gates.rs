//! T6 fix round 3, the gates attacked from outside, one child process a scenario (the probes keep their answers for the life of the process):
//!
//! * `chains`: round 2's scenario with the waste held to nothing: an `ffmpeg` wrapper changes only the standalone copy of **one chain's** toy, so that
//!   chain's probe finds a difference and no other does; where the perturbed chain is a **base** chain (the move inside the frame, the engine's
//!   fit), the kind is not in the caps at all, and `prepare` must not make the copy of a layer whose own chain matched either.
//! * `cropped`: the fit pictures whose text has a `crop` besides the guard (a Contain frame behind the clip's static crop, the Cover overhang after a
//!   first scale) are no chain: `prepare` measures nothing and makes nothing for them, the plan refuses them with every chain pinned and with
//!   the measured ones, and the chains beside them (the staged fit, the engine's behind a crop) are measured, made and drawn.
//! * `skip`: a refusal of the frame as a whole that no measurement can lift (a scaler the compositor does not implement, an animated title, a GIF, a
//!   delivery format, a pick rate, a cut FFmpeg 9 flags full range) makes `prepare` measure nothing and copy nothing for a moving still in it, and a
//!   refusal a measurement can lift (a title the text model is measured for) does not.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_t6_round3_gates -- --ignored --nocapture
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

const SCENARIO: &str = "KERF_R3_SCENARIO";
const DIR: &str = "KERF_R3_DIR";
const MODE: &str = "KERF_R3_MODE";
const REAL_FFMPEG: &str = "KERF_R3_REAL_FFMPEG";
const REAL_FFPROBE: &str = "KERF_R3_REAL_FFPROBE";

/// The `ffmpeg` wrapper: a standalone copy of the still probe's (an argv naming `kerf-still-probe-<pid>-<kind>` with `-vf`) is classified by the
/// text of its filters into a chain, and the chain named in `$DIR/mode` (`<kind>:<chain>`) has its scaler changed, so that the copy differs from the
/// export's picture. Everything else is the real `ffmpeg`.
const FFMPEG_WRAPPER: &str = r#"#!/bin/bash
D="$KERF_R3_DIR"; REAL="$KERF_R3_REAL_FFMPEG"
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
        .join("checker-t6-round3-gates")
        .join(format!("{scenario}-{}", mode.replace(':', "-")));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let ffmpeg = dir.join("ffmpeg-wrapper.sh");
    executable(&ffmpeg, FFMPEG_WRAPPER);
    std::fs::write(dir.join("mode"), mode).expect("write the mode");
    let (out, err) = (dir.join("child.out"), dir.join("child.err"));
    let real_ffprobe = std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string());
    let mut cmd = Command::new(std::env::current_exe().expect("this test binary"));
    cmd.args(["--ignored", "--exact", "--nocapture", "checker_t6_round3_gates_child"])
        .env(SCENARIO, scenario)
        .env(MODE, mode)
        .env(DIR, &dir)
        .env(REAL_FFMPEG, kerf_core::ffmpeg_path())
        .env(REAL_FFPROBE, &real_ffprobe)
        .env("KERF_FFMPEG", &ffmpeg);
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
fn checker_t6_round3_gates_child() {
    let Ok(scenario) = std::env::var(SCENARIO) else {
        return;
    };
    let dir = PathBuf::from(env(DIR));
    let mode = env(MODE);
    match scenario.as_str() {
        "chains" => chains_child(&dir, &mode),
        "cropped" => cropped_child(&dir),
        "skip" => skip_child(&dir),
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
            } else {
                assert_eq!(
                    after_prepare, before,
                    "{name}: prepare made a copy for a layer whose chain matched but whose kind is refused (its base chains differ)"
                );
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

// ---- the plans of this round ------------------------------------------------------------------------------------------------

/// A still on a move at the delivery's frame (320x180, 30 fps unless `fps` says), frame 10 of the export: `edit` shapes the clip, `fit` is the
/// delivery's, `crop` the clip's static crop.
fn shaped(
    asset: &kerf_core::Asset,
    fit: Fit,
    keys: Vec<Keyframe>,
    crop: Option<(f64, f64, f64, f64)>,
    scaler: Option<&str>,
    policy: CompositeColorPolicy,
) -> RenderPlan {
    let mut c = Clip::new(asset.id, 0.0, 2.0, 0.0);
    c.keyframes = keys;
    if let Some((l, t, r, b)) = crop {
        c.transform.crop_left = l;
        c.transform.crop_top = t;
        c.transform.crop_right = r;
        c.transform.crop_bottom = b;
    }
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
        scaler: scaler.map(String::from),
        ..ExportOptions::default()
    };
    Planner::new(&tl, std::slice::from_ref(asset), &opts, PlanRequest::motion(policy))
        .expect("a planner")
        .at_frame(10)
        .expect("a plan")
}

fn off_keys() -> Vec<Keyframe> {
    vec![key(0.0, 1.0, -0.03, 0.0), key(2.0, 1.18, 0.03, -0.02)]
}

fn held_keys() -> Vec<Keyframe> {
    vec![key(0.0, 0.8, -0.04, 0.0), key(2.0, 0.8, 0.04, -0.02)]
}

fn inside_keys() -> Vec<Keyframe> {
    vec![key(0.0, 1.2, 0.04, 0.03), key(2.0, 1.0, 0.0, 0.0)]
}

fn compositors() -> (Compositor, Compositor) {
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    let measured = Compositor::new(gpu.clone()).expect("a compositor");
    let pinned = Compositor::new(gpu).expect("a compositor").with_still_motion(StillKinds::ALL);
    (measured, pinned)
}

fn no_probe_ran(dir: &Path) -> bool {
    lines(&dir.join("ffmpeg.log")).iter().all(|k| k == "none")
}

/// A fit picture whose text has a `crop` besides the guard is no chain: nothing is measured or made for it, and no caps set draws it.
fn cropped_child(dir: &Path) {
    let policy = kerf_core::composite_color_policy();
    assert_ne!(
        policy,
        CompositeColorPolicy::Unknown,
        "the composite colour probe could not read this FFmpeg"
    );
    let png = photo(dir, "p.png", &["-pix_fmt", "rgb24"]);
    let (measured, pinned) = compositors();
    let rgb = StillKinds::RGB24;
    let cropped = StillChain::edge(FitShape::Cropped, false);
    let crop = Some((0.1, 0.05, 0.0, 0.0));

    // The plans, and what they say they are.
    let mut refused_plans: Vec<(&str, RenderPlan)> = Vec::new();
    for scaler in [None, Some("bicubic")] {
        let tag = if scaler.is_some() { "bicubic " } else { "" };
        let a = shaped(&png, Fit::Contain, off_keys(), crop, scaler, policy);
        let b = shaped(&png, Fit::Cover, held_keys(), None, scaler, policy);
        let c = shaped(&png, Fit::Contain, off_keys(), Some((0.3, 0.0, 0.0, 0.3)), scaler, policy);
        let d = shaped(&png, Fit::Cover, held_keys(), crop, scaler, policy);
        for (name, plan) in [
            ("contain behind a crop", a),
            ("cover overhang", b),
            ("contain behind a big crop", c),
            ("held behind a crop", d),
        ] {
            let want = StillChain::edge(FitShape::Cropped, scaler.is_some());
            assert_eq!(plan.still_chain(&plan.layers[0], (320, 180)), Some(want), "{tag}{name}");
            let text = plan.fit_copy(&plan.layers[0], (320, 180)).expect("a fit").filters;
            assert!(text.matches("crop=").count() >= 2, "{tag}{name}: {text}");
            refused_plans.push((Box::leak(format!("{tag}{name}").into_boxed_str()), plan));
        }
    }
    assert!(!cropped.measurable() && rgb.chain(cropped) == StillKinds::NONE);

    // Cold: preparing them measures nothing and makes nothing.
    let held0 = kerf_gpu::copies::held().0;
    for (name, plan) in &refused_plans {
        kerf_gpu::prepare(plan);
        assert_eq!(
            kerf_gpu::still_probe::measured(),
            StillKinds::NONE,
            "{name}: a probe ran for a text no toy takes"
        );
        assert_eq!(
            kerf_gpu::copies::held().0,
            held0,
            "{name}: a copy was made for a text no toy takes"
        );
        assert!(no_probe_ran(dir), "{name}: a probe child ran");
    }
    // ... and none is drawn, cold, measured, or with every chain pinned.
    for (name, plan) in &refused_plans {
        for (which, compositor) in [("cold", &measured), ("pinned", &pinned)] {
            assert!(
                refused_as_moving(compositor, plan),
                "{name}: not refused as a moving still ({which})"
            );
            assert!(drawn(compositor, plan).is_some(), "{name}: drawn ({which})");
        }
    }

    // The chains beside them are measured, made and drawn: the staged fit (a Contain frame, no crop) and the engine's own behind a crop.
    let staged = shaped(&png, Fit::Contain, off_keys(), None, None, policy);
    let engine_crop = shaped(&png, Fit::Cover, off_keys(), crop, None, policy);
    let held_shape = {
        // A held zoom out of a picture of the frame's own shape: two `scale`s and no crop.
        let flat = photo(dir, "flat.png", &["-pix_fmt", "rgb24", "-vf", "scale=640:360"]);
        shaped(&flat, Fit::Cover, held_keys(), None, None, policy)
    };
    assert_eq!(
        staged.still_chain(&staged.layers[0], (320, 180)),
        Some(StillChain::edge(FitShape::Staged, false))
    );
    assert_eq!(
        held_shape.still_chain(&held_shape.layers[0], (320, 180)),
        Some(StillChain::edge(FitShape::Staged, false)),
        "{}",
        held_shape.fit_copy(&held_shape.layers[0], (320, 180)).unwrap().filters
    );
    assert_eq!(
        engine_crop.still_chain(&engine_crop.layers[0], (320, 180)),
        Some(StillChain::edge(FitShape::EngineCrop, false))
    );
    for (name, plan, chain) in [
        ("staged", &staged, StillChain::edge(FitShape::Staged, false)),
        ("held, no overhang", &held_shape, StillChain::edge(FitShape::Staged, false)),
        (
            "engine behind a crop",
            &engine_crop,
            StillChain::edge(FitShape::EngineCrop, false),
        ),
    ] {
        let before = kerf_gpu::copies::held().0;
        kerf_gpu::prepare(plan);
        let m = kerf_gpu::still_probe::measured();
        assert!(m.contains(rgb) && m.contains_chain(rgb, chain), "{name}: {m:?}");
        assert!(!m.contains_chain(rgb, cropped), "{name}: the caps hold a Cropped chain");
        assert_eq!(kerf_gpu::copies::held().0, before + 1, "{name}: the copy was not made ahead");
        assert!(
            !refused_as_moving(&measured, plan),
            "{name}: refused though its chain matched"
        );
        assert_eq!(drawn(&measured, plan), None, "{name}: not drawn");
    }
    // Measured and drawn beside them, the Cropped ones are still refused (the staged chain being in the caps does not let a crop through).
    for (name, plan) in &refused_plans {
        assert!(
            refused_as_moving(&measured, plan),
            "{name}: refused once the staged chain is measured"
        );
        assert!(
            drawn(&measured, plan).is_some(),
            "{name}: drawn once the staged chain is measured"
        );
        kerf_gpu::prepare(plan);
        assert_eq!(kerf_gpu::copies::held().0, held0 + 3, "{name}: preparing it made a copy");
    }
    assert_eq!(
        kerf_gpu::still_probe::measured(),
        rgb.union(rgb.chain(StillChain::edge(FitShape::Staged, false)))
            .union(rgb.chain(StillChain::edge(FitShape::EngineCrop, false))),
        "the caps hold the chains a toy ran and nothing else"
    );
}

/// `prepare` skips the probe and the copy for a stack that FFmpeg draws whole whatever its stills are, and not for one a measurement can lift.
fn skip_child(dir: &Path) {
    use kerf_core::{Container, TextOverlay};
    let policy = kerf_core::composite_color_policy();
    assert_ne!(
        policy,
        CompositeColorPolicy::Unknown,
        "the composite colour probe could not read this FFmpeg"
    );
    let png = photo(dir, "p.png", &["-pix_fmt", "rgb24"]);
    let jpeg = photo(dir, "p.jpg", &["-q:v", "3", "-pix_fmt", "yuvj420p"]);
    let (measured, _) = compositors();
    let mut caps = measured.caps();
    caps.still_motion = StillKinds::ALL;
    let rgb = StillKinds::RGB24;

    struct Cause<'a> {
        name: &'a str,
        fps: f64,
        opts: ExportOptions,
        policy: CompositeColorPolicy,
        svg: bool,
        text: bool,
        jpeg_elsewhere: bool,
        expect: fn(&kerf_core::Unsupported) -> bool,
    }
    let base = |name: &'static str, expect: fn(&kerf_core::Unsupported) -> bool| Cause {
        name,
        fps: 30.0,
        opts: ExportOptions {
            fps: Some(30.0),
            ..ExportOptions::default()
        },
        policy,
        svg: false,
        text: false,
        jpeg_elsewhere: false,
        expect,
    };
    let with_scaler = |name: &'static str, scaler: &str| Cause {
        opts: ExportOptions {
            fps: Some(30.0),
            scaler: Some(scaler.to_string()),
            ..ExportOptions::default()
        },
        ..base(name, |u| matches!(u, kerf_core::Unsupported::Scaler(_)))
    };
    let causes = vec![
        // (The names the engine lists are drawn where the scaler probe proved them, and refuse the moving still itself: below. A spelling it
        // does not list, and a scaler it does not know, are refused for the frame whatever is measured.)
        with_scaler("a scaler spelled other than the engine's list has it", "Lanczos"),
        with_scaler("a scaler the engine does not list", "area"),
        Cause {
            opts: ExportOptions {
                fps: Some(30.0),
                container: Container::Gif,
                ..ExportOptions::default()
            },
            ..base("gif", |u| matches!(u, kerf_core::Unsupported::Gif))
        },
        Cause {
            opts: ExportOptions {
                fps: Some(30.0),
                pix_fmt: Some("yuv444p".to_string()),
                ..ExportOptions::default()
            },
            ..base("a delivery that is not 4:2:0", |u| {
                matches!(u, kerf_core::Unsupported::Delivery(_))
            })
        },
        Cause {
            fps: 29.970029,
            opts: ExportOptions {
                fps: Some(29.970029),
                ..ExportOptions::default()
            },
            ..base("a pick rate", |u| matches!(u, kerf_core::Unsupported::PickRate { .. }))
        },
        Cause {
            svg: true,
            ..base("an animated title on screen", |u| matches!(u, kerf_core::Unsupported::Svg))
        },
        Cause {
            policy: CompositeColorPolicy::BottomLayerTag,
            jpeg_elsewhere: true,
            ..base("a cut FFmpeg 9 flags full range", |u| {
                matches!(u, kerf_core::Unsupported::FullRangeCut)
            })
        },
    ];
    let plan_of = |c: &Cause| -> RenderPlan {
        let mut clip = Clip::new(png.id, 0.0, 2.0, 0.0);
        clip.keyframes = inside_keys();
        let mut tracks = vec![Track {
            clips: vec![clip],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }];
        let mut assets = vec![png.clone()];
        if c.jpeg_elsewhere {
            tracks.push(Track {
                clips: vec![Clip::new(jpeg.id, 0.0, 1.0, 5.0)],
                ..Track::new(StreamKind::Video, "V2".to_string())
            });
            assets.push(jpeg.clone());
        }
        let mut tl = Timeline {
            tracks,
            overlays: Vec::new(),
            svg_overlays: Vec::new(),
            markers: Vec::new(),
            format: Delivery::new(320, 180, Fit::Cover).with_fps(Some(c.fps)).ok(),
            master: Default::default(),
            thumbnails: Vec::new(),
        };
        if c.svg {
            tl.svg_overlays.push(kerf_core::SvgOverlay {
                id: uuid::Uuid::new_v4(),
                name: String::new(),
                svg: "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"><rect width=\"10\" height=\"10\"/></svg>"
                    .to_string(),
                start: 0.0,
                end: 2.0,
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                opacity: 1.0,
                offset: 0.0,
                preset: None,
                template: None,
            });
        }
        if c.text {
            tl.overlays.push(TextOverlay::new("title", 0.0, 2.0));
        }
        Planner::new(&tl, &assets, &c.opts, PlanRequest::motion(c.policy))
            .expect("a planner")
            .at_frame(10)
            .expect("a plan")
    };
    let held0 = kerf_gpu::copies::held().0;
    // Frames FFmpeg draws whole: cold, nothing is measured and nothing made, though the still would be drawn were it not for the frame.
    for cause in &causes {
        let plan = plan_of(cause);
        let layer = plan
            .layers
            .iter()
            .position(|l| l.still.is_some())
            .expect("the moving still is a layer of the frame");
        let reasons = plan.size_free_reasons(&caps);
        assert!(
            reasons.iter().any(cause.expect),
            "{}: the frame is not refused for what the case is about: {reasons:?}",
            cause.name
        );
        assert!(
            reasons.iter().all(|u| u.layer() != Some(layer)),
            "{}: the still's layer is refused for itself, so the skip proves nothing: {reasons:?}",
            cause.name
        );
        kerf_gpu::prepare(&plan);
        assert_eq!(
            kerf_gpu::still_probe::measured(),
            StillKinds::NONE,
            "{}: a probe ran for a frame FFmpeg draws whole",
            cause.name
        );
        assert_eq!(kerf_gpu::copies::held().0, held0, "{}: a copy was made for it", cause.name);
        assert!(no_probe_ran(dir), "{}: a probe child ran", cause.name);
    }
    // The names the engine lists are drawn by a compositor whose probe proved them. A moving still under one is a chain of its own (`:flags=<name>`
    // is in every `scale` of its text): the move inside the frame is measured under each name (`still_route_scaler`), so the still is refused for itself
    // while that route is not in the caps, and drawn when it is. (A plan that takes the route under a name does measure it and make its copy:
    // that is `stills_prepare`'s and the parity case's; this test is about the frames FFmpeg draws whole.)
    for scaler in ["lanczos", "spline", "bilinear", "neighbor"] {
        let named = with_scaler("named", scaler);
        let plan = plan_of(&named);
        let layer = plan
            .layers
            .iter()
            .position(|l| l.still.is_some())
            .expect("the moving still is a layer of the frame");
        let name = kerf_core::ScalerKinds::of_name(scaler).expect("a name of the engine's list");
        assert_eq!(plan.still_route_scaler(&plan.layers[layer]), Some(name), "{scaler}");
        assert!(
            plan.still_chain(&plan.layers[layer], (320, 180)).is_none(),
            "{scaler}: a chain"
        );
        let mut all = caps;
        all.scalers = kerf_core::ScalerKinds::ALL;
        let refused = |all: &kerf_core::GpuCaps| -> Vec<String> {
            plan.size_free_reasons(all)
                .iter()
                .filter_map(|u| match u {
                    kerf_core::Unsupported::StillMotion(l, why) if l.index == layer => Some(why.clone()),
                    _ => None,
                })
                .collect()
        };
        all.still_motion = StillKinds::NONE;
        assert!(!refused(&all).is_empty(), "{scaler}: the cold moving still is not refused");
        assert!(
            plan.size_free_reasons(&all)
                .iter()
                .all(|u| !matches!(u, kerf_core::Unsupported::Scaler(_))),
            "{scaler}: refused as a scaler though its tables are proven"
        );
        // The base chains of its kind are measured and its route under this name is not: refused, naming the scaler.
        all.still_motion = rgb;
        let why = refused(&all);
        assert!(
            !why.is_empty() && why.iter().all(|w| w.contains(scaler)),
            "{scaler}: the moving still is not refused for the scaler named: {why:?}"
        );
        // ... and drawn once that route is: the route of another name does not do.
        all.still_motion = rgb.union(rgb.named_route(name));
        assert!(
            refused(&all).is_empty(),
            "{scaler}: refused with its route measured: {:?}",
            refused(&all)
        );
        let other = if name == kerf_core::ScalerKinds::LANCZOS {
            kerf_core::ScalerKinds::SPLINE
        } else {
            kerf_core::ScalerKinds::LANCZOS
        };
        all.still_motion = rgb.union(rgb.named_route(other));
        assert!(!refused(&all).is_empty(), "{scaler}: drawn on another name's route");
    }
    // A refusal of the frame that a measurement lifts (a title: the text model is measured by the same `prepare`) is not one of those: the still
    // under it is measured and its copy made, so the harness would have seen a skip that was too wide.
    let titled = plan_of(&Cause {
        text: true,
        ..base("a title", |_| true)
    });
    assert!(
        titled
            .size_free_reasons(&caps)
            .iter()
            .all(|u| u.layer().is_none_or(|n| titled.layers[n].still.is_none())),
        "the title's frame refuses the still for itself"
    );
    kerf_gpu::prepare(&titled);
    assert_eq!(kerf_gpu::still_probe::measured(), rgb, "a frame with a title was skipped");
    assert_eq!(kerf_gpu::copies::held().0, held0 + 1, "the copy under a title was not made");
    // ... and the same stack without the title is the same picture: measured already, its copy kept.
    let control = plan_of(&base("control", |_| true));
    kerf_gpu::prepare(&control);
    assert_eq!(kerf_gpu::still_probe::measured(), rgb, "the control was not measured");
    assert_eq!(kerf_gpu::copies::held().0, held0 + 1, "the control's copy was not kept");
}

#[test]
#[ignore = "needs ffmpeg and an adapter (lavapipe is enough)"]
fn r3_gates_a_control_unperturbed_ffmpeg_measures_each_chain_a_plan_takes_and_no_other() {
    child("chains", "none:pass", Duration::from_secs(400));
}

#[test]
#[ignore = "needs ffmpeg and an adapter (lavapipe is enough)"]
fn r3_gates_one_chain_that_differs_is_refused_alone_and_a_base_chain_that_differs_makes_no_copy_for_any() {
    for chain in [
        "inside",
        "engine",
        "engine_crop",
        "staged",
        "bicubic_inside",
        "bicubic_engine",
        "bicubic_engine_crop",
        "bicubic_staged",
    ] {
        child("chains", &format!("rgb:{chain}"), Duration::from_secs(400));
    }
}

#[test]
#[ignore = "needs ffmpeg and an adapter (lavapipe is enough)"]
fn r3_gates_a_text_with_a_crop_besides_the_guard_is_never_measured_made_or_drawn() {
    child("cropped", "none:pass", Duration::from_secs(400));
}

#[test]
#[ignore = "needs ffmpeg and an adapter (lavapipe is enough)"]
fn r3_gates_a_frame_ffmpeg_draws_whole_is_neither_measured_nor_copied_for() {
    child("skip", "none:pass", Duration::from_secs(400));
}
