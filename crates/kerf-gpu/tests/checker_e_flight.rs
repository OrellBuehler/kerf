//! `checker/e/r4`: the blend probe is single-flight and bounded. Callers that ask together share one measurement (the same number of
//! `ffmpeg` children as a lone caller starts), at most six children run at a time under one deadline, and a child that fails says why and
//! what it wrote on stderr (the tail, bounded). Needs `ffmpeg` (`KERF_FFMPEG`) and a shell; no adapter. `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_e_flight -- --ignored --nocapture
//! ```
//!
//! The probe's answer is kept per process, so each scenario runs in a child process of this test (`checker_e_flight_child`, selected by
//! `CHECKER_E_FLIGHT_MODE`), with `KERF_FFMPEG` pointing at a wrapper that logs every start and end and passes the run on.

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use kerf_gpu::blend;

struct Collector;

struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, " {}={:?}", field.name(), value);
    }
}

impl tracing::Subscriber for Collector {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        eprintln!("TRACE {}{}", event.metadata().level(), fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// The scenario this process is a child of (a plain `--ignored` run of the file finds none and passes).
#[test]
#[ignore = "run by the scenarios of this file, in a process of its own"]
fn checker_e_flight_child() {
    let Ok(mode) = std::env::var("CHECKER_E_FLIGHT_MODE") else {
        return;
    };
    let callers: usize = std::env::var("CHECKER_E_FLIGHT_CALLERS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(1);
    let _ = tracing::subscriber::set_global_default(Collector);
    let barrier = std::sync::Barrier::new(callers);
    let started = Instant::now();
    let answers: Vec<_> = std::thread::scope(|scope| {
        let asks: Vec<_> = (0..callers)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    (blend::kernels(), blend::extras())
                })
            })
            .collect();
        asks.into_iter().map(|a| a.join().expect("a caller panicked")).collect()
    });
    eprintln!(
        "CHILD {mode}: {callers} callers answered in {:?}: {:?}",
        started.elapsed(),
        answers[0]
    );
    assert!(
        answers.iter().all(|a| *a == answers[0]),
        "every caller gets the one answer: {answers:?}"
    );
}

struct Run {
    stderr: String,
    log: Vec<(u128, char)>,
    took: Duration,
    success: bool,
}

fn wrapper(dir: &Path, body: &str) -> PathBuf {
    let real = kerf_core::ffmpeg_path();
    let path = dir.join("ffmpeg-wrapper");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nnow() {{ date +%s%N; }}\nprintf 'S %s\\n' \"$(now)\" >> \"$CHECKER_E_FLIGHT_LOG\"\n{body}\n'{real}' \"$@\"\nrc=$?\nprintf 'E %s\\n' \"$(now)\" >> \"$CHECKER_E_FLIGHT_LOG\"\nexit $rc\n"
        ),
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
            "checker_e_flight_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("KERF_FFMPEG", wrapper)
        .env("CHECKER_E_FLIGHT_LOG", &log)
        .env("CHECKER_E_FLIGHT_MODE", mode)
        .env("CHECKER_E_FLIGHT_CALLERS", callers.to_string())
        .output()
        .expect("run the child");
    let mut events: Vec<(u128, char)> = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (kind, at) = l.split_once(' ')?;
            Some((at.trim().parse().ok()?, kind.chars().next()?))
        })
        .collect();
    events.sort();
    Run {
        stderr: String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
        log: events,
        took: started.elapsed(),
        success: out.status.success(),
    }
}

fn starts(run: &Run) -> usize {
    run.log.iter().filter(|(_, k)| *k == 'S').count()
}

fn peak(run: &Run) -> usize {
    let (mut now, mut peak) = (0usize, 0usize);
    for (_, kind) in &run.log {
        if *kind == 'S' {
            now += 1;
            peak = peak.max(now);
        } else {
            now = now.saturating_sub(1);
        }
    }
    peak
}

/// A lone caller starts some number of children; sixteen callers asking at once start the same number, never more than six at a time, and all
/// get the one answer.
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_e_callers_that_ask_together_share_one_measurement_of_at_most_six_children_at_a_time() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-e-flight");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Each child dawdles a little, so that the children of a worker pool overlap whatever the machine's load.
    let wrapper = wrapper(&dir, "sleep 0.25");
    let lone = child(&dir, &wrapper, "lone", 1);
    eprintln!(
        "lone: {} children, peak {}, {:?}\n{}",
        starts(&lone),
        peak(&lone),
        lone.took,
        lone.stderr
    );
    assert!(lone.success, "the lone caller's child process failed:\n{}", lone.stderr);
    let crowd = child(&dir, &wrapper, "crowd", 16);
    eprintln!("crowd: {} children, peak {}, {:?}", starts(&crowd), peak(&crowd), crowd.took);
    assert!(crowd.success, "the crowd's child process failed:\n{}", crowd.stderr);
    assert!(
        starts(&lone) >= 12,
        "a measurement is a few dozen children: {}",
        starts(&lone)
    );
    assert_eq!(
        starts(&crowd),
        starts(&lone),
        "sixteen callers share the one measurement a lone caller makes"
    );
    assert_eq!(peak(&lone), 6, "the pool is six workers");
    assert_eq!(peak(&crowd), 6, "and stays six under a crowd");
    assert!(
        crowd.took < lone.took + Duration::from_secs(10),
        "the crowd waits for the one measurement: {:?} against {:?}",
        crowd.took,
        lone.took
    );
}

/// A child that fails says why, with the job it was a probe of and the tail of its stderr (bounded: the head of a long message is dropped).
#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_e_a_failed_probe_child_logs_its_status_and_the_tail_of_its_stderr() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-e-flight-stderr");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut noise = String::from("HEADMARK ");
    for i in 0..400 {
        let _ = write!(noise, "line{i} ");
    }
    noise.push_str("TAILMARK");
    let body = format!("case \"$*\" in *'mode=darken'*) printf '%s\\n' '{noise}' >&2; exit 3;; esac");
    let wrapper = wrapper(&dir, &body);
    let run = child(&dir, &wrapper, "stderr", 1);
    let traces: Vec<&str> = run.stderr.lines().filter(|l| l.starts_with("TRACE")).collect();
    eprintln!(
        "{} trace lines\n{}",
        traces.len(),
        traces.iter().map(|t| &t[..t.len().min(400)]).collect::<Vec<_>>().join("\n")
    );
    assert!(run.success, "the child process (a failed probe is no panic):\n{}", run.stderr);
    let failed: Vec<&&str> = traces.iter().filter(|t| t.contains("did not answer probes=")).collect();
    assert!(!failed.is_empty(), "a child that failed is logged:\n{}", run.stderr);
    for line in &failed {
        assert!(line.contains("TAILMARK"), "the tail of its stderr is logged: {line}");
        assert!(
            !line.contains("HEADMARK"),
            "and only the tail: {}",
            &line[..line.len().min(300)]
        );
        assert!(line.contains("exited with status 3"), "and why it failed: {line}");
        assert!(line.contains("probes="), "and what it was a probe of: {line}");
    }
    assert!(failed.len() >= 2, "every darken child is logged: {}", failed.len());
}
