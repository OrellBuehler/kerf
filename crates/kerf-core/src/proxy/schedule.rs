//! Who builds which proxy next: the ordered queue the workers pull from, and the change
//! notification the adapters hang an event on.
//!
//! Three locks exist in this corner and are only ever taken in this order, never the other way:
//! the registry (`proxy::jobs`: what each job is doing), then the [`Scheduler`] (what order they
//! are in), then the heavy-job gate's own (`cpu::reserve`, a reservation's drop). The gate polls a
//! waiting job's cancel flag *under its own lock*, so what a job's cancel check reads here is an
//! atomic and never the scheduler.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use uuid::Uuid;

use super::estimate::Rates;
use super::{JobKey, Notify, Pipeline};
use crate::engine::cpu;

/// Where a job stands in the line. Best first, so the derived order is the run order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Class {
    /// Someone asked for this one to be built next (the latest ask first).
    Next,
    /// A preview is waiting on it (Proxy only, a clip under the playhead).
    Waited,
    /// A clip of the timeline uses it.
    Used,
    /// Everything else, in the order it arrived.
    Rest,
}

/// What a job is, whichever stage it is at.
#[derive(Debug, Clone)]
pub(super) struct JobInfo {
    pub key: JobKey,
    pub asset_id: Uuid,
    pub name: String,
    /// The source's picture size in pixels.
    pub source: (u32, u32),
    /// How wide the proxy is.
    pub width: u32,
    /// The source's length in seconds.
    pub duration: f64,
    pub pipeline: Pipeline,
    /// Arrival order: what ties in the same class are broken by, kept when a job steps back.
    pub seq: u64,
    /// The stamp of the latest ask to build this next; `0` when nobody asked.
    pub next: u64,
}

/// A job waiting for a worker.
pub(super) struct Queued {
    pub info: JobInfo,
    /// Which submission this is (see `Entry::gen`).
    pub gen: u64,
    pub cancel: Arc<AtomicBool>,
    /// Its place in front of the background lane, held while the queue is running: a paused queue
    /// holds none, or analysis would wait for proxies that are not coming.
    pub reservation: Option<cpu::Reservation>,
    pub notify: Notify,
}

/// A job a worker has taken: it holds the machine's slot, or is on its way to asking for it.
struct Running {
    info: JobInfo,
    gen: u64,
    cancel: Arc<AtomicBool>,
    give_way: Arc<AtomicBool>,
    holds_slot: Arc<AtomicBool>,
}

/// A job handed to a worker, with the two flags it is told by.
pub(super) struct Claimed {
    pub job: Queued,
    /// Set while the worker is still waiting for the slot and the queue wants it to step back (the
    /// queue was paused, or a job that goes before this one arrived). Read by the job's cancel
    /// check before the encode starts and never after: a running encode is not interrupted.
    pub give_way: Arc<AtomicBool>,
    /// Set by the worker the moment it holds the slot and starts the encode.
    pub holds_slot: Arc<AtomicBool>,
}

/// One job of the line, for a snapshot.
#[derive(Debug, Clone)]
pub(super) struct Line {
    pub info: JobInfo,
    pub gen: u64,
    /// A worker has it (it may still be waiting for the machine).
    pub claimed: bool,
    pub cancel: Arc<AtomicBool>,
}

/// The line itself: pure state, no registry, no threads, so its rules are unit-tested on one.
#[derive(Default)]
pub(super) struct Scheduler {
    pending: Vec<Queued>,
    running: Vec<Running>,
    paused: bool,
    waits: HashSet<Uuid>,
    used: HashSet<Uuid>,
    /// Jobs somebody cancelled this session: not queued again by themselves.
    cancelled: HashSet<JobKey>,
    seq: u64,
    stamp: u64,
    pub rates: Rates,
}

type Rank = (Class, Reverse<u64>, u64);

