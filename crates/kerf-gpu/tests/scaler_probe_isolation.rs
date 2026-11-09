//! The scaler probe's questions are separate: the base scaler (the unflagged scale and the bicubic and bilinear chains, ten children) and each
//! named scaler (its three chains, and Lanczos's `sin` check) have a backoff of their own. A child of a name that fails or hangs leaves that
//! name out of `Model::named` and the base's answer what its own ten children measured; the name is asked again alone, after its backoff. A
//! child of the base that fails or hangs leaves the base `Unknown`, as it always did.
//!
//! The probe is the process's (`kerf_gpu::scaler::kernels()`, measured once), so each scenario runs in a child of this test binary with
//! `KERF_FFMPEG` swapped for a shell wrapper that misbehaves for the chains it names and passes every other through to the real FFmpeg.
//! Needs `ffmpeg` and a shell; no adapter. `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test scaler_probe_isolation -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kerf_core::ScalerKinds;
use kerf_gpu::scaler::{self, Kernels};

const SCENARIO: &str = "KERF_SPI_SCENARIO";
const LOG: &str = "KERF_SPI_LOG";
const FAIL: &str = "KERF_SPI_FAIL";
const PIDFILE: &str = "KERF_SPI_PID";
const ALL_LINE: &str = "Match named bilinear=1 lanczos=1 neighbor=1 spline=1";

fn line_of(k: Kernels) -> String {
    match k {
        Kernels::Match(m) => {
            let b = |kind| u8::from(m.named.contains(kind));
            format!(
                "Match named bilinear={} lanczos={} neighbor={} spline={}",
                b(ScalerKinds::BILINEAR),
                b(ScalerKinds::LANCZOS),
                b(ScalerKinds::NEIGHBOR),
                b(ScalerKinds::SPLINE)
            )
        }
        Kernels::Differs => "Differs".to_string(),
        Kernels::Unknown => "Unknown".to_string(),
    }
}

fn invocations() -> usize {
    std::env::var_os(LOG)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map_or(0, |t| t.lines().count())
}

/// The child: asks for the process's probe twice, and (for the scenarios that repair the wrapper) a third time after the backoff.
#[test]
#[ignore = "run by the scenario tests below, in a child process"]
fn scaler_probe_isolation_child() {
    let Ok(scenario) = std::env::var(SCENARIO) else {
        return;
    };
    let started = Instant::now();
    let first = scaler::kernels();
    eprintln!(
        "RESULT1 {} ms={} runs={} exact={}",
        line_of(first),
        started.elapsed().as_millis(),
        invocations(),
        first.exact()
    );
    // Asked again at once: a measured answer is kept, an open question is not asked before its backoff.
    let again = Instant::now();
    let second = scaler::kernels();
    eprintln!(
        "RESULT2 {} ms={} runs={}",
        line_of(second),
        again.elapsed().as_millis(),
        invocations()
    );
    if scenario.ends_with("_then_recover") || scenario == "hang_retry" {
        // The failing child is repaired; the backoff (5 s after the first failure) passes; the open question is asked again.
        if scenario != "hang_retry" {
            if let Some(p) = std::env::var_os(FAIL) {
                let _ = std::fs::remove_file(p);
            }
        }
        std::thread::sleep(Duration::from_millis(5600));
        let started = Instant::now();
        let third = scaler::kernels();
        eprintln!(
            "RESULT3 {} ms={} runs={}",
            line_of(third),
            started.elapsed().as_millis(),
            invocations()
        );
        // A name's retry runs on a detached thread: the call that found it due was told what was known, and the name joins the set when the
        // retry answers (a hanging one is killed at its deadline, which this waits for so that no child outlives the test).
        let until = Instant::now() + Duration::from_secs(if scenario == "hang_retry" { 25 } else { 30 });
        let mut last = third;
        while Instant::now() < until && (scenario == "hang_retry" || line_of(last) != ALL_LINE) {
            std::thread::sleep(Duration::from_millis(100));
            last = scaler::kernels();
        }
        eprintln!("RESULT4 {} runs={}", line_of(last), invocations());
    }
}

struct Run {
    lines: Vec<String>,
    dir: PathBuf,
}

