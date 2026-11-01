//! The gates of the keyed placement, attacked from outside: a placement probe that finds a difference, cannot tell, stalls
//! or is killed; the file's range flag asked of a cold cache, a hung `ffprobe`, a probe that prints nothing usable and a file
//! replaced under a running session; and the frame pick at the ends of the integers.
//!
//! Each scenario that changes what the process runs (`KERF_FFMPEG`, `KERF_FFPROBE`: wrapper scripts that perturb, stall or
//! cut off the probe's graph and count every call) is a child process of its own, because the probes' answers are kept for the
//! life of the process. The test binary runs itself again with `--exact checker_a_gates_child` and the scenario in the
//! environment. Needs `ffmpeg`, `ffprobe` and an adapter; `#[ignore]`d, bar the pure frame-pick sweep.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_a_gates -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kerf_core::frame_pick::fps_span;
use kerf_core::{
    fps_pick, Behavior, Clip, CompositeColorPolicy, ExportOptions, FpsPick, Keyframe, Pick, PickProgress, Project, Rational,
    RenderPlan, SourceFrames, StreamKind, Timeline, Track,
};
use kerf_gpu::placement_probe::Placement;
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuError, GpuOptions, Hint, YuvFrame};

const SCENARIO: &str = "KERF_GATE_SCENARIO";
const DIR: &str = "KERF_GATE_DIR";
const MODE: &str = "KERF_GATE_MODE";
const REAL_FFMPEG: &str = "KERF_GATE_REAL_FFMPEG";
const REAL_FFPROBE: &str = "KERF_GATE_REAL_FFPROBE";

/// The `ffmpeg` wrapper. A call whose argv names `perspective` (the placement probe's graph; no other graph of this process
/// has one) is logged and, by the mode in `$DIR/mode`, changed, stalled, cut short or failed; everything else is the real
/// `ffmpeg`.
const FFMPEG_WRAPPER: &str = r#"#!/bin/bash
D="$KERF_GATE_DIR"; REAL="$KERF_GATE_REAL_FFMPEG"
args=("$@")
hit=0
for a in "${args[@]}"; do case "$a" in *perspective*) hit=1;; esac; done
echo "$hit" >> "$D/ffmpeg.log"
if [ "$hit" = 1 ]; then
  echo x >> "$D/hit.log"
  edit() { for i in "${!args[@]}"; do args[$i]=$(printf '%s' "${args[$i]}" | sed "$1"); done; }
  case "$(cat "$D/mode" 2>/dev/null)" in
    cubic) edit 's/interpolation=linear/interpolation=cubic/g' ;;
    noframe) edit 's/:eval=frame//g' ;;
    sinp) edit 's/sin(2\*PI\*/sin(2.01*PI*/g' ;;
    fail) exit 1 ;;
    short) "$REAL" "${args[@]}" | head -c 17; exit 0 ;;
    stall) sleep 36; exit 1 ;;
  esac
fi
exec "$REAL" "${args[@]}"
"#;

/// The `ffprobe` wrapper: every call is logged; once `$DIR/armed` exists, a file by the name of its kind is answered the way
/// that kind says (hung, silent, cut off, an error document, junk).
const FFPROBE_WRAPPER: &str = r#"#!/bin/bash
D="$KERF_GATE_DIR"; REAL="$KERF_GATE_REAL_FFPROBE"
echo "$*" >> "$D/ffprobe.log"
if [ -e "$D/armed" ]; then
  case "$*" in
    */hung-*) exec sleep 120 ;;
    */empty-*) exit 0 ;;
    */trunc-*) "$REAL" "$@" | head -c 40; exit 0 ;;
    */errdoc-*) echo '{"error":{"code":-2,"string":"No such file or directory"}}'; exit 0 ;;
    */junk-*) echo 'this is not json'; exit 0 ;;
  esac
fi
exec "$REAL" "$@"
"#;

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write a wrapper");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
}

fn lines(log: &Path) -> usize {
    std::fs::read_to_string(log).map_or(0, |t| t.lines().count())
}

