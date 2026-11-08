//! `checker/scalers/probe/...`: what the scaler probe does when one of its children fails, hangs or returns another picture than the
//! model's, and which names it then leaves out. The probe is the process's (`kerf_gpu::scaler::kernels()`, measured once), so each
//! scenario runs in a child of this test binary with `KERF_FFMPEG` swapped for a shell wrapper that misbehaves for the chains it names
//! and passes every other through to the real FFmpeg. Needs `ffmpeg` and a shell; no adapter. `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_scalers_probe -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kerf_core::ScalerKinds;
use kerf_gpu::scaler::{self, Kernels};

const SCENARIO: &str = "KERF_SC_SCENARIO";
const LOG: &str = "KERF_SC_LOG";
const FAIL: &str = "KERF_SC_FAIL";
const PIDFILE: &str = "KERF_SC_PID";

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

/// The child: asks for the process's probe and says what came back (and how long it took, and how many FFmpegs it ran).
#[test]
#[ignore = "run by the scenario tests below, in a child process"]
fn checker_scalers_probe_child() {
    let Ok(scenario) = std::env::var(SCENARIO) else {
        return;
    };
    let started = Instant::now();
    let first = scaler::kernels();
    eprintln!(
        "RESULT1 {} ms={} runs={}",
        line_of(first),
        started.elapsed().as_millis(),
        invocations()
    );
    // Asked again at once: the answer is kept (a measured one) or not tried again before the backoff (an unknown one).
    let again = Instant::now();
    let second = scaler::kernels();
    eprintln!(
        "RESULT2 {} ms={} runs={}",
        line_of(second),
        again.elapsed().as_millis(),
        invocations()
    );
    if scenario == "fail_then_recover" {
        // The failing chain is repaired; the backoff (5 s after the first failure) passes; the probe measures again.
        if let Some(p) = std::env::var_os(FAIL) {
            let _ = std::fs::remove_file(p);
        }
        std::thread::sleep(Duration::from_millis(5600));
        let third = scaler::kernels();
        eprintln!("RESULT3 {} runs={}", line_of(third), invocations());
    }
}

struct Run {
    lines: Vec<String>,
    dir: PathBuf,
    elapsed: Duration,
}

impl Run {
    fn line(&self, tag: &str) -> &str {
        self.lines
            .iter()
            .find(|l| l.starts_with(tag))
            .unwrap_or_else(|| panic!("{tag} not printed: {:#?}", self.lines))
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
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-sc-probe-{name}"));
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
    let started = Instant::now();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "--nocapture",
            "--test-threads=1",
            "checker_scalers_probe_child",
        ])
        .env(SCENARIO, name)
        .env("KERF_FFMPEG", &wrapper)
        .env(LOG, &log)
        .env(FAIL, &fail)
        .env(PIDFILE, dir.join("pid"))
        .stdin(Stdio::null())
        .output()
        .expect("run the child");
    let elapsed = started.elapsed();
    let text = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    let lines: Vec<String> = text.lines().filter(|l| l.starts_with("RESULT")).map(str::to_string).collect();
    eprintln!("{name}: {lines:#?} (child took {elapsed:?})");
    assert!(out.status.success(), "{name}: the child failed: {text}");
    Run { lines, dir, elapsed }
}

