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
//!   [`effective_window`] frames: [`REUSE_WINDOW_PROXY`] (24) or [`REUSE_WINDOW_ORIGINAL`] (96),
//!   which is where reading on stops being cheaper than restarting (24 proxy frames at 3.6 ms are
//!   86 ms against a 90 ms restart, 96 original frames at 2.5 ms are 240 ms against 260) —
//!   **and at most half of what the frame cache holds**. The two have to be related: on 1080p the
//!   default cap holds 86 frames, so a 96-frame read-forward would push every other layer's
//!   frames out and the next request would restart a run to get one back (ping-pong). The nearest
//!   run behind the target wins. It reads forward and every frame it **keeps** is cached; the
//!   ones it only *passes* (before [`Request::warm_from`]) go in cold (`FrameCache::insert_cold`:
//!   free room only, never a warm frame's place), so a long read cannot flush anything either.
//! * Otherwise **start** a run: into a free slot (at most [`MAX_RUNS_PER_FILE`] of a file and
//!   [`MAX_RUNS`] in all) or by replacing the least recently used idle one — of the file when it
//!   is at its cap, of the whole process when that is. A request *behind every run of its file*
//!   is a backward move (a scrub, a loop), and starts [`BACKWARD_LEAD`] frames early so the few
//!   frames just before it are decoded too and the next step back is a hit.
//! * **`Exact`** (an agent asking for a frame) **never evicts a run**: it reuses an idle one that
//!   qualifies, and otherwise gets a [`Route::OneShot`] — a decode of its own that is not
//!   registered, for nothing to evict. Looking at footage must not interrupt a playback. (The
//!   owner bounds how many one-shots run at once: that is a semaphore, not a routing decision.)
//! * **`Prefetch`** (the next clip's first frame) only ever uses a **spare** slot, and does
//!   nothing when a run already covers the frame.
//! * A run that is *busy* (serving a request, or held as a cursor) is neither reused nor evicted;
//!   if that leaves no slot, the answer is [`Route::Busy`].
//! * **Thrash guard** ([`ThrashGuard`]): a playback that spends most of its time restarting
//!   runs is not going to keep up, and the caller should render with FFmpeg's stream instead. It
//!   weighs restarts by what they **cost** (restart-seconds in the last second, so a 90 ms proxy
//!   restart and a 260 ms original one are not the same), counts a run that **died or produced
//!   nothing** (it frees its slot at once and would never be counted otherwise), and does not
//!   count the replacement of a **stale** run of *another* file (a montage of more than six
//!   files evicts a finished clip's run at every cut: that is not ping-pong).

use std::collections::VecDeque;

use crate::frame_cache::SourceId;

/// Runs one file may have at once (two clips of one file, and one spare).
pub const MAX_RUNS_PER_FILE: usize = 3;
/// Runs in the whole process.
pub const MAX_RUNS: usize = 6;
/// A request behind every run of its file starts this many frames early.
pub const BACKWARD_LEAD: u32 = 15;

/// The frames a run on a **proxy** is read forward to serve a request rather than restarted:
/// 24 of them at 3.6 ms is 86 ms, a restart 90 (finding 9 of the design note).
pub const REUSE_WINDOW_PROXY: u32 = 24;
/// The same for an **original**: 96 frames at 2.5 ms is 240 ms, a restart 260.
pub const REUSE_WINDOW_ORIGINAL: u32 = 96;

/// The frames a run may be read forward to serve a request, by the cost of doing it alone.
pub const fn reuse_window(proxy: bool) -> u32 {
    if proxy {
        REUSE_WINDOW_PROXY
    } else {
        REUSE_WINDOW_ORIGINAL
    }
}

/// [`reuse_window`], held to **half of what the cache holds** (`cache_cap / frame_bytes / 2`
/// frames): a read-forward must never be able to turn over the whole cache. An unknown frame
/// size (`0`) leaves the window as it was.
pub fn effective_window(proxy: bool, cache_cap: usize, frame_bytes: usize) -> u32 {
    let window = reuse_window(proxy);
    if frame_bytes == 0 {
        return window;
    }
    u32::try_from(cache_cap / frame_bytes / 2).map_or(window, |held| held.min(window))
}

/// The span of time the guard weighs restarts over.
pub const THRASH_WINDOW: f64 = 1.0;
/// Restart-seconds within [`THRASH_WINDOW`] at which a `Forward` start is refused.
pub const THRASH_BUSY_SECS: f64 = 0.75;
/// What a run that died, or produced no frame, is charged at least: a spawn and a failed seek.
pub const FAILED_RUN_COST: f64 = 0.25;
/// A run idle for this long (the guard's window: it took no part in the last second) is stale.
pub const STALE_IDLE_SECS: f64 = THRASH_WINDOW;

/// A run, by an id the owner chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunId(pub u64);