/// Run this test binary again as the child of `scenario` and fail unless it passes.
fn child(scenario: &str, mode: &str, limit: Duration) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("checker-a-gates")
        .join(format!("{scenario}-{mode}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let real_ffmpeg = kerf_core::ffmpeg_path();
    let real_ffprobe = std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string());
    let (ffmpeg, ffprobe) = (dir.join("ffmpeg-wrapper.sh"), dir.join("ffprobe-wrapper.sh"));
    executable(&ffmpeg, FFMPEG_WRAPPER);
    executable(&ffprobe, FFPROBE_WRAPPER);
    std::fs::write(dir.join("mode"), mode).expect("write the mode");
    let (out, err) = (dir.join("child.out"), dir.join("child.err"));
    let mut process = Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--ignored", "--exact", "--nocapture", "checker_a_gates_child"])
        .env(SCENARIO, scenario)
        .env(MODE, mode)
        .env(DIR, &dir)
        .env(REAL_FFMPEG, real_ffmpeg)
        .env(REAL_FFPROBE, real_ffprobe)
        .env("KERF_FFMPEG", &ffmpeg)
        .env("KERF_FFPROBE", &ffprobe)
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

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

fn make(dir: &Path, name: &str, flags: &[&str]) -> PathBuf {
    let path = dir.join(name);
    let mut args = vec![
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
    path
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// A zoom that moves.
    Zoom,
    /// A constant pose with a breath on top.
    Breathe,
    /// A held zoom and position: drawn without a placement.
    Held,
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

/// A plan of `kind` over `path`. `unrecorded`: the asset did not record its range (probed before the flag was kept).
fn plan(path: &Path, kind: Kind, policy: CompositeColorPolicy, unrecorded: bool) -> RenderPlan {
    let mut asset = Project::probe_asset(path).expect("probe the asset");
    if unrecorded {
        for stream in &mut asset.streams {
            stream.color_range = None;
        }
    }
    let mut clip = Clip::new(asset.id, 0.0, 1.0, 0.0);
    match kind {
        Kind::Zoom => clip.keyframes = vec![key(0.0, 1.0), key(1.0, 1.6)],
        Kind::Breathe => {
            clip.behaviors = vec![Behavior::Breathe {
                amount: 0.2,
                speed: 1.5,
                phase: 0.0,
            }];
        }
        Kind::Held => {
            clip.transform.scale = 0.6;
            clip.transform.pos_x = 0.1;
        }
    }
    let timeline = Timeline {
        tracks: vec![Track {
            clips: vec![clip],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    RenderPlan::at(&timeline, &[asset], &ExportOptions::default(), 0.5, policy).expect("a plan")
}

fn flat(plan: &RenderPlan) -> YuvFrame {
    let (w, h) = (plan.layers[0].stream.width, plan.layers[0].stream.height);
    YuvFrame {
        width: w,
        height: h,
        y: vec![90; (w * h) as usize],
        u: vec![120; (w.div_ceil(2) * h.div_ceil(2)) as usize],
        v: vec![130; (w.div_ceil(2) * h.div_ceil(2)) as usize],
    }
}

/// `Some(why)` for a frame the compositor refuses, `None` for one it drew. Any other error is a failure of the test.
fn refusal<T>(result: Result<T, GpuError>) -> Option<String> {
    match result {
        Ok(_) => None,
        Err(GpuError::Unsupported(why)) => Some(why),
        Err(other) => panic!("not a refusal: {other}"),
    }
}

fn composite(compositor: &Compositor, plan: &RenderPlan) -> Option<String> {
    refusal(compositor.composite(plan, &[Some(flat(plan))], plan.size(u32::MAX)))
}

#[test]
#[ignore = "this binary's own child: runs the scenario named in the environment"]
fn checker_a_gates_child() {
    let Ok(scenario) = std::env::var(SCENARIO) else {
        return;
    };
    let dir = PathBuf::from(env(DIR));
    let mode = env(MODE);
    match scenario.as_str() {
        "placement" => placement_child(&dir, &mode),
        "range" => range_child(&dir, &mode),
        "selftest" => selftest_child(&dir, &mode),
        other => panic!("no scenario {other}"),
    }
}

fn placement_child(dir: &Path, mode: &str) {
    let (hits, all) = (dir.join("hit.log"), dir.join("ffmpeg.log"));
    let file = make(dir, "clip.mkv", &[]);
    let policy = CompositeColorPolicy::FixedBt601;
    let (zoom, breathe, held) = (
        plan(&file, Kind::Zoom, policy, false),
        plan(&file, Kind::Breathe, policy, false),
        plan(&file, Kind::Held, policy, false),
    );
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    let compositor = Compositor::new(gpu).expect("a compositor");
    let size = zoom.size(u32::MAX);

    // Cold: nothing measured, so a moving zoom is refused by the caps, quickly, with no process spawned by the render.
    let (calls, started) = (lines(&all), Instant::now());
    assert!(!compositor.caps().placement && !compositor.caps().behaviors, "cold caps");
    let cold = composite(&compositor, &zoom).expect("a cold moving zoom is drawn");
    assert!(cold.contains("moving zoom or position"), "{cold}");
    assert!(composite(&compositor, &breathe).is_some(), "a cold breath is drawn");
    let held_cold = composite(&compositor, &held);
    assert!(held_cold.is_none(), "a held zoom needs no placement: {held_cold:?}");
    assert_eq!(lines(&all), calls, "the render spawned a process");
    assert!(started.elapsed() < Duration::from_secs(6), "{:?}", started.elapsed());

    let started = Instant::now();
    kerf_gpu::prepare(&zoom);
    let took = started.elapsed();
    let verdict = kerf_gpu::placement_probe::measured();
    eprintln!("mode {mode}: {verdict:?} after {took:.1?}, {} probe runs", lines(&hits));

    let refused = |label: &str| {
        assert!(!compositor.caps().placement && !compositor.caps().behaviors, "{label}: caps");
        for (name, p) in [("zoom", &zoom), ("breathe", &breathe)] {
            let c = composite(&compositor, p);
            assert!(c.is_some(), "{label}: a {name} was drawn by composite");
            let r = refusal(compositor.render_plan(p, size));
            assert!(r.is_some(), "{label}: a {name} was drawn by render_plan");
            let y = refusal(compositor.composite_yuv(p, &[Some(flat(p))], size));
            assert!(y.is_some(), "{label}: a {name} was drawn by composite_yuv");
        }
        let held_now = composite(&compositor, &held);
        assert!(held_now.is_none(), "{label}: nothing else moves with it: {held_now:?}");
    };
    match mode {
        "pass" => {
            assert_eq!(verdict, Placement::Match, "the control: an unperturbed FFmpeg");
            assert!(compositor.caps().placement && compositor.caps().behaviors);
            for (name, p) in [("zoom", &zoom), ("breathe", &breathe), ("held", &held)] {
                assert_eq!(composite(&compositor, p), None, "{name} drawn once the placement is known");
                assert_eq!(refusal(compositor.render_plan(p, size)), None, "{name} by render_plan");
            }
            // Measured once: a Match is kept without another run.
            let runs = lines(&hits);
            kerf_gpu::prepare(&zoom);
            assert_eq!(lines(&hits), runs, "a match was measured twice");
        }
        "cubic" | "sinp" => {
            let Placement::Differs(why) = &verdict else {
                panic!("a perturbed chain was not found to differ: {verdict:?}");
            };
            eprintln!("  first case: {why}");
            refused(mode);
            // A difference is for good: not asked again, not softened by time.
            let runs = lines(&hits);
            kerf_gpu::prepare(&zoom);
            assert_eq!(kerf_gpu::placement_probe::placement(), verdict);
            assert_eq!(lines(&hits), runs, "a difference was measured again");
        }
        "noframe" | "fail" | "short" | "stall" => {
            assert_ne!(verdict, Placement::Match, "{mode}");
            refused(mode);
            if verdict == Placement::Unknown {
                // Unknown is not believed and not asked again inside the backoff.
                let (runs, again) = (lines(&hits), Instant::now());
                kerf_gpu::prepare(&zoom);
                kerf_gpu::prepare(&breathe);
                assert_eq!(kerf_gpu::placement_probe::placement(), Placement::Unknown);
                assert_eq!(lines(&hits), runs, "an unknown was asked again inside the backoff");
                assert!(again.elapsed() < Duration::from_secs(4), "{:?}", again.elapsed());
                refused("inside the backoff");
            }
            if mode == "stall" {
                // Killed at its 20 s limit and not waited for: the wrapper's `sleep 36` holds the pipes for 16 s more.
                assert_eq!(verdict, Placement::Unknown);
                assert!(
                    took >= Duration::from_secs(19) && took < Duration::from_secs(30),
                    "the stalled probe took {took:?}"
                );
                assert_eq!(lines(&hits), 1, "one run, and the first one decides");
                // After the backoff, with the stall gone, the placement is measured and drawn.
                std::fs::write(dir.join("mode"), "pass").expect("clear the stall");
                std::thread::sleep(Duration::from_secs(31).saturating_sub(started.elapsed().saturating_sub(took)));
                kerf_gpu::prepare(&zoom);
                assert_eq!(
                    kerf_gpu::placement_probe::measured(),
                    Placement::Match,
                    "recovered after the backoff"
                );
                assert_eq!(composite(&compositor, &zoom), None);
                assert_eq!(composite(&compositor, &breathe), None);
            }
        }
        other => panic!("no mode {other}"),
    }
}

fn selftest_child(dir: &Path, mode: &str) {
    let file = make(dir, "clip.mkv", &[]);
    let zoom = plan(&file, Kind::Zoom, CompositeColorPolicy::FixedBt601, false);
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    let compositor = Compositor::new(std::sync::Arc::clone(&gpu)).expect("a compositor");
    let names = |report: &kerf_gpu::SelfTestReport| report.cases.iter().map(|c| c.name).collect::<Vec<_>>();
    let started = Instant::now();
    let report =
        kerf_gpu::parity_self_test(&gpu).unwrap_or_else(|e| panic!("a placement the machine does not do closed the check: {e}"));
    let took = started.elapsed();
    eprintln!("mode {mode}: {} after {took:.1?}", report.summary());
    assert!(report.passed(), "{}", report.summary());
    assert!(
        !names(&report).contains(&"place"),
        "a placed frame was drawn without a placement to compare it with: {:?}",
        names(&report)
    );
    assert!(names(&report).starts_with(&["fit", "pip", "bt709"]), "{:?}", names(&report));
    assert_ne!(kerf_gpu::placement_probe::measured(), Placement::Match);
    assert!(
        composite(&compositor, &zoom).is_some(),
        "a moving zoom was drawn after a self-test that left its frame out"
    );
    if mode == "stall" {
        assert!(
            took >= Duration::from_secs(19),
            "the stalled probe was waited for no longer than {took:?}"
        );
        // The stall is gone, the backoff over: a probe that succeeds now must not open a placement the check never looked at.
        std::fs::write(dir.join("mode"), "pass").expect("clear the stall");
        std::thread::sleep(Duration::from_secs(31));
        kerf_gpu::prepare(&zoom);
        assert_eq!(
            kerf_gpu::placement_probe::placement(),
            Placement::Unknown,
            "the placement was opened without its frame checked"
        );
        assert!(
            composite(&compositor, &zoom).is_some(),
            "drawn on a placement the self-test never checked"
        );
        // The next self-test that can tell lifts it, draws the placed frame and passes it.
        let report = kerf_gpu::parity_self_test(&gpu).unwrap_or_else(|e| panic!("the second self-test: {e}"));
        eprintln!("second self-test: {}", report.summary());
        assert!(report.passed(), "{}", report.summary());
        assert!(names(&report).contains(&"place"), "{:?}", names(&report));
        assert_eq!(kerf_gpu::placement_probe::measured(), Placement::Match);
        assert_eq!(composite(&compositor, &zoom), None);
    }
}

fn range_child(dir: &Path, mode: &str) {
    let (probes, armed) = (dir.join("ffprobe.log"), dir.join("armed"));
    let tv = ["-color_range", "tv"];
    let pc = ["-color_range", "pc"];
    let policy = CompositeColorPolicy::BottomLayerTag;
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
    // Pinned, so that the placement probe is not what this is about.
    let compositor = Compositor::new(gpu).expect("a compositor").with_placement(Placement::Match);
    let known = |p: &Path| kerf_core::source_is_full_range_known(p);
    let asked = "not asked ahead of the render";
    match mode {
        "hang" => {
            let limited = make(dir, "limited.mkv", &tv);
            let hung = make(dir, "hung-full.mkv", &pc);
            let (p_limited, p_hung) = (
                plan(&limited, Kind::Zoom, policy, true),
                plan(&hung, Kind::Zoom, policy, true),
            );
            std::fs::write(&armed, "").expect("arm the wrapper");
            let hung_path = PathBuf::from(&p_hung.layers[0].path);
            let before = lines(&probes);
            let asking = std::thread::spawn({
                let hung_path = hung_path.clone();
                move || {
                    let started = Instant::now();
                    (kerf_core::source_is_full_range(&hung_path), started.elapsed())
                }
            });
            let wait = Instant::now();
            while lines(&probes) == before {
                assert!(wait.elapsed() < Duration::from_secs(5), "the probe never started");
                std::thread::sleep(Duration::from_millis(10));
            }
            // A probe of this file is under way (and will be for 20 s). The render does not wait for it and does not draw.
            let started = Instant::now();
            assert_eq!(known(&hung_path), None);
            let why = composite(&compositor, &p_hung).expect("a file under probe was drawn");
            assert!(why.contains(asked), "{why}");
            assert!(
                started.elapsed() < Duration::from_secs(6),
                "the render waited {:?}",
                started.elapsed()
            );
            // Another file's probe is not held up by it, and the render of that one is.
            let started = Instant::now();
            kerf_gpu::prepare(&p_limited);
            assert!(started.elapsed() < Duration::from_secs(12), "{:?}", started.elapsed());
            assert_eq!(known(Path::new(&p_limited.layers[0].path)), Some(false));
            assert_eq!(composite(&compositor, &p_limited), None, "the limited file is drawn");
            // The hung probe is killed at its limit and the file stays unknown (and refused).
            let (answer, took) = asking.join().expect("the asking thread");
            assert_eq!(answer, None);
            assert!(took < Duration::from_secs(25), "the probe took {took:?}");
            assert_eq!(known(&hung_path), None);
            assert!(composite(&compositor, &p_hung).expect("refused").contains(asked));
            kerf_gpu::prepare(&p_hung);
            assert_eq!(known(&hung_path), None, "a failure is not an answer");
            assert!(composite(&compositor, &p_hung).is_some());
        }
        "silent" => {
            // A probe that exits 0 with nothing usable on its output used to read as "limited range": the files are full range.
            let control = make(dir, "full-control.mkv", &pc);
            let files: Vec<(&str, PathBuf)> = ["empty", "trunc", "errdoc", "junk"]
                .into_iter()
                .map(|kind| (kind, make(dir, &format!("{kind}-full.mkv"), &pc)))
                .collect();
            let plans: Vec<(&str, RenderPlan)> = files
                .iter()
                .map(|(kind, path)| (*kind, plan(path, Kind::Zoom, policy, true)))
                .collect();
            let p_control = plan(&control, Kind::Zoom, policy, true);
            std::fs::write(&armed, "").expect("arm the wrapper");
            for (kind, p) in &plans {
                let path = PathBuf::from(&p.layers[0].path);
                assert!(composite(&compositor, p).expect("refused").contains(asked), "{kind}: cold");
                kerf_gpu::prepare(p);
                assert_eq!(
                    kerf_core::source_is_full_range(&path),
                    None,
                    "{kind}: an ffprobe that said nothing usable"
                );
                assert_eq!(known(&path), None, "{kind}");
                let why = composite(&compositor, p).unwrap_or_else(|| panic!("{kind}: a full-range file was drawn as limited"));
                assert!(why.contains("would not say"), "{kind}: {why}");
            }
            kerf_gpu::prepare(&p_control);
            assert_eq!(
                known(Path::new(&p_control.layers[0].path)),
                Some(true),
                "the control says full range"
            );
            let why = composite(&compositor, &p_control).expect("a full-range file is refused");
            assert!(why.contains("full-range"), "{why}");
        }
        "decode" => {
            // The decode makes the answer exist: a first frame of a limited file is drawn, a full-range one refused, and a file
            // replaced by a full-range one under the running source is refused by its next frame.
            let limited = make(dir, "limited.mkv", &tv);
            let full = make(dir, "full.mkv", &pc);
            let (p_limited, p_full) = (
                plan(&limited, Kind::Zoom, policy, true),
                plan(&full, Kind::Zoom, policy, true),
            );
            let size = p_limited.size(u32::MAX);
            let source = FrameSource::new(FrameSourceConfig::default());
            assert_eq!(known(&limited), None);
            assert_eq!(known(&full), None);
            let drawn = |p: &RenderPlan| refusal(compositor.render_plan_with(p, size, &source, Hint::Scrub));
            assert_eq!(drawn(&p_limited), None, "a limited file, first frame");
            assert_eq!(known(&limited), Some(false));
            let why = drawn(&p_full).expect("a full-range file was drawn");
            assert!(why.contains("full-range"), "{why}");
            assert_eq!(known(&full), Some(true));
            let calls = lines(&probes);
            assert_eq!(drawn(&p_limited), None);
            assert!(drawn(&p_full).is_some());
            assert_eq!(lines(&probes), calls, "a second frame of a known file asked ffprobe again");
            // Replaced: the same path, another file, a later modification time.
            std::thread::sleep(Duration::from_millis(1100));
            std::fs::copy(&full, &limited).expect("replace the file");
            let why = drawn(&p_limited).expect("a file replaced by a full-range one was drawn");
            assert!(why.contains("full-range"), "{why}");
            assert_eq!(known(&limited), Some(true));
        }
        other => panic!("no mode {other}"),
    }
    let _ = lines(&probes);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_gates_a_placement_the_machine_does_not_do_is_refused_not_drawn() {
    let mut fails = Vec::new();
    for (mode, limit) in [
        ("pass", 120),
        ("cubic", 120),
        ("sinp", 120),
        ("noframe", 120),
        ("fail", 120),
        ("short", 120),
    ] {
        let result = catch_unwind(AssertUnwindSafe(|| child("placement", mode, Duration::from_secs(limit))));
        if let Err(e) = result {
            fails.push(format!("{mode}: {:?}", e.downcast_ref::<String>()));
        }
    }
    assert!(fails.is_empty(), "{fails:?}");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); takes about a minute"]
fn checker_a_gates_a_placement_probe_that_stalls_is_killed_unknown_refused_and_asked_again_after_the_backoff() {
    child("placement", "stall", Duration::from_secs(150));
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_gates_a_self_test_over_a_placement_that_differs_draws_no_placed_frame_and_passes() {
    child("selftest", "cubic", Duration::from_secs(150));
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); takes about two minutes"]
fn checker_a_gates_a_self_test_that_could_not_tell_holds_the_placement_back_until_one_can() {
    child("selftest", "stall", Duration::from_secs(240));
}

#[test]
#[ignore = "needs ffmpeg, ffprobe and a GPU adapter (lavapipe is enough)"]
fn checker_a_gates_a_cold_range_cache_refuses_in_the_render_and_never_waits_for_a_probe() {
    child("range", "hang", Duration::from_secs(120));
}

#[test]
#[ignore = "needs ffmpeg, ffprobe and a GPU adapter (lavapipe is enough)"]
fn checker_a_gates_an_ffprobe_that_says_nothing_usable_is_no_limited_range() {
    child("range", "silent", Duration::from_secs(120));
}

#[test]
#[ignore = "needs ffmpeg, ffprobe and a GPU adapter (lavapipe is enough)"]
fn checker_a_gates_the_decode_asks_the_file_and_asks_a_replaced_file_again() {
    child("range", "decode", Duration::from_secs(120));
}

static SPOTS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// A small generator.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }

    fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[(self.next() % of.len() as u64) as usize]
    }
}

/// What one sweep of the frame pick found.
struct Sweep {
    checked: u64,
    showed: u64,
    panics: Vec<String>,
    spots: std::collections::BTreeMap<String, usize>,
    /// The first case to panic at each place.
    first: std::collections::BTreeMap<String, String>,
    inconsistent: Vec<String>,
}

/// Sweep the frame pick over random combinations of hostile numbers. The clip's own numbers (window, start, end, lead, speed,
/// rate, output frame, time base) are always hostile; `file_at_the_ends` makes the file's too: pts, the container's start and
/// the last duration at the ends of the `i64`. A debug build panics on an overflow and a release build wraps; what is asked
/// of the pick is that it never panics and that, wherever it answers, the answer is a frame of the file (or none) and the
/// same one whichever way it is asked (`fps_pick`, `Pick::select`, the cursor's `progress` over the whole file,
/// `output_index`, `fps_span`).
fn hostile_sweep(file_at_the_ends: bool, in_at_the_ends: bool, rounds: u32) -> Sweep {
    let ends_pts: [&[i64]; 7] = [
        &[],
        &[0],
        &[i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX - 1, i64::MAX],
        &[0, i64::MAX],
        &[i64::MIN, 0],
        &[i64::MAX - 2, i64::MAX - 1, i64::MAX],
        &[i64::MIN, i64::MIN + 1, i64::MIN + 2],
    ];
    let sane_pts: [&[i64]; 7] = [
        &[],
        &[0],
        &[-5, -3, -1, 1, 3, 5],
        &[0, 1024, 2048, 3072, 4096, 5120, 6144, 7168],
        &[1 << 40, (1 << 40) + 1024, (1 << 40) + 2048],
        &[-(1 << 40), -(1 << 40) + 1024, 0, 1024],
        &[1 << 61, (1 << 61) + 3000, (1 << 61) + 6000],
    ];
    let bases: [(u32, u32); 8] = [
        (1, 1),
        (1, 90_000),
        (1, 1_000_000_000),
        (1001, 30_000),
        (1, u32::MAX),
        (u32::MAX, 1),
        (u32::MAX, u32::MAX),
        (1, 10_240),
    ];
    let (starts_us, durations): (&[i64], &[i64]) = if file_at_the_ends {
        (
            &[
                i64::MIN,
                i64::MIN + 1,
                -1_000_000,
                -1,
                0,
                1,
                1_000_000,
                i64::MAX - 1,
                i64::MAX,
            ],
            &[i64::MIN, -1, 0, 1, 1024, i64::MAX],
        )
    } else {
        (
            &[-1_000_000_000_000, -1_000_000, -1, 0, 1, 1_000_000, 1_000_000_000_000],
            &[-1, 0, 1, 1024, 1 << 40],
        )
    };
    let speeds = [1.0, 0.5, 2.0, 1e-300, 1e300, f64::NAN, f64::INFINITY, -1.0, 0.0];
    let windows: Vec<(f64, f64)> = if in_at_the_ends {
        vec![
            (0.0, 1.0),
            (0.0, f64::INFINITY),
            (1.5, 3.0),
            (1e300, f64::MAX),
            (f64::NAN, f64::NAN),
            (-1e300, 1e300),
            (9.2e12, 9.3e12),
            (0.25, 0.25),
        ]
    } else {
        // (An in-point is inside a file: seconds to years. Its end may be anything.)
        vec![
            (0.0, 1.0),
            (0.0, f64::INFINITY),
            (1.5, 3.0),
            (0.0, f64::MAX),
            (0.0, f64::NAN),
            (3.0e8, 1e300),
            (0.25, 0.25),
            (1.0e9, 9.2e12),
        ]
    };
    let hostile_floats = [
        0.0,
        0.5,
        1.0,
        1e12,
        9.2e12,
        9.3e12,
        1e15,
        1e300,
        f64::MAX,
        f64::INFINITY,
        f64::NAN,
        -1.0,
        -1e13,
        -1e300,
    ];
    let leads: &[f64] = if in_at_the_ends {
        &hostile_floats
    } else {
        &[0.0, 0.5, 1.0, -1.0, 3.0e8]
    };
    let floats = hostile_floats;
    let ends = [f64::INFINITY, 0.0, 1.0, 1e300, f64::NAN];
    let frames: [u64; 9] = [
        0,
        1,
        29,
        1 << 31,
        u64::from(u32::MAX),
        i64::MAX as u64 - 1,
        i64::MAX as u64,
        i64::MAX as u64 + 1,
        u64::MAX,
    ];
    let rates: [(u32, u32); 7] = [
        (30, 1),
        (1, 1),
        (60_000, 1001),
        (u32::MAX, 1),
        (1, u32::MAX),
        (u32::MAX, u32::MAX),
        (24_000, 1001),
    ];
    let images = [None, None, None, Some((30, 1)), Some((u32::MAX, 1)), Some((1, u32::MAX))];
    let mut rng = Lcg(0x6b65_7266);
    let mut out = Sweep {
        checked: 0,
        showed: 0,
        panics: Vec::new(),
        spots: Default::default(),
        first: Default::default(),
        inconsistent: Vec::new(),
    };
    let me = std::thread::current().id();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == me {
            let at = info
                .location()
                .map_or("?".to_string(), |l| format!("{}:{}", l.file(), l.line()));
            SPOTS.lock().unwrap_or_else(|e| e.into_inner()).push(at);
        } else {
            previous(info);
        }
    }));
    SPOTS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    for _ in 0..rounds {
        let pts = if file_at_the_ends {
            rng.pick(&ends_pts)
        } else {
            rng.pick(&sane_pts)
        };
        let (num, den) = rng.pick(&bases);
        let src = SourceFrames {
            pts,
            time_base: Rational { num, den },
            start_us: rng.pick(starts_us),
            last_duration: rng.pick(durations),
        };
        let (fnum, fden) = rng.pick(&rates);
        let image = rng.pick(&images);
        let p = FpsPick {
            speed: rng.pick(&speeds),
            reverse: rng.next().is_multiple_of(3),
            window: rng.pick(&windows),
            start: rng.pick(&floats),
            end: rng.pick(&ends),
            frame: rng.pick(&frames),
            fps: Rational { num: fnum, den: fden },
            drop_first: rng.next().is_multiple_of(4),
            lead: rng.pick(leads),
            image: image.map(|(num, den)| Rational { num, den }),
            pinned: rng.next().is_multiple_of(2),
        };
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let shown = fps_pick(&p, &src);
            let span = fps_span(&p, &src);
            let pick = Pick::Fps(p);
            let selected = pick.select(&src);
            let index = pick.output_index(&src);
            let progress = pick.progress(&src, true);
            let _ = pick.progress(
                &SourceFrames {
                    pts: &pts[..pts.len().min(1)],
                    ..src
                },
                false,
            );
            let _ = (p.seek(), p.seek_arg());
            let _ = kerf_core::seek_ticks(p.window.0, src.time_base);
            if in_at_the_ends {
                // A still at a source time that is not one: the same seek, to the `-ss` the still graph writes.
                let _ = Pick::AtOrAfter(p.start).select(&src);
                let _ = Pick::Before(p.start).select(&src);
                let _ = Pick::AtOrAfter(p.start).progress(&src, true);
                let _ = Pick::Before(p.lead).progress(&src, false);
                let _ = kerf_core::seek_ticks(p.start, src.time_base);
            }
            (shown, span, selected, index, progress)
        }));
        let (shown, span, selected, index, progress) = match outcome {
            Ok(r) => r,
            Err(e) => {
                let why = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()));
                let case = format!("{p:?} over {src:?}: {why:?}");
                let at = SPOTS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .last()
                    .cloned()
                    .unwrap_or_default();
                out.first.entry(at).or_insert_with(|| case.clone());
                out.panics.push(case);
                continue;
            }
        };
        out.checked += 1;
        let mut wrong = Vec::new();
        if let Some(i) = shown {
            out.showed += 1;
            let limit = if image.is_some() { 1 } else { pts.len() };
            if i >= limit {
                wrong.push(format!("picked frame {i} of {limit}"));
            }
        }
        if shown != selected {
            wrong.push(format!("fps_pick {shown:?} but select {selected:?}"));
        }
        // (A still image over a file with no frames read has its one frame made up: the cursor has nothing to wait on.)
        if !(image.is_some() && pts.is_empty()) {
            match progress {
                PickProgress::Ready { shown: s, .. } if s != shown => {
                    wrong.push(format!("progress ready on {s:?}, the pick {shown:?}"));
                }
                PickProgress::Ready { .. } => {}
                other => wrong.push(format!("progress over the whole file is {other:?}")),
            }
        }
        if let (Some((first, end)), Ok(k)) = (span, i64::try_from(p.frame)) {
            if p.speed > 0.0 && p.speed.is_finite() && (shown.is_some() != (first <= k && k < end)) {
                wrong.push(format!("pick {shown:?} at {k} but the clip's slots are {first}..{end}"));
            }
            if shown.is_some() && index.is_none() && k.checked_sub(first).is_some() {
                wrong.push("a frame is shown and no output index".to_string());
            }
        }
        if !wrong.is_empty() {
            out.inconsistent.push(format!("{p:?} over {src:?}: {}", wrong.join("; ")));
        }
    }
    for at in SPOTS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        *out.spots.entry(at.clone()).or_default() += 1;
    }
    out
}

