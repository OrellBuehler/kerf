//! How much of the machine the media engine is allowed to take.
//!
//! FFmpeg is written to finish as fast as possible: every run grabs every core,
//! and nothing coordinates one run with the next. That is right for a single
//! export and wrong for everything else — an agent that analyzes eight sources
//! over MCP spawns eight full-file decodes *at once*, each with as many threads
//! as there are cores, and the desktop stops responding while they fight each
//! other. The wall-clock is barely better than running them one at a time; only
//! the machine is worse.
//!
//! So the engine keeps a budget, and it has exactly two moving parts:
//!
//! * **One heavy job at a time.** Every pass that reads a whole file (analysis,
//!   transcription, proxy, stitch, export) takes [`lease`] first and waits its
//!   turn. Interactive work — a scrubbed frame, a preview stream, a clip's audio
//!   — never queues, so the UI stays live behind a running render. The queue has
//!   two lanes ([`Priority`]): a preview proxy jumps ahead of everything waiting
//!   (and is registered the moment it is queued, [`reserve`], so analysis that
//!   starts a heartbeat later still finds it in front), because a proxy is what
//!   the preview is waiting for while analysis is only wanted eventually. A job
//!   already running is never interrupted; the proxy goes next.
//! * **A share of the cores for that job**, from [`cpu_percent`]: the thread caps
//!   [`limit_args`] writes into the ffmpeg command line, plus below-normal
//!   scheduling priority ([`background`]) so the rest of the desktop always
//!   preempts it.
//!
//! At **100%** the second half is off entirely: no thread flags are added and
//! priority is untouched, so a full-speed render produces byte-identical ffmpeg
//! invocations to the ones Kerf has always issued. The percentage is seeded from
//! `KERF_CPU_PERCENT` and set at runtime by the app's settings.

use std::cell::Cell;
use std::process::Command;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

/// The narrowest slice of the machine that can be asked for. Below this a big
/// export stops being worth starting.
pub const MIN_CPU_PERCENT: u8 = 10;

/// What a fresh install allows. Not 100%: leaving roughly a quarter of the
/// machine alone costs a render very little and is the difference between
/// "Kerf is busy" and "the computer is unusable".
pub const DEFAULT_CPU_PERCENT: u8 = 75;

/// Logical cores this machine reports.
pub fn cores() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// 0 means "not resolved yet" — the first read seeds it from the environment.
static PERCENT: AtomicU8 = AtomicU8::new(0);

pub fn clamp_percent(percent: u8) -> u8 {
    percent.clamp(MIN_CPU_PERCENT, 100)
}

/// The share of the machine one heavy job may use, in percent.
pub fn cpu_percent() -> u8 {
    match PERCENT.load(Ordering::Relaxed) {
        0 => {
            let seed = std::env::var("KERF_CPU_PERCENT")
                .ok()
                .and_then(|v| v.trim().parse::<u8>().ok())
                .map(clamp_percent)
                .unwrap_or(DEFAULT_CPU_PERCENT);
            PERCENT.store(seed, Ordering::Relaxed);
            seed
        }
        percent => percent,
    }
}

/// Set the share of the machine heavy jobs may use, returning the clamped
/// value. Takes effect on the next job to start; a render already running keeps
/// the threads it was launched with (ffmpeg has no way to be told otherwise).
pub fn set_cpu_percent(percent: u8) -> u8 {
    let percent = clamp_percent(percent);
    PERCENT.store(percent, Ordering::Relaxed);
    // Wake anything queued so a raised budget is picked up promptly.
    gate().free.notify_all();
    percent
}

/// How many threads a single heavy job may use at the current budget.
pub fn budget_threads() -> usize {
    threads_for(cores(), cpu_percent())
}

/// Cores to threads at `percent` — pure, so the rounding is unit-tested. Always
/// at least one thread and never more than the machine has, so 10% of a 4-core
/// laptop is 1 rather than 0.
pub fn threads_for(cores: usize, percent: u8) -> usize {
    let cores = cores.max(1) as f64;
    let want = (cores * f64::from(clamp_percent(percent)) / 100.0).round();
    (want.max(1.0).min(cores)) as usize
}