/// What the router knows of a run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunState {
    pub id: RunId,
    pub file: SourceId,
    /// Where the run is: the tick just past the last frame it has produced, or its seek tick
    /// before the first. A request at or past it can be reached by reading on.
    pub head: i64,
    /// Seconds since the run last served a request or produced a frame that was wanted.
    /// Larger is older: the run to replace is the one with the most.
    pub idle: f64,
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
    /// Bytes of one decoded frame of this file (`w * h * 3 / 2`) and the cache's cap: they
    /// bound the window ([`effective_window`]).
    pub frame_bytes: usize,
    pub cache_cap: usize,
}

impl Request {
    /// The frames a run may be read forward to reach this target.
    pub fn window(&self) -> u32 {
        effective_window(self.proxy, self.cache_cap, self.frame_bytes)
    }

    /// The first tick the run's frames are **kept** from, for the route taken: the target less
    /// the backward lead of a run that is started early; the target itself for a run read
    /// forward. A frame the run produces *before* it was only passed over, and goes into the
    /// cache with `FrameCache::insert_cold`; one at or after it with `insert`.
    pub fn warm_from(&self, route: &Route) -> i64 {
        let lead = match *route {
            Route::Start { lead, .. } | Route::Restart { lead, .. } => i64::from(lead),
            _ => 0,
        };
        self.target.saturating_sub(lead.saturating_mul(self.frame_ticks.max(1)))
    }
}

/// A run to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Eviction {
    pub run: RunId,
    /// It belongs to another file and has not been used for [`STALE_IDLE_SECS`]: nothing
    /// wants it back, so replacing it is not thrash.
    pub stale: bool,
}

/// What to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Read this run forward to the target.
    Reuse(RunId),
    /// Start a run `lead` frames before the target, first stopping `evict` if given.
    Start { evict: Option<Eviction>, lead: u32 },
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
    /// This route starts a process.
    pub fn starts_a_run(&self) -> bool {
        matches!(self, Route::Start { .. } | Route::Restart { .. })
    }

    /// This route stops a run that is alive.
    pub fn replaces(&self) -> bool {
        matches!(self, Route::Restart { .. } | Route::Start { evict: Some(_), .. })
    }

    /// This route is a restart in the sense [`ThrashGuard`] counts: it stops a run somebody may
    /// want back. Filling a free slot is not one, nor is replacing a stale run of another file.
    pub fn is_thrash(&self) -> bool {
        match self {
            Route::Restart { .. } => true,
            Route::Start { evict: Some(e), .. } => !e.stale,
            _ => false,
        }
    }
}

/// The idle run that has been idle longest.
fn oldest_idle<'a>(runs: impl Iterator<Item = &'a RunState>) -> Option<&'a RunState> {
    runs.filter(|r| !r.busy).max_by(|a, b| a.idle.total_cmp(&b.idle))
}

/// Route `req` over the runs that exist (all files', `runs`).
pub fn route(runs: &[RunState], req: &Request) -> Route {
    let window = i64::from(req.window()).saturating_mul(req.frame_ticks.max(1));
    let file = req.file;
    let mine = || runs.iter().filter(move |r| r.file == file);
    // The idle run nearest behind the target that can read forward to it (then the one used last).
    let reachable = mine()
        .filter(|r| !r.busy && r.head <= req.target && req.target.saturating_sub(r.head) <= window)
        .max_by(|a, b| a.head.cmp(&b.head).then(b.idle.total_cmp(&a.idle)));
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
                oldest_idle(mine()).map_or(Route::Busy, |r| Route::Restart { run: r.id, lead })
            } else {
                oldest_idle(runs.iter()).map_or(Route::Busy, |r| Route::Start {
                    evict: Some(Eviction {
                        run: r.id,
                        stale: r.file != req.file && r.idle >= STALE_IDLE_SECS,
                    }),
                    lead,
                })
            }
        }
    }
}

/// Too much of the last second went to restarting decodes: render with FFmpeg's stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy;

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "decoding spent {THRASH_BUSY_SECS} s of the last {THRASH_WINDOW} s restarting"
        )
    }
}

impl std::error::Error for Busy {}

/// How a start ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    /// The run delivered a frame.
    Frame,
    /// It died, or ended without one.
    Failed,
}

/// A start the guard let through: hand it back to [`ThrashGuard::finished`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ticket {
    start: f64,
    /// The interval in the guard, when the start is one it counts.
    id: Option<u64>,
}

struct Interval {
    id: u64,
    start: f64,
    /// `None` while the restart is still under way.
    end: Option<f64>,
}

