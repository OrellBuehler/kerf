//! `output_bounded`: a deadline that kills, and pipes that never block the child.

use super::*;
use std::io::ErrorKind;
use std::time::Duration;

fn sh(script: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", script]);
    cmd
}

fn gone(pid: &str) -> bool {
    !Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn a_process_that_outlasts_the_limit_is_killed_and_reported_as_timed_out() {
    let dir = std::env::temp_dir().join(format!("kerf-output-bounded-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pidfile = dir.join("pid");
    // `exec` makes the shell's pid the sleep's.
    let mut cmd = sh(&format!("echo $$ > '{}'; exec sleep 30", pidfile.display()));
    let started = Instant::now();
    let err = output_bounded(&mut cmd, Duration::from_secs(1)).expect_err("sleep 30 is past 1 s");
    assert_eq!(err.kind(), ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    // The shell writes its pid first thing; under load that can be later than the deadline of a
    // short limit, so the file is waited for. A child killed before it ever ran has no pid to be
    // gone, and the timeout above is all there is to check.
    let until = Instant::now() + Duration::from_secs(5);
    let pid = loop {
        match std::fs::read_to_string(&pidfile) {
            Ok(text) if text.ends_with('\n') => break Some(text.trim().to_string()),
            _ if Instant::now() >= until => break None,
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    if let Some(pid) = pid {
        let until = Instant::now() + Duration::from_secs(5);
        while !gone(&pid) {
            assert!(Instant::now() < until, "process {pid} is still there");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn more_than_a_pipes_worth_on_both_outputs_does_not_block_the_child() {
    // 300 kB each way, past the 64 kB a pipe holds, in both orders: a reader that took one pipe
    // to its end before the other would deadlock on the second.
    for script in [
        "head -c 300000 /dev/zero; head -c 300000 /dev/zero >&2",
        "head -c 300000 /dev/zero >&2; head -c 300000 /dev/zero",
        "i=0; while [ $i -lt 40 ]; do head -c 10000 /dev/zero; head -c 10000 /dev/zero >&2; i=$((i+1)); done",
    ] {
        let started = Instant::now();
        let out = output_bounded(&mut sh(script), Duration::from_secs(30)).expect(script);
        assert!(out.status.success(), "{script}");
        assert!(
            out.stdout.len() >= 300_000 - 20_000 && out.stderr.len() >= 300_000 - 20_000,
            "{script}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{script}: {:?}",
            started.elapsed()
        );
    }
}

#[test]
fn a_process_that_ends_in_time_gives_its_status_and_both_outputs() {
    let out = output_bounded(&mut sh("echo out; echo err >&2; exit 3"), Duration::from_secs(30)).unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(out.stdout, b"out\n");
    assert_eq!(out.stderr, b"err\n");
}

#[test]
fn a_program_that_is_not_there_is_the_spawn_error() {
    let err = output_bounded(&mut Command::new("kerf-no-such-program"), Duration::from_secs(1)).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn a_run_a_caller_cancels_is_killed_at_once_and_reported_as_no_answer() {
    let stop = std::sync::atomic::AtomicBool::new(false);
    let started = Instant::now();
    let answer = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        output_bounded_cancellable(
            sh("exec sleep 30"),
            Duration::from_secs(60),
            Some(&|| stop.load(std::sync::atomic::Ordering::SeqCst)),
        )
    });
    assert!(answer.expect("a stop is not an error").is_none());
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
}

#[test]
fn a_cancellable_run_keeps_the_deadline_and_the_outputs_of_the_plain_one() {
    let never = || false;
    let err = output_bounded_cancellable(sh("exec sleep 30"), Duration::from_millis(300), Some(&never)).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::TimedOut);

    let out = output_bounded_cancellable(sh("echo out; echo err >&2; exit 3"), Duration::from_secs(30), Some(&never))
        .unwrap()
        .expect("it finished");
    assert_eq!(out.status.code(), Some(3));
    assert_eq!((out.stdout, out.stderr), (b"out\n".to_vec(), b"err\n".to_vec()));

    // Without a cancel it is the plain bounded run.
    let out = output_bounded_cancellable(sh("echo out"), Duration::from_secs(30), None)
        .unwrap()
        .unwrap();
    assert_eq!(out.stdout, b"out\n");
    let err = output_bounded_cancellable(Command::new("kerf-no-such-program"), Duration::from_secs(1), Some(&never)).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}
