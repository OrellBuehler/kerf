//! `checker/p/cold`: the display gate in a process that has measured nothing (the answers are cached once per process, so each scenario
//! runs in a child process of its own: the test re-runs itself by name with `CHECKER_P_COLD_CHILD` set, and the child is the body).
//!
//! * `pinned_and_unpinned`: a compositor whose display conversion is pinned (`with_display`) runs no probe in `render_plan` (the one-shot
//!   entry), whatever the pin says, and refuses the RGB it would hand out for anything but `Match`; an unpinned one asks the probe, once,
//!   and what `display_probe::offered()` (what `get_render_bar` reads) says afterwards is exactly whether the probe found a `Match`; the
//!   YUV planes are never refused by the gate.
//! * `prepare_timeline_measures`: nothing else has asked, and `prepare_timeline` (what `get_render_bar` calls first) leaves the display
//!   conversion measured, for a plain cut with nothing in it that needs another probe.
//! * `not_a_match`: under `KERF_FFMPEG=<wrapper that adds -cpuflags 0>` this FFmpeg's `yuv420p -> rgb24` is the C kernel and the probe says
//!   `Differs`: nothing is on offer, the caps the app builds for the render bar are `None` (the bar then has no GPU span, where the
//!   ungated caps would have one), `render_plan` is refused as RGB, and the YUV planes are not.
//! * `the_render_bar_reads_the_gate`: the app's `get_render_bar` (a Tauri command: no test reaches it) builds the caps behind
//!   `display_probe::offered()`; this holds the text of that wiring.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_p_cold -- --ignored --nocapture
//! ```
#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use kerf_core::render_bar::{render_bar, BarRequest, BarState};
use kerf_core::{Asset, Clip, ExportOptions, OriginalMedia, Project, RenderPlan, StreamKind, Timeline, Track};
use kerf_gpu::display_probe::{self, Display};
use kerf_gpu::scaler::{Kernels as Scaler, Model};
use kerf_gpu::{Compositor, Gpu, GpuError, GpuOptions};

#[path = "common/child.rs"]
mod child;

const CHILD: &str = "CHECKER_P_COLD_CHILD";
const REAL: &str = "CHECKER_P_REAL_FFMPEG";

