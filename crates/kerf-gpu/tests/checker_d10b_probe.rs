//! `checker_d10b_probe`: `delivery10::ten_bit()` (the probe that gates the 10-bit deliveries) says `Unknown` -- not `Differs`, never `Match` -- when it
//! could not tell (a child that stalls, fails, is cut short, or a machine too slow for its 120 s bound), `Differs` for a build whose planes are not the
//! model's, and `Match` with the kinds the build has otherwise, under load and with another CPU's flags. Each scenario runs in a child process of this
//! test binary, since a probe's verdict is kept for the process; the premises are bent by a wrapper script in place of ffmpeg (`KERF_FFMPEG`).
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_d10b_probe -- --ignored --nocapture
//! ```
//!
//! Needs `ffmpeg` and a shell; no adapter.

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD: &str = "checker_d10b_probe_child";

/// The child: with `KERF_D10B_PROBE` set, asks the probe and prints its verdict, the kinds, the details and the time on one line.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_d10b_probe_child() {
    if std::env::var_os("KERF_D10B_PROBE").is_none() {
        return;
    }
    let started = Instant::now();
    let verdict = kerf_gpu::delivery10::ten_bit();
    let kinds = kerf_gpu::delivery10::measured_kinds();
    let short = |v: &str| v.chars().take(160).collect::<String>().replace('\n', " ");
    eprintln!(
        "D10BPROBE verdict={} kinds={:?} secs={:.1}",
        short(&format!("{verdict:?}")).replace(' ', "_"),
        kinds,
        started.elapsed().as_secs_f64()
    );
}

struct Found {
    verdict: String,
    kinds: u8,
    secs: f64,
}

fn run_child(name: &str, wrapper: &std::path::Path) -> Found {
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args(["--ignored", "--exact", CHILD, "--nocapture", "--test-threads=1"])
        .env("KERF_D10B_PROBE", name)
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
        .find(|l| l.starts_with("D10BPROBE "))
        .unwrap_or_else(|| panic!("{name}: the child printed no verdict:\n{text}"));
    eprintln!("{name}: {line}");
    let field = |key: &str| {
        let at = line.find(key).unwrap() + key.len();
        let rest = &line[at..];
        rest[..rest.find(' ').unwrap_or(rest.len())].to_string()
    };
    Found {
        verdict: field("verdict="),
        kinds: field("kinds=TenBitKinds(").trim_end_matches(')').parse().unwrap_or(255),
        secs: line.rsplit("secs=").next().unwrap().trim().parse().unwrap(),
    }
}