fn report(what: &str, sweep: &Sweep) {
    eprintln!(
        "{what}: {} picks ({} showed a frame), {} panics, {} inconsistent",
        sweep.checked,
        sweep.showed,
        sweep.panics.len(),
        sweep.inconsistent.len()
    );
    for (at, n) in &sweep.spots {
        eprintln!(
            "  panicked {n} times at {at}, first: {}",
            sweep.first.get(at).map_or("", String::as_str)
        );
    }
    for line in sweep.inconsistent.iter().take(5) {
        eprintln!("  {line}");
    }
}

/// Every number the clip brings to the pick hostile bar where it starts inside its file (a start past the 9.2e12 seconds whose
/// microseconds fit an `i64`, a speed of 1e-300, an output frame of `u64::MAX`, a window end of `f64::MAX`, a time base of
/// 1/`u32::MAX`), over files whose timestamps are ones a file has: what the saturating arithmetic of `frame_pick` is for.
#[test]
fn checker_a_frame_pick_with_hostile_clip_numbers_neither_panics_nor_wanders() {
    let sweep = hostile_sweep(false, false, 300_000);
    report("hostile clip numbers", &sweep);
    assert!(sweep.showed > 0, "the sweep never drew a frame");
    assert!(
        sweep.panics.is_empty(),
        "{} panics, first: {}",
        sweep.panics.len(),
        sweep.panics[0]
    );
    assert!(
        sweep.inconsistent.is_empty(),
        "{} inconsistent picks, first: {}",
        sweep.inconsistent.len(),
        sweep.inconsistent[0]
    );
}

