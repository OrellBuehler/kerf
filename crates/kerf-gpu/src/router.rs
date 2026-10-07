//! Which decode run serves a request: the pure half of `FrameSource`'s scheduling.
//!
//! A **run** is one `ffmpeg` process decoding a file forward from a start point (`-ss`); every
//! frame it produces lands in the cache. Starting one costs a spawn, a seek and a decode up to
//! the wanted frame (about 90 ms for a 720p all-intra proxy, 260 ms for a long-GOP 1080p
//! original, against 2.5 to 3.6 ms a frame once it is streaming), so the router's whole job is
//! to say **when to read an existing run forward instead of starting another**. It is a pure
//! function of the runs that exist and the request, so every rule is a test and the stateful
//! half — processes, threads, deadlines — only has to do what it is told:
//!
//! * **Reuse** a run of the file that is *behind* the wanted frame by no more than
//!   [`reuse_window`] frames — 24 on a proxy, 96 on an original, which is where reading on stops
//!   being cheaper than restarting: 24 proxy frames at 3.6 ms are 86 ms against a 90 ms restart,
//!   96 original frames at 2.5 ms are 240 ms against 260. The nearest run behind the target wins.
//!   It reads forward and every frame it passes
//!   is cached, so the next request is a hit.
//! * Otherwise **start** a run: into a free slot (at most [`MAX_RUNS_PER_FILE`] of a file and
//!   [`MAX_RUNS`] in all) or by replacing the least recently used idle one — of the file when it
//!   is at its cap, of the whole process when that is. A request *behind every run of its file*
//!   is a backward move (a scrub, a loop), and starts [`BACKWARD_LEAD`] frames early so the few
//!   frames just before it are decoded too and the next step back is a hit.
//! * **`Exact`** (an agent asking for a frame) **never evicts a run**: it reuses an idle one that
//!   qualifies, and otherwise gets a [`Route::OneShot`] — a decode of its own that is not
//!   registered, for nothing to evict. Looking at footage must not interrupt a playback.
//! * **`Prefetch`** (the next clip's first frame) only ever uses a **spare** slot, and does
//!   nothing when a run already covers the frame.
//! * A run that is *busy* (serving a request, or held as a cursor) is neither reused nor evicted;
//!   if that leaves no slot, the answer is [`Route::Busy`].
//! * **Thrash guard** ([`ThrashGuard`]): a playback that keeps having to replace runs — more than
//!   [`MAX_RESTARTS_PER_SEC`] a second — is not going to keep up, and the caller should render
//!   with FFmpeg's stream instead. Only routes that destroy a run count
//!   ([`Route::replaces`]); filling a free slot is how a 6-layer frame starts.

use std::collections::VecDeque;

use crate::frame_cache::SourceId;

/// Runs one file may have at once (two clips of one file, and one spare).
pub const MAX_RUNS_PER_FILE: usize = 3;
/// Runs in the whole process.
pub const MAX_RUNS: usize = 6;
/// A request behind every run of its file starts this many frames early.
pub const BACKWARD_LEAD: u32 = 15;
/// More run replacements than this in a second on a `Forward` request is `Busy`.
pub const MAX_RESTARTS_PER_SEC: usize = 4;

/// The frames a run on a **proxy** is read forward to serve a request rather than restarted:
/// 24 of them at 3.6 ms is 86 ms, a restart 90 (finding 9 of the design note).
pub const REUSE_WINDOW_PROXY: u32 = 24;
/// The same for an **original**: 96 frames at 2.5 ms is 240 ms, a restart 260.
pub const REUSE_WINDOW_ORIGINAL: u32 = 96;

/// The frames a run may be read forward to serve a request, rather than restarted.
pub const fn reuse_window(proxy: bool) -> u32 {
    if proxy {
        REUSE_WINDOW_PROXY
    } else {
        REUSE_WINDOW_ORIGINAL
    }
}

/// A run, by an id the owner chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunId(pub u64);

/// What the router knows of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunState {
    pub id: RunId,
    pub file: SourceId,
    /// Where the run is: the tick just past the last frame it has produced, or its seek tick
    /// before the first. A request at or past it can be reached by reading on.
    pub head: i64,
    /// Larger is more recent.
    pub last_used: u64,
    /// Serving a request or held by a cursor: not to be reused or evicted.
    pub busy: bool,
}