impl Run {
    fn line(&self, tag: &str) -> &str {
        self.lines
            .iter()
            .find(|l| l.starts_with(tag))
            .unwrap_or_else(|| panic!("{tag} not printed: {:#?}", self.lines))
    }

    /// The answer a line gives: what follows the tag, up to the figures.
    fn answer(&self, tag: &str) -> &str {
        let l = self.line(tag).strip_prefix(tag).unwrap_or_default().trim_start();
        l.split(" ms=").next().and_then(|a| a.split(" runs=").next()).unwrap_or(l)
    }

    fn number(&self, tag: &str, key: &str) -> u128 {
        let l = self.line(tag);
        let at = l.find(key).unwrap_or_else(|| panic!("{key} in {l}")) + key.len();
        l[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap()
    }
}

/// Run the child under a wrapper of `body` (shell, after the log line and before the real FFmpeg).
fn scenario(name: &str, body: &str) -> Run {
    let real = kerf_core::ffmpeg_path();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("scaler-probe-isolation-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let wrapper = dir.join("ffmpeg-wrapper");
    let log = dir.join("runs.log");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\necho \"$$ $*\" >> \"{log}\"\nREAL='{real}'\n{body}\nexec \"$REAL\" \"$@\"\n",
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let fail = dir.join("fail");
    std::fs::write(&fail, "x").unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "--nocapture",
            "--test-threads=1",
            "scaler_probe_isolation_child",
        ])
        .env(SCENARIO, name)
        .env("KERF_FFMPEG", &wrapper)
        .env(LOG, &log)
        .env(FAIL, &fail)
        .env(PIDFILE, dir.join("pid"))
        .stdin(Stdio::null())
        .output()
        .expect("run the child");
    let text = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    let lines: Vec<String> = text.lines().filter(|l| l.starts_with("RESULT")).map(str::to_string).collect();
    eprintln!("{name}: {lines:#?}");
    assert!(out.status.success(), "{name}: the child failed: {text}");
    Run { lines, dir }
}

const ALL: &str = "Match named bilinear=1 lanczos=1 neighbor=1 spline=1";

