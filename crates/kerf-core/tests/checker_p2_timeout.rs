//! `checker/p2/timeout`: the helper every blur child runs through (`kerf_core::run_piped_until`: the blur probe's `Budget::ffmpeg` and
//! `tests/blur_slices.rs`' `run`, with a deadline of 45 s and 60 s from the moment of the call) kills a child that does not end, answers
//! `None` for it at the deadline, and leaves no process behind, whether the child sleeps, ignores its input or fills its output.
#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

/// Whether a process whose command line contains `marker` is running.
fn running(marker: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", marker])
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

#[test]
fn checker_p2_timeout_a_child_that_never_ends_is_killed_at_the_deadline() {
    // (`sleep` is the child itself: killing the child ends it.)
    for (name, marker) in [("sleeps", "1000.3141"), ("sleeps with a big input", "1000.2718")] {
        let mut cmd = Command::new("sleep");
        cmd.arg(marker);
        let input = if name.contains("input") {
            vec![7u8; 8 << 20]
        } else {
            Vec::new()
        };
        let started = Instant::now();
        let got = kerf_core::run_piped_until(&mut cmd, input, Instant::now() + Duration::from_millis(1200));
        let took = started.elapsed();
        assert!(got.is_none(), "{name}: a child past its deadline gave an answer");
        assert!(took < Duration::from_secs(4), "{name}: {took:?} for a 1.2 s deadline");
        assert!(!running(marker), "{name}: the child outlived the call");
    }
}

#[test]
fn checker_p2_timeout_a_child_that_ends_in_time_is_answered_and_one_that_fails_is_not() {
    let mut ok = Command::new("sh");
    ok.args(["-c", "cat"]);
    let echoed = kerf_core::run_piped_until(&mut ok, b"abc".to_vec(), Instant::now() + Duration::from_secs(20));
    assert_eq!(echoed.as_deref(), Some(&b"abc"[..]));
    let mut failing = Command::new("sh");
    failing.args(["-c", "cat; exit 3"]);
    assert!(kerf_core::run_piped_until(&mut failing, b"abc".to_vec(), Instant::now() + Duration::from_secs(20)).is_none());
}
