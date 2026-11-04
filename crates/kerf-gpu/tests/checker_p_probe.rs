//! `checker/p/probe`: what the blur probe feeds FFmpeg, and what it answers, on machines it was not written on.
//!
//! The probe runs once per process, so every configuration is a child process of this test binary (re-run by name with
//! `CHECKER_P_PROBE_CHILD` set) under `KERF_FFMPEG=<a spy>`: a shell script that runs the real FFmpeg (with the `-cpuflags` the
//! configuration asks for), and logs each run's arguments and exit status. The log is the evidence.
//!
//! * `checker_p_probe_never_feeds_ffmpeg_a_plane_one_sample_across`: at the slice threads of a one-thread budget (10%), two (50%), three
//!   (75%), the default (100%) and a single core (`taskset -c 0`), no picture the probe runs has a luma or chroma plane one sample wide or
//!   tall (`KernelHazard::OneSample`: the kernel's loops run once too many on it), and no run of it fails.
//! * `checker_p_probe_cannot_match_a_kernel_the_rule_does_not_model`: FFmpeg run with `-cpuflags -avx512` (the 8-lane kernel) is a match
//!   whose width is 8 (where the machine has AVX-512); with `-cpuflags -avx2-avx512` (SSE4's horizontal pass, the C vertical one) and
//!   `-cpuflags 0` (the C loops) it is not a match, and the width is not set.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_p_probe -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
#![cfg(unix)]
#![allow(clippy::print_stderr)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use kerf_core::gblur::{plane_hazard, KernelHazard};

#[path = "common/child.rs"]
mod child;

const CHILD: &str = "CHECKER_P_PROBE_CHILD";

