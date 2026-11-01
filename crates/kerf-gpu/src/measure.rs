//! What the probes of this FFmpeg have in common: the verdict they give, the process-wide memory of it with its
//! backoff, and the bounded child process they run ([`crate::conversion`], [`crate::placement_probe`]).
//!
//! A probe answers once per process and the answer is kept for good: [`Verdict::Match`] (this FFmpeg does what the
//! compositor reproduces) or [`Verdict::Differs`] (it does not, and the detail says where). One that could not tell
//! (FFmpeg would not run, timed out, wrote something unreadable) is [`Verdict::Unknown`], which is **never kept as an
//! answer and never believed as a match**: it is asked again after a backoff, and until then the compositor draws
//! nothing that depends on it.

//! What the probes of this FFmpeg have in common: the verdict they give, the process-wide memory of it with its
//! backoff, its single flight and its deadline, and the bounded child process they run ([`crate::conversion`],
//! [`crate::placement_probe`]).
//!
//! A probe answers once per process and the answer is kept for good: [`Verdict::Match`] (this FFmpeg does what the
//! compositor reproduces) or [`Verdict::Differs`] (it does not, and the detail says where). One that could not tell
//! (FFmpeg would not run, timed out, wrote something unreadable) is [`Verdict::Unknown`], which is **never kept as an
//! answer and never believed as a match**: it is asked again after a backoff, and until then the compositor draws
//! nothing that depends on it.
//!
//! * **One probe at a time** (single flight). A probe is a few dozen child processes; the render bar, a frame's `prepare`, a
//!   play session's primer and the self-test ask for the same answer from different threads, and only the first runs it. The
//!   others wait for it on a condition variable, no longer than the probe is bounded to (its deadline and a short grace), and
//!   take its answer; a waiter that is not answered by then gets `Unknown`, "not yet", and refuses. **No lock is held while a
//!   child runs**: the state is locked to read it and to store the answer, never across the probe, and a probe that panics
//!   gives its place up ([`Lead`]'s drop).
//! * **One deadline** for the whole probe ([`Timing::overall`], shared by all its children through a [`Budget`]): each child is
//!   given the shorter of its own limit and what is left, and a probe that has run out of time is `Unknown`, so a machine that
//!   is slow for every one of twenty-five children is not waited for twenty-five times.
//! * **The self-test's hold** ([`checked`]): the check draws a probe's frame only on a `Match`. Where it could not tell it
//!   did not draw that frame, so nothing is believed until a self-test has (a probe that succeeds later must not open a path the
//!   check never looked at). It does not take an earlier failure's backoff as its verdict: it waits the backoff out (it is a
//!   check run once, off any frame's path) and tries again, once, so a probe that timed out a moment before does not hold the
//!   path back for the session.

use std::process::Command;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
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

/// How long a probe takes, at most, and how soon it is asked again.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timing {
    /// A probe that could not tell is not asked again before this.
    pub backoff: Duration,
    /// The whole probe, all its children: past it the probe is `Unknown`.
    pub overall: Duration,
    /// One child.
    pub per_run: Duration,
}

/// What the process knows of one probe.
#[derive(Debug)]
struct Slot {
    /// The answer, once there is one (`Match` or `Differs`; kept for good).
    measured: Option<Verdict>,
    /// A probe that could not tell is not asked again before this.
    retry_at: Option<Instant>,
    /// A self-test could not tell whether the compositor draws what this probe is about as this FFmpeg draws it, so it
    /// did not draw that frame: nothing is believed until a self-test has (a probe that succeeds later must not open a
    /// path the check never looked at).
    held_back: bool,
    /// A probe is running, and is bounded to end by this.
    running: Option<Instant>,
}

/// One probe's memory and its single flight.
#[derive(Debug)]
pub(crate) struct Probe {
    slot: Mutex<Slot>,
    /// Signalled when a running probe ends.
    ended: Condvar,
}

