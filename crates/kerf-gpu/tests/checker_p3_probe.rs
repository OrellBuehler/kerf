//! `checker/p3/probe`: how the blur probe judges its children, as a black box. A wrapper stands in for `ffmpeg` (`KERF_FFMPEG`) and, for the
//! probe's `-vf gblur` children only, fails one, fails all, hangs one, or runs the real FFmpeg and moves its first luma byte by a level; a
//! fresh process then asks `blur_probe::blur()` and builds a compositor on it.
//!
//! * a difference found in any child that ran wins over a child that did not (a failing picture child before a wrong one, a failing shape
//!   child beside a wrong picture, a failing picture child beside a wrong shape), and over a child that hangs;
//! * a clean probe with one child missing, with every child failing, or with a child that outlasts its time is `Unknown`, never `Match`;
//! * a verdict that is not `Match` leaves the blur out of the compositor's caps, and the plan refuses a blurred frame, and drawing it
//!   anyway is refused: a probe that cannot run leaves the feature refused, not allowed.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_p3_probe -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
#![cfg(unix)]
#![allow(clippy::print_stderr)]

#[path = "common/checker_p3.rs"]
mod p3;

use std::time::Duration;

use kerf_core::{EffectKinds, ExportOptions, PlanRequest, Planner};
use p3::{blurred, compositor, noise_clip, one_track, rerun};

const CHILD: &str = "CHECKER_P3_PROBE_CHILD";

#[test]
#[ignore = "run by the parent below, in a process of its own"]
fn checker_p3_probe_child() {
    if std::env::var(CHILD).is_err() {
        return;
    }
    let verdict = kerf_gpu::blur_probe::blur();
    let word = match &verdict {
        kerf_gpu::blur_probe::Blur::Match => "Match",
        kerf_gpu::blur_probe::Blur::Differs(_) => "Differs",
        kerf_gpu::blur_probe::Blur::Unknown => "Unknown",
    };
    let comp = compositor(None);
    let caps = comp.caps();
    let asset = noise_clip(64, 64, "yuv420p");
    let tl = one_track(vec![blurred(&asset, 3.0, 0.3)]);
    let opts = ExportOptions::default();
    let plan = Planner::new(
        &tl,
        std::slice::from_ref(&asset),
        &opts,
        PlanRequest::still(kerf_core::composite_color_policy()).with_media(&kerf_core::OriginalMedia),
    )
    .expect("planner")
    .at(0.1)
    .expect("plan");
    let size = plan.size(u32::MAX);
    let reasons = plan.reasons(&caps, size);
    let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
    let drawn = comp.composite_yuv(&plan, &frames, size);
    eprintln!(
        "CHECKER-P3 probe: verdict={word} detail={verdict:?} blur_in_caps={} plan_reasons={} draw_refused={} lanes={}",
        caps.effects.contains(EffectKinds::BLUR),
        reasons.len(),
        drawn.is_err(),
        kerf_gpu::blur_probe::measured_lanes()
    );
}

/// The wrapper: passes everything through but the probe's `-vf gblur` children, which behave as `P3_PROBE_MODE` says.
fn wrapper(dir: &std::path::Path, real: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("ffmpeg");
    let script = format!(
        r#"#!/bin/sh
real='{real}'
args=" $* "
has() {{ case "$args" in *"$1"*) return 0;; esac; return 1; }}
corrupt() {{
  "$real" "$@" | python3 -I -c 'import sys; d=bytearray(sys.stdin.buffer.read()); d[0]=(d[0]+1)&255; sys.stdout.buffer.write(d)'
  exit $?
}}
if has " -vf gblur"; then
  case "$P3_PROBE_MODE" in
    fail-all) exit 1;;
    fail-shape) if has " -s 40x6 " && has " -filter_threads 3 "; then exit 1; fi;;
    hang-shape) if has " -s 40x6 " && has " -filter_threads 3 "; then exec sleep 300; fi;;
    corrupt-pic+fail-shape)
      if has " -s 40x6 " && has " -filter_threads 3 "; then exit 1; fi
      if has " -s 37x23 "; then corrupt "$@"; fi;;
    corrupt-pic+fail-first)
      if has " -s 160x96 " && has "gblur=sigma=250 "; then exit 1; fi
      if has " -s 37x23 "; then corrupt "$@"; fi;;
    fail-first+corrupt-shape)
      if has " -s 160x96 " && has "gblur=sigma=250 "; then exit 1; fi
      if has " -s 40x40 "; then corrupt "$@"; fi;;
    corrupt-pic+hang-shape)
      if has " -s 40x6 " && has " -filter_threads 3 "; then exec sleep 300; fi
      if has " -s 37x23 "; then corrupt "$@"; fi;;
  esac
fi
exec "$real" "$@"
"#
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn have_python() -> bool {
    std::process::Command::new("python3")
        .args(["-I", "-c", "pass"])
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p3_probe_a_difference_wins_over_a_child_that_did_not_run_and_nothing_unmeasured_is_allowed() {
    let dir = p3::scratch("checker-p3").join(format!("probe-wrap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ffmpeg = wrapper(&dir, &kerf_core::ffmpeg_path());
    let python = have_python();
    if !python {
        eprintln!("CHECKER-P3 probe: no python3 on this host: the scenarios that change a child's output are not run");
    }
    // (mode, the verdict it must come to, whether it needs a child's output changed)
    let scenarios = [
        ("ok", "Match", false),
        ("fail-shape", "Unknown", false),
        ("fail-all", "Unknown", false),
        ("corrupt-pic+fail-shape", "Differs", true),
        ("corrupt-pic+fail-first", "Differs", true),
        ("fail-first+corrupt-shape", "Differs", true),
        ("hang-shape", "Unknown", false),
        ("corrupt-pic+hang-shape", "Differs", true),
    ];
    let mut failures = Vec::new();
    for (mode, want, needs_python) in scenarios {
        if needs_python && !python {
            continue;
        }
        let started = std::time::Instant::now();
        let (ok, text) = rerun(
            "checker_p3_probe_child",
            None,
            &[
                (CHILD, "1"),
                ("P3_PROBE_MODE", mode),
                ("KERF_FFMPEG", ffmpeg.to_str().unwrap()),
                ("KERF_CPU_PERCENT", "100"),
            ],
            Duration::from_secs(300),
        );
        let Some(line) = text.lines().find(|l| l.starts_with("CHECKER-P3 probe:")) else {
            failures.push(format!("{mode}: the child said nothing (ok {ok}):\n{text}"));
            continue;
        };
        eprintln!("{mode} ({:.0?}): {line}", started.elapsed());
        let get = |key: &str| {
            line.split(&format!("{key}="))
                .nth(1)
                .unwrap_or("?")
                .split(' ')
                .next()
                .unwrap_or("?")
                .to_string()
        };
        let verdict = get("verdict");
        if verdict != want {
            failures.push(format!("{mode}: the probe came to {verdict}, not {want}: {line}"));
        }
        let allowed = verdict == "Match";
        if !allowed && (get("blur_in_caps") != "false" || get("plan_reasons") == "0" || get("draw_refused") != "true") {
            failures.push(format!("{mode}: not a match, and the blur is allowed anyway: {line}"));
        }
        if allowed && (get("blur_in_caps") != "true" || get("plan_reasons") != "0" || get("draw_refused") != "false") {
            failures.push(format!("{mode}: a match, and the blur is refused: {line}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}