/// The file's own numbers at the ends of the `i64` as well: pts at `i64::MIN` / `i64::MAX`, the container's start time, the last
/// duration. Not numbers FFmpeg can be compared at (its own `int64_t` arithmetic overflows there), so the pick is asked only
/// to be a frame of the file or none, the same however it is asked, and no panic.
#[test]
#[ignore = "fails today: frame_pick.rs still overflows its i64 sums at pts, container start and last duration at the ends; remove this when it saturates them"]
fn checker_a_frame_pick_with_timestamps_at_the_ends_of_the_integers_neither_panics_nor_wanders() {
    let sweep = hostile_sweep(true, false, 300_000);
    report("timestamps at the ends of the integers", &sweep);
    assert!(
        sweep.panics.is_empty(),
        "{} panics, first: {}",
        sweep.panics.len(),
        sweep.panics[0]
    );
    assert!(
        sweep.inconsistent.is_empty(),
        "{} inconsistent picks, first: {}",
        sweep.inconsistent.len(),
        sweep.inconsistent[0]
    );
}

/// The clip's in-point and the file's lead at the ends of the floats too (a source in-point of 9.2e12 seconds, 1e300): the
/// seek then saturates at the end of the `i64` and every sum with the container's start or a timestamp is past it.
#[test]
#[ignore = "fails today: frame_pick.rs still overflows seek_shift and the window's differences at a saturated seek; remove this when it saturates them"]
fn checker_a_frame_pick_with_an_in_point_at_the_ends_of_the_floats_neither_panics_nor_wanders() {
    let sweep = hostile_sweep(false, true, 300_000);
    report("in-point and lead at the ends", &sweep);
    assert!(
        sweep.panics.is_empty(),
        "{} panics, first: {}",
        sweep.panics.len(),
        sweep.panics[0]
    );
    assert!(
        sweep.inconsistent.is_empty(),
        "{} inconsistent picks, first: {}",
        sweep.inconsistent.len(),
        sweep.inconsistent[0]
    );
}