impl Scheduler {
    pub fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn rank(&self, info: &JobInfo) -> Rank {
        if info.next > 0 {
            // Several asks stack, the latest first.
            (Class::Next, Reverse(info.next), info.seq)
        } else if self.waits.contains(&info.asset_id) {
            (Class::Waited, Reverse(0), info.seq)
        } else if self.used.contains(&info.asset_id) {
            (Class::Used, Reverse(0), info.seq)
        } else {
            (Class::Rest, Reverse(0), info.seq)
        }
    }

    /// The job that goes next, and what it ranks.
    fn best_pending(&self) -> Option<(usize, Rank)> {
        self.pending
            .iter()
            .enumerate()
            .map(|(at, job)| (at, self.rank(&job.info)))
            .min_by_key(|(_, rank)| *rank)
    }

    /// A job arrives.
    pub fn push(&mut self, job: Queued) {
        self.pending.push(job);
        self.refresh_give_way();
    }

    /// A worker takes the job that goes next, unless the queue is paused or empty.
    pub fn claim_next(&mut self) -> Option<Claimed> {
        if self.paused {
            return None;
        }
        let (at, _) = self.best_pending()?;
        let job = self.pending.remove(at);
        let give_way = Arc::new(AtomicBool::new(false));
        let holds_slot = Arc::new(AtomicBool::new(false));
        self.running.push(Running {
            info: job.info.clone(),
            gen: job.gen,
            cancel: job.cancel.clone(),
            give_way: give_way.clone(),
            holds_slot: holds_slot.clone(),
        });
        self.refresh_give_way();
        Some(Claimed {
            job,
            give_way,
            holds_slot,
        })
    }

    /// A worker is done with the submission `gen` of `key` (it ended, failed, was cancelled or stepped
    /// back). The stamp of the latest ask to build it next that came while the worker had it.
    pub fn release(&mut self, key: &JobKey, gen: u64) -> u64 {
        let next = self
            .running
            .iter()
            .find(|r| r.info.key == *key && r.gen == gen)
            .map_or(0, |r| r.info.next);
        self.running.retain(|r| !(r.info.key == *key && r.gen == gen));
        self.refresh_give_way();
        next
    }

    /// A job a worker had stepped back from takes its old place again (`asked`: the stamp
    /// [`Scheduler::release`] gave).
    pub fn step_back(&mut self, mut job: Queued, asked: u64) {
        job.info.next = job.info.next.max(asked);
        job.reservation = (!self.paused).then(cpu::reserve);
        self.push(job);
    }

    /// Tell every worker still waiting for the slot whether to step back: when the queue is paused,
    /// or a job that goes before it is waiting.
    pub fn refresh_give_way(&mut self) {
        // Nearly always nobody is waiting for the slot (a build under way holds it): nothing to rank.
        if self.running.iter().all(|r| r.holds_slot.load(Ordering::SeqCst)) {
            return;
        }
        let best = self.best_pending().map(|(_, rank)| rank);
        for running in &self.running {
            if running.holds_slot.load(Ordering::SeqCst) {
                continue;
            }
            let outranked = best.is_some_and(|best| best < self.rank(&running.info));
            running.give_way.store(self.paused || outranked, Ordering::SeqCst);
        }
    }

    /// Ask for the job `key` to be built before anything else asked for earlier. Whether it is here.
    pub fn ask_next(&mut self, key: &JobKey) -> bool {
        let stamp = self.stamp + 1;
        let found = match self.pending.iter_mut().find(|q| q.info.key == *key) {
            Some(job) => {
                job.info.next = stamp;
                true
            }
            None => match self.running.iter_mut().find(|r| r.info.key == *key) {
                Some(running) => {
                    running.info.next = stamp;
                    true
                }
                None => false,
            },
        };
        if found {
            self.stamp = stamp;
            self.refresh_give_way();
        }
        found
    }

    /// Replace what the line is ranked by. Whether the order of the line changed.
    pub fn rerank(&mut self, waits: &HashSet<Uuid>, used: &HashSet<Uuid>) -> bool {
        if self.waits == *waits && self.used == *used {
            return false;
        }
        let before = self.order();
        self.waits.clone_from(waits);
        self.used.clone_from(used);
        self.refresh_give_way();
        before != self.order()
    }

