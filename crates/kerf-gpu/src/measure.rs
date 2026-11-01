//! What the probes of this FFmpeg have in common: the verdict they give, the process-wide memory of it with its
//! backoff, and the bounded child process they run ([`crate::conversion`], [`crate::placement_probe`]).
//!
//! A probe answers once per process and the answer is kept for good: [`Verdict::Match`] (this FFmpeg does what the
//! compositor reproduces) or [`Verdict::Differs`] (it does not, and the detail says where). One that could not tell
//! (FFmpeg would not run, timed out, wrote something unreadable) is [`Verdict::Unknown`], which is **never kept as an
//! answer and never believed as a match**: it is asked again after a backoff, and until then the compositor draws
//! nothing that depends on it.

use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// What a probe found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// This FFmpeg does what the compositor reproduces.
    Match,
    /// It does not (the detail says where).
    Differs(String),
    /// Not known: not measured yet, FFmpeg would not run or timed out (asked again after a backoff), or a self-test
    /// could not tell (see [`checked`]).
    Unknown,
}

/// What the process knows of one probe.
#[derive(Debug)]
pub(crate) struct Slot {
    /// The answer, once there is one (`Match` or `Differs`; kept for good).
    measured: Option<Verdict>,
    /// A probe that could not tell is not asked again before this.
    retry_at: Option<Instant>,
    /// A self-test could not tell whether the compositor draws what this probe is about as this FFmpeg draws it, so it
    /// did not draw that frame: nothing is believed until a self-test has (a probe that succeeds later must not open a
    /// path the check never looked at).
    held_back: bool,
}

impl Slot {
    pub(crate) const fn new() -> Self {
        Self {
            measured: None,
            retry_at: None,
            held_back: false,
        }
    }
}

fn lock(state: &Mutex<Slot>) -> MutexGuard<'_, Slot> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What is known without running anything: the measured answer, else [`Verdict::Unknown`] (not measured yet, backing
/// off after a probe that failed, or held back by a self-test).
pub(crate) fn known(state: &Mutex<Slot>) -> Verdict {
    let s = lock(state);
    if s.held_back {
        return Verdict::Unknown;
    }
    s.measured.clone().unwrap_or(Verdict::Unknown)
}

/// The answer, from `probe` on the first call (child processes: a blocking thread's, with no lock held, and nothing
/// waits behind them: callers that arrive meanwhile probe too, boundedly, and the first answer stored wins). An
/// `Unknown` is not remembered but for `backoff`, inside which the probe is not run again.
pub(crate) fn cached(state: &Mutex<Slot>, now: Instant, backoff: Duration, probe: impl FnOnce() -> Verdict) -> Verdict {
    {
        let s = lock(state);
        if s.held_back {
            return Verdict::Unknown;
        }
        if let Some(known) = &s.measured {
            return known.clone();
        }
        if s.retry_at.is_some_and(|t| t > now) {
            return Verdict::Unknown;
        }
    }
    let found = probe();
    let mut s = lock(state);
    if let Some(known) = &s.measured {
        return known.clone();
    }
    if found == Verdict::Unknown {
        s.retry_at = Some(Instant::now() + backoff);
    } else {
        s.measured = Some(found.clone());
    }
    found
}

/// [`cached`] for the runtime self-test, which draws this probe's frame only on a `Match`: an `Unknown` here holds the
/// path back for the process until a self-test that can tell has run, so that the frames the GPU draws are never ones
/// its check skipped. A `Differs` is permanent and holds nothing back.
pub(crate) fn checked(state: &Mutex<Slot>, now: Instant, backoff: Duration, probe: impl FnOnce() -> Verdict) -> Verdict {
    lock(state).held_back = false;
    let found = cached(state, now, backoff, probe);
    if found == Verdict::Unknown {
        lock(state).held_back = true;
    }
    found
}

