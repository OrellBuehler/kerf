//! `checker/mx/probe`: `convert::scaled()` (the probe that gates the cascade of a `scale` that resizes as it converts the matrix) comes out
//! `Differs` or `Unknown`, never `Match`, when its premises break, and costs a bounded time when ffmpeg does not answer.
//!
//! The premises are broken by a wrapper script in place of ffmpeg (`KERF_FFMPEG`): the real build run with another CPU
//! (`-cpuflags`: no SIMD, no MMX output functions), with another scaler (`bilinear`), with the accurate-rounding or bit-exact flags (which
//! select other output functions and take the unscaled `bgr24 -> yuv420p` converter away), or with a child that stalls or fails. Each
//! scenario runs in a child process of this test binary, since a probe's verdict is kept for the process.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_mx_probe -- --ignored --nocapture
//! ```
//!
//! Needs `ffmpeg` and a shell; no adapter.

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kerf_gpu::convert::{Kernels, Scaled};

const CHILD: &str = "checker_mx_probe_child";

/// The child: with `KERF_MX_PROBE` set, runs both probes and prints their verdicts and the time they took on one line; else does nothing.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_mx_probe_child() {
    if std::env::var_os("KERF_MX_PROBE").is_none() {
        return;
    }
    let started = Instant::now();
    let kernels = kerf_gpu::convert::kernels();
    let scaled = kerf_gpu::convert::scaled();
    let measured = kerf_gpu::convert::scaled_measured();
    eprintln!(
        "MXPROBE kernels={kernels:?} scaled={scaled:?} measured={measured:?} secs={:.1}",
        started.elapsed().as_secs_f64()
    );
}

struct Verdict {
    kernels: String,
    scaled: String,
    secs: f64,
}

fn run_child(name: &str, wrapper: &std::path::Path) -> Verdict {
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args(["--ignored", "--exact", CHILD, "--nocapture", "--test-threads=1"])
        .env("KERF_MX_PROBE", name)
        .env("KERF_FFMPEG", wrapper)
        .stdin(Stdio::null())
        .output()
        .expect("the child");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let line = text
        .lines()
        .find(|l| l.starts_with("MXPROBE "))
        .unwrap_or_else(|| panic!("{name}: the child printed no verdict:\n{text}"));
    let field = |key: &str| {
        let at = line.find(key).unwrap() + key.len();
        let rest = &line[at..];
        let end = if rest.starts_with("Differs(") {
            rest.find(')').unwrap() + 1
        } else {
            rest.find(' ').unwrap_or(rest.len())
        };
        rest[..end].to_string()
    };
    let secs: f64 = line.rsplit("secs=").next().unwrap().trim().parse().unwrap();
    eprintln!("{name}: {line}");
    Verdict {
        kernels: field("kernels="),
        scaled: field("scaled="),
        secs,
    }
}

