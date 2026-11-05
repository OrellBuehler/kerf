//! `checker/p2/count`: the cores `kerf_core::ffmpeg_cpu_count()` says FFmpeg counts, against the one source nothing in Kerf computes: the
//! kernel's own account of the affinity (`/proc/self/status`, and `taskset` setting it from outside), with no FFmpeg in the way. (The
//! other half, that FFmpeg's graph really runs on the threads that count gives, is `kerf-gpu`'s `checker_p2_threads.rs`.)
//!
//! `av_cpu_count` in both 6.1 and 9.0 is `CPU_COUNT` of `sched_getaffinity(0)`, one when the call fails, and the plan collapses an
//! uncapped export's slice threads to `min(count + 1, 16)` (one on one cpu). The test binary is re-run under several `taskset -c` masks
//! at the uncapped budget, and each says what the library gives.
#![cfg(target_os = "linux")]
#![allow(clippy::print_stderr)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD: &str = "CHECKER_P2_COUNT_CHILD";

/// The cpus of a `Cpus_allowed_list` / `taskset -c` list: `0-3,8,10-11`.
fn cpus_in(list: &str) -> usize {
    list.split(',')
        .filter(|p| !p.trim().is_empty())
        .map(|part| match part.trim().split_once('-') {
            Some((a, b)) => b.parse::<usize>().unwrap() - a.parse::<usize>().unwrap() + 1,
            None => 1,
        })
        .sum()
}

fn allowed_list() -> String {
    std::fs::read_to_string("/proc/self/status")
        .expect("/proc/self/status")
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:").map(|v| v.trim().to_string()))
        .expect("Cpus_allowed_list")
}

fn ffmpeg_default(count: usize) -> usize {
    if count > 1 {
        (count + 1).min(16)
    } else {
        1
    }
}

fn say() -> String {
    let (lo, hi) = kerf_core::cpu_graph_thread_range();
    format!(
        "CHECKER-P2 count child: allowed={} count={} range={lo}-{hi}",
        allowed_list(),
        kerf_core::ffmpeg_cpu_count().map_or(-1, |c| c as i64)
    )
}

#[test]
fn checker_p2_count_child() {
    if std::env::var(CHILD).is_ok() {
        eprintln!("{}", say());
    }
}

/// `cmd`'s stdout and stderr, killed after `limit`.
fn output_within(mut cmd: Command, limit: Duration) -> Option<String> {
    use std::io::Read;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let mut err = child.stderr.take()?;
    let a = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = out.read_to_string(&mut text);
        text
    });
    let b = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = err.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    Some(format!("{}{}", a.join().ok()?, b.join().ok()?))
}

#[test]
fn checker_p2_count_ffmpeg_cpu_count_is_the_kernels_account_of_the_affinity() {
    // The process's own: whatever mask the test runner has.
    let own = cpus_in(&allowed_list());
    assert_eq!(
        kerf_core::ffmpeg_cpu_count(),
        Some(own),
        "this process's own affinity ({})",
        allowed_list()
    );
    let have_taskset = Command::new("taskset")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !have_taskset {
        eprintln!("CHECKER-P2 count: no taskset on this host: only the process's own mask was held");
        return;
    }
    let exe = std::env::current_exe().expect("the test binary");
    let mut tried = 0;
    for set in ["0", "0,1", "0-2", "0-3", "1,3", "2", "0,2-3"] {
        let mut cmd = Command::new("taskset");
        cmd.args(["-c", set])
            .arg(&exe)
            .args(["checker_p2_count_child", "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .env("KERF_CPU_PERCENT", "100");
        let text = output_within(cmd, Duration::from_secs(60)).expect("the child ends");
        let line = text
            .lines()
            .find(|l| l.contains("CHECKER-P2 count child:"))
            .unwrap_or_else(|| panic!("the child said nothing:\n{text}"));
        // (A mask naming a cpu this host does not have fails taskset: nothing to hold then.)
        let get = |key: &str| -> String {
            line.split(&format!("{key}="))
                .nth(1)
                .unwrap()
                .split(' ')
                .next()
                .unwrap()
                .to_string()
        };
        let (allowed, count) = (get("allowed"), get("count"));
        let want = cpus_in(&allowed);
        assert_eq!(count.parse::<i64>().unwrap(), want as i64, "taskset -c {set}: {line}");
        let (lo, hi) = get("range")
            .split_once('-')
            .map(|(a, b)| (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()))
            .unwrap();
        assert_eq!(
            (lo, hi),
            (ffmpeg_default(want), ffmpeg_default(want)),
            "taskset -c {set}: the uncapped range is exact: {line}"
        );
        tried += 1;
    }
    eprintln!("CHECKER-P2 count: {tried} masks held to /proc/self/status");
    assert!(tried >= 4);
}