/// Whether the budget is capping anything at all. At 100% every ffmpeg
/// invocation and its scheduling priority are exactly what they were before the
/// budget existed.
fn limited() -> bool {
    cpu_percent() < 100
}

// ---- the one-heavy-job-at-a-time gate --------------------------------------

/// Which lane of the queue a heavy job waits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Analysis, transcription, stitching, export: wanted eventually, in order.
    Normal,
    /// A preview proxy: the preview is decoding the original until it lands, so it
    /// goes before every `Normal` job that is waiting or arrives meanwhile.
    High,
}

struct GateState {
    busy: bool,
    /// `High` jobs that have asked for the slot or [reserved](reserve) one and not
    /// got it yet. While any exist, no `Normal` job takes the slot.
    high: usize,
    /// Tickets of the `Normal` jobs waiting, in arrival order: they take the slot
    /// first-come first-served, so a long queue of analyses runs in the order it
    /// was asked for rather than whichever thread the scheduler wakes.
    normal: std::collections::VecDeque<u64>,
    next_ticket: u64,
}

struct Gate {
    state: Mutex<GateState>,
    free: Condvar,
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| Gate {
        state: Mutex::new(GateState {
            busy: false,
            high: 0,
            normal: std::collections::VecDeque::new(),
            next_ticket: 0,
        }),
        free: Condvar::new(),
    })
}

/// The gate's state, whether or not a panicking job poisoned it: a poisoned gate
/// must not wedge every later job for the rest of the session.
fn state(gate: &Gate) -> std::sync::MutexGuard<'_, GateState> {
    gate.state.lock().unwrap_or_else(|e| e.into_inner())
}

thread_local! {
    /// Nesting depth on this thread. A leased job that calls another leased
    /// helper (an export's second pass, a stitch inside an import) must not
    /// queue behind itself.
    static HELD: Cell<usize> = const { Cell::new(0) };
}

/// The heavy-job slot, held until dropped.
pub struct Lease {
    threads: usize,
    /// A nested lease owns no slot; only the outermost releases it.
    nested: bool,
}

impl Lease {
    /// How many CPU threads this job may use.
    pub fn threads(&self) -> usize {
        self.threads
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        HELD.with(|h| h.set(h.get().saturating_sub(1)));
        if self.nested {
            return;
        }
        let gate = gate();
        state(gate).busy = false;
        // Everyone re-checks: which waiter is next depends on the lanes, and a
        // single wake-up could land on one that has to keep waiting.
        gate.free.notify_all();
    }
}

/// A queued `High` job's place in front of the `Normal` lane, held from the moment
/// it is queued until it takes the slot (or is dropped, which gives the place up).
///
/// Without it a proxy would only outrank analysis once its worker thread had got
/// as far as asking for the slot — after an ffprobe — by which time the analysis
/// of the very file it was queued for had taken the slot first.
#[must_use = "a dropped reservation gives its place up"]
pub struct Reservation {
    held: bool,
}

/// Put a `High` job in front of the `Normal` lane now, and take the slot with
/// [`Reservation::lease`] when it is its turn.
pub fn reserve() -> Reservation {
    state(gate()).high += 1;
    Reservation { held: true }
}

impl Reservation {
    /// Wait for the slot and take it, ahead of every `Normal` job.
    pub fn lease(mut self) -> Lease {
        self.held = false;
        lease_high(true)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.held {
            let gate = gate();
            let mut s = state(gate);
            s.high = s.high.saturating_sub(1);
            drop(s);
            gate.free.notify_all();
        }
    }
}

/// Whether a `High` job is queued or waiting — what a `Normal` job that is about to
/// ask for the slot would be made to wait behind. For saying so ("waiting for the
/// preview proxy"); the gate itself never needs it.
pub fn high_pending() -> bool {
    state(gate()).high > 0
}

/// Why a `Normal` job asking for the slot now would have to wait, if it would: a
/// `High` job in front of it, or the slot being taken. For saying so; never for
/// deciding (the answer is stale the moment it is given).
pub fn normal_wait() -> Option<Wait> {
    let s = state(gate());
    if s.high > 0 {
        Some(Wait::Proxy)
    } else if s.busy || !s.normal.is_empty() {
        Some(Wait::Job)
    } else {
        None
    }
}