fn wrapper(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let real = kerf_core::ffmpeg_path();
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\nREAL='{real}'\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn scratch() -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-d10b-probe");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The probe says what it can and cannot tell.
#[test]
#[ignore = "needs ffmpeg and a shell; about four minutes"]
fn checker_d10b_the_probe_is_unknown_where_it_could_not_tell_and_differs_where_the_planes_are_not_the_models() {
    let dir = scratch();
    // The control: the real build passed through.
    let control = run_child("control", &wrapper(&dir, "ffmpeg-control", "exec \"$REAL\" \"$@\""));
    assert!(control.verdict.starts_with("Match"), "the control: {}", control.verdict);
    assert!(control.kinds & 3 == 3, "both layouts: {}", control.kinds);
    eprintln!(
        "control kinds {} (4 = restarts the dither at each slice), {:.1} s",
        control.kinds, control.secs
    );

    // A child that does not answer: killed at the probe's per-run bound; the verdict is Unknown, in a bounded time.
    let stalled = run_child(
        "stalled-export",
        &wrapper(
            &dir,
            "ffmpeg-stalled",
            "case \" $* \" in *' -filter_complex '*10le*) exec sleep 1000;; esac\nexec \"$REAL\" \"$@\"",
        ),
    );
    assert!(
        stalled.verdict.starts_with("Unknown"),
        "a stalled export: {}",
        stalled.verdict
    );
    assert_eq!(stalled.kinds, 0);
    assert!(stalled.secs < 130.0, "the stalled probe took {:.1} s", stalled.secs);

    // A child that fails.
    let failing = run_child(
        "failing-export",
        &wrapper(
            &dir,
            "ffmpeg-failing",
            "case \" $* \" in *' -filter_complex '*10le*) echo boom >&2; exit 1;; esac\nexec \"$REAL\" \"$@\"",
        ),
    );
    assert!(
        failing.verdict.starts_with("Unknown"),
        "a failing export: {}",
        failing.verdict
    );
    assert_eq!(failing.kinds, 0);

    // A child cut short: the planes are fewer than the toy has frames; that is not a difference of the build.
    let cut = run_child(
        "truncated-export",
        &wrapper(
            &dir,
            "ffmpeg-truncated",
            "case \" $* \" in *' -filter_complex '*10le*) \"$REAL\" \"$@\" | head -c 5000; exit 0;; esac\nexec \"$REAL\" \"$@\"",
        ),
    );
    assert!(cut.verdict.starts_with("Unknown"), "a cut-short export: {}", cut.verdict);
    assert_eq!(cut.kinds, 0);

    // The planes of a build whose conversion is not the model's: a byte of the export's output changed. Differs, and no layout drawn.
    let bent = run_child(
        "changed-export",
        &wrapper(
            &dir,
            "ffmpeg-changed",
            "case \" $* \" in *' -filter_complex '*10le*) \"$REAL\" \"$@\" | tr '\\100' '\\101'; exit 0;; esac\nexec \"$REAL\" \"$@\"",
        ),
    );
    assert!(bent.verdict.starts_with("Differs"), "a changed export: {}", bent.verdict);
    assert_eq!(bent.kinds, 0);
}

/// A slow machine: each child of the 10-bit probe takes four seconds longer than it does, so forty-eight of them cannot be done in the probe's 120 s. It says `Unknown` at its
/// bound (not `Differs`, not `Match` from a half-run), and does not run on.
#[test]
#[ignore = "needs ffmpeg and a shell; about two and a half minutes"]
fn checker_d10b_a_machine_too_slow_for_the_probe_is_unknown_at_its_bound() {
    let dir = scratch();
    let slow = run_child(
        "slow-machine",
        &wrapper(
            &dir,
            "ffmpeg-slow",
            "case \" $* \" in *' -filter_complex '*10le*) sleep 4;; esac\nexec \"$REAL\" \"$@\"",
        ),
    );
    assert!(slow.verdict.starts_with("Unknown"), "a slow machine: {}", slow.verdict);
    assert_eq!(slow.kinds, 0);
    assert!(slow.secs > 100.0 && slow.secs < 150.0, "took {:.1} s", slow.secs);
}

/// A loaded machine (every core busy at the probe's priority): the probe still answers, with the control's kinds, in the time it has.
#[test]
#[ignore = "needs ffmpeg and a shell; about two minutes"]
fn checker_d10b_a_loaded_machine_still_gets_the_builds_answer() {
    let dir = scratch();
    let control = run_child("control-idle", &wrapper(&dir, "ffmpeg-control", "exec \"$REAL\" \"$@\""));
    let cores = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let mut burners: Vec<std::process::Child> = (0..cores * 2)
        .map(|_| {
            Command::new("sh")
                .args(["-c", "while :; do :; done"])
                .stdin(Stdio::null())
                .spawn()
                .expect("a burner")
        })
        .collect();
    let started = Instant::now();
    let loaded = run_child("control-loaded", &wrapper(&dir, "ffmpeg-control", "exec \"$REAL\" \"$@\""));
    for b in &mut burners {
        let _ = b.kill();
        let _ = b.wait();
    }
    eprintln!(
        "idle {:.1} s, loaded {:.1} s ({:?} wall)",
        control.secs,
        loaded.secs,
        started.elapsed()
    );
    assert!(
        loaded.verdict.starts_with("Match") || loaded.verdict.starts_with("Unknown"),
        "a loaded machine: {}",
        loaded.verdict
    );
    if loaded.verdict.starts_with("Match") {
        assert_eq!(loaded.kinds, control.kinds, "the loaded machine's kinds");
    }
    assert!(!loaded.verdict.starts_with("Differs"));
    let _ = Duration::from_secs(1);
}

/// Another CPU's flags (no SIMD, no AVX2): the build is the same binary, so the probe has to find the model true or false itself. Whatever it says is
/// then held to the export by the parity cases run under the same wrapper (`KERF_FFMPEG`), which is the other half of this check.
#[test]
#[ignore = "needs ffmpeg and a shell; about two minutes"]
fn checker_d10b_other_cpu_flags_are_asked_not_assumed() {
    let dir = scratch();
    let report = |name: &str, flags: &str| {
        let f = run_child(
            name,
            &wrapper(
                &dir,
                &format!("ffmpeg-{name}"),
                &format!("exec \"$REAL\" -cpuflags {flags} \"$@\""),
            ),
        );
        eprintln!(
            "{name} (-cpuflags {flags}): {} kinds {} in {:.1} s",
            f.verdict, f.kinds, f.secs
        );
        f
    };
    let none = report("cpuflags-0", "0");
    let no_avx2 = report("cpuflags-minus-avx2", "-avx2");
    let no_ssse3 = report("cpuflags-minus-ssse3", "-ssse3");
    for f in [&none, &no_avx2, &no_ssse3] {
        assert!(!f.verdict.starts_with("Unknown"), "{}", f.verdict);
    }
}