impl Probe {
    pub(crate) const fn new() -> Self {
        Self {
            slot: Mutex::new(Slot {
                measured: None,
                retry_at: None,
                held_back: false,
                running: None,
            }),
            ended: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The answer if there is one to give now, else the turn to measure. A probe that is running is waited for (no longer
    /// than it is bounded to, plus [`GRACE`]); a backoff is not, unless `patience` reaches past it (the self-test's).
    fn enter(&self, mut now: Instant, timing: &Timing, patience: Option<Instant>) -> Entry<'_> {
        let mut s = self.lock();
        loop {
            if s.held_back {
                return Entry::Answer(Verdict::Unknown);
            }
            if let Some(known) = &s.measured {
                return Entry::Answer(known.clone());
            }
            if let Some(deadline) = s.running {
                let limit = deadline + GRACE;
                let at = Instant::now();
                if at >= limit {
                    // (A probe that outlived its own bound: "not yet".)
                    return Entry::Answer(Verdict::Unknown);
                }
                s = self
                    .ended
                    .wait_timeout(s, limit - at)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
                now = Instant::now();
                continue;
            }
            if let Some(retry) = s.retry_at.filter(|t| *t > now) {
                if patience.is_none_or(|until| retry > until) {
                    return Entry::Answer(Verdict::Unknown);
                }
                // Waited out on the condition variable, so that a probe another thread finishes meanwhile ends the wait.
                s = self
                    .ended
                    .wait_timeout(s, retry.saturating_duration_since(Instant::now()))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
                now = Instant::now();
                continue;
            }
            s.running = Some(Instant::now() + timing.overall);
            return Entry::Lead(Lead {
                probe: self,
                backoff: timing.backoff,
                finished: false,
            });
        }
    }
}

/// How much longer than its deadline a waiter waits for a probe that is running (its children are killed at the deadline).
const GRACE: Duration = Duration::from_millis(500);

enum Entry<'a> {
    Answer(Verdict),
    Lead(Lead<'a>),
}

/// The turn to run a probe. Ending it ([`Lead::finish`]) stores the answer and wakes the waiters; dropping it any other way
/// (the probe panicked) gives the place up and backs off, as a probe that could not tell does.
struct Lead<'a> {
    probe: &'a Probe,
    backoff: Duration,
    finished: bool,
}

impl Lead<'_> {
    fn finish(mut self, found: Verdict) -> Verdict {
        self.finished = true;
        let mut s = self.probe.lock();
        s.running = None;
        let out = match &s.measured {
            Some(known) => known.clone(),
            None => {
                if found == Verdict::Unknown {
                    s.retry_at = Some(Instant::now() + self.backoff);
                } else {
                    s.measured = Some(found.clone());
                }
                found
            }
        };
        drop(s);
        self.probe.ended.notify_all();
        out
    }
}

impl Drop for Lead<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut s = self.probe.lock();
        s.running = None;
        s.retry_at = Some(Instant::now() + self.backoff);
        drop(s);
        self.probe.ended.notify_all();
    }
}

/// What is known without running anything: the measured answer, else [`Verdict::Unknown`] (not measured yet, backing
/// off after a probe that failed, or held back by a self-test).
pub(crate) fn known(probe: &Probe) -> Verdict {
    let s = probe.lock();
    if s.held_back {
        return Verdict::Unknown;
    }
    s.measured.clone().unwrap_or(Verdict::Unknown)
}

/// The answer, from `run` on the first call (child processes: a blocking thread's, with no lock held). Callers that arrive
/// while it runs wait for it, boundedly, and take its answer ([`Probe`]); `run` is given the probe's [`Budget`]. An `Unknown`
/// is not remembered but for the backoff, inside which the probe is not run again.
pub(crate) fn cached(probe: &Probe, now: Instant, timing: Timing, run: impl FnOnce(&Budget) -> Verdict) -> Verdict {
    match probe.enter(now, &timing, None) {
        Entry::Answer(answer) => answer,
        Entry::Lead(lead) => lead.finish(run(&Budget::new(&timing))),
    }
}

/// How many times the self-test runs a probe that cannot tell: once, and once more after the backoff.
const SELF_TEST_ATTEMPTS: usize = 2;