/// Why a frame is wanted, which is how far the router may go for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// A frame to look at, once (an agent's `get_frame`): never takes a run from anyone.
    Exact,
    /// The playhead is being dragged: restart freely, start early when going back.
    Scrub,
    /// Playback or export, frame after frame: restart as needed, but not without limit.
    Forward,
    /// A frame wanted soon (the next clip's first): only a spare slot.
    Prefetch,
}

/// One frame wanted of one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    pub file: SourceId,
    /// The frame's tick (its `-ss`, rounded).
    pub target: i64,
    /// Ticks in one frame of this file (what the window is counted in); at least 1.
    pub frame_ticks: i64,
    /// The file is a proxy ([`reuse_window`]).
    pub proxy: bool,
    pub intent: Intent,
}

/// What to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Read this run forward to the target.
    Reuse(RunId),
    /// Start a run `lead` frames before the target, first stopping `evict` if given.
    Start { evict: Option<RunId>, lead: u32 },
    /// Stop this run and start another in its place (the file's least recently used), `lead`
    /// frames before the target.
    Restart { run: RunId, lead: u32 },
    /// Decode the frame on its own and register nothing (an `Exact` that no run can serve).
    OneShot,
    /// Nothing to do (a prefetch that is covered, or has no spare slot).
    Skip,
    /// Every run that could be replaced is in use.
    Busy,
}

impl Route {
    /// This route stops a run that is alive: what [`ThrashGuard`] counts.
    pub fn replaces(&self) -> bool {
        matches!(self, Route::Restart { .. } | Route::Start { evict: Some(_), .. })
    }
}

/// The least recently used run that is not in use.
fn oldest_idle<'a>(runs: impl Iterator<Item = &'a RunState>) -> Option<RunId> {
    runs.filter(|r| !r.busy).min_by_key(|r| r.last_used).map(|r| r.id)
}

/// Route `req` over the runs that exist (all files', `runs`).
pub fn route(runs: &[RunState], req: &Request) -> Route {
    let window = i64::from(reuse_window(req.proxy)) * req.frame_ticks.max(1);
    let file = req.file;
    let mine = || runs.iter().filter(move |r| r.file == file);
    // The idle run nearest behind the target that can read forward to it.
    let reachable = mine()
        .filter(|r| !r.busy && r.head <= req.target && req.target - r.head <= window)
        .max_by_key(|r| (r.head, r.last_used));
    if let Some(run) = reachable {
        return if req.intent == Intent::Prefetch {
            Route::Skip
        } else {
            Route::Reuse(run.id)
        };
    }
    let free = mine().count() < MAX_RUNS_PER_FILE && runs.len() < MAX_RUNS;
    match req.intent {
        Intent::Exact => Route::OneShot,
        Intent::Prefetch if free => Route::Start { evict: None, lead: 0 },
        Intent::Prefetch => Route::Skip,
        Intent::Scrub | Intent::Forward => {
            let lead = if mine().count() > 0 && mine().all(|r| req.target < r.head) {
                BACKWARD_LEAD
            } else {
                0
            };
            if free {
                Route::Start { evict: None, lead }
            } else if mine().count() >= MAX_RUNS_PER_FILE {
                oldest_idle(mine()).map_or(Route::Busy, |run| Route::Restart { run, lead })
            } else {
                oldest_idle(runs.iter()).map_or(Route::Busy, |run| Route::Start { evict: Some(run), lead })
            }
        }
    }
}

/// Counts run replacements in a sliding second. Time is the caller's (seconds on any
/// monotonic clock), so the rule is testable without waiting.
#[derive(Debug, Default)]
pub struct ThrashGuard {
    at: VecDeque<f64>,
}

/// Too many runs have been replaced too fast: render with FFmpeg's stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy;

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "decoding keeps restarting (more than {MAX_RESTARTS_PER_SEC} a second)")
    }
}

impl std::error::Error for Busy {}