/// What a waiting `Normal` job is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// A preview proxy is queued or being built, and goes first.
    Proxy,
    /// Another heavy job holds the slot (or is ahead in the queue).
    Job,
}

/// Wait for the heavy-job slot and take it.
///
/// Every pass that reads a whole file goes through here, which is what keeps
/// eight concurrent agent analyses from becoming eight concurrent full-file
/// decodes. Callers must not hold the project lock across this — the wait is
/// unbounded by design (a queued job waits out the render ahead of it).
pub fn lease() -> Lease {
    lease_priority(Priority::Normal)
}

/// [`lease`] in the given lane.
pub fn lease_priority(priority: Priority) -> Lease {
    match priority {
        Priority::Normal => lease_normal(),
        Priority::High => lease_high(false),
    }
}

/// A lease for a thread that already holds one: owns no slot, so it cannot wait.
fn nested_lease() -> Option<Lease> {
    let depth = HELD.with(|h| h.get());
    HELD.with(|h| h.set(depth + 1));
    (depth > 0).then(|| Lease {
        threads: budget_threads(),
        nested: true,
    })
}

fn lease_normal() -> Lease {
    if let Some(nested) = nested_lease() {
        return nested;
    }
    let gate = gate();
    let mut s = state(gate);
    let ticket = s.next_ticket;
    s.next_ticket += 1;
    s.normal.push_back(ticket);
    while s.busy || s.high > 0 || s.normal.front() != Some(&ticket) {
        s = gate.free.wait(s).unwrap_or_else(|e| e.into_inner());
    }
    s.normal.pop_front();
    s.busy = true;
    drop(s);
    Lease {
        threads: budget_threads(),
        nested: false,
    }
}

/// `reserved`: the caller already counts in `high` (a [`Reservation`]).
fn lease_high(reserved: bool) -> Lease {
    let gate = gate();
    if let Some(nested) = nested_lease() {
        if reserved {
            let mut s = state(gate);
            s.high = s.high.saturating_sub(1);
            drop(s);
            gate.free.notify_all();
        }
        return nested;
    }
    let mut s = state(gate);
    if !reserved {
        s.high += 1;
    }
    while s.busy {
        s = gate.free.wait(s).unwrap_or_else(|e| e.into_inner());
    }
    s.high = s.high.saturating_sub(1);
    s.busy = true;
    drop(s);
    Lease {
        threads: budget_threads(),
        nested: false,
    }
}

// ---- thread caps on the command line ---------------------------------------

/// The thread-cap flags for `threads`, or nothing when the budget is off.
///
/// `-filter_threads` / `-filter_complex_threads` are true global options;
/// `-threads` is a per-file codec option, so where it sits decides what it
/// means. These are the *front* flags, which land in the first input's option
/// group and so cap the decoder — the expensive half of every analysis pass.
fn head_flags(threads: usize) -> Vec<String> {
    head_flags_with(threads, limited())
}

/// [`head_flags`] with the "is the budget on" decision made by the caller: `capped`
/// writes the flags whatever the budget is (see [`cap_args`]).
fn head_flags_with(threads: usize, capped: bool) -> Vec<String> {
    if !capped || threads == 0 || threads >= cores() {
        return Vec::new();
    }
    let n = threads.to_string();
    vec![
        "-threads".to_string(),
        n.clone(),
        "-filter_threads".to_string(),
        n.clone(),
        "-filter_complex_threads".to_string(),
        n,
    ]
}

/// Cap a built ffmpeg argument list to `threads`.
///
/// Two insertions, because ffmpeg assigns `-threads` to whichever file group it
/// appears in: the [`head_flags`] cap the decode, and a second `-threads` goes
/// immediately before the last argument — which for every command the engine
/// builds is the output sink — so the *encoder* is capped too. A no-op at 100%,
/// which is what keeps the pure argument builders' tests describing exactly what
/// ffmpeg is handed.
pub fn limit_args(args: &mut Vec<String>, threads: usize) {
    splice_caps(args, threads, limited());
}