/// [`cached`] for the runtime self-test, which draws this probe's frame only on a `Match`. An `Unknown` here holds the path
/// back for the process until a self-test that can tell has run, so that the frames the GPU draws are never ones its check
/// skipped. A `Differs` is permanent and holds nothing back.
///
/// The self-test does not take the backoff of an earlier failure (a probe a frame's `prepare` ran a moment before, which timed
/// out) for its verdict: it waits the backoff out and runs the probe, and one that cannot tell is run once more after the
/// backoff, so a transient failure does not hold the path back for the session. A probe that cannot tell twice, a backoff
/// apart, is held back.
pub(crate) fn checked(probe: &Probe, timing: Timing, mut run: impl FnMut(&Budget) -> Verdict) -> Verdict {
    probe.lock().held_back = false;
    let mut found = Verdict::Unknown;
    for _ in 0..SELF_TEST_ATTEMPTS {
        let patience = Some(Instant::now() + timing.backoff);
        found = match probe.enter(Instant::now(), &timing, patience) {
            Entry::Answer(answer) => answer,
            Entry::Lead(lead) => lead.finish(run(&Budget::new(&timing))),
        };
        if found != Verdict::Unknown {
            break;
        }
    }
    if found == Verdict::Unknown {
        probe.lock().held_back = true;
        tracing::warn!("a probe could not tell at the self-test: the path it decides stays with FFmpeg until a self-test can");
    }
    found
}

/// What is left of a probe's time: its one deadline, and the limit of one child.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Budget {
    deadline: Instant,
    per_run: Duration,
}

impl Budget {
    pub(crate) fn new(timing: &Timing) -> Self {
        Self {
            deadline: Instant::now() + timing.overall,
            per_run: timing.per_run,
        }
    }

    /// How long the next child may run: the shorter of its own limit and the time left.
    fn limit(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now()).min(self.per_run)
    }

    /// One FFmpeg run: `args` after the quiet flags and the thread caps of a one-frame read (a probe's graph is read like
    /// the compositor's per-layer decodes), `input` on its stdin, its stdout; `None` when it would not run, failed, outlasted
    /// [`Budget::limit`] (killed) or the probe's deadline had passed. A `-filter_complex` graph over the engine's argv rule
    /// ([`kerf_core::GRAPH_ARG_MAX`]) goes through a script file, as the engine's own long graphs do (Windows caps the whole
    /// command line at 32,767 characters). **Nothing waits on a child that will not end**: on the timeout path the side
    /// threads that serve the pipes are left to finish on their own (a wrapper script that did not `exec` keeps a pipe open
    /// past the kill, and a join would wait for it as long as the real FFmpeg lives).
    pub(crate) fn ffmpeg(&self, args: &[&str], input: Vec<u8>) -> Option<Vec<u8>> {
        let limit = self.limit();
        if limit.is_zero() {
            tracing::warn!("a probe ran out of time before its next FFmpeg run");
            return None;
        }
        let mut argv = argv(args);
        let _script = match kerf_core::spill_long_graph(&mut argv, "probe") {
            Ok(script) => script,
            Err(error) => {
                tracing::warn!(%error, "could not write a probe's graph to a script file");
                return None;
            }
        };
        let mut cmd: Command = kerf_core::ffmpeg_command();
        cmd.args(&argv);
        kerf_core::run_piped_until(&mut cmd, input, Instant::now() + limit)
    }
}