/// A file whose timestamps start a long way up the line (a day, a year, most of an `i64` of nanoseconds) and are spaced as any
/// other is picked as the same file at zero, for a window at the head (no seek) that takes them all: the chain re-bases on the
/// first frame, and arithmetic that is not exact in the ticks would show here as a frame off.
#[test]
fn checker_a_frame_picks_do_not_depend_on_where_the_timestamps_start() {
    let mut rng = Lcg(0x0000_7069_636b);
    let mut fails = Vec::new();
    for (num, den, step) in [
        (1u32, 1_000_000_000u32, 33_333_333i64),
        (1, 90_000, 3000),
        (1, 1000, 33),
        (1001, 30_000, 1),
        (1, 10_240, 1024),
    ] {
        let zero: Vec<i64> = (0..40).map(|i| i * step).collect();
        let tb = Rational { num, den };
        // The shifts that keep every frame inside what the window can say (its end is whole microseconds in an `i64`).
        let cap = (9.0e9 * f64::from(den) / f64::from(num)) as i64 / 2;
        for shift in [
            1_i64 << 20,
            86_400 * 1_000_000_000,
            1 << 50,
            i64::MAX / 4,
            i64::MAX - 40 * step - 1,
        ] {
            let shift = shift.min(cap);
            let up: Vec<i64> = zero.iter().map(|p| p + shift).collect();
            let covers = (shift as f64 + 41.0 * step as f64) * f64::from(num) / f64::from(den) * 1.01 + 10.0;
            for _ in 0..2000 {
                let (fnum, fden) = rng.pick(&[(30u32, 1u32), (24, 1), (60_000, 1001), (25, 1), (48, 1), (120, 1)]);
                let p = FpsPick {
                    speed: rng.pick(&[1.0, 0.5, 2.0, 1.25]),
                    reverse: rng.next().is_multiple_of(4),
                    window: (0.0, covers),
                    start: (rng.next() % 3000) as f64 / 1000.0,
                    end: f64::INFINITY,
                    frame: rng.next() % 120,
                    fps: Rational { num: fnum, den: fden },
                    drop_first: false,
                    lead: 0.0,
                    image: None,
                    pinned: rng.next().is_multiple_of(2),
                };
                let at = |pts: &[i64]| {
                    fps_pick(
                        &p,
                        &SourceFrames {
                            pts,
                            time_base: tb,
                            start_us: 0,
                            last_duration: step,
                        },
                    )
                };
                let (a, b) = (at(&zero), at(&up));
                if a != b {
                    fails.push(format!("1/{den} x{num} shift {shift}: {p:?}: at zero {a:?}, shifted {b:?}"));
                }
            }
        }
    }
    eprintln!("{} picks moved with the origin", fails.len());
    for f in fails.iter().take(8) {
        eprintln!("  {f}");
    }
    assert!(
        fails.is_empty(),
        "{} picks moved with the origin, first: {}",
        fails.len(),
        fails[0]
    );
}