/// A rewrite of every argument that matches `from` into `to` (the chain then returns another picture than the one the probe asks about).
fn rewrite(from: &str, to: &str) -> String {
    format!(
        "n=$#\ni=0\nwhile [ $i -lt $n ]; do\n  a=$1; shift\n  case \"$a\" in *'{from}'*) a=$(printf '%s' \"$a\" | sed 's|{from}|{to}|g');; esac\n  set -- \"$@\" \"$a\"\n  i=$((i+1))\ndone"
    )
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_runs_twenty_children_and_proves_every_name() {
    let r = scenario("plain", "");
    assert!(
        r.line("RESULT1")
            .contains("Match named bilinear=1 lanczos=1 neighbor=1 spline=1"),
        "{:?}",
        r.lines
    );
    assert_eq!(r.number("RESULT1", "runs="), 20, "the probe's children");
    assert_eq!(r.number("RESULT2", "runs="), 20, "a measured answer is kept");
    assert!(r.number("RESULT1", "ms=") < 20_000, "{:?}", r.lines);
    assert!(r.elapsed < Duration::from_secs(60));
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_child_that_fails_leaves_the_answer_open_and_is_tried_again_after_the_backoff() {
    let r = scenario(
        "fail_then_recover",
        "if [ -e \"$KERF_SC_FAIL\" ]; then case \"$*\" in *flags=spline*) exit 1;; esac; fi",
    );
    assert_eq!(
        r.line("RESULT1").split(" ms=").next(),
        Some("RESULT1 Unknown"),
        "{:?}",
        r.lines
    );
    let (first, second) = (r.number("RESULT1", "runs="), r.number("RESULT2", "runs="));
    assert_eq!(r.line("RESULT2").split(" ms=").next(), Some("RESULT2 Unknown"));
    assert_eq!(first, second, "an open answer is not measured again before the backoff");
    assert!(
        r.line("RESULT3")
            .contains("Match named bilinear=1 lanczos=1 neighbor=1 spline=1"),
        "{:?}",
        r.lines
    );
    assert_eq!(r.number("RESULT3", "runs="), first + 20, "measured again, all twenty");
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_child_that_hangs_is_killed_at_its_deadline_and_leaves_the_answer_open() {
    for (name, needle) in [
        ("hang_sin", "sin("),
        ("hang_neighbor", "flags=neighbor"),
        ("hang_base", "flags=bilinear"),
    ] {
        let r = scenario(
            name,
            &format!("case \"$*\" in *'{needle}'*) echo $$ > \"$KERF_SC_PID\"; exec sleep 120;; esac"),
        );
        assert_eq!(
            r.line("RESULT1").split(" ms=").next(),
            Some("RESULT1 Unknown"),
            "{name}: {:?}",
            r.lines
        );
        let ms = r.number("RESULT1", "ms=");
        assert!(
            (19_000..30_000).contains(&ms),
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
        assert_eq!(r.line("RESULT2").split(" ms=").next(), Some("RESULT2 Unknown"), "{name}");
        assert_eq!(
            r.number("RESULT1", "runs="),
            r.number("RESULT2", "runs="),
            "{name}: not run again at once"
        );
    }
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_name_whose_picture_differs_is_the_only_one_left_out() {
    for (name, flag, expect) in [
        (
            "diff_lanczos",
            "lanczos",
            "Match named bilinear=1 lanczos=0 neighbor=1 spline=1",
        ),
        (
            "diff_spline",
            "spline",
            "Match named bilinear=1 lanczos=1 neighbor=1 spline=0",
        ),
        (
            "diff_neighbor",
            "neighbor",
            "Match named bilinear=1 lanczos=1 neighbor=0 spline=1",
        ),
    ] {
        // (`flags=<name>` of the three chains of that name becomes bicubic: another picture than the name's model.)
        let r = scenario(name, &rewrite(&format!("flags={flag}"), "flags=bicubic"));
        assert!(r.line("RESULT1").contains(expect), "{name}: {:?}", r.lines);
        assert!(r.line("RESULT2").contains(expect), "{name}: the answer is kept");
    }
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_scalers_probe_a_sin_that_is_not_ours_leaves_lanczos_out_alone() {
    // The `sin` chain's argument is nudged by a part in 1e14 (a libm that differs in the digits below a double's 44th bit, at every
    // argument): the lanczos chains (the real tables) still match, the function does not.
    let r = scenario("sin_differs", &rewrite("*0.0023)", "*0.00230000000000001)"));
    assert!(
        r.line("RESULT1")
            .contains("Match named bilinear=1 lanczos=0 neighbor=1 spline=1"),
        "{:?}",
        r.lines
    );
    // ... and a single argument of the 4096 nudged (a libm that differs at one argument in 4096, on the grid): seen as well.
    let r = scenario(
        "sin_differs_at_one_argument",
        &rewrite("*0.0023)", "*0.0023+eq(floor(X/8)+64*Y,1000)*0.00000000000001)"),
    );
    assert!(
        r.line("RESULT1")
            .contains("Match named bilinear=1 lanczos=0 neighbor=1 spline=1"),
        "{:?}",
        r.lines
    );
}