fn without(kind: &str) -> String {
    ALL.replace(&format!("{kind}=1"), &format!("{kind}=0"))
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn a_name_whose_child_fails_is_left_out_alone_and_asked_again_alone_after_its_backoff() {
    for (name, needle, kind, retry) in [
        ("spline", "flags=spline", "spline", 3),
        ("neighbor", "flags=neighbor", "neighbor", 3),
        ("lanczos_chain", "flags=lanczos", "lanczos", 4),
        // (The `sin` check is a child of Lanczos's: the same name, four children.)
        ("lanczos_sin", "sin(", "lanczos", 4),
    ] {
        let r = scenario(
            &format!("fail_{name}_then_recover"),
            &format!("if [ -e \"$KERF_SPI_FAIL\" ]; then case \"$*\" in *'{needle}'*) exit 1;; esac; fi"),
        );
        let want = without(kind);
        // The base's answer is its own ten children's: a match, exact; only the name is out.
        assert_eq!(r.answer("RESULT1"), want, "{name}: {:?}", r.lines);
        assert!(r.line("RESULT1").ends_with("exact=true"), "{name}: {:?}", r.lines);
        let first = r.number("RESULT1", "runs=");
        assert_eq!(first, 20, "{name}: the probe's twenty children");
        // Not asked again before its backoff.
        assert_eq!(r.answer("RESULT2"), want, "{name}");
        assert_eq!(r.number("RESULT2", "runs="), first, "{name}: asked again within the backoff");
        // After it: the call that finds the name due is told what is known at once (the retry is on a detached thread) ...
        assert_eq!(r.answer("RESULT3"), want, "{name}: {:?}", r.lines);
        assert!(
            r.number("RESULT3", "ms=") < 3_000,
            "{name}: the caller waited for the retry: {:?}",
            r.lines
        );
        // ... and the name joins the set when the retry answers: its own children and no other.
        assert_eq!(r.answer("RESULT4"), ALL, "{name}: {:?}", r.lines);
        assert_eq!(
            r.number("RESULT4", "runs="),
            first + retry,
            "{name}: the retry is the name's own children and no other"
        );
    }
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn the_retry_of_a_name_that_hangs_again_is_off_the_callers_path_and_killed_at_its_deadline() {
    let r = scenario(
        "hang_retry",
        "case \"$*\" in *'flags=spline'*) echo $$ > \"$KERF_SPI_PID\"; exec sleep 120;; esac",
    );
    let want = without("spline");
    assert_eq!(r.answer("RESULT1"), want, "{:?}", r.lines);
    assert!((19_000..60_000).contains(&r.number("RESULT1", "ms=")), "{:?}", r.lines);
    let first = r.number("RESULT1", "runs=");
    assert_eq!(first, 20);
    // The caller that finds the name due is not held up by its hanging children (it used to wait their 20 s again) ...
    assert_eq!(r.answer("RESULT3"), want, "{:?}", r.lines);
    assert!(
        r.number("RESULT3", "ms=") < 3_000,
        "the caller waited for the retry of a name that hangs: {:?}",
        r.lines
    );
    // ... the retry is the name's own three children, killed at their deadline, and the name is still out.
    assert_eq!(r.answer("RESULT4"), want, "{:?}", r.lines);
    assert_eq!(r.number("RESULT4", "runs="), first + 3, "{:?}", r.lines);
    let pid = std::fs::read_to_string(r.dir.join("pid")).unwrap_or_default();
    let pid = pid.trim();
    assert!(!pid.is_empty(), "the stall was never reached");
    let alive = Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    assert!(!alive, "the stalled child {pid} outlived the retry");
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn a_base_child_that_fails_leaves_the_scaler_unknown_and_everything_is_asked_again_after_the_backoff() {
    let r = scenario(
        "fail_base_then_recover",
        "if [ -e \"$KERF_SPI_FAIL\" ]; then case \"$*\" in *flags=bilinear*) exit 1;; esac; fi",
    );
    assert_eq!(r.answer("RESULT1"), "Unknown", "{:?}", r.lines);
    assert!(r.line("RESULT1").ends_with("exact=false"));
    let first = r.number("RESULT1", "runs=");
    assert_eq!(first, 20);
    assert_eq!(r.answer("RESULT2"), "Unknown");
    assert_eq!(r.number("RESULT2", "runs="), first, "asked again within the backoff");
    // The base and every name are asked together (the names had no model to be compared with, so none is counted as failed).
    // (The base is the caller's own question, measured on its thread.)
    assert_eq!(r.answer("RESULT3"), ALL, "{:?}", r.lines);
    assert_eq!(r.number("RESULT3", "runs="), first + 20);
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn a_child_that_hangs_is_killed_at_its_deadline_and_leaves_only_its_own_question_open() {
    for (name, needle, expected) in [
        ("hang_sin", "sin(", without("lanczos")),
        ("hang_lanczos", "flags=lanczos", without("lanczos")),
        ("hang_spline", "flags=spline", without("spline")),
        ("hang_neighbor", "flags=neighbor", without("neighbor")),
        // A base child: the scaler is unknown, as it was.
        ("hang_base", "flags=bilinear", "Unknown".to_string()),
    ] {
        let r = scenario(
            name,
            &format!("case \"$*\" in *'{needle}'*) echo $$ > \"$KERF_SPI_PID\"; exec sleep 120;; esac"),
        );
        assert_eq!(r.answer("RESULT1"), expected, "{name}: {:?}", r.lines);
        assert_eq!(r.line("RESULT1").ends_with("exact=true"), expected != "Unknown", "{name}");
        let ms = r.number("RESULT1", "ms=");
        assert!(
            (19_000..60_000).contains(&ms),
            "{name}: killed at the 20 s deadline, not at once and not after the stall: {ms} ms"
        );
        // The stalled child is gone (`exec sleep` is the wrapper's own pid).
        let pid = std::fs::read_to_string(r.dir.join("pid")).unwrap_or_default();
        let pid = pid.trim();
        assert!(!pid.is_empty(), "{name}: the stall was never reached");
        let alive = Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(!alive, "{name}: the stalled child {pid} outlived the probe");
        assert_eq!(r.answer("RESULT2"), expected, "{name}");
        assert_eq!(
            r.number("RESULT1", "runs="),
            r.number("RESULT2", "runs="),
            "{name}: not run again at once"
        );
    }
}