fn wrapper(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let real = kerf_core::ffmpeg_path();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\nREAL='{real}'\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_mx_the_cascade_probe_is_not_match_where_its_premises_break() {
    if kerf_core::composite_color_policy() != kerf_core::CompositeColorPolicy::BottomLayerTag {
        eprintln!("{}: policy is not BottomLayerTag, nothing to probe", kerf_core::ffmpeg_path());
        return;
    }
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-mx-probe");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A rewrite of every `scale=...` in an argument: the inline sed keeps the rest.
    let rewrite = |from: &str, to: &str| {
        format!(
            "for a in \"$@\"; do shift; case \"$a\" in *'{from}'*) a=$(printf '%s' \"$a\" | sed 's/{from}/{to}/g');; esac; set -- \"$@\" \"$a\"; done\nexec \"$REAL\" \"$@\""
        )
    };
    let x86 = cfg!(target_arch = "x86_64") || cfg!(target_arch = "x86");

    // The control: the real build, passed through.
    let control = run_child("control", &wrapper(&dir, "ffmpeg-control", "exec \"$REAL\" \"$@\""));
    assert_eq!(control.kernels, "Match", "the control's conversion");
    assert_eq!(control.scaled, "Match", "the control's cascade");

    let mut broken: Vec<(&str, Verdict)> = vec![(
        "bilinear",
        run_child(
            "bilinear",
            &wrapper(
                &dir,
                "ffmpeg-bilinear",
                &rewrite(
                    "force_original_aspect_ratio=decrease",
                    "force_original_aspect_ratio=decrease:flags=bilinear",
                ),
            ),
        ),
    )];
    // The accurate-rounding flag: other output functions, and no `bgr24ToYv12Wrapper`.
    broken.push((
        "accurate_rnd",
        run_child(
            "accurate_rnd",
            &wrapper(
                &dir,
                "ffmpeg-accurate",
                &rewrite(
                    "force_original_aspect_ratio=decrease",
                    "force_original_aspect_ratio=decrease:flags=accurate_rnd",
                ),
            ),
        ),
    ));
    // Bit-exact: the C output functions on every row (a build without the inline MMX).
    broken.push((
        "bitexact",
        run_child(
            "bitexact",
            &wrapper(
                &dir,
                "ffmpeg-bitexact",
                &rewrite(
                    "force_original_aspect_ratio=decrease",
                    "force_original_aspect_ratio=decrease:flags=bitexact",
                ),
            ),
        ),
    ));
    // Full chroma interpolation: another chroma path.
    broken.push((
        "full_chroma_int",
        run_child(
            "full_chroma_int",
            &wrapper(
                &dir,
                "ffmpeg-fullchroma",
                &rewrite(
                    "force_original_aspect_ratio=decrease",
                    "force_original_aspect_ratio=decrease:flags=full_chroma_int",
                ),
            ),
        ),
    ));
    if x86 {
        broken.push((
            "cpuflags 0",
            run_child(
                "cpuflags0",
                &wrapper(&dir, "ffmpeg-cpu0", "exec \"$REAL\" -cpuflags 0 \"$@\""),
            ),
        ));
        broken.push((
            "no mmxext",
            run_child(
                "nommx",
                &wrapper(&dir, "ffmpeg-nommx", "exec \"$REAL\" -cpuflags -mmxext \"$@\""),
            ),
        ));
        // (A CPU flagged for slow AVX2 gathers, as FFmpeg flags some real ones: the unscaled kernel is another one, `Differs` in the
        // conversion's own probe, and so is the cascade: such a machine draws no mixed-matrix cut, safely.)
        broken.push((
            "slow gather",
            run_child(
                "slowgather",
                &wrapper(&dir, "ffmpeg-slowgather", "exec \"$REAL\" -cpuflags slowgather \"$@\""),
            ),
        ));
        broken.push((
            "sse2 only",
            run_child("sse2", &wrapper(&dir, "ffmpeg-sse2", "exec \"$REAL\" -cpuflags sse2 \"$@\"")),
        ));
    }
    // CPUs whose wider SIMD is switched off: the same arithmetic in the horizontal scaler (identical sums), so the verdict must stay
    // `Match` (a probe that said `Differs` here would refuse the layers on every machine without AVX2 for nothing).
    if x86 {
        for (name, flags) in [("no avx2", "-avx2"), ("no avx512", "-avx512"), ("no bmi2", "-bmi2")] {
            let v = run_child(
                name,
                &wrapper(&dir, "ffmpeg-simd", &format!("exec \"$REAL\" -cpuflags {flags} \"$@\"")),
            );
            assert_eq!(
                (v.kernels.as_str(), v.scaled.as_str()),
                ("Match", "Match"),
                "{name}: the same arithmetic on another SIMD"
            );
        }
    }
    for (name, v) in &broken {
        assert_ne!(
            v.scaled, "Match",
            "{name}: the probe says the model holds for a build whose premise is broken: {} {}",
            v.kernels, v.scaled
        );
    }
    let _ = (Kernels::Match, Scaled::Match);
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_mx_a_child_that_stalls_or_fails_in_the_cascade_probe_is_unknown_within_its_deadline() {
    if kerf_core::composite_color_policy() != kerf_core::CompositeColorPolicy::BottomLayerTag {
        eprintln!("{}: policy is not BottomLayerTag, nothing to probe", kerf_core::ffmpeg_path());
        return;
    }
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-mx-probe-stall");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Every child that names `yuva420p` at the end of a fit (the scaled probe's graphs) stalls; the conversion's own probes pass through.
    let stall = wrapper(
        &dir,
        "ffmpeg-stalls",
        "case \"$*\" in *'pad=64:48:'*) sleep 120 & wait;; esac\nexec \"$REAL\" \"$@\"",
    );
    let stalled = run_child("stalls", &stall);
    assert_eq!(stalled.kernels, "Match", "the conversion's probe is not the one that stalls");
    assert_eq!(stalled.scaled, "Unknown", "a child that does not answer is not measured");
    assert!(stalled.secs < 70.0, "the probe returned after {} s", stalled.secs);
    eprintln!("a stalled cascade probe returned Unknown after {:.1} s", stalled.secs);
    // A child that exits with an error.
    let fails = wrapper(
        &dir,
        "ffmpeg-fails",
        "case \"$*\" in *'pad=64:48:'*) echo boom >&2; exit 1;; esac\nexec \"$REAL\" \"$@\"",
    );
    let failed = run_child("fails", &fails);
    assert_eq!(failed.scaled, "Unknown", "a child that fails is not measured");
    // A child that answers with the wrong bytes: a frame of zeros.
    let zeros = wrapper(
        &dir,
        "ffmpeg-zeros",
        "case \"$*\" in *'pad=64:48:'*) head -c 9216 /dev/zero; exit 0;; esac\nexec \"$REAL\" \"$@\"",
    );
    let wrong = run_child("zeros", &zeros);
    assert!(
        wrong.scaled.starts_with("Differs") || wrong.scaled == "Unknown",
        "wrong bytes are not a match: {}",
        wrong.scaled
    );
    // The machine's whole startup cost with an ffmpeg that never answers: every probe child is bounded.
    let dead = wrapper(&dir, "ffmpeg-dead", "sleep 120 & wait");
    let started = Instant::now();
    let dead_verdict = run_child("dead", &dead);
    assert_eq!(dead_verdict.scaled, "Unknown");
    assert_eq!(dead_verdict.kernels, "Unknown");
    assert!(started.elapsed() < Duration::from_secs(90), "{:?}", started.elapsed());
    eprintln!(
        "an ffmpeg that never answers: both probes Unknown after {:.1} s",
        dead_verdict.secs
    );
}

/// A build that never cascades (FFmpeg 6.1.1: one fixed matrix for the composite, no matrix handed to `scale`): both probes come out
/// `Differs` without a run of the scaled graphs, quickly, and the cascade is not in the caps. The build under test is the one the
/// harness runs (`KERF_FFMPEG`, else the system's); where it negotiates the matrix there is no such build in hand and the case is skipped.
#[test]
#[ignore = "needs ffmpeg and a shell; the build under test must not negotiate the composite's matrix (FFmpeg 6.1.1)"]
fn checker_mx_a_build_that_never_cascades_is_differs_in_both_probes() {
    if kerf_core::composite_color_policy() != kerf_core::CompositeColorPolicy::FixedBt601 {
        eprintln!(
            "{}: policy {:?}, not a build that never cascades: skipped",
            kerf_core::ffmpeg_path(),
            kerf_core::composite_color_policy()
        );
        return;
    }
    let path = std::path::PathBuf::from(kerf_core::ffmpeg_path());
    let version = Command::new(&path)
        .arg("-version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let v = run_child("fixed-matrix ffmpeg", &path);
    eprintln!(
        "{}: kernels {} scaled {} after {:.1} s",
        version.lines().next().unwrap_or(""),
        v.kernels,
        v.scaled,
        v.secs
    );
    assert_ne!(v.scaled, "Match", "{version}");
    assert!(v.secs < 30.0, "{} s", v.secs);
}
