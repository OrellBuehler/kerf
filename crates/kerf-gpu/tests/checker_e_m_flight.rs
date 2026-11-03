//! `checker/e/m`: callers of the blend probe that arrive while a measurement is stuck. The measurement is single-flight on a condition variable
//! and a waiter is bounded by the measurement's own bound plus a grace: with every child of the probe stalled, a crowd of callers all come back
//! with "nothing measured" when the one leader's deadline kills the children, none of them starts a child of its own, and a caller after them
//! is held to the backoff (no child, no wait). Needs `ffmpeg` (`KERF_FFMPEG`) and a shell; no adapter. `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_e_m_flight -- --ignored --nocapture
//! ```
//!
//! The probe's state is per process, so each scenario runs in a child process of this test (`checker_e_m_flight_child`, selected by
//! `CHECKER_E_M_MODE`), with `KERF_FFMPEG` pointing at a wrapper that logs every start and never answers.

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use kerf_core::BlendKinds;
use kerf_gpu::blend;

/// The scenario this process is a child of (a plain `--ignored` run of the file finds none and passes).
#[test]
#[ignore = "run by the scenario of this file, in a process of its own"]
fn checker_e_m_flight_child() {
    let Ok(mode) = std::env::var("CHECKER_E_M_MODE") else {
        return;
    };
    let callers: usize = std::env::var("CHECKER_E_M_CALLERS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(1);
    let barrier = std::sync::Barrier::new(callers);
    let answers: Vec<(Duration, [BlendKinds; 2])> = std::thread::scope(|scope| {
        let asks: Vec<_> = (0..callers)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    let at = Instant::now();
                    let kinds = blend::kernels();
                    (at.elapsed(), kinds)
                })
            })
            .collect();
        asks.into_iter().map(|a| a.join().expect("a caller panicked")).collect()
    });
    let slowest = answers.iter().map(|a| a.0).max().unwrap();
    let fastest = answers.iter().map(|a| a.0).min().unwrap();
    eprintln!("CHILD {mode}: {callers} callers, fastest {fastest:?}, slowest {slowest:?}");
    assert!(
        answers.iter().all(|a| a.1 == [BlendKinds::NONE; 2]),
        "every child stalled: nothing is measured, nothing is drawn: {answers:?}"
    );
    assert!(
        slowest < Duration::from_secs(27),
        "no caller waits past the bound and its grace: {slowest:?}"
    );
    assert!(
        fastest >= Duration::from_secs(15),
        "a caller returns when the measurement ends, not before it started: {fastest:?}"
    );
    assert_eq!(
        blend::measured(),
        [BlendKinds::NONE; 2],
        "and the stalled verdict is not kept"
    );
    let again = Instant::now();
    assert_eq!(blend::kernels(), [BlendKinds::NONE; 2]);
    assert!(
        again.elapsed() < Duration::from_secs(2),
        "inside the backoff a caller is not made to wait or to measure: {:?}",
        again.elapsed()
    );
}

struct Run {
    stderr: String,
    starts: usize,
    took: Duration,
    success: bool,
}

fn wrapper(dir: &Path) -> PathBuf {
    let path = dir.join("ffmpeg-that-never-answers");
    std::fs::write(
        &path,
        "#!/bin/sh\nprintf 'S %s\\n' \"$(date +%s%N)\" >> \"$CHECKER_E_M_LOG\"\nexec sleep 90\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn child(dir: &Path, wrapper: &Path, mode: &str, callers: usize) -> Run {
    let log = dir.join(format!("log-{mode}-{callers}.txt"));
    let _ = std::fs::remove_file(&log);
    let started = Instant::now();
    let out = Command::new(std::env::current_exe().expect("this test's executable"))
        .args([
            "--ignored",
            "--exact",
            "checker_e_m_flight_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("KERF_FFMPEG", wrapper)
        .env("CHECKER_E_M_LOG", &log)
        .env("CHECKER_E_M_MODE", mode)
        .env("CHECKER_E_M_CALLERS", callers.to_string())
        .output()
        .expect("run the child");
    Run {
        stderr: String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
        starts: std::fs::read_to_string(&log).unwrap_or_default().lines().count(),
        took: started.elapsed(),
        success: out.status.success(),
    }
}

/// With every child stalled, a lone caller and twelve that ask together start the same children (the pool's first six, then the deadline kills
/// them), the twelve all come back with nothing at that deadline and not after the bound plus its grace, and the next caller is held to the
/// backoff.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_e_m_callers_over_a_stalled_measurement_come_back_with_the_leader_and_start_nothing() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-e-m-flight");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wrapper = wrapper(&dir);
    let lone = child(&dir, &wrapper, "lone", 1);
    eprintln!("lone: {} children, {:?}\n{}", lone.starts, lone.took, lone.stderr);
    assert!(lone.success, "the lone caller's process failed:\n{}", lone.stderr);
    let crowd = child(&dir, &wrapper, "crowd", 12);
    eprintln!("crowd: {} children, {:?}\n{}", crowd.starts, crowd.took, crowd.stderr);
    assert!(crowd.success, "the crowd's process failed:\n{}", crowd.stderr);
    assert!(
        lone.starts >= 1 && lone.starts <= 6,
        "the pool's first jobs only: {}",
        lone.starts
    );
    assert_eq!(
        crowd.starts, lone.starts,
        "twelve callers share the one measurement, stalled or not"
    );
    assert!(
        crowd.took < lone.took + Duration::from_secs(8),
        "the crowd waits for the one leader's deadline: {:?} against {:?}",
        crowd.took,
        lone.took
    );
}