/// Weighs the time `Forward` requests spend restarting runs. Time is the caller's (seconds on any
/// monotonic clock), so every rule is testable without waiting.
///
/// A restart is an interval, open from the moment a counted route is taken ([`Route::is_thrash`])
/// until the run delivers its first frame ([`ThrashGuard::finished`]); a start that **failed**
/// is charged [`FAILED_RUN_COST`] at least, whatever the route (a run that dies at once frees
/// its slot, so the next request fills it again, for free, forever). The cost is the restart
/// time inside the last [`THRASH_WINDOW`] — an interval still open counts up to now — and a
/// start is refused ([`Busy`]) when it reaches [`THRASH_BUSY_SECS`]. A refusal is not recorded,
/// so a caller that stops asking recovers within the window. Only `Forward` is guarded (a scrub
/// restarts as fast as the hand moves, and an `Exact` frame starts nothing registered).
#[derive(Default)]
pub struct ThrashGuard {
    intervals: VecDeque<Interval>,
    next_id: u64,
}

impl ThrashGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Restart-seconds inside the window ending at `now`.
    pub fn cost(&self, now: f64) -> f64 {
        let from = now - THRASH_WINDOW;
        self.intervals
            .iter()
            .map(|i| (i.end.unwrap_or(now).min(now) - i.start.max(from)).max(0.0))
            .sum()
    }

    /// A start is about to be made on `route` for a request of `intent`: `Err(Busy)` if the last
    /// second already went to restarts, else a [`Ticket`] to hand back to
    /// [`ThrashGuard::finished`] (`None` when the route starts nothing, or the request is not
    /// `Forward`).
    pub fn check(&mut self, intent: Intent, route: &Route, now: f64) -> Result<Option<Ticket>, Busy> {
        if intent != Intent::Forward || !route.starts_a_run() {
            return Ok(None);
        }
        let from = now - THRASH_WINDOW;
        self.intervals.retain(|i| i.end.is_none_or(|e| e > from));
        if self.cost(now) >= THRASH_BUSY_SECS {
            return Err(Busy);
        }
        let id = route.is_thrash().then(|| {
            self.next_id += 1;
            self.intervals.push_back(Interval {
                id: self.next_id,
                start: now,
                end: None,
            });
            self.next_id
        });
        Ok(Some(Ticket { start: now, id }))
    }

    /// The start `ticket` ended at `now`. A counted restart's interval closes there (a failed
    /// one no earlier than [`FAILED_RUN_COST`] after it began); a start that was not counted
    /// is charged only if it failed.
    pub fn finished(&mut self, ticket: Option<Ticket>, now: f64, outcome: StartOutcome) {
        let Some(ticket) = ticket else { return };
        let end = match outcome {
            StartOutcome::Frame => now,
            StartOutcome::Failed => now.max(ticket.start + FAILED_RUN_COST),
        };
        match ticket.id {
            Some(id) => {
                if let Some(i) = self.intervals.iter_mut().find(|i| i.id == id) {
                    i.end = Some(end);
                }
            }
            None if outcome == StartOutcome::Failed => {
                self.next_id += 1;
                self.intervals.push_back(Interval {
                    id: self.next_id,
                    start: ticket.start,
                    end: Some(end),
                });
            }
            None => {}
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

    /// Idle seconds are made up per test: a run used just now is `0.0`, the oldest the largest.
    fn run(id: u64, of: SourceId, head: i64, idle: f64) -> RunState {
        RunState {
            id: RunId(id),
            file: of,
            head,
            idle,
            busy: false,
        }
    }

    fn busy(mut r: RunState) -> RunState {
        r.busy = true;
        r
    }

    fn request(of: SourceId, target: i64, proxy: bool, intent: Intent) -> Request {
        Request {
            file: of,
            target,
            frame_ticks: FT,
            proxy,
            intent,
            // A frame of one byte in a cache of a terabyte: the window is the constant.
            frame_bytes: 1,
            cache_cap: 1 << 40,
        }
    }

    fn ask(runs: &[RunState], of: SourceId, target: i64, proxy: bool, intent: Intent) -> Route {
        route(runs, &request(of, target, proxy, intent))
    }

    fn start(evict: Option<Eviction>, lead: u32) -> Route {
        Route::Start { evict, lead }
    }

    fn evict(run: u64, stale: bool) -> Option<Eviction> {
        Some(Eviction { run: RunId(run), stale })
    }

    #[test]
    fn a_run_behind_the_target_within_the_window_is_read_forward() {
        let runs = [run(1, F, 1000, 1.0)];
        // At the head, and up to 96 frames (9600 ticks) on an original, 24 (2400) on a proxy.
        assert_eq!(ask(&runs, F, 1000, false, Intent::Forward), Route::Reuse(RunId(1)));
        assert_eq!(ask(&runs, F, 1000 + 96 * FT, false, Intent::Forward), Route::Reuse(RunId(1)));
        assert_eq!(ask(&runs, F, 1000 + 24 * FT, true, Intent::Scrub), Route::Reuse(RunId(1)));
        assert_eq!((REUSE_WINDOW_ORIGINAL, REUSE_WINDOW_PROXY), (96, 24));
        assert_eq!((reuse_window(false), reuse_window(true)), (96, 24));
        // One frame past it is a restart, and with a slot free that is a new run, not a kill.
        assert_eq!(ask(&runs, F, 1000 + 97 * FT, false, Intent::Forward), start(None, 0));
        assert_eq!(ask(&runs, F, 1000 + 25 * FT, true, Intent::Forward), start(None, 0));
        // A run another file owns, or one *ahead* of the target, cannot serve it.
        assert_eq!(ask(&runs, G, 1000, false, Intent::Forward), start(None, 0));
        assert_eq!(ask(&runs, F, 999, false, Intent::Forward), start(None, BACKWARD_LEAD));
    }

    #[test]
    fn the_window_is_held_to_half_the_cache_so_a_read_forward_cannot_turn_it_over() {
        const CAP: usize = 256 << 20;
        let frame = |w: usize, h: usize| w * h * 3 / 2;
        // 1080p original: the cache holds 86 frames, so the window is 43, not 96.
        assert_eq!(CAP / frame(1920, 1080), 86);
        assert_eq!(effective_window(false, CAP, frame(1920, 1080)), 43);
        // 4K: 21 frames, a window of 10. A 720p proxy holds 194: the constant 24 stands.
        assert_eq!(effective_window(false, CAP, frame(3840, 2160)), 10);
        assert_eq!(effective_window(true, CAP, frame(1280, 720)), 24);
        // A cache of one frame, or two; an unknown frame size; a count too big for u32.
        assert_eq!(effective_window(false, frame(1920, 1080), frame(1920, 1080)), 0);
        assert_eq!(effective_window(false, 2 * frame(1920, 1080), frame(1920, 1080)), 1);
        assert_eq!(effective_window(false, CAP, 0), 96);
        assert_eq!(effective_window(true, usize::MAX, 1), 24);
        // The router uses it: a run 60 frames behind a 1080p original's target serves it from a
        // big cache and not from the default one.
        let runs = [run(1, F, 0, 1.0)];
        let mut req = request(F, 60 * FT, false, Intent::Forward);
        assert_eq!(route(&runs, &req), Route::Reuse(RunId(1)));
        (req.frame_bytes, req.cache_cap) = (frame(1920, 1080), CAP);
        assert_eq!(req.window(), 43);
        assert_eq!(route(&runs, &req), start(None, 0));
        req.target = 43 * FT;
        assert_eq!(route(&runs, &req), Route::Reuse(RunId(1)));
    }

    #[test]
    fn frames_before_the_target_less_the_lead_are_only_passed() {
        let req = request(F, 10_000, false, Intent::Forward);
        // Read forward from a run: everything before the target is passed over.
        assert_eq!(req.warm_from(&Route::Reuse(RunId(1))), 10_000);
        // A run started early keeps what it decodes from its seek point on.
        assert_eq!(req.warm_from(&start(None, 0)), 10_000);
        assert_eq!(req.warm_from(&start(None, BACKWARD_LEAD)), 10_000 - 15 * FT);
        assert_eq!(
            req.warm_from(&Route::Restart {
                run: RunId(1),
                lead: BACKWARD_LEAD
            }),
            10_000 - 15 * FT
        );
        // A target near the start of time does not wrap.
        let mut near = request(F, i64::MIN + 3, false, Intent::Scrub);
        near.frame_ticks = i64::MAX;
        assert_eq!(near.warm_from(&start(None, BACKWARD_LEAD)), i64::MIN);
    }

    #[test]
    fn the_nearest_run_behind_wins_and_a_busy_one_is_not_reused() {
        let runs = [run(1, F, 0, 1.0), run(2, F, 5000, 9.0), run(3, F, 9000, 8.0)];
        assert_eq!(ask(&runs, F, 5500, false, Intent::Forward), Route::Reuse(RunId(2)));
        assert_eq!(ask(&runs, F, 9000, false, Intent::Forward), Route::Reuse(RunId(3)));
        // Run 2 is held: run 1, 55 frames back, is within the window and serves.
        let held = [run(1, F, 0, 1.0), busy(run(2, F, 5000, 9.0))];
        assert_eq!(ask(&held, F, 5500, false, Intent::Forward), Route::Reuse(RunId(1)));
        // Equal heads: the more recently used.
        let tie = [run(1, F, 100, 7.0), run(2, F, 100, 3.0)];
        assert_eq!(ask(&tie, F, 150, false, Intent::Forward), Route::Reuse(RunId(2)));
        // The extremes of a tick do not overflow.
        let runs = [run(1, F, i64::MIN, 0.0)];
        assert_eq!(ask(&runs, F, i64::MAX, false, Intent::Forward), start(None, 0));
        let mut huge = request(F, i64::MAX, false, Intent::Forward);
        huge.frame_ticks = i64::MAX;
        assert_eq!(route(&runs, &huge), Route::Reuse(RunId(1)));
    }

    #[test]
    fn a_file_at_its_cap_restarts_its_least_recently_used_idle_run() {
        let runs = [run(1, F, 0, 5.0), run(2, F, 100_000, 8.0), run(3, F, 200_000, 1.0)];
        let restart = |run, lead| Route::Restart { run: RunId(run), lead };
        // Far from all three, going forward: the oldest of the file is replaced, no lead.
        assert_eq!(ask(&runs, F, 900_000, false, Intent::Forward), restart(2, 0));
        // Behind all three: a backward move, so it starts early.
        assert_eq!(ask(&runs, F, -5000, false, Intent::Scrub), restart(2, BACKWARD_LEAD));
        // Between runs is not backward.
        assert_eq!(ask(&runs, F, 150_000 + 97 * FT * 10, false, Intent::Scrub), restart(2, 0));
        // The oldest being busy, the next oldest is taken; all busy: nothing can be replaced.
        let some_busy = [
            busy(run(1, F, 0, 5.0)),
            busy(run(2, F, 100_000, 8.0)),
            run(3, F, 200_000, 1.0),
        ];
        assert_eq!(ask(&some_busy, F, 900_000, false, Intent::Forward), restart(3, 0));
        let all_busy = [
            busy(run(1, F, 0, 5.0)),
            busy(run(2, F, 100_000, 8.0)),
            busy(run(3, F, 200_000, 1.0)),
        ];
        assert_eq!(ask(&all_busy, F, 900_000, false, Intent::Forward), Route::Busy);
    }

    #[test]
    fn the_process_cap_replaces_the_least_recently_used_idle_run_of_any_file() {
        let runs = [
            run(1, G, 0, 6.0),
            run(2, G, 0, 9.0),
            run(3, G, 0, 2.0),
            run(4, F, 0, 4.0),
            run(5, F, 0, 3.0),
            run(6, file(3), 0, 7.0),
        ];
        assert_eq!(runs.len(), MAX_RUNS);
        // F has two runs (room for a third) but the process has six: take the oldest overall,
        // which is another file's and has been idle for 9 s: stale.
        assert_eq!(ask(&runs, F, 900_000, false, Intent::Forward), start(evict(2, true), 0));
        // Busy runs are skipped, even the oldest.
        let mut held = runs;
        held[1].busy = true;
        assert_eq!(ask(&held, F, 900_000, false, Intent::Forward), start(evict(6, true), 0));
        for r in &mut held {
            r.busy = true;
        }
        assert_eq!(ask(&held, F, 900_000, false, Intent::Forward), Route::Busy);
        // Another file's run that was used within the last second is not stale: somebody may
        // want it back.
        let mut recent = runs.map(|mut r| {
            r.idle = 0.25;
            r
        });
        recent[1].idle = 0.5;
        assert_eq!(ask(&recent, F, 900_000, false, Intent::Forward), start(evict(2, false), 0));
        // A victim of the same file is never stale, however long it has been idle: when the file
        // is at its cap (three runs of it) that is a `Restart`, and always counted.
        let mut own = runs;
        for r in &mut own[..3] {
            r.file = F;
        }
        assert!(matches!(
            ask(&own, F, 900_000, false, Intent::Forward),
            Route::Restart { run: RunId(2), .. }
        ));
    }

    #[test]
    fn an_exact_frame_never_takes_a_run_from_anyone() {
        // Reuse of an idle run that is in reach is fine; anything else is a decode of its own.
        let runs = [run(1, F, 0, 1.0)];
        assert_eq!(ask(&runs, F, 500, false, Intent::Exact), Route::Reuse(RunId(1)));
        assert_eq!(ask(&runs, F, 5_000_000, false, Intent::Exact), Route::OneShot);
        assert_eq!(ask(&[], F, 0, false, Intent::Exact), Route::OneShot);
        // ...whatever the state of the slots: a full process, a busy run in reach.
        let full: Vec<RunState> = (0..MAX_RUNS as u64).map(|i| run(i, G, 0, i as f64)).collect();
        assert_eq!(ask(&full, F, 0, false, Intent::Exact), Route::OneShot);
        assert_eq!(ask(&[busy(run(1, F, 0, 1.0))], F, 500, false, Intent::Exact), Route::OneShot);
        // Never a route that destroys a run or starts one: sweep heads, targets, caps, busy flags.
        for heads in [vec![], vec![0], vec![0, 7000], vec![0, 7000, 400_000]] {
            for target in [-300, 0, 50, 9600, 9601, 500_000, 9_000_000] {
                for busy_all in [false, true] {
                    let mut runs: Vec<RunState> = heads
                        .iter()
                        .enumerate()
                        .map(|(i, h)| run(i as u64, F, *h, i as f64))
                        .collect();
                    runs.extend((10..13).map(|i| run(i, G, 0, 0.0)));
                    for r in &mut runs {
                        r.busy = busy_all;
                    }
                    let r = ask(&runs, F, target, false, Intent::Exact);
                    assert!(!r.replaces() && !r.starts_a_run(), "{r:?}");
                }
            }
        }
    }

    #[test]
    fn a_prefetch_uses_a_spare_slot_or_nothing() {
        // Covered by a run already: nothing to do.
        assert_eq!(ask(&[run(1, F, 0, 1.0)], F, 500, false, Intent::Prefetch), Route::Skip);
        // Not covered, a slot free: start there, no lead, nobody evicted.
        assert_eq!(ask(&[run(1, F, 0, 1.0)], F, 900_000, false, Intent::Prefetch), start(None, 0));
        assert_eq!(ask(&[], F, 0, false, Intent::Prefetch), start(None, 0));
        // The file at its cap, or the process: skip, however old the runs.
        let file_full = [run(1, F, 0, 1.0), run(2, F, 0, 2.0), run(3, F, 0, 3.0)];
        assert_eq!(ask(&file_full, F, 900_000, false, Intent::Prefetch), Route::Skip);
        let proc_full: Vec<RunState> = (0..MAX_RUNS as u64).map(|i| run(i, G, 0, i as f64)).collect();
        assert_eq!(ask(&proc_full, F, 900_000, false, Intent::Prefetch), Route::Skip);
        // A prefetch behind a run is a backward move, which is not one a spare slot makes early.
        assert_eq!(ask(&[run(1, F, 5000, 1.0)], F, 0, false, Intent::Prefetch), start(None, 0));
    }

    #[test]
    fn a_free_slot_is_filled_with_no_one_evicted_so_many_layers_can_start_at_once() {
        // A cut with six layers of six files starts six runs in one frame.
        let mut runs = Vec::new();
        for i in 0..MAX_RUNS as u64 {
            let r = ask(&runs, file(100 + i), 0, true, Intent::Forward);
            assert_eq!(r, start(None, 0), "layer {i}");
            assert!(!r.replaces() && !r.is_thrash() && r.starts_a_run());
            runs.push(run(i, file(100 + i), 0, 0.0));
        }
        // The first run, per file: no lead, there is no run to be behind.
        assert_eq!(ask(&[], F, -1000, false, Intent::Scrub), start(None, 0));
    }

    #[test]
    fn only_a_restart_somebody_may_want_back_is_thrash() {
        let restart = Route::Restart { run: RunId(1), lead: 0 };
        let fresh = start(evict(1, false), 0);
        let stale = start(evict(1, true), 0);
        assert!(restart.replaces() && fresh.replaces() && stale.replaces());
        assert!(restart.is_thrash() && fresh.is_thrash());
        assert!(!stale.is_thrash(), "a finished clip's run, on another file");
        for r in [
            start(None, 0),
            Route::Reuse(RunId(1)),
            Route::OneShot,
            Route::Skip,
            Route::Busy,
        ] {
            assert!(!r.replaces() && !r.is_thrash(), "{r:?}");
        }
        assert!(restart.starts_a_run() && fresh.starts_a_run() && start(None, 0).starts_a_run());
        assert!(!Route::Reuse(RunId(1)).starts_a_run() && !Route::OneShot.starts_a_run());
    }

    // ---- the guard. Times are binary fractions, so the edges of the window are exact.

    use StartOutcome::{Failed, Frame};

    /// A `Forward` start on `route` at `now`, which ends `took` seconds later in `outcome`.
    fn go(g: &mut ThrashGuard, route: &Route, now: f64, took: f64, outcome: StartOutcome) -> Result<(), Busy> {
        let ticket = g.check(Intent::Forward, route, now)?;
        g.finished(ticket, now + took, outcome);
        Ok(())
    }

    const RESTART: Route = Route::Restart { run: RunId(1), lead: 0 };

    #[test]
    fn slow_restarts_trip_the_guard_sooner_than_fast_ones() {
        // Originals: a quarter of a second (260 ms) a restart. Three of them, back to back, are
        // 0.75 s of the last second: the fourth is refused — a count of "more than four a second"
        // would have let it through.
        let mut g = ThrashGuard::new();
        for i in 0..3 {
            assert_eq!(go(&mut g, &RESTART, f64::from(i) * 0.25, 0.25, Frame), Ok(()), "restart {i}");
        }
        assert_eq!(g.cost(0.75), 0.75);
        assert_eq!(go(&mut g, &RESTART, 0.75, 0.25, Frame), Err(Busy));
        // Proxies: 62.5 ms a restart. Eight a second is half the second: every one goes through
        // (a count refused the fifth), and it never trips however long it goes on.
        let mut g = ThrashGuard::new();
        for i in 0..16 {
            assert_eq!(
                go(&mut g, &RESTART, f64::from(i) * 0.125, 0.0625, Frame),
                Ok(()),
                "restart {i}"
            );
        }
        assert!(g.cost(2.0) <= 0.5, "{}", g.cost(2.0));
    }

    #[test]
    fn a_restart_still_under_way_counts_up_to_now_and_a_refusal_is_not_recorded() {
        let mut g = ThrashGuard::new();
        let open = g.check(Intent::Forward, &RESTART, 10.0).unwrap();
        assert!(open.is_some());
        assert_eq!(g.cost(10.5), 0.5);
        // Pending for three quarters of a second: decoding is not keeping up, whatever else.
        assert_eq!(g.check(Intent::Forward, &RESTART, 10.75), Err(Busy));
        assert_eq!(g.check(Intent::Forward, &start(None, 0), 10.75), Err(Busy), "any start");
        // The refusals recorded nothing: the cost is the open restart's alone, and it stops
        // growing when that one finishes.
        g.finished(open, 10.75, Frame);
        assert_eq!(g.cost(10.75), 0.75);
        assert_eq!(g.cost(11.5), 0.25, "and ages out of the window");
        // A pause empties the window.
        assert_eq!(go(&mut g, &RESTART, 12.0, 0.0625, Frame), Ok(()));
        assert_eq!(g.cost(12.0625), 0.0625);
        assert_eq!(g.cost(14.0), 0.0);
    }

    #[test]
    fn runs_that_die_at_once_are_charged_though_they_free_their_slot() {
        let free = start(None, 0);
        // Free-slot starts are not counted when they work, however many...
        let mut g = ThrashGuard::new();
        for i in 0..40 {
            assert_eq!(go(&mut g, &free, f64::from(i) * 0.0078125, 0.0, Frame), Ok(()));
        }
        assert_eq!(g.cost(0.5), 0.0);
        // ...but each one that dies costs a quarter of a second at least, wherever it started,
        // accruing over that quarter second: 0.25 + 0.1875 + 0.125 at 10.25.
        let mut g = ThrashGuard::new();
        for i in 0..3 {
            let now = 10.0 + f64::from(i) * 0.0625;
            assert_eq!(go(&mut g, &free, now, 0.0, Failed), Ok(()), "failure {i}");
        }
        assert_eq!(g.cost(10.25), 0.5625);
        // At 10.5 the three have cost 0.75 between them: the next start is refused.
        assert_eq!(g.cost(10.5), 0.75);
        assert_eq!(g.check(Intent::Forward, &free, 10.5), Err(Busy));
        // A restart that dies is charged its quarter second too, however quickly it died.
        let mut g = ThrashGuard::new();
        let ticket = g.check(Intent::Forward, &RESTART, 20.0).unwrap();
        g.finished(ticket, 20.0078125, Failed);
        assert_eq!(g.cost(20.25), 0.25);
        // Six layers failing in one frame, frame after frame: refused within four frames.
        let mut g = ThrashGuard::new();
        let mut refused = None;
        'frames: for frame in 0..30 {
            for _ in 0..6 {
                if go(&mut g, &free, f64::from(frame) / 32.0, 0.0, Failed).is_err() {
                    refused = Some(frame);
                    break 'frames;
                }
            }
        }
        assert!(refused.is_some_and(|f| f <= 4), "{refused:?}");
    }

    #[test]
    fn a_montage_across_many_files_is_not_ping_pong_but_layers_across_many_files_are() {
        // Twenty clips of twenty files, a quarter of a second each: at every cut the new clip's
        // run replaces the run of a clip that ended seconds ago. Never counted, never refused.
        let mut g = ThrashGuard::new();
        for clip in 0..20u32 {
            let route = start(evict(u64::from(clip), true), 0);
            assert!(!route.is_thrash());
            assert_eq!(go(&mut g, &route, f64::from(clip) * 0.25, 0.25, Frame), Ok(()), "clip {clip}");
        }
        assert_eq!(g.cost(5.0), 0.0);
        // Eight layers of eight files in every frame (two more than there are slots): each start
        // replaces a run that was used one frame ago. That is ping-pong, and it is counted.
        let mut g = ThrashGuard::new();
        let mut refused = None;
        'frames: for frame in 0..30 {
            for layer in 0..8 {
                let route = start(evict(layer, false), 0);
                if go(&mut g, &route, f64::from(frame) / 32.0, 0.0625, Frame).is_err() {
                    refused = Some((frame, layer));
                    break 'frames;
                }
            }
        }
        assert!(matches!(refused, Some((f, _)) if f <= 4), "{refused:?}");
    }

    #[test]
    fn only_forward_starts_are_guarded() {
        let mut g = ThrashGuard::new();
        // Pile on cost, then ask for everything else.
        let ticket = g.check(Intent::Forward, &RESTART, 1.0).unwrap();
        g.finished(ticket, 2.0, Frame);
        assert_eq!(g.check(Intent::Forward, &RESTART, 2.0), Err(Busy));
        for (intent, route) in [
            (Intent::Scrub, RESTART),
            (Intent::Exact, Route::OneShot),
            (Intent::Prefetch, start(None, 0)),
            (Intent::Forward, Route::Reuse(RunId(1))),
            (Intent::Forward, Route::Skip),
            (Intent::Forward, Route::Busy),
            (Intent::Forward, Route::OneShot),
        ] {
            assert_eq!(g.check(intent, &route, 2.0), Ok(None), "{intent:?} {route:?}");
        }
        // And what the caller gets from a refusal: the error that says "render through FFmpeg".
        assert!(Busy.to_string().contains("restarting"));
        let err = crate::gpu::GpuError::from(Busy);
        assert!(
            matches!(&err, crate::gpu::GpuError::Busy(why) if why.contains("restarting")),
            "{err}"
        );
    }

    // ---- the pieces together

    /// Plays requests through the router the way `FrameSource` will: a run is read forward to the
    /// target (its head moves one frame past it), a start or restart puts one at the target (less
    /// the lead) and costs `cost` seconds, and the guard is asked at the wall-clock second.
    /// Returns (starts, restarts, reuses, the time of the first refusal).
    fn play(requests: &[(f64, SourceId, i64)], intent: Intent, cost: f64) -> (usize, usize, usize, Option<f64>) {
        let (mut runs, mut next, mut guard) = (Vec::<RunState>::new(), 0u64, ThrashGuard::new());
        let (mut starts, mut restarts, mut reuses) = (0, 0, 0);
        let mut now = 0.0;
        for &(at, of, target) in requests {
            for r in &mut runs {
                r.idle += at - now;
            }
            now = at;
            // A frame the cache holds is never asked about: one a run has already passed is a hit.
            if runs
                .iter()
                .any(|r| r.file == of && r.head > target && target >= r.head - 30 * FT)
            {
                continue;
            }
            let r = ask(&runs, of, target, true, intent);
            let Ok(ticket) = guard.check(intent, &r, now) else {
                return (starts, restarts, reuses, Some(now));
            };
            guard.finished(ticket, now + cost, Frame);
            let head = |lead: u32| target - i64::from(lead) * FT + FT;
            match r {
                Route::Reuse(id) => {
                    reuses += 1;
                    let run = runs.iter_mut().find(|r| r.id == id).unwrap();
                    (run.head, run.idle) = (target + FT, 0.0);
                }
                Route::Start { evict, lead } => {
                    starts += 1;
                    runs.retain(|r| Some(r.id) != evict.map(|e| e.run));
                    next += 1;
                    runs.push(run(next, of, head(lead), 0.0));
                }
                Route::Restart { run: id, lead } => {
                    restarts += 1;
                    runs.retain(|r| r.id != id);
                    next += 1;
                    runs.push(run(next, of, head(lead), 0.0));
                }
                other => panic!("{other:?}"),
            }
        }
        (starts, restarts, reuses, None)
    }

    #[test]
    fn playback_is_one_run_per_clip_and_never_a_restart() {
        // 500 frames at 30 fps of one clip, then the same at twice the speed (every other frame).
        let one: Vec<_> = (0..500).map(|k| (f64::from(k) / 30.0, F, 5000 + i64::from(k) * FT)).collect();
        assert_eq!(play(&one, Intent::Forward, 0.1), (1, 0, 499, None));
        let double: Vec<_> = (0..500)
            .map(|k| (f64::from(k) / 30.0, F, 5000 + 2 * i64::from(k) * FT))
            .collect();
        assert_eq!(play(&double, Intent::Forward, 0.1), (1, 0, 499, None));
        // Two layers of one file, a minute apart in the source, interleaved: two runs, no restarts.
        let two: Vec<_> = (0..500)
            .flat_map(|k| {
                let (at, k) = (f64::from(k) / 30.0, i64::from(k));
                [(at, F, k * FT), (at, F, 600_000 + k * FT)]
            })
            .collect();
        assert_eq!(play(&two, Intent::Forward, 0.1), (2, 0, 998, None));
        // A jump (a seek, or the next clip) is one start.
        let jump: Vec<_> = (0..100)
            .map(|k| (f64::from(k) / 30.0, F, i64::from(k) * FT + if k < 50 { 0 } else { 9_000_000 }))
            .collect();
        assert_eq!(play(&jump, Intent::Forward, 0.1), (2, 0, 98, None));
    }

    #[test]
    fn four_layers_of_one_file_thrash_and_the_guard_hands_the_frame_back() {
        // Four widely spaced positions in one file, a frame each in turn: the file keeps three
        // runs, so every request restarts one, and each restart costs a quarter of a second.
        let layers: Vec<_> = (0..120)
            .map(|i| (f64::from(i) / 32.0, F, i64::from(i % 4) * 5_000_000))
            .collect();
        let (starts, restarts, _, refused) = play(&layers, Intent::Forward, 0.25);
        assert_eq!(starts, 3, "the cap");
        // Restarts at frames 3 to 9 are 0.656 s of the second at frame 9 and 0.875 s at frame
        // 10 (restart-seconds, overlapping ones counted each): the eleventh frame is refused.
        assert_eq!((restarts, refused), (7, Some(10.0 / 32.0)));
        // A scrub is not playback: it is never refused, and restarts all the way.
        let (_, restarts, _, refused) = play(&layers, Intent::Scrub, 0.25);
        assert_eq!((restarts, refused), (117, None));
    }
}