/// The argv of a probe's run: the quiet flags, `args`, and the thread caps of a frame read ([`kerf_core::limit_frame_args`]: the
/// budget's threads, as the grade's and the text's probes take them).
fn argv(args: &[&str]) -> Vec<String> {
    let mut argv: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]
        .into_iter()
        .chain(args.iter().copied())
        .map(String::from)
        .collect();
    kerf_core::limit_frame_args(&mut argv, 1);
    argv
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const BACKOFF: Duration = Duration::from_secs(30);
    const TIMING: Timing = Timing {
        backoff: BACKOFF,
        overall: Duration::from_secs(45),
        per_run: Duration::from_secs(20),
    };
    /// A probe that backs off for a moment only, for the tests that wait it out.
    const QUICK: Timing = Timing {
        backoff: Duration::from_millis(120),
        overall: Duration::from_secs(5),
        per_run: Duration::from_secs(2),
    };

    fn fresh() -> Probe {
        Probe::new()
    }

    #[test]
    fn nothing_is_known_before_a_probe_has_run() {
        assert_eq!(known(&fresh()), Verdict::Unknown);
    }

    #[test]
    fn an_unknown_is_asked_again_after_the_backoff_and_not_before() {
        let state = fresh();
        let t0 = Instant::now();
        assert_eq!(cached(&state, t0, TIMING, |_| Verdict::Unknown), Verdict::Unknown);
        assert_eq!(known(&state), Verdict::Unknown, "a failure is not an answer");
        // Inside the backoff the probe is not run again, whatever it would say.
        assert_eq!(
            cached(&state, t0 + Duration::from_secs(29), TIMING, |_| panic!(
                "probed inside the backoff"
            )),
            Verdict::Unknown
        );
        // After it, it is: the retry is the backoff from the failure, `Instant::now()` at the time of it.
        let later = Instant::now() + BACKOFF + Duration::from_secs(1);
        assert_eq!(cached(&state, later, TIMING, |_| Verdict::Match), Verdict::Match);
        assert_eq!(known(&state), Verdict::Match);
    }

    #[test]
    fn a_probe_that_fails_again_is_asked_again_later_and_may_succeed() {
        let state = fresh();
        let t0 = Instant::now();
        assert_eq!(cached(&state, t0, TIMING, |_| Verdict::Unknown), Verdict::Unknown);
        let later = Instant::now() + BACKOFF + Duration::from_secs(1);
        assert_eq!(cached(&state, later, TIMING, |_| Verdict::Unknown), Verdict::Unknown);
        assert_eq!(known(&state), Verdict::Unknown, "two failures are still no answer");
        let much_later = Instant::now() + 2 * BACKOFF + Duration::from_secs(2);
        assert_eq!(cached(&state, much_later, TIMING, |_| Verdict::Match), Verdict::Match);
        assert_eq!(known(&state), Verdict::Match);
    }

    #[test]
    fn a_match_and_a_difference_are_kept_for_good() {
        for answer in [Verdict::Match, Verdict::Differs("yuv444p: 12 samples".into())] {
            let state = fresh();
            let t0 = Instant::now();
            assert_eq!(cached(&state, t0, TIMING, |_| answer.clone()), answer);
            assert_eq!(known(&state), answer);
            // Asked again, now or hours later, nothing is run and the answer stands.
            for at in [t0, t0 + Duration::from_secs(3600)] {
                assert_eq!(cached(&state, at, TIMING, |_| panic!("measured twice")), answer);
            }
        }
    }

    #[test]
    fn the_probe_runs_with_no_lock_held() {
        let state = fresh();
        let got = cached(&state, Instant::now(), TIMING, |_| {
            assert!(state.slot.try_lock().is_ok(), "the state is locked while the probe runs");
            assert_eq!(known(&state), Verdict::Unknown, "and can be read meanwhile");
            Verdict::Match
        });
        assert_eq!(got, Verdict::Match);
        assert_eq!(known(&state), Verdict::Match);
    }

    /// Callers that arrive while a probe runs wait for it and take its answer; the probe runs once.
    #[test]
    fn concurrent_callers_share_one_probe_and_its_answer() {
        for answer in [Verdict::Match, Verdict::Differs("a case".into()), Verdict::Unknown] {
            let state = Arc::new(fresh());
            let runs = Arc::new(AtomicUsize::new(0));
            let threads: Vec<_> = (0..6)
                .map(|_| {
                    let (state, runs, answer) = (Arc::clone(&state), Arc::clone(&runs), answer.clone());
                    std::thread::spawn(move || {
                        cached(&state, Instant::now(), TIMING, |_| {
                            runs.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(200));
                            answer
                        })
                    })
                })
                .collect();
            for t in threads {
                assert_eq!(t.join().expect("a caller"), answer);
            }
            assert_eq!(
                runs.load(Ordering::SeqCst),
                1,
                "{answer:?}: the probe ran once for six callers"
            );
        }
    }

    /// A waiter is bounded by the probe's deadline (and a grace), not by the probe: a probe that outlives its bound is "not yet".
    #[test]
    fn a_waiter_does_not_wait_for_a_probe_longer_than_it_is_bounded_to() {
        let state = Arc::new(fresh());
        let quick = Timing {
            overall: Duration::from_millis(60),
            ..TIMING
        };
        let (go, started) = std::sync::mpsc::channel();
        let leader = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                cached(&state, Instant::now(), quick, |_| {
                    go.send(()).expect("announce");
                    std::thread::sleep(Duration::from_millis(2500));
                    Verdict::Match
                })
            })
        };
        started.recv().expect("the probe started");
        let began = Instant::now();
        assert_eq!(
            cached(&state, Instant::now(), quick, |_| panic!("ran beside a running probe")),
            Verdict::Unknown
        );
        assert!(began.elapsed() < Duration::from_millis(1500), "waited {:?}", began.elapsed());
        assert_eq!(leader.join().expect("the leader"), Verdict::Match);
        assert_eq!(known(&state), Verdict::Match, "the late answer still stands");
    }

    /// A probe that panics gives its place up: the next caller (after the backoff) is not left waiting for a probe that is gone.
    #[test]
    fn a_probe_that_panics_leaves_the_slot_free() {
        let state = fresh();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cached(&state, Instant::now(), QUICK, |_| panic!("a probe with a bug"))
        }));
        assert!(panicked.is_err());
        assert!(state.lock().running.is_none());
        assert_eq!(known(&state), Verdict::Unknown);
        std::thread::sleep(QUICK.backoff + Duration::from_millis(50));
        assert_eq!(cached(&state, Instant::now(), QUICK, |_| Verdict::Match), Verdict::Match);
    }

    #[test]
    fn a_self_test_that_cannot_tell_holds_the_path_back_until_one_can() {
        let state = fresh();
        let runs = AtomicUsize::new(0);
        let began = Instant::now();
        assert_eq!(
            checked(&state, QUICK, |_| {
                runs.fetch_add(1, Ordering::SeqCst);
                Verdict::Unknown
            }),
            Verdict::Unknown
        );
        assert_eq!(runs.load(Ordering::SeqCst), 2, "once, and once more after the backoff");
        assert!(began.elapsed() >= QUICK.backoff, "the second run waited the backoff out");
        // A probe that would succeed now is not believed (and not run): the check skipped the frame.
        std::thread::sleep(QUICK.backoff + Duration::from_millis(50));
        assert_eq!(
            cached(&state, Instant::now(), QUICK, |_| panic!("probed while held back")),
            Verdict::Unknown
        );
        assert_eq!(known(&state), Verdict::Unknown);
        // The next self-test that can tell lifts the hold, and the answer stands.
        assert_eq!(checked(&state, QUICK, |_| Verdict::Match), Verdict::Match);
        assert_eq!(known(&state), Verdict::Match);
        // A difference holds nothing back.
        let state = fresh();
        let d = Verdict::Differs("a case".into());
        assert_eq!(checked(&state, QUICK, |_| d.clone()), d);
        assert_eq!(known(&state), d);
    }

    /// One failure is not held for the session: the self-test tries again after the backoff, and a probe that can tell then
    /// is the self-test's (its frame is drawn).
    #[test]
    fn a_transient_failure_does_not_hold_the_path_back() {
        let state = fresh();
        let runs = AtomicUsize::new(0);
        let got = checked(&state, QUICK, |_| {
            if runs.fetch_add(1, Ordering::SeqCst) == 0 {
                Verdict::Unknown
            } else {
                Verdict::Match
            }
        });
        assert_eq!(got, Verdict::Match);
        assert_eq!(known(&state), Verdict::Match, "not held back");
    }

    /// An earlier probe's backoff (a frame's `prepare` that timed out a moment ago) is waited out by the self-test, not taken for
    /// its verdict.
    #[test]
    fn the_self_test_waits_out_the_backoff_of_an_earlier_failure_instead_of_holding_the_path() {
        let state = fresh();
        assert_eq!(cached(&state, Instant::now(), QUICK, |_| Verdict::Unknown), Verdict::Unknown);
        assert_eq!(
            cached(&state, Instant::now(), QUICK, |_| panic!("asked inside the backoff")),
            Verdict::Unknown
        );
        let began = Instant::now();
        assert_eq!(checked(&state, QUICK, |_| Verdict::Match), Verdict::Match);
        assert!(began.elapsed() < QUICK.backoff + Duration::from_secs(2));
        assert_eq!(known(&state), Verdict::Match);
    }

    /// A self-test waits for a probe that is running and takes its answer, rather than running another.
    #[test]
    fn the_self_test_takes_the_answer_of_a_probe_that_is_running() {
        let state = Arc::new(fresh());
        let (go, started) = std::sync::mpsc::channel();
        let leader = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                cached(&state, Instant::now(), TIMING, |_| {
                    go.send(()).expect("announce");
                    std::thread::sleep(Duration::from_millis(300));
                    Verdict::Match
                })
            })
        };
        started.recv().expect("the probe started");
        assert_eq!(
            checked(&state, TIMING, |_| panic!("ran beside a running probe")),
            Verdict::Match
        );
        assert_eq!(leader.join().expect("the leader"), Verdict::Match);
    }

    #[test]
    fn a_child_gets_the_shorter_of_its_limit_and_the_time_left() {
        let b = Budget::new(&Timing {
            backoff: BACKOFF,
            overall: Duration::from_secs(100),
            per_run: Duration::from_secs(20),
        });
        assert_eq!(b.limit(), Duration::from_secs(20));
        let b = Budget::new(&Timing {
            backoff: BACKOFF,
            overall: Duration::from_secs(3),
            per_run: Duration::from_secs(20),
        });
        assert!(
            b.limit() <= Duration::from_secs(3) && b.limit() > Duration::from_secs(1),
            "{:?}",
            b.limit()
        );
        // Out of time: nothing is started.
        let spent = Budget {
            deadline: Instant::now(),
            per_run: Duration::from_secs(20),
        };
        std::thread::sleep(Duration::from_millis(5));
        let began = Instant::now();
        assert_eq!(spent.ffmpeg(&["-version"], Vec::new()), None);
        assert!(began.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_probes_argv_is_the_quiet_flags_its_args_and_the_thread_caps_of_a_frame_read() {
        let args = [
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x36",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "pipe:1",
        ];
        let got = argv(&args);
        let mut want: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]
            .into_iter()
            .chain(args)
            .map(String::from)
            .collect();
        kerf_core::limit_frame_args(&mut want, 1);
        assert_eq!(got, want);
        assert_eq!(got.last().map(String::as_str), Some("pipe:1"), "the sink stays last");
        assert!(got.iter().any(|a| a == "-nostdin") && got.iter().any(|a| a == "testsrc2=size=64x36"));
    }

    /// A graph over the engine's argv rule goes through a script file (a probe is not exempt from the 32,767 characters
    /// Windows gives a command line), and an ordinary one stays in argv.
    #[test]
    fn a_long_graph_goes_through_a_script_file_and_a_short_one_stays() {
        let long = format!("[0:v]{}[out]", "null,".repeat(kerf_core::GRAPH_ARG_MAX / 5 + 10));
        assert!(long.len() > kerf_core::GRAPH_ARG_MAX);
        let mut spilled = argv(&["-i", "pipe:0", "-filter_complex", &long, "-map", "[out]", "pipe:1"]);
        let script = kerf_core::spill_long_graph(&mut spilled, "probe-test").expect("a script file");
        assert!(
            spilled.iter().all(|a| a.len() <= kerf_core::GRAPH_ARG_MAX),
            "an argument is still too long"
        );
        let at = spilled
            .iter()
            .position(|a| a == "-filter_complex_script" || a == "-/filter_complex")
            .expect("the graph is given as a file");
        assert_eq!(std::fs::read_to_string(&spilled[at + 1]).expect("the script"), long);
        let path = spilled[at + 1].clone();
        drop(script);
        assert!(!std::path::Path::new(&path).exists(), "the script is removed with the run");
        let mut short = argv(&["-i", "pipe:0", "-filter_complex", "[0:v]null[out]", "-map", "[out]", "pipe:1"]);
        let before = short.clone();
        let _none = kerf_core::spill_long_graph(&mut short, "probe-test").expect("nothing to spill");
        assert_eq!(short, before);
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