fn scratch() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-p-probe");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The spy: the real FFmpeg (`CHECKER_P_REAL`) run with `$CHECKER_P_CPU` ahead of the arguments, each run logged to `$CHECKER_P_LOG` as its
/// arguments and its exit status. (Not an `exec`: the status has to be read.)
fn spy() -> PathBuf {
    let path = scratch().join("ffmpeg-spy.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\n\"$CHECKER_P_REAL\" $CHECKER_P_CPU \"$@\"\nrc=$?\nprintf '%s rc=%s\\n' \"$*\" \"$rc\" >> \"$CHECKER_P_LOG\"\nexit $rc\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// What the child found, and every run of FFmpeg it made.
struct Found {
    verdict: String,
    lanes: u8,
    runs: Vec<Run>,
}

/// One logged run of the blur probe: the picture's size, the slice threads it was given, whether it succeeded.
#[derive(Debug)]
struct Run {
    size: (u32, u32),
    threads: u32,
    ok: bool,
    line: String,
}

/// The probe in a process of its own: under `percent` of the cores (`KERF_CPU_PERCENT`), on `cores` cores (`taskset`), over `cpu` flags.
fn probe_in_a_child(tag: &str, percent: u8, cores: Option<&str>, cpu: &str) -> Found {
    let real = kerf_core::ffmpeg_path();
    let log = scratch().join(format!("log-{tag}.txt"));
    let _ = std::fs::remove_file(&log);
    let exe = std::env::current_exe().expect("the test binary");
    let mut cmd = match cores {
        Some(set) => {
            let mut c = Command::new("taskset");
            c.args(["-c", set]).arg(&exe);
            c
        }
        None => Command::new(&exe),
    };
    cmd.args([
        "checker_p_probe_the_child",
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD, "1")
    .env("KERF_CPU_PERCENT", percent.to_string())
    .env("KERF_FFMPEG", spy())
    .env("CHECKER_P_REAL", real)
    .env("CHECKER_P_CPU", cpu)
    .env("CHECKER_P_LOG", &log);
    let out = child::output_within(cmd, Duration::from_secs(300)).unwrap_or_else(|e| panic!("{tag}: the child {e}"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{tag}: the child failed:\n{stdout}\n{stderr}");
    let said = stderr
        .lines()
        .chain(stdout.lines())
        .find_map(|l| l.split_once("CHECKER-P probe child: ").map(|(_, rest)| rest.to_string()))
        .unwrap_or_else(|| panic!("{tag}: the child said nothing:\n{stdout}\n{stderr}"));
    let (verdict, lanes) = said.rsplit_once(" lanes ").expect("the child's line");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let runs = text.lines().filter(|l| l.contains("gblur=sigma=")).map(parse_run).collect();
    Found {
        verdict: verdict.to_string(),
        lanes: lanes.trim().parse().expect("lanes"),
        runs,
    }
}

fn parse_run(line: &str) -> Run {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let value = |flag: &str| {
        tokens
            .iter()
            .rposition(|t| *t == flag)
            .and_then(|at| tokens.get(at + 1))
            .copied()
    };
    let (w, h) = value("-s").and_then(|s| s.split_once('x')).expect("-s WxH");
    Run {
        size: (w.parse().expect("w"), h.parse().expect("h")),
        // (The probe's own flags come after the budget's and win.)
        threads: value("-filter_threads").map_or(0, |t| t.parse().expect("threads")),
        ok: tokens.last() == Some(&"rc=0"),
        line: line.to_string(),
    }
}

/// The planes of a picture the probe fed: luma, and the half-size chroma (rounded up).
fn planes((w, h): (u32, u32)) -> [(u32, u32); 2] {
    [(w, h), (w.div_ceil(2), h.div_ceil(2))]
}

/// The child: run the probe and say what it found. Does nothing in the normal run.
#[test]
#[ignore = "run by the parents below, in a process of its own"]
fn checker_p_probe_the_child() {
    if std::env::var(CHILD).is_err() {
        return;
    }
    let verdict = kerf_gpu::blur_probe::blur();
    let lanes = kerf_gpu::blur_probe::measured_lanes();
    let shown = match &verdict {
        kerf_gpu::blur_probe::Blur::Match => "Match".to_string(),
        kerf_gpu::blur_probe::Blur::Unknown => "Unknown".to_string(),
        kerf_gpu::blur_probe::Blur::Differs(_) => "Differs".to_string(),
    };
    eprintln!("CHECKER-P probe child: {shown} lanes {lanes}");
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_p_probe_never_feeds_ffmpeg_a_plane_one_sample_across() {
    let mut failures = Vec::new();
    let mut fed_hazards = std::collections::BTreeMap::<String, usize>::new();
    // (10%: one slice thread; 50% of 4 cores: two; 75%: three; 100%: FFmpeg's own default; a single core: one, with the default's
    // ceiling above it.)
    for (tag, percent, cores) in [
        ("p10", 10, None),
        ("p50", 50, None),
        ("p75", 75, None),
        ("p100", 100, None),
        ("one-core", 75, Some("0")),
    ] {
        let found = probe_in_a_child(tag, percent, cores, "");
        eprintln!(
            "CHECKER-P probe {tag}: {} lanes {}, {} gblur runs",
            found.verdict,
            found.lanes,
            found.runs.len()
        );
        assert!(!found.runs.is_empty(), "{tag}: the probe ran no gblur ({})", found.verdict);
        for run in &found.runs {
            if !run.ok {
                failures.push(format!("{tag}: a probe run failed: {}", run.line));
            }
            for (pw, ph) in planes(run.size) {
                if let Some(kind) = [8, 16].into_iter().find_map(|l| plane_hazard((pw, ph), run.threads, l)) {
                    *fed_hazards.entry(format!("{kind:?}")).or_default() += 1;
                    if kind == KernelHazard::OneSample {
                        failures.push(format!(
                            "{tag}: the probe fed gblur a plane {pw}x{ph} (picture {}x{}) on {} slice thread(s): {}",
                            run.size.0, run.size.1, run.threads, run.line
                        ));
                    }
                }
            }
        }
    }
    eprintln!("CHECKER-P probe: hazards fed, by kind {fed_hazards:?}");
    failures.sort();
    failures.dedup();
    assert!(
        failures.is_empty(),
        "{} problems, first:\n{}",
        failures.len(),
        failures.iter().take(8).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_p_probe_cannot_match_a_kernel_the_rule_does_not_model() {
    let own = probe_in_a_child("own", 75, None, "");
    eprintln!("CHECKER-P probe own: {} lanes {}", own.verdict, own.lanes);
    assert_eq!(own.verdict, "Match", "the control: this FFmpeg's own kernel is the model's");
    assert!(matches!(own.lanes, 8 | 16), "the width of the kernel is found: {}", own.lanes);
    if own.lanes == 16 {
        let avx2 = probe_in_a_child("avx2", 75, None, "-cpuflags -avx512");
        eprintln!("CHECKER-P probe avx2: {} lanes {}", avx2.verdict, avx2.lanes);
        assert_eq!(
            (avx2.verdict.as_str(), avx2.lanes),
            ("Match", 8),
            "AVX2 on a machine that also has AVX-512 is the 8-lane kernel"
        );
    }
    // The width is found at every budget's shapes, not only at the default's.
    if own.lanes == 16 {
        for (tag, percent, cores) in [("p10", 10, None), ("p50", 50, None), ("one-core", 75, Some("0"))] {
            let wide = probe_in_a_child(&format!("own-{tag}"), percent, cores, "");
            let narrow = probe_in_a_child(&format!("avx2-{tag}"), percent, cores, "-cpuflags -avx512");
            eprintln!(
                "CHECKER-P probe {tag}: own {} lanes {}, avx2 {} lanes {}",
                wide.verdict, wide.lanes, narrow.verdict, narrow.lanes
            );
            assert_eq!((wide.verdict.as_str(), wide.lanes), ("Match", 16), "{tag}");
            assert_eq!((narrow.verdict.as_str(), narrow.lanes), ("Match", 8), "{tag}");
        }
    }
    for (tag, cpu) in [
        ("sse4", "-cpuflags -avx2-avx512"),
        ("scalar", "-cpuflags 0"),
        ("sse2", "-cpuflags -ssse3-avx2-avx512"),
    ] {
        let found = probe_in_a_child(tag, 75, None, cpu);
        eprintln!("CHECKER-P probe {tag}: {} lanes {}", found.verdict, found.lanes);
        assert_eq!(
            found.verdict, "Differs",
            "{tag} ({cpu}): a kernel the rule does not model is no match"
        );
        assert_eq!(found.lanes, 0, "{tag}: no width is recorded for it");
    }
}