    /// The submissions of the line in the order they go, a snapshot to tell a change of order by.
    fn order(&self) -> Vec<u64> {
        let mut ranked: Vec<(Rank, u64)> = self
            .running
            .iter()
            .map(|r| (self.rank(&r.info), r.gen))
            .chain(self.pending.iter().map(|q| (self.rank(&q.info), q.gen)))
            .collect();
        ranked.sort_unstable();
        ranked.into_iter().map(|(_, gen)| gen).collect()
    }

    /// Every job of the line in the order it goes (those a worker holds included, ranked as the
    /// pending ones are: one that is about to step back is placed where it will be).
    pub fn lines(&self) -> Vec<Line> {
        let mut lines: Vec<(Rank, Line)> = Vec::with_capacity(self.pending.len() + self.running.len());
        for r in &self.running {
            lines.push((
                self.rank(&r.info),
                Line {
                    info: r.info.clone(),
                    gen: r.gen,
                    claimed: true,
                    cancel: r.cancel.clone(),
                },
            ));
        }
        for q in &self.pending {
            lines.push((
                self.rank(&q.info),
                Line {
                    info: q.info.clone(),
                    gen: q.gen,
                    claimed: false,
                    cancel: q.cancel.clone(),
                },
            ));
        }
        lines.sort_by_key(|(rank, _)| *rank);
        lines.into_iter().map(|(_, line)| line).collect()
    }

    /// Take the jobs `keys` out of the line. A job a worker has is left to the worker (its cancel
    /// flag is what stops it); one still waiting is dropped, and its reservation with it. The asset
    /// each was for, of those that were here.
    pub fn remove(&mut self, keys: &[JobKey]) -> HashMap<JobKey, Uuid> {
        let wanted: HashSet<&JobKey> = keys.iter().collect();
        let mut found = HashMap::new();
        self.pending.retain(|q| {
            let take = wanted.contains(&q.info.key);
            if take {
                found.insert(q.info.key.clone(), q.info.asset_id);
            }
            !take
        });
        for r in self.running.iter().filter(|r| wanted.contains(&r.info.key)) {
            found.entry(r.info.key.clone()).or_insert(r.info.asset_id);
        }
        self.refresh_give_way();
        found
    }

    /// Drop every job still waiting for a worker (and the places they held).
    #[cfg(test)]
    pub fn clear_pending(&mut self) {
        self.pending.clear();
        self.refresh_give_way();
    }