impl ThrashGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// A run is about to be replaced at `now`: `Err(Busy)` when that would be more than
    /// [`MAX_RESTARTS_PER_SEC`] in the last second. A refusal is not counted, so a caller that
    /// stops asking recovers a second later.
    pub fn restart(&mut self, now: f64) -> Result<(), Busy> {
        while self.at.front().is_some_and(|&t| now - t >= 1.0) {
            self.at.pop_front();
        }
        if self.at.len() >= MAX_RESTARTS_PER_SEC {
            return Err(Busy);
        }
        self.at.push_back(now);
        Ok(())
    }

    /// [`ThrashGuard::restart`] for `route`, when it matters: only a `Forward` request that
    /// replaces a run is counted (a scrub restarts as fast as the hand moves).
    pub fn check(&mut self, intent: Intent, route: &Route, now: f64) -> Result<(), Busy> {
        if intent == Intent::Forward && route.replaces() {
            self.restart(now)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame_cache::DecodeFormat;

    const fn file(n: u64) -> SourceId {
        SourceId {
            file: n,
            format: DecodeFormat::Yuv420p8,
        }
    }
    const F: SourceId = file(1);
    const G: SourceId = file(2);

    /// Ticks in a frame throughout: 100 (10 fps on 1/1000 would be 100).
    const FT: i64 = 100;

    fn run(id: u64, of: SourceId, head: i64, last_used: u64) -> RunState {
        RunState {
            id: RunId(id),
            file: of,
            head,
            last_used,
            busy: false,
        }
    }

    fn busy(mut r: RunState) -> RunState {
        r.busy = true;
        r
    }

    fn ask(runs: &[RunState], of: SourceId, target: i64, proxy: bool, intent: Intent) -> Route {
        route(
            runs,
            &Request {
                file: of,
                target,
                frame_ticks: FT,
                proxy,
                intent,
            },
        )
    }

    #[test]
    fn a_run_behind_the_target_within_the_window_is_read_forward() {
        let runs = [run(1, F, 1000, 1)];
        // At the head, and up to 96 frames (9600 ticks) on an original, 24 (2400) on a proxy.
        assert_eq!(ask(&runs, F, 1000, false, Intent::Forward), Route::Reuse(RunId(1)));
        assert_eq!(ask(&runs, F, 1000 + 96 * FT, false, Intent::Forward), Route::Reuse(RunId(1)));
        assert_eq!(ask(&runs, F, 1000 + 24 * FT, true, Intent::Scrub), Route::Reuse(RunId(1)));
        assert_eq!((REUSE_WINDOW_ORIGINAL, REUSE_WINDOW_PROXY), (96, 24));
        assert_eq!((reuse_window(false), reuse_window(true)), (96, 24));
        // One frame past it is a restart, and with a slot free that is a new run, not a kill.
        let past = ask(&runs, F, 1000 + 97 * FT, false, Intent::Forward);
        assert_eq!(past, Route::Start { evict: None, lead: 0 });
        assert_eq!(
            ask(&runs, F, 1000 + 25 * FT, true, Intent::Forward),
            Route::Start { evict: None, lead: 0 }
        );
        // A run another file owns, or one *ahead* of the target, cannot serve it.
        assert_eq!(
            ask(&runs, G, 1000, false, Intent::Forward),
            Route::Start { evict: None, lead: 0 }
        );
        assert_eq!(
            ask(&runs, F, 999, false, Intent::Forward),
            Route::Start {
                evict: None,
                lead: BACKWARD_LEAD
            }
        );
    }

    #[test]
    fn the_nearest_run_behind_wins_and_a_busy_one_is_not_reused() {
        let runs = [run(1, F, 0, 9), run(2, F, 5000, 1), run(3, F, 9000, 2)];
        assert_eq!(ask(&runs, F, 5500, false, Intent::Forward), Route::Reuse(RunId(2)));
        assert_eq!(ask(&runs, F, 9000, false, Intent::Forward), Route::Reuse(RunId(3)));
        // Run 2 is held: run 1, 55 frames back, is within the window and serves.
        let held = [run(1, F, 0, 9), busy(run(2, F, 5000, 1))];
        assert_eq!(ask(&held, F, 5500, false, Intent::Forward), Route::Reuse(RunId(1)));
        // Equal heads: the more recently used.
        let tie = [run(1, F, 100, 3), run(2, F, 100, 7)];
        assert_eq!(ask(&tie, F, 150, false, Intent::Forward), Route::Reuse(RunId(2)));
    }

    #[test]
    fn a_file_at_its_cap_restarts_its_least_recently_used_idle_run() {
        let runs = [run(1, F, 0, 5), run(2, F, 100_000, 2), run(3, F, 200_000, 9)];
        // Far from all three, going forward: the oldest of the file is replaced, no lead.
        assert_eq!(
            ask(&runs, F, 900_000, false, Intent::Forward),
            Route::Restart { run: RunId(2), lead: 0 }
        );
        // Behind all three: a backward move, so it starts early.
        assert_eq!(
            ask(&runs, F, -5000, false, Intent::Scrub),
            Route::Restart {
                run: RunId(2),
                lead: BACKWARD_LEAD
            }
        );
        // Between runs is not backward.
        assert_eq!(
            ask(&runs, F, 150_000 + 97 * FT * 10, false, Intent::Scrub),
            Route::Restart { run: RunId(2), lead: 0 }
        );
        // The oldest being busy, the next oldest is taken; all busy: nothing can be replaced.
        let some_busy = [busy(run(1, F, 0, 5)), busy(run(2, F, 100_000, 2)), run(3, F, 200_000, 9)];
        assert_eq!(
            ask(&some_busy, F, 900_000, false, Intent::Forward),
            Route::Restart { run: RunId(3), lead: 0 }
        );
        let all_busy = [
            busy(run(1, F, 0, 5)),
            busy(run(2, F, 100_000, 2)),
            busy(run(3, F, 200_000, 9)),
        ];
        assert_eq!(ask(&all_busy, F, 900_000, false, Intent::Forward), Route::Busy);
    }

    #[test]
    fn the_process_cap_replaces_the_least_recently_used_idle_run_of_any_file() {
        let runs = [
            run(1, G, 0, 4),
            run(2, G, 0, 1),
            run(3, G, 0, 8),
            run(4, F, 0, 6),
            run(5, F, 0, 7),
            run(6, file(3), 0, 3),
        ];
        assert_eq!(runs.len(), MAX_RUNS);
        // F has two runs (room for a third) but the process has six: take the oldest overall.
        assert_eq!(
            ask(&runs, F, 900_000, false, Intent::Forward),
            Route::Start {
                evict: Some(RunId(2)),
                lead: 0
            }
        );
        // Busy runs are skipped, even the oldest.
        let mut held = runs;
        held[1].busy = true;
        assert_eq!(
            ask(&held, F, 900_000, false, Intent::Forward),
            Route::Start {
                evict: Some(RunId(6)),
                lead: 0
            }
        );
        for r in &mut held {
            r.busy = true;
        }
        assert_eq!(ask(&held, F, 900_000, false, Intent::Forward), Route::Busy);
    }

    #[test]
    fn an_exact_frame_never_takes_a_run_from_anyone() {
        // Reuse of an idle run that is in reach is fine; anything else is a decode of its own.
        let runs = [run(1, F, 0, 1)];
        assert_eq!(ask(&runs, F, 500, false, Intent::Exact), Route::Reuse(RunId(1)));
        assert_eq!(ask(&runs, F, 5_000_000, false, Intent::Exact), Route::OneShot);
        assert_eq!(ask(&[], F, 0, false, Intent::Exact), Route::OneShot);
        // ...whatever the state of the slots: a full process, a busy run in reach.
        let full: Vec<RunState> = (0..MAX_RUNS as u64).map(|i| run(i, G, 0, i)).collect();
        assert_eq!(ask(&full, F, 0, false, Intent::Exact), Route::OneShot);
        assert_eq!(ask(&[busy(run(1, F, 0, 1))], F, 500, false, Intent::Exact), Route::OneShot);
        // Never a route that destroys a run: sweep heads, targets, caps, busy flags.
        for heads in [vec![], vec![0], vec![0, 7000], vec![0, 7000, 400_000]] {
            for target in [-300, 0, 50, 9600, 9601, 500_000, 9_000_000] {
                for busy_all in [false, true] {
                    let mut runs: Vec<RunState> = heads
                        .iter()
                        .enumerate()
                        .map(|(i, h)| run(i as u64, F, *h, i as u64))
                        .collect();
                    runs.extend((10..13).map(|i| run(i, G, 0, 0)));
                    for r in &mut runs {
                        r.busy = busy_all;
                    }
                    let r = ask(&runs, F, target, false, Intent::Exact);
                    assert!(!r.replaces() && !matches!(r, Route::Start { .. }), "{r:?}");
                }
            }
        }
    }

    #[test]
    fn a_prefetch_uses_a_spare_slot_or_nothing() {
        // Covered by a run already: nothing to do.
        assert_eq!(ask(&[run(1, F, 0, 1)], F, 500, false, Intent::Prefetch), Route::Skip);
        // Not covered, a slot free: start there, no lead, nobody evicted.
        assert_eq!(
            ask(&[run(1, F, 0, 1)], F, 900_000, false, Intent::Prefetch),
            Route::Start { evict: None, lead: 0 }
        );
        assert_eq!(ask(&[], F, 0, false, Intent::Prefetch), Route::Start { evict: None, lead: 0 });
        // The file at its cap, or the process: skip, however old the runs.
        let file_full = [run(1, F, 0, 1), run(2, F, 0, 2), run(3, F, 0, 3)];
        assert_eq!(ask(&file_full, F, 900_000, false, Intent::Prefetch), Route::Skip);
        let proc_full: Vec<RunState> = (0..MAX_RUNS as u64).map(|i| run(i, G, 0, i)).collect();
        assert_eq!(ask(&proc_full, F, 900_000, false, Intent::Prefetch), Route::Skip);
        // A prefetch behind a run is a backward move, which is not one a spare slot makes early.
        assert_eq!(
            ask(&[run(1, F, 5000, 1)], F, 0, false, Intent::Prefetch),
            Route::Start { evict: None, lead: 0 }
        );
    }

    #[test]
    fn a_free_slot_is_filled_with_no_one_evicted_so_many_layers_can_start_at_once() {
        // A cut with six layers of six files starts six runs in one frame.
        let mut runs = Vec::new();
        for i in 0..MAX_RUNS as u64 {
            let r = ask(&runs, file(100 + i), 0, true, Intent::Forward);
            assert_eq!(r, Route::Start { evict: None, lead: 0 }, "layer {i}");
            assert!(!r.replaces());
            runs.push(run(i, file(100 + i), 0, i));
        }
        // The first run, per file: no lead, there is no run to be behind.
        assert_eq!(
            ask(&[], F, -1000, false, Intent::Scrub),
            Route::Start { evict: None, lead: 0 }
        );
    }

    #[test]
    fn more_than_four_replacements_in_a_second_is_busy_and_a_refusal_is_not_counted() {
        let mut g = ThrashGuard::new();
        // (Quarter seconds: exact in binary, so the edges of the window are exact too.)
        for t in [10.0, 10.25, 10.5, 10.75] {
            assert_eq!(g.restart(t), Ok(()), "restart at {t}");
        }
        assert_eq!(MAX_RESTARTS_PER_SEC, 4);
        assert_eq!(g.restart(10.875), Err(Busy));
        assert_eq!(g.restart(10.9375), Err(Busy), "still the same second");
        // The window slides: the restart at 10.0 is a second old at 11.0 and no longer counts.
        assert_eq!(g.restart(11.0), Ok(()));
        assert_eq!(g.restart(11.0), Err(Busy), "10.25, 10.5, 10.75 and 11.0 are within a second");
        assert_eq!(g.restart(11.25), Ok(()), "and the refusals above were never counted");
        // A pause empties it.
        assert_eq!(g.restart(60.0), Ok(()));
        assert!(Busy.to_string().contains('4'));
        // What the caller gets: the error that says "render this one through FFmpeg".
        let err = crate::gpu::GpuError::from(Busy);
        assert!(
            matches!(&err, crate::gpu::GpuError::Busy(why) if why.contains("restarting")),
            "{err}"
        );
    }

    #[test]
    fn only_a_forward_request_that_replaces_a_run_counts() {
        let restart = Route::Restart { run: RunId(1), lead: 0 };
        let evicting = Route::Start {
            evict: Some(RunId(1)),
            lead: 0,
        };
        let filling = Route::Start { evict: None, lead: 0 };
        assert!(restart.replaces() && evicting.replaces());
        for r in [filling, Route::Reuse(RunId(1)), Route::OneShot, Route::Skip, Route::Busy] {
            assert!(!r.replaces(), "{r:?}");
        }
        let mut g = ThrashGuard::new();
        for _ in 0..20 {
            assert_eq!(g.check(Intent::Forward, &filling, 1.0), Ok(()));
            assert_eq!(g.check(Intent::Forward, &Route::Reuse(RunId(1)), 1.0), Ok(()));
            assert_eq!(g.check(Intent::Scrub, &restart, 1.0), Ok(()), "a drag is not playback");
            assert_eq!(g.check(Intent::Exact, &Route::OneShot, 1.0), Ok(()));
        }
        for _ in 0..MAX_RESTARTS_PER_SEC {
            assert_eq!(g.check(Intent::Forward, &restart, 2.0), Ok(()));
        }
        assert_eq!(g.check(Intent::Forward, &evicting, 2.0), Err(Busy));
    }

    /// Plays requests through the router the way `FrameSource` will: a run is read forward to the
    /// target (its head moves one frame past it), a start or restart puts one at the target (less
    /// the lead), and the guard is asked at the wall-clock second. Returns (starts, restarts,
    /// reuses, the time of the first refusal).
    fn play(requests: &[(f64, SourceId, i64)], intent: Intent) -> (usize, usize, usize, Option<f64>) {
        let (mut runs, mut next, mut guard) = (Vec::<RunState>::new(), 0u64, ThrashGuard::new());
        let (mut starts, mut restarts, mut reuses) = (0, 0, 0);
        for (clock, &(now, of, target)) in requests.iter().enumerate() {
            let clock = clock as u64 + 1;
            // A frame the cache holds is never asked about: one a run has already passed is a hit.
            if runs
                .iter()
                .any(|r| r.file == of && r.head > target && target >= r.head - 30 * FT)
            {
                continue;
            }
            let r = ask(&runs, of, target, true, intent);
            if guard.check(intent, &r, now).is_err() {
                return (starts, restarts, reuses, Some(now));
            }
            let at = |lead: u32| target - i64::from(lead) * FT + FT;
            match r {
                Route::Reuse(id) => {
                    reuses += 1;
                    let run = runs.iter_mut().find(|r| r.id == id).unwrap();
                    (run.head, run.last_used) = (target + FT, clock);
                }
                Route::Start { evict, lead } => {
                    starts += 1;
                    runs.retain(|r| Some(r.id) != evict);
                    next += 1;
                    runs.push(run(next, of, at(lead), clock));
                }
                Route::Restart { run: id, lead } => {
                    restarts += 1;
                    runs.retain(|r| r.id != id);
                    next += 1;
                    runs.push(run(next, of, at(lead), clock));
                }
                other => panic!("{other:?}"),
            }
        }
        (starts, restarts, reuses, None)
    }

    #[test]
    fn playback_is_one_run_per_clip_and_never_a_restart() {
        // 500 frames at 30 fps of one clip, then the same at twice the speed (every other frame).
        let one: Vec<_> = (0..500).map(|k| (k as f64 / 30.0, F, 5000 + k * FT)).collect();
        assert_eq!(play(&one, Intent::Forward), (1, 0, 499, None));
        let double: Vec<_> = (0..500).map(|k| (k as f64 / 30.0, F, 5000 + 2 * k * FT)).collect();
        assert_eq!(play(&double, Intent::Forward), (1, 0, 499, None));
        // Two layers of one file, a minute apart in the source, interleaved: two runs, no restarts.
        let two: Vec<_> = (0..500)
            .flat_map(|k| [(k as f64 / 30.0, F, k * FT), (k as f64 / 30.0, F, 600_000 + k * FT)])
            .collect();
        assert_eq!(play(&two, Intent::Forward), (2, 0, 998, None));
        // A jump (a seek, or the next clip) is one start.
        let jump: Vec<_> = (0..100)
            .map(|k| (k as f64 / 30.0, F, k * FT + if k < 50 { 0 } else { 9_000_000 }))
            .collect();
        assert_eq!(play(&jump, Intent::Forward), (2, 0, 98, None));
    }

    #[test]
    fn four_layers_of_one_file_thrash_and_the_guard_hands_the_frame_back() {
        // Four widely spaced positions in one file, a frame each in turn: the file keeps three
        // runs, so every request restarts one. The fifth within a second is refused.
        let layers: Vec<_> = (0..120).map(|i| (i as f64 / 30.0, F, (i % 4) * 5_000_000)).collect();
        let (starts, restarts, _, refused) = play(&layers, Intent::Forward);
        assert_eq!(starts, 3, "the cap");
        assert_eq!(restarts, MAX_RESTARTS_PER_SEC, "four restarts, then Busy");
        assert!(refused.is_some_and(|t| t < 1.0), "{refused:?}");
        // A scrub is not playback: it is never refused, and restarts all the way.
        let (_, restarts, _, refused) = play(&layers, Intent::Scrub);
        assert_eq!((restarts, refused), (117, None));
    }
}