/// Whether this process is the child that runs `scenario`. When it is not, runs the test `name` again as that child (env `extra` added)
/// and checks it passed: the caller returns.
fn is_child(name: &str, scenario: &str, extra: &[(&str, &str)]) -> bool {
    if std::env::var(CHILD).as_deref() == Ok(scenario) {
        return true;
    }
    let mut cmd = Command::new(std::env::current_exe().expect("the test binary"));
    cmd.args([name, "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .env(CHILD, scenario);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = child::output_within(cmd, Duration::from_secs(600)).unwrap_or_else(|e| panic!("{scenario}: the child {e}"));
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    for line in stderr.lines().chain(stdout.lines()).filter(|l| l.contains("CHECKER-P")) {
        eprintln!("{line}");
    }
    assert!(out.status.success(), "{scenario}: the child failed:\n{stdout}\n{stderr}");
    false
}

fn scratch() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-p-cold");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Makes `path` once, atomically: `make` writes a file of its own (named by process and thread) that is renamed into place, so that a
/// test racing this one never probes a half-written file (a second test saw `path` exist while the first was still writing it:
/// "EBML header parsing failed").
fn made_once(path: &Path, make: impl FnOnce(&Path)) {
    if path.exists() {
        return;
    }
    let name = path.file_name().expect("a file name").to_string_lossy().into_owned();
    let own = path.with_file_name(format!("tmp-{}-{:?}-{name}", std::process::id(), std::thread::current().id()));
    make(&own);
    std::fs::rename(&own, path).expect("move the finished file into place");
}

fn noise_clip(w: u32, h: u32) -> Asset {
    let path = scratch().join(format!("noise-{w}x{h}.mkv"));
    made_once(&path, |own| {
        let lavfi = format!(
            "nullsrc=size={w}x{h}:rate=30:duration=0.5,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'"
        );
        // (Made by the real FFmpeg, whatever KERF_FFMPEG is: a child that swaps it for a wrapper reads the clip the parent made.)
        let real = std::env::var(REAL).unwrap_or_else(|_| kerf_core::ffmpeg_path());
        let mut cmd = Command::new(real);
        cmd.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &lavfi,
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(own);
        let out = child::output_within(cmd, Duration::from_secs(120)).unwrap_or_else(|e| panic!("making the clip: {e}"));
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    });
    let mut asset = Project::probe_asset(&path).expect("probe");
    for stream in &mut asset.streams {
        stream.color_range = None;
    }
    asset
}

fn plain_cut(w: u32, h: u32) -> (Timeline, Asset) {
    let asset = noise_clip(w, h);
    let tl = Timeline {
        tracks: vec![Track {
            clips: vec![Clip::new(asset.id, 0.0, 0.4, 0.0)],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        ..Timeline::new()
    };
    (tl, asset)
}

fn compositor(gpu: &Arc<Gpu>) -> Compositor {
    Compositor::new(Arc::clone(gpu))
        .expect("a compositor")
        .with_scaler_kernels(Scaler::Match(Model::ASSUMED))
}

fn a_cold_process() {
    assert_eq!(
        display_probe::measured(),
        Display::Unknown,
        "a probe ran before the test asked"
    );
    assert!(
        !display_probe::offered(),
        "RGB is on offer before the conversion was measured"
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p_cold_a_pinned_display_runs_no_probe_and_an_unpinned_one_is_what_offered_says() {
    if !is_child(
        "checker_p_cold_a_pinned_display_runs_no_probe_and_an_unpinned_one_is_what_offered_says",
        "pinned_and_unpinned",
        &[],
    ) {
        return;
    }
    a_cold_process();
    let (tl, asset) = plain_cut(64, 36);
    let policy = kerf_core::composite_color_policy();
    let plan = RenderPlan::at(&tl, std::slice::from_ref(&asset), &ExportOptions::default(), 0.1, policy).expect("plan");
    let size = plan.size(u32::MAX);
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let comp = || compositor(&gpu);
    assert_eq!(
        display_probe::measured(),
        Display::Unknown,
        "building a compositor ran the display probe"
    );

    // Pinned: no probe, whatever the pin.
    for pin in [Display::Differs("rehearsed".into()), Display::Unknown] {
        let err = comp()
            .with_display(pin.clone())
            .render_plan(&plan, size)
            .expect_err("RGB handed out on a conversion that is not measured to be the model's");
        assert!(
            matches!(&err, GpuError::Unsupported(why) if why.contains("yuv420p -> rgb24")),
            "{pin:?}: {err}"
        );
        assert_eq!(
            display_probe::measured(),
            Display::Unknown,
            "render_plan ran the display probe although the display was pinned to {pin:?}"
        );
    }
    let (frame, _) = comp()
        .with_display(Display::Match)
        .render_plan(&plan, size)
        .expect("a pinned match draws");
    assert_eq!((frame.width, frame.height), size);
    assert_eq!(
        display_probe::measured(),
        Display::Unknown,
        "render_plan ran the display probe although the display was pinned to a match"
    );
    // The planes need no conversion: not refused by a pin that refuses RGB.
    let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
    comp()
        .with_display(Display::Differs("rehearsed".into()))
        .composite_yuv(&plan, &frames, size)
        .expect("the YUV planes are not refused");
    assert_eq!(
        display_probe::measured(),
        Display::Unknown,
        "composite_yuv ran the display probe"
    );
    // The entry that hands out RGB from frames is refused by the pin as well, and does not ask.
    let err = comp()
        .with_display(Display::Unknown)
        .composite(&plan, &frames, size)
        .expect_err("composite handed out RGB on an unmeasured conversion");
    assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
    assert_eq!(display_probe::measured(), Display::Unknown, "a pinned entry asked");

    // Unpinned: the one-shot entry asks, once, and what is on offer is what it found.
    let unpinned = comp().render_plan(&plan, size);
    let found = display_probe::measured();
    eprintln!(
        "CHECKER-P cold: display {found:?}, offered {}, unpinned render_plan ok: {}",
        display_probe::offered(),
        unpinned.is_ok()
    );
    assert_ne!(found, Display::Unknown, "the unpinned one-shot entry did not ask the probe");
    assert_eq!(display_probe::offered(), found == Display::Match);
    assert_eq!(unpinned.is_ok(), found == Display::Match, "{unpinned:?}");
    if found != Display::Match {
        assert!(matches!(unpinned, Err(GpuError::Unsupported(_))));
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_p_cold_prepare_timeline_leaves_the_display_conversion_measured() {
    if !is_child(
        "checker_p_cold_prepare_timeline_leaves_the_display_conversion_measured",
        "prepare_timeline_measures",
        &[],
    ) {
        return;
    }
    a_cold_process();
    let (tl, _asset) = plain_cut(64, 36);
    kerf_gpu::prepare_timeline(&tl);
    let found = display_probe::measured();
    eprintln!("CHECKER-P cold: after prepare_timeline, display {found:?}");
    assert_ne!(
        found,
        Display::Unknown,
        "prepare_timeline left the display conversion unmeasured: get_render_bar would find nothing on offer on its first call"
    );
    assert_eq!(display_probe::offered(), found == Display::Match);
    // Asked once: a second call is free, and says the same.
    let again = std::time::Instant::now();
    kerf_gpu::prepare_timeline(&tl);
    assert!(again.elapsed() < Duration::from_secs(2), "prepare_timeline measured again");
    assert_eq!(display_probe::measured(), found);
}

/// A shell script that runs the real FFmpeg with `-cpuflags 0` ahead of the rest.
#[cfg(unix)]
fn no_simd_wrapper() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let wrapper = scratch().join("ffmpeg-no-simd.sh");
    std::fs::write(&wrapper, format!("#!/bin/sh\nexec \"${REAL}\" -cpuflags 0 \"$@\"\n")).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    wrapper
}

#[cfg(unix)]
#[test]
#[ignore = "needs ffmpeg, a shell and a GPU adapter (lavapipe is enough)"]
fn checker_p_cold_a_conversion_that_is_not_the_models_offers_no_rgb_and_no_gpu_span() {
    let real = std::env::var(REAL).unwrap_or_else(|_| kerf_core::ffmpeg_path());
    let wrapper = no_simd_wrapper();
    // (The clip is made by the parent, with the real FFmpeg, before the child swaps it.)
    let _ = plain_cut(64, 36);
    if !is_child(
        "checker_p_cold_a_conversion_that_is_not_the_models_offers_no_rgb_and_no_gpu_span",
        "not_a_match",
        &[(REAL, &real), ("KERF_FFMPEG", wrapper.to_str().unwrap())],
    ) {
        return;
    }
    a_cold_process();
    let (tl, asset) = plain_cut(64, 36);
    let policy = kerf_core::composite_color_policy();
    kerf_gpu::prepare_timeline(&tl);
    let found = display_probe::measured();
    eprintln!("CHECKER-P cold: no-SIMD FFmpeg, display {found:?}");
    assert!(
        matches!(found, Display::Differs(_)),
        "an FFmpeg with no SIMD kernels converts yuv420p to RGB with the C tables, which are not the x86 kernel's: {found:?}"
    );
    assert!(!display_probe::offered());

    // The app's caps: none, so the render bar shows no GPU span; the caps that are not gated would show one (the control).
    let caps_of = || {
        Compositor::session_caps_for(
            kerf_gpu::grade::measured(),
            kerf_gpu::scaler::measured(),
            kerf_gpu::convert::measured(),
        )
    };
    let app_caps = display_probe::offered().then(caps_of);
    assert!(app_caps.is_none());
    let bar = |caps| {
        render_bar(
            &tl,
            std::slice::from_ref(&asset),
            &BarRequest {
                caps,
                width: 640,
                color: policy,
                media: &OriginalMedia,
                waits: Vec::new(),
                ask: None,
            },
        )
    };
    let spans = |bar: &kerf_core::render_bar::RenderBar| bar.spans.iter().map(|s| s.state).collect::<Vec<_>>();
    let shown = spans(&bar(app_caps));
    assert!(
        shown.iter().all(|s| *s != BarState::Gpu),
        "the render bar shows a GPU span on a machine whose RGB conversion is not the model's: {shown:?}"
    );
    let control = spans(&bar(Some(caps_of())));
    assert!(
        control.contains(&BarState::Gpu),
        "the control: the caps without the gate draw this cut on the GPU ({control:?}), so the gate is what keeps the span off the bar"
    );

    // The frames: RGB refused, the planes not.
    let plan = RenderPlan::at(&tl, std::slice::from_ref(&asset), &ExportOptions::default(), 0.1, policy).expect("plan");
    let size = plan.size(u32::MAX);
    let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
    let comp = compositor(&gpu);
    let err = comp
        .render_plan(&plan, size)
        .expect_err("RGB handed out on a conversion that is not the model's");
    assert!(
        matches!(&err, GpuError::Unsupported(why) if why.contains("yuv420p -> rgb24")),
        "{err}"
    );
    let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
    assert!(comp.composite(&plan, &frames, size).is_err());
    comp.composite_yuv(&plan, &frames, size)
        .expect("the YUV planes need no conversion and are not refused");
}

/// `get_render_bar` is a Tauri command, which no test reaches. Its caps are built behind the display gate: `offered()` is asked after
/// `prepare_timeline` and before `session_caps_for`, and nothing else builds the bar's caps in the app.
#[test]
fn checker_p_cold_the_render_bar_reads_the_gate() {
    let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("../kerf-app/src/lib.rs");
    let text = std::fs::read_to_string(&lib)
        .unwrap_or_else(|e| panic!("{}: {e}", lib.display()))
        .replace("\r\n", "\n");
    let at = text.find("async fn get_render_bar(").expect("get_render_bar");
    let body = &text[at..];
    let end = body.find("\n}\n").expect("the end of get_render_bar");
    let body = &body[..end];
    let prepare = body.find("prepare_timeline(").expect("get_render_bar prepares the cut");
    let gate = body
        .find("display_probe::offered()")
        .expect("get_render_bar reads the display gate");
    let caps = body.find("session_caps_for(").expect("get_render_bar builds caps");
    assert!(
        prepare < gate && gate < caps,
        "prepare_timeline, then the gate, then the caps"
    );
    assert_eq!(body.matches("session_caps_for(").count(), 1);
    let between = &body[gate..caps];
    assert!(between.contains(".then("), "the caps are behind the gate: {between}");
    assert!(body.contains(".flatten()"), "the gate's `None` is the bar's `None`");
}