/// [`limit_args`] that holds at **any** budget, 100% included.
///
/// For the background jobs that run *beside* something else rather than behind
/// it, where the cap is what keeps them out of the way and not the user's share of
/// the machine: a filmstrip decodes video while a proxy encode or an export owns
/// the heavy-job lease, and the GPU path's parallel layer decodes
/// ([`limit_args_shared`]) would otherwise each ask for every core. "At 100%
/// nothing is capped" describes a job the user asked for and is waiting on.
pub fn cap_args(args: &mut Vec<String>, threads: usize) {
    splice_caps(args, threads, true);
}

/// The one place thread caps are written into an argv: the decode flags at the
/// front and the encoder's `-threads` just before the sink, when `capped`.
fn splice_caps(args: &mut Vec<String>, threads: usize, capped: bool) {
    let head = head_flags_with(threads, capped);
    if head.is_empty() {
        return;
    }
    if let Some(sink) = args.len().checked_sub(1) {
        args.splice(sink..sink, ["-threads".to_string(), threads.to_string()]);
    }
    args.splice(0..0, head);
}

/// The threads one of `share` processes running side by side may use: the
/// budget's threads divided among them, at least one each.
pub fn shared_threads(budget: usize, share: usize) -> usize {
    (budget / share.max(1)).max(1)
}

/// Cap an argv for one of `share` ffmpeg processes that run side by side (the
/// GPU path decodes every layer of a frame in parallel). A lone process is
/// [`limit_args`] exactly — nothing at a full budget. With several, the budget's
/// threads are *divided*, and the cap is written even at 100% ([`cap_args`]): left
/// alone, N processes would each ask for every core, N times what the budget
/// allows.
pub fn limit_args_shared(args: &mut Vec<String>, share: usize) {
    let threads = shared_threads(budget_threads(), share);
    if share <= 1 {
        return limit_args(args, threads);
    }
    cap_args(args, threads);
}

/// Cap a `Command` that is being built up fluently, before any of its own
/// arguments are pushed. Only the decode side — a command assembled this way
/// has no output sink to insert before yet.
pub fn limit_cmd(cmd: &mut Command, threads: usize) {
    let head = head_flags(threads);
    if !head.is_empty() {
        cmd.args(head);
    }
}

/// Drop a child to below-normal scheduling priority.
///
/// The thread cap decides how much of the machine ffmpeg *asks* for; this
/// decides who wins when it asks for too much. It is the half that keeps the
/// desktop usable, because the scheduler will hand the foreground window a core
/// the instant it wants one. Left alone at 100%.
pub fn background(cmd: &mut Command) {
    if !limited() {
        return;
    }
    lower_priority(cmd);
}

/// [`background`] at any budget, 100% included — the priority half of
/// [`cap_args`], for jobs that run beside a render rather than behind it.
pub fn background_always(cmd: &mut Command) {
    lower_priority(cmd);
}

fn lower_priority(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // `creation_flags` replaces the whole set, so the no-console flag
        // `cli::command` set has to be repeated here or a terminal flashes over
        // the GUI on every background ffmpeg.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: runs in the forked child before exec; `nice` is
        // async-signal-safe and touches nothing this process owns.
        unsafe {
            cmd.pre_exec(|| {
                libc::nice(10);
                Ok(())
            });
        }
    }
}