/// One FFmpeg run: `args` after the quiet flags, `input` on its stdin, its stdout; `None` when it would not run,
/// failed, or outlasted `limit` (killed). **Nothing waits on a child that will not end**: on the timeout path the
/// side threads that serve the pipes are left to finish on their own (a wrapper script that did not `exec` keeps a
/// pipe open past the kill, and a join would wait for it as long as the real FFmpeg lives).
pub(crate) fn ffmpeg(args: &[&str], input: Vec<u8>, limit: Duration) -> Option<Vec<u8>> {
    let mut cmd: Command = kerf_core::ffmpeg_command();
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]).args(args);
    kerf_core::run_piped_until(&mut cmd, input, Instant::now() + limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BACKOFF: Duration = Duration::from_secs(30);

    fn fresh() -> Mutex<Slot> {
        Mutex::new(Slot::new())
    }

    #[test]
    fn nothing_is_known_before_a_probe_has_run() {
        assert_eq!(known(&fresh()), Verdict::Unknown);
    }

    #[test]
    fn an_unknown_is_asked_again_after_the_backoff_and_not_before() {
        let state = fresh();
        let t0 = Instant::now();
        assert_eq!(cached(&state, t0, BACKOFF, || Verdict::Unknown), Verdict::Unknown);
        assert_eq!(known(&state), Verdict::Unknown, "a failure is not an answer");
        // Inside the backoff the probe is not run again, whatever it would say.
        assert_eq!(
            cached(&state, t0 + Duration::from_secs(29), BACKOFF, || panic!(
                "probed inside the backoff"
            )),
            Verdict::Unknown
        );
        // After it, it is: the retry is the backoff from the failure, `Instant::now()` at the time of it.
        let later = Instant::now() + BACKOFF + Duration::from_secs(1);
        assert_eq!(cached(&state, later, BACKOFF, || Verdict::Match), Verdict::Match);
        assert_eq!(known(&state), Verdict::Match);
    }

    #[test]
    fn a_probe_that_fails_again_is_asked_again_later_and_may_succeed() {
        let state = fresh();
        let t0 = Instant::now();
        assert_eq!(cached(&state, t0, BACKOFF, || Verdict::Unknown), Verdict::Unknown);
        let later = Instant::now() + BACKOFF + Duration::from_secs(1);
        assert_eq!(cached(&state, later, BACKOFF, || Verdict::Unknown), Verdict::Unknown);
        assert_eq!(known(&state), Verdict::Unknown, "two failures are still no answer");
        let much_later = Instant::now() + 2 * BACKOFF + Duration::from_secs(2);
        assert_eq!(cached(&state, much_later, BACKOFF, || Verdict::Match), Verdict::Match);
        assert_eq!(known(&state), Verdict::Match);
    }

    #[test]
    fn a_match_and_a_difference_are_kept_for_good() {
        for answer in [Verdict::Match, Verdict::Differs("yuv444p: 12 samples".into())] {
            let state = fresh();
            let t0 = Instant::now();
            assert_eq!(cached(&state, t0, BACKOFF, || answer.clone()), answer);
            assert_eq!(known(&state), answer);
            // Asked again, now or hours later, nothing is run and the answer stands.
            for at in [t0, t0 + Duration::from_secs(3600)] {
                assert_eq!(cached(&state, at, BACKOFF, || panic!("measured twice")), answer);
            }
        }
    }

    #[test]
    fn the_probe_runs_with_no_lock_held_and_the_first_answer_stored_wins() {
        let state = fresh();
        let now = Instant::now();
        let got = cached(&state, now, BACKOFF, || {
            assert!(state.try_lock().is_ok(), "the state is locked while the probe runs");
            assert_eq!(cached(&state, now, BACKOFF, || Verdict::Match), Verdict::Match);
            Verdict::Differs("late".into())
        });
        assert_eq!(got, Verdict::Match, "the first writer wins");
        assert_eq!(known(&state), Verdict::Match);
        // A failure that lost the race to an answer changes nothing.
        let state = fresh();
        let got = cached(&state, now, BACKOFF, || {
            cached(&state, now, BACKOFF, || Verdict::Match);
            Verdict::Unknown
        });
        assert_eq!(got, Verdict::Match);
    }

    #[test]
    fn a_self_test_that_cannot_tell_holds_the_path_back_until_one_can() {
        let state = fresh();
        let t0 = Instant::now();
        assert_eq!(checked(&state, t0, BACKOFF, || Verdict::Unknown), Verdict::Unknown);
        // A probe that would succeed now is not believed (and not run): the check skipped the frame.
        let later = Instant::now() + BACKOFF + Duration::from_secs(1);
        assert_eq!(
            cached(&state, later, BACKOFF, || panic!("probed while held back")),
            Verdict::Unknown
        );
        assert_eq!(known(&state), Verdict::Unknown);
        // The next self-test that can tell lifts the hold, and the answer stands.
        assert_eq!(checked(&state, later, BACKOFF, || Verdict::Match), Verdict::Match);
        assert_eq!(known(&state), Verdict::Match);
        // A difference holds nothing back.
        let state = fresh();
        let d = Verdict::Differs("a case".into());
        assert_eq!(checked(&state, t0, BACKOFF, || d.clone()), d);
        assert_eq!(known(&state), d);
    }

    /// A child that ends up never held the caller: a `ffmpeg` that is a wrapper which does not `exec` leaves the real
    /// one holding the pipe past the kill, and the run must return at its limit all the same.
    #[cfg(unix)]
    #[test]
    #[ignore = "spawns a shell script and sleeps"]
    fn a_child_that_outlives_its_kill_does_not_hold_the_caller() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kerf-measure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        let wrapper = dir.join("ffmpeg-wrapper.sh");
        // Not `exec`: the sleeper inherits the stdout pipe and outlives the shell that is killed.
        std::fs::write(&wrapper, "#!/bin/bash\nsleep 6\n").expect("a script");
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).expect("executable");
        let mut cmd = Command::new(&wrapper);
        let started = Instant::now();
        let got = kerf_core::run_piped_until(&mut cmd, Vec::new(), Instant::now() + Duration::from_millis(300));
        let took = started.elapsed();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got, None);
        assert!(
            took < Duration::from_secs(3),
            "the run waited {took:?} for a pipe held open past the kill"
        );
    }
}