    /// How many jobs workers have taken and not yet finished with or stepped back from.
    #[cfg(test)]
    pub fn claimed(&self) -> usize {
        self.running.len()
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Pause or resume. Pausing drops the places the waiting jobs hold in the heavy-job queue (a
    /// paused queue must not keep analysis waiting for proxies that are not coming) and tells the
    /// workers still waiting for the slot to step back; resuming takes the places again. Whether
    /// anything changed.
    pub fn set_paused(&mut self, paused: bool) -> bool {
        if self.paused == paused {
            return false;
        }
        self.paused = paused;
        for job in &mut self.pending {
            job.reservation = (!paused).then(cpu::reserve);
        }
        self.refresh_give_way();
        true
    }

    /// Remember that the user cancelled `key`: it is not queued by itself again this session.
    pub fn mark_cancelled(&mut self, key: JobKey) {
        self.cancelled.insert(key);
    }

    /// Something asked for `key` on purpose (a rebuild, a build-next): it may be queued again.
    pub fn forgive(&mut self, key: &JobKey) {
        self.cancelled.remove(key);
    }

    pub fn was_cancelled(&self, key: &JobKey) -> bool {
        self.cancelled.contains(key)
    }

    /// Forget the cancels `wanted` picks: a change of settings starts the queue afresh.
    pub fn forget_cancelled(&mut self, wanted: impl Fn(&JobKey) -> bool) {
        self.cancelled.retain(|key| !wanted(key));
    }
}

/// The line, the workers waiting on it and the change notification.
pub(super) struct Shared {
    state: Mutex<Scheduler>,
    wake: Condvar,
    changes: Arc<Changes>,
    workers: usize,
}

impl Shared {
    pub fn new(workers: usize) -> Self {
        Self::with_notify_interval(workers, QUEUE_NOTIFY_EVERY)
    }

    pub fn with_notify_interval(workers: usize, every: Duration) -> Self {
        Self {
            state: Mutex::new(Scheduler::default()),
            wake: Condvar::new(),
            changes: Arc::new(Changes::new(every)),
            workers,
        }
    }

    /// How many workers pull from the line.
    pub fn workers(&self) -> usize {
        self.workers
    }

    pub fn lock(&self) -> MutexGuard<'_, Scheduler> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wait for the job that goes next.
    pub fn claim(&self) -> Claimed {
        let mut line = self.lock();
        loop {
            if let Some(claimed) = line.claim_next() {
                return claimed;
            }
            line = self.wake.wait(line).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// There may be something for a worker to take.
    pub fn wake(&self) {
        self.wake.notify_all();
    }

    /// The line changed in a way somebody watching it would want to know.
    pub fn touch(&self) {
        self.changes.touch();
    }

    /// Call `listener` after a change, once registered (see [`Changes`]).
    pub fn listen(&self, listener: QueueListener) {
        Changes::listen(&self.changes, listener);
    }
}

/// What the adapters give to be told that the queue changed: its order, its paused flag, or the
/// set of its jobs (or of the failed ones).
pub type QueueListener = Arc<dyn Fn() + Send + Sync>;

/// How soon after a change another one is told. A change is the user's click or a job ending,
/// and an import queues a hundred assets at once: one call each would be a hundred events and
/// a hundred reads of the queue, each already out of date. The first change is told at once and
/// the rest that come meanwhile as one, when the interval is over.
pub(super) const QUEUE_NOTIFY_EVERY: Duration = Duration::from_millis(250);

/// Collects changes and tells one listener, at most once per interval, from a thread of its own
/// (never from under a lock, and a listener that blocks or panics holds nobody up).
pub(super) struct Changes {
    dirty: Mutex<bool>,
    changed: Condvar,
    listener: Mutex<Option<QueueListener>>,
    every: Duration,
    started: AtomicBool,
}

impl Changes {
    pub fn new(every: Duration) -> Self {
        Self {
            dirty: Mutex::new(false),
            changed: Condvar::new(),
            listener: Mutex::new(None),
            every,
            started: AtomicBool::new(false),
        }
    }

    pub fn touch(&self) {
        *self.dirty.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.changed.notify_one();
    }

    /// Register `listener` (a second one replaces the first) and start the thread that calls it.
    pub fn listen(this: &Arc<Self>, listener: QueueListener) {
        *this.listener.lock().unwrap_or_else(|e| e.into_inner()) = Some(listener);
        if !this.started.swap(true, Ordering::SeqCst) {
            let this = Arc::clone(this);
            std::thread::spawn(move || this.pump());
        }
    }

    /// Tell the listener, wait out the interval, repeat: changes that come in the interval are
    /// the next call. Ends when nothing but this thread holds the collector any more.
    fn pump(self: Arc<Self>) {
        loop {
            {
                let mut dirty = self.dirty.lock().unwrap_or_else(|e| e.into_inner());
                while !*dirty {
                    if Arc::strong_count(&self) == 1 {
                        return;
                    }
                    dirty = match self.changed.wait_timeout(dirty, Duration::from_secs(1)) {
                        Ok((guard, _)) => guard,
                        Err(e) => e.into_inner().0,
                    };
                }
                *dirty = false;
            }
            let listener = self.listener.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(listener) = listener {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener())).is_err() {
                    tracing::warn!("a proxy queue listener panicked");
                }
            }
            std::thread::sleep(self.every);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A job on `line` with no place held in the heavy-job queue, so these tests need not share the
    /// process's gate. Its name is its path.
    fn job(line: &mut Scheduler, name: &str) -> (Uuid, JobKey) {
        let asset_id = Uuid::new_v4();
        let key: JobKey = (name.to_string(), 1280);
        let seq = line.next_seq();
        line.push(Queued {
            info: JobInfo {
                key: key.clone(),
                asset_id,
                name: name.to_string(),
                source: (1920, 1080),
                width: 1280,
                duration: 10.0,
                pipeline: Pipeline::Software,
                seq,
                next: 0,
            },
            gen: seq,
            cancel: Arc::new(AtomicBool::new(false)),
            reservation: None,
            notify: Arc::new(|_| {}),
        });
        (asset_id, key)
    }

    fn remove_one(line: &mut Scheduler, key: &JobKey) -> Option<Uuid> {
        line.remove(std::slice::from_ref(key)).get(key).copied()
    }

    fn names(line: &Scheduler) -> Vec<String> {
        line.lines().into_iter().map(|l| l.info.name).collect()
    }

    fn set(ids: &[Uuid]) -> HashSet<Uuid> {
        ids.iter().copied().collect()
    }

    /// What the workers would take, in the order they would take it.
    fn drain(line: &mut Scheduler) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(claimed) = line.claim_next() {
            out.push(claimed.job.info.name.clone());
            line.release(&claimed.job.info.key, claimed.job.gen);
        }
        out
    }

    #[test]
    fn jobs_go_in_the_order_they_arrived_until_something_ranks_them() {
        let mut line = Scheduler::default();
        for name in ["a", "b", "c", "d"] {
            job(&mut line, name);
        }
        assert_eq!(names(&line), ["a", "b", "c", "d"]);
        assert_eq!(drain(&mut line), ["a", "b", "c", "d"]);
    }

    #[test]
    fn what_a_preview_waits_on_goes_before_what_the_cut_uses_before_the_rest_and_arrival_breaks_ties() {
        let mut line = Scheduler::default();
        let ids: Vec<Uuid> = ["a", "b", "c", "d", "e", "f"].iter().map(|n| job(&mut line, n).0).collect();
        let (a, b, c, d, e, f) = (ids[0], ids[1], ids[2], ids[3], ids[4], ids[5]);
        // Used: b, d, e. Waited on: e, f. Nothing asked for.
        assert!(line.rerank(&set(&[e, f]), &set(&[b, d, e])));
        // Waited (e, f in arrival order), used (b, d), the rest (a, c).
        assert_eq!(names(&line), ["e", "f", "b", "d", "a", "c"]);
        // The same sets again move nothing.
        assert!(!line.rerank(&set(&[e, f]), &set(&[b, d, e])));
        let _ = (a, c);
        assert_eq!(drain(&mut line), ["e", "f", "b", "d", "a", "c"]);
    }

    #[test]
    fn a_new_wait_moves_the_line_and_a_cleared_one_puts_it_back() {
        let mut line = Scheduler::default();
        let ids: Vec<Uuid> = ["a", "b", "c"].iter().map(|n| job(&mut line, n).0).collect();
        assert!(line.rerank(&set(&[ids[2]]), &set(&[])));
        assert_eq!(names(&line), ["c", "a", "b"]);
        // The playhead moved on to b: c goes back to where it arrived.
        assert!(line.rerank(&set(&[ids[1]]), &set(&[])));
        assert_eq!(names(&line), ["b", "a", "c"]);
        assert!(line.rerank(&set(&[]), &set(&[])));
        assert_eq!(names(&line), ["a", "b", "c"]);
        // A set that ranks nothing present (an asset of another project) changes no order.
        assert!(!line.rerank(&set(&[Uuid::new_v4()]), &set(&[])));
        assert_eq!(names(&line), ["a", "b", "c"]);
    }

    #[test]
    fn a_job_that_arrives_after_the_ranking_is_ranked_by_it_too() {
        let mut line = Scheduler::default();
        let (a, _) = job(&mut line, "a");
        line.rerank(&set(&[]), &set(&[a]));
        job(&mut line, "b");
        // Only the job already used is ahead; the newcomer is a rest job and goes last.
        assert_eq!(names(&line), ["a", "b"]);
        let late = Uuid::new_v4();
        let seq = line.next_seq();
        line.push(Queued {
            info: JobInfo {
                key: ("late".to_string(), 1280),
                asset_id: late,
                name: "late".to_string(),
                source: (1, 1),
                width: 1280,
                duration: 1.0,
                pipeline: Pipeline::Software,
                seq,
                next: 0,
            },
            gen: seq,
            cancel: Arc::new(AtomicBool::new(false)),
            reservation: None,
            notify: Arc::new(|_| {}),
        });
        line.rerank(&set(&[late]), &set(&[a]));
        assert_eq!(names(&line), ["late", "a", "b"]);
    }

    #[test]
    fn asks_to_build_next_go_before_everything_and_stack_the_latest_first() {
        let mut line = Scheduler::default();
        let ids: Vec<(Uuid, JobKey)> = ["a", "b", "c", "d", "e"].iter().map(|n| job(&mut line, n)).collect();
        // c is waited on, so it already goes first of the lot.
        line.rerank(&set(&[ids[2].0]), &set(&[]));
        assert_eq!(names(&line), ["c", "a", "b", "d", "e"]);
        assert!(line.ask_next(&ids[4].1));
        assert_eq!(names(&line), ["e", "c", "a", "b", "d"]);
        assert!(line.ask_next(&ids[1].1));
        assert_eq!(names(&line), ["b", "e", "c", "a", "d"], "the latest ask is first");
        // Asking again for the first one is the latest ask.
        assert!(line.ask_next(&ids[4].1));
        assert_eq!(names(&line), ["e", "b", "c", "a", "d"]);
        // A job that is not here cannot be asked for.
        assert!(!line.ask_next(&("nobody".to_string(), 1280)));
        // A new ranking does not undo an ask.
        line.rerank(&set(&[ids[3].0]), &set(&[]));
        assert_eq!(names(&line), ["e", "b", "d", "a", "c"]);
        assert_eq!(drain(&mut line), ["e", "b", "d", "a", "c"]);
    }

    #[test]
    fn a_cancelled_job_leaves_the_line_and_is_remembered_until_something_asks_again() {
        let mut line = Scheduler::default();
        let (a_id, a) = job(&mut line, "a");
        job(&mut line, "b");
        assert_eq!(remove_one(&mut line, &a), Some(a_id));
        assert_eq!(names(&line), ["b"]);
        assert_eq!(remove_one(&mut line, &a), None, "it is gone");
        line.mark_cancelled(a.clone());
        assert!(line.was_cancelled(&a));
        line.forgive(&a);
        assert!(!line.was_cancelled(&a));
        line.mark_cancelled(a.clone());
        line.mark_cancelled(("b".to_string(), 1280));
        // Forgetting by a predicate: a change of settings forgets them all.
        line.forget_cancelled(|k| k.0 == "a");
        assert!(!line.was_cancelled(&a) && line.was_cancelled(&("b".to_string(), 1280)));
        line.forget_cancelled(|_| true);
        assert!(!line.was_cancelled(&("b".to_string(), 1280)));
    }

    #[test]
    fn a_job_a_worker_holds_is_not_taken_twice_and_is_left_alone_by_remove() {
        let mut line = Scheduler::default();
        let (a_id, a) = job(&mut line, "a");
        job(&mut line, "b");
        let first = line.claim_next().unwrap();
        assert_eq!(first.job.info.name, "a");
        // The worker's job is still in the line's view, and the next claim is the other one.
        assert_eq!(names(&line), ["a", "b"]);
        assert_eq!(line.claim_next().unwrap().job.info.name, "b");
        assert!(line.claim_next().is_none());
        // Removing it does not take it from the worker: its cancel flag does that.
        assert_eq!(remove_one(&mut line, &a), Some(a_id));
        assert_eq!(names(&line), ["a", "b"]);
        line.release(&a, first.job.gen);
        assert_eq!(names(&line), ["b"]);
    }

    #[test]
    fn a_worker_still_waiting_for_the_machine_steps_back_for_a_job_that_goes_first_but_not_once_it_has_it() {
        // Stepping back and resuming take places in the process's heavy-job queue.
        let _serial = cpu::test_lock();
        let mut line = Scheduler::default();
        job(&mut line, "a");
        let (b_id, b) = job(&mut line, "b");
        let waiting = line.claim_next().unwrap();
        assert_eq!(waiting.job.info.name, "a");
        assert!(!waiting.give_way.load(Ordering::SeqCst), "nothing goes before it");
        // The preview now waits on b: a, which has not started, is told to step back.
        line.rerank(&set(&[b_id]), &set(&[]));
        assert!(waiting.give_way.load(Ordering::SeqCst));
        // Which it does, taking its old place behind b (with the arrival it had).
        let key = waiting.job.info.key.clone();
        let asked = line.release(&key, waiting.job.gen);
        line.step_back(waiting.job, asked);
        assert_eq!(names(&line), ["b", "a"]);
        let next = line.claim_next().unwrap();
        assert_eq!(next.job.info.name, "b");
        // b holds the slot now: asking for a goes before b but does not take the slot from it.
        next.holds_slot.store(true, Ordering::SeqCst);
        line.ask_next(&key);
        assert_eq!(names(&line), ["a", "b"]);
        assert!(!next.give_way.load(Ordering::SeqCst));
        assert!(!line.rerank(&set(&[b_id]), &set(&[])), "a ranking that has not changed");
        // Asking for what a worker already has is not a reason for it to step back either.
        line.ask_next(&b);
        assert!(!next.give_way.load(Ordering::SeqCst));
    }

    #[test]
    fn a_job_a_worker_has_that_was_asked_for_does_not_step_back_for_the_others() {
        let mut line = Scheduler::default();
        let (_, a) = job(&mut line, "a");
        let (_, b) = job(&mut line, "b");
        let waiting = line.claim_next().unwrap();
        assert_eq!(waiting.job.info.name, "a");
        // Somebody asks for b: a has to give way.
        line.ask_next(&b);
        assert!(waiting.give_way.load(Ordering::SeqCst));
        // Then asks for a, later: a is first again and the flag drops.
        line.ask_next(&a);
        assert!(!waiting.give_way.load(Ordering::SeqCst));
    }

    #[test]
    fn an_ask_that_came_while_a_worker_had_the_job_survives_its_stepping_back() {
        let _serial = cpu::test_lock();
        let mut line = Scheduler::default();
        let (_, a) = job(&mut line, "a");
        job(&mut line, "b");
        let (_, c) = job(&mut line, "c");
        let waiting = line.claim_next().unwrap();
        assert_eq!(waiting.job.info.name, "a");
        // c is asked for, then a (which a worker has): a is asked for later, so it is first.
        line.ask_next(&c);
        line.ask_next(&a);
        let asked = line.release(&a, waiting.job.gen);
        line.step_back(waiting.job, asked);
        assert_eq!(names(&line), ["a", "c", "b"]);
        line.clear_pending();
    }

    #[test]
    fn a_paused_line_hands_out_nothing_and_tells_waiting_workers_to_step_back() {
        // Stepping back and resuming take places in the process's heavy-job queue.
        let _serial = cpu::test_lock();
        let mut line = Scheduler::default();
        job(&mut line, "a");
        job(&mut line, "b");
        let waiting = line.claim_next().unwrap();
        assert!(line.set_paused(true));
        assert!(!line.set_paused(true), "already paused");
        assert!(line.is_paused());
        assert!(waiting.give_way.load(Ordering::SeqCst));
        assert!(line.claim_next().is_none(), "nothing new starts");
        // A worker that already holds the slot is not told anything: its encode finishes.
        let started = {
            let mut line = Scheduler::default();
            job(&mut line, "x");
            let claimed = line.claim_next().unwrap();
            claimed.holds_slot.store(true, Ordering::SeqCst);
            line.set_paused(true);
            claimed.give_way.load(Ordering::SeqCst)
        };
        assert!(!started);
        // Jobs still arrive and wait.
        job(&mut line, "c");
        assert_eq!(names(&line), ["a", "b", "c"]);
        assert!(line.set_paused(false));
        assert!(!waiting.give_way.load(Ordering::SeqCst));
        assert_eq!(line.claim_next().unwrap().job.info.name, "b");
    }

    #[test]
    fn a_pause_gives_up_the_places_held_in_front_of_analysis_and_a_resume_takes_them_again() {
        let _serial = cpu::test_lock();
        let mut line = Scheduler::default();
        let held = |line: &Scheduler| line.pending.iter().filter(|q| q.reservation.is_some()).count();
        for name in ["a", "b", "c"] {
            let (_, key) = job(&mut line, name);
            let at = line.pending.iter().position(|q| q.info.key == key).unwrap();
            line.pending[at].reservation = Some(cpu::reserve());
        }
        assert_eq!(held(&line), 3);
        assert!(cpu::high_pending());
        line.set_paused(true);
        assert_eq!(held(&line), 0);
        assert!(!cpu::high_pending(), "a paused queue must not hold analysis back");
        line.set_paused(false);
        assert_eq!(held(&line), 3);
        assert!(cpu::high_pending());
        // A job that steps back while paused takes no place either.
        let claimed = line.claim_next().unwrap();
        line.set_paused(true);
        let key = claimed.job.info.key.clone();
        let asked = line.release(&key, claimed.job.gen);
        line.step_back(claimed.job, asked);
        assert_eq!(held(&line), 0);
        assert!(!cpu::high_pending());
        line.clear_pending();
        assert!(!cpu::high_pending());
        line.set_paused(false);
        assert!(!cpu::high_pending(), "nothing is queued, nothing is held");
    }

    #[test]
    fn dropping_a_waiting_job_gives_its_place_back() {
        let _serial = cpu::test_lock();
        let mut line = Scheduler::default();
        let (_, a) = job(&mut line, "a");
        line.pending[0].reservation = Some(cpu::reserve());
        assert!(cpu::high_pending());
        remove_one(&mut line, &a);
        assert!(!cpu::high_pending());
    }

    #[test]
    fn changes_are_told_at_once_and_a_burst_of_them_as_one_call_after() {
        let changes = Arc::new(Changes::new(Duration::from_millis(300)));
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        Changes::listen(
            &changes,
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let until = |want: usize| {
            let end = std::time::Instant::now() + Duration::from_secs(5);
            while calls.load(Ordering::SeqCst) < want && std::time::Instant::now() < end {
                std::thread::sleep(Duration::from_millis(5));
            }
            calls.load(Ordering::SeqCst)
        };
        changes.touch();
        assert_eq!(until(1), 1, "the first change is told at once");
        for _ in 0..50 {
            changes.touch();
        }
        assert_eq!(until(2), 2, "the burst arrives after the interval");
        std::thread::sleep(Duration::from_millis(700));
        let seen = calls.load(Ordering::SeqCst);
        assert!((2..=3).contains(&seen), "fifty changes were {seen} calls");
        // A change after a quiet moment is told again.
        changes.touch();
        assert_eq!(until(seen + 1), seen + 1);
    }

    #[test]
    fn a_listener_that_panics_does_not_stop_the_next_change_being_told() {
        let changes = Arc::new(Changes::new(Duration::from_millis(20)));
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        Changes::listen(
            &changes,
            Arc::new(move || {
                if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("a listener's own bug");
                }
            }),
        );
        let until = |want: usize| {
            let end = std::time::Instant::now() + Duration::from_secs(5);
            while calls.load(Ordering::SeqCst) < want && std::time::Instant::now() < end {
                std::thread::sleep(Duration::from_millis(5));
            }
            calls.load(Ordering::SeqCst)
        };
        changes.touch();
        assert_eq!(until(1), 1);
        changes.touch();
        assert_eq!(until(2), 2);
    }

    #[test]
    fn a_change_with_nobody_listening_is_nothing() {
        let shared = Shared::new(1);
        shared.touch();
        shared.touch();
        assert_eq!(shared.workers(), 1);
    }
}