/// Held by every test that takes the heavy-job slot or moves the budget: they share one
/// process-wide gate, and a test that waits for it (or counts who is waiting) cannot be
/// run beside one that holds it.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget and the gate are process-global, so the tests that move them cannot run
    /// beside each other (cargo runs them on threads of one process) — nor beside any
    /// other test that takes the slot (see [`test_lock`]).
    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        test_lock()
    }

    #[test]
    fn threads_scale_with_the_budget() {
        assert_eq!(threads_for(16, 100), 16);
        assert_eq!(threads_for(16, 75), 12);
        assert_eq!(threads_for(16, 50), 8);
        assert_eq!(threads_for(12, 75), 9);
    }

    #[test]
    fn threads_never_reach_zero_or_exceed_the_machine() {
        // 10% of a 4-core laptop rounds to nothing; a job still needs a thread.
        assert_eq!(threads_for(4, 10), 1);
        assert_eq!(threads_for(1, 10), 1);
        // Out-of-range percentages clamp rather than overcommit.
        assert_eq!(threads_for(8, 200), 8);
        assert_eq!(threads_for(0, 100), 1);
    }

    #[test]
    fn limit_args_caps_both_the_decoder_and_the_encoder() {
        let _serial = exclusive();
        // Pin the budget below the machine so the flags are actually written.
        let restore = cpu_percent();
        set_cpu_percent(MIN_CPU_PERCENT);
        let mut args: Vec<String> = ["-hide_banner", "-i", "in.mp4", "-c:v", "libx264", "out.mp4"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        limit_args(&mut args, 1);
        // Decode caps lead, before the first `-i`.
        assert_eq!(
            &args[..6],
            &["-threads", "1", "-filter_threads", "1", "-filter_complex_threads", "1"]
        );
        // The encoder cap sits in the output group: after the last input,
        // immediately before the sink.
        assert_eq!(&args[args.len() - 3..], &["-threads", "1", "out.mp4"]);
        set_cpu_percent(restore);
    }

    #[test]
    fn side_by_side_processes_split_the_budget() {
        assert_eq!(shared_threads(12, 1), 12);
        assert_eq!(shared_threads(12, 3), 4);
        assert_eq!(shared_threads(12, 5), 2);
        // Never zero, and a share of zero is a share of one.
        assert_eq!(shared_threads(4, 16), 1);
        assert_eq!(shared_threads(4, 0), 4);
    }

    #[test]
    fn shared_processes_are_capped_even_at_a_full_budget_but_a_lone_one_is_not() {
        let _serial = exclusive();
        let restore = cpu_percent();
        set_cpu_percent(100);
        let original: Vec<String> = ["-i", "in.mp4", "out.mp4"].iter().map(|s| s.to_string()).collect();
        let mut lone = original.clone();
        limit_args_shared(&mut lone, 1);
        assert_eq!(lone, original);
        let mut shared = original;
        limit_args_shared(&mut shared, 4);
        if cores() > 1 {
            let n = shared_threads(cores(), 4).to_string();
            assert_eq!(&shared[..2], &["-threads", n.as_str()], "{shared:?}");
            assert_eq!(&shared[shared.len() - 3..], &["-threads", n.as_str(), "out.mp4"]);
        }
        set_cpu_percent(restore);
    }

    #[test]
    fn a_hard_cap_holds_at_a_full_budget() {
        let _serial = exclusive();
        let restore = cpu_percent();
        set_cpu_percent(100);
        let original: Vec<String> = ["-i", "in.mp4", "out.mp4"].iter().map(|s| s.to_string()).collect();
        // The budget-following cap stands down at 100%, the hard one does not (on a
        // one-core machine there is nothing below the machine to cap to).
        let mut soft = original.clone();
        limit_args(&mut soft, 1);
        assert_eq!(soft, original);
        let mut hard = original.clone();
        cap_args(&mut hard, 1);
        if cores() > 1 {
            assert_eq!(
                &hard[..6],
                &["-threads", "1", "-filter_threads", "1", "-filter_complex_threads", "1"]
            );
            assert_eq!(&hard[hard.len() - 3..], &["-threads", "1", "out.mp4"]);
        } else {
            assert_eq!(hard, original);
        }
        set_cpu_percent(restore);
    }

    #[test]
    fn a_full_budget_writes_no_flags() {
        let _serial = exclusive();
        let restore = cpu_percent();
        set_cpu_percent(100);
        let original: Vec<String> = ["-i", "in.mp4", "out.mp4"].iter().map(|s| s.to_string()).collect();
        let mut args = original.clone();
        limit_args(&mut args, 1);
        assert_eq!(args, original, "a 100% budget must leave every command line untouched");
        set_cpu_percent(restore);
    }

    #[test]
    fn a_nested_lease_does_not_wait_for_itself() {
        // The budget must hold still: `threads()` is sampled per lease.
        let _serial = exclusive();
        let outer = lease();
        // Would deadlock against a non-reentrant gate: an export's second pass
        // and a stitch inside an import both lease under an outer lease.
        let inner = lease();
        assert_eq!(inner.threads(), outer.threads());
        drop(inner);
        drop(outer);
        // The slot is free again once the outermost lease is dropped.
        let _next = lease();
    }

    /// Run `f` on a thread, reporting `label` on `order` once it holds the lease and
    /// releasing it straight away.
    fn waiter(
        label: &'static str,
        priority: Priority,
        order: &std::sync::Arc<Mutex<Vec<&'static str>>>,
    ) -> std::thread::JoinHandle<()> {
        let order = order.clone();
        std::thread::spawn(move || {
            let _lease = lease_priority(priority);
            order.lock().unwrap().push(label);
        })
    }

    /// How many `Normal` jobs are waiting for the slot.
    fn queued_normal() -> usize {
        state(gate()).normal.len()
    }

    fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
        let start = std::time::Instant::now();
        while !ready() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(10),
                "timed out waiting for {what}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn a_high_job_goes_before_normal_jobs_that_were_already_waiting() {
        let _serial = exclusive();
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        let held = lease();
        let a = waiter("analysis-1", Priority::Normal, &order);
        wait_until("the first analysis to queue", || queued_normal() == 1);
        let b = waiter("analysis-2", Priority::Normal, &order);
        wait_until("the second analysis to queue", || queued_normal() == 2);
        let proxy = waiter("proxy", Priority::High, &order);
        wait_until("the proxy to queue", high_pending);
        drop(held);
        for t in [a, b, proxy] {
            t.join().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), ["proxy", "analysis-1", "analysis-2"]);
    }

    #[test]
    fn normal_jobs_take_the_slot_in_the_order_they_asked() {
        let _serial = exclusive();
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        let held = lease();
        let mut threads = Vec::new();
        for (i, label) in ["a", "b", "c", "d"].into_iter().enumerate() {
            threads.push(waiter(label, Priority::Normal, &order));
            wait_until("the waiter to queue", || queued_normal() == i + 1);
        }
        drop(held);
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), ["a", "b", "c", "d"]);
    }

    #[test]
    fn a_reservation_keeps_normal_jobs_out_before_its_thread_has_asked() {
        let _serial = exclusive();
        // The slot is free, but a proxy is queued: an analysis that starts now must
        // not slip in ahead of it just because the proxy's worker is still probing.
        let reservation = reserve();
        assert!(high_pending());
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        let analysis = waiter("analysis", Priority::Normal, &order);
        wait_until("the analysis to queue", || queued_normal() == 1);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(order.lock().unwrap().is_empty(), "the analysis ran ahead of a queued proxy");
        let proxy = reservation.lease();
        order.lock().unwrap().push("proxy");
        // …and it does not run beside the proxy either: one heavy job at a time.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(*order.lock().unwrap(), ["proxy"]);
        drop(proxy);
        analysis.join().unwrap();
        assert_eq!(*order.lock().unwrap(), ["proxy", "analysis"]);
        assert!(!high_pending());
    }

    #[test]
    fn a_dropped_reservation_lets_the_queue_move() {
        let _serial = exclusive();
        let reservation = reserve();
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        let analysis = waiter("analysis", Priority::Normal, &order);
        wait_until("the analysis to queue", || queued_normal() == 1);
        drop(reservation);
        analysis.join().unwrap();
        assert_eq!(*order.lock().unwrap(), ["analysis"]);
        assert!(!high_pending());
    }

    #[test]
    fn a_high_lease_inside_a_lease_does_not_wait_for_itself() {
        let _serial = exclusive();
        let outer = lease();
        // A proxy made inside a leased job (an import that stitches, then proxies).
        let inner = reserve().lease();
        assert_eq!(inner.threads(), outer.threads());
        assert!(!high_pending(), "the reservation was spent");
        drop(inner);
        drop(outer);
        let _next = lease();
    }

    #[test]
    fn a_normal_job_can_be_told_why_it_would_wait() {
        let _serial = exclusive();
        assert_eq!(normal_wait(), None, "the slot is free");
        let held = lease();
        assert_eq!(normal_wait(), Some(Wait::Job), "another job has it");
        let reservation = reserve();
        assert_eq!(normal_wait(), Some(Wait::Proxy), "a proxy is queued, and goes first");
        drop(reservation);
        drop(held);
        assert_eq!(normal_wait(), None);
    }
}
