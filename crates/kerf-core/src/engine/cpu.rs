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
//!   three lanes ([`Priority`]): **foreground** (an export, a measurement, a voiceover —
//!   what a user is actively waiting on) before **proxy** (a preview proxy is what the
//!   preview is waiting for, and is registered the moment it is queued, [`reserve`], so
//!   analysis that starts a heartbeat later still finds it in front) before **background**
//!   (analysis and transcription, wanted eventually). A job already running is never
//!   interrupted; the next one goes. A job that waits says why and can be stopped while it
//!   does ([`lease_waiting`]).
//! * **A share of the cores for that job**, from [`cpu_percent`]: the thread caps
//!   [`limit_args`] writes into the ffmpeg command line, plus below-normal
//!   scheduling priority ([`background`]) so the rest of the desktop always
//!   preempts it.
//!
//! A job that is hardware from end to end (decode, encode and nothing heavy between) spends
//! little of the cores and takes a slot of its own instead ([`Resource::Gpu`], [`MAX_GPU_JOBS`]
//! beside the one CPU job), with the same three lanes. Only an export can be one: a preview proxy
//! is an x264 encode on the cores whatever decodes it, so it takes the cores' slot.
//!
//! At **100%** the second half is off entirely: no thread flags are added and
//! priority is untouched, so a full-speed render produces byte-identical ffmpeg
//! invocations to the ones Kerf has always issued. The percentage is seeded from
//! `KERF_CPU_PERCENT` and set at runtime by the app's settings.

use crate::error::{Error, Result};
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

/// The slice threads an export's filter graph runs: the budget's where [`limit_args`] caps them, else
/// FFmpeg's own default (`avpriv_slicethread_create`: one more than the cores, at most 16; one on a single
/// core). A filter that slices a frame can depend on it (`chromakey` starts its window over at each
/// slice). FFmpeg counts the cores it may run on, `cores` also a cgroup's quota: where the two differ, so
/// can this.
pub fn graph_threads() -> usize {
    graph_threads_for(cores(), budget_threads(), limited())
}

/// [`graph_threads`] for `cores`, a `budget` and whether it caps: pure, for the tests.
pub fn graph_threads_for(cores: usize, budget: usize, limited: bool) -> usize {
    if limited && budget > 0 && budget < cores {
        budget
    } else if cores > 1 {
        (cores + 1).min(16)
    } else {
        1
    }
}

// ---- the heavy-job gate -----------------------------------------------------

/// Which of the machine's resources a heavy job spends, and so which slot it takes. The cores
/// are one slot: a software job uses as many as it is given, so a second beside it is two shares of
/// the machine where `cpu_percent` says one. The graphics hardware's decoder and encoder are
/// another: a job that is **hardware from end to end** (decode, scale on the way through, encode:
/// no tone-map, no per-pixel filter, no software encoder) uses 0.3 to 0.7 of a core to feed them,
/// does not slow a software job and is not slowed by one, and two of them together finish about a
/// third sooner than one after the other (measured; a third adds nothing). Whether a job is one is
/// the caller's to decide from what it is about to run, and it is conservative: a job that fell
/// back to software while holding only a hardware slot would be a second software job on the
/// cores, which is why an export's software rerun is a new attempt that asks for the cores' slot
/// (judged again on its own argv).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    /// The cores: one job at a time, whatever it runs.
    Cpu,
    /// The hardware's decoder and encoder: up to [`MAX_GPU_JOBS`] together, beside the one CPU job.
    Gpu,
}

/// Hardware jobs that may run together: a third gains nothing (measured on one machine).
pub const MAX_GPU_JOBS: usize = 2;

/// Which lane of the queue a heavy job waits in. One job runs at a time per resource; when it ends
/// the next comes from the highest lane that has anyone waiting, in arrival order within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Anything a user is actively waiting on: an export (and each variant of one), a
    /// loudness measurement, a voiceover, the stitch of an Insta360 pair at import, a smart
    /// crop. Goes before everything else — an export started on a cold project must not sit at
    /// 0% behind every proxy build of a project of eighty clips. The default lane.
    Foreground,
    /// A preview proxy: the preview is decoding the original until it lands, so it goes
    /// before analysis (and everything else in [`Priority::Background`]) that is waiting or
    /// arrives meanwhile. After the foreground: someone waiting on a render is waiting on
    /// *that*; a proxy is wanted soon, not now.
    Proxy,
    /// Analysis steps and transcription: wanted eventually, in order.
    Background,
}

/// The queue for one resource: the three lanes, and the jobs that hold its slots.
struct Class {
    /// How many jobs may hold a slot at once.
    capacity: usize,
    /// The lane of each job holding a slot — for saying what a waiter waits for.
    running: Vec<Priority>,
    /// Tickets of the `Foreground` / `Background` jobs waiting, in arrival order: each lane
    /// takes the slot first-come first-served, so a long queue of analyses runs in the order
    /// it was asked for rather than whichever thread the scheduler wakes.
    foreground: std::collections::VecDeque<u64>,
    background: std::collections::VecDeque<u64>,
    /// `Proxy` jobs that have asked for the slot or [reserved](reserve) one and not got it
    /// yet. While any exist, no `Background` job takes the slot.
    proxy: usize,
}

impl Class {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            running: Vec::new(),
            foreground: std::collections::VecDeque::new(),
            background: std::collections::VecDeque::new(),
            proxy: 0,
        }
    }

    fn full(&self) -> bool {
        self.running.len() >= self.capacity
    }

    /// Whether the waiter `ticket` of lane `prio` may take a slot now.
    fn may_go(&self, prio: Priority, ticket: u64) -> bool {
        !self.full()
            && match prio {
                Priority::Foreground => self.foreground.front() == Some(&ticket),
                Priority::Proxy => self.foreground.is_empty(),
                Priority::Background => self.foreground.is_empty() && self.proxy == 0 && self.background.front() == Some(&ticket),
            }
    }

    /// The waiter leaves its queue and holds a slot.
    fn take(&mut self, prio: Priority) {
        match prio {
            Priority::Foreground => {
                self.foreground.pop_front();
            }
            Priority::Background => {
                self.background.pop_front();
            }
            Priority::Proxy => self.proxy = self.proxy.saturating_sub(1),
        }
        self.running.push(prio);
    }

    /// A job of lane `prio` gives its slot back.
    fn release(&mut self, prio: Priority) {
        if let Some(at) = self.running.iter().position(|p| *p == prio) {
            self.running.remove(at);
        }
    }

    /// The waiter gives its place up without having held a slot.
    fn forget(&mut self, prio: Priority, ticket: u64) {
        match prio {
            Priority::Foreground => self.foreground.retain(|t| *t != ticket),
            Priority::Background => self.background.retain(|t| *t != ticket),
            Priority::Proxy => self.proxy = self.proxy.saturating_sub(1),
        }
    }

    /// What a waiter of lane `prio` is waiting for.
    fn reason(&self, prio: Priority) -> Wait {
        let proxy_ahead = self.running.contains(&Priority::Proxy) || (prio == Priority::Background && self.proxy > 0);
        if proxy_ahead && prio != Priority::Proxy {
            Wait::Proxy
        } else {
            Wait::Job
        }
    }
}

struct GateState {
    cpu: Class,
    gpu: Class,
    next_ticket: u64,
}

impl GateState {
    fn class(&mut self, resource: Resource) -> &mut Class {
        match resource {
            Resource::Cpu => &mut self.cpu,
            Resource::Gpu => &mut self.gpu,
        }
    }
}

struct Gate {
    state: Mutex<GateState>,
    free: Condvar,
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| Gate {
        state: Mutex::new(GateState {
            cpu: Class::new(1),
            gpu: Class::new(MAX_GPU_JOBS),
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

/// How often a job waiting for the slot looks at its cancel flag.
const WAIT_POLL: std::time::Duration = std::time::Duration::from_millis(250);

thread_local! {
    /// Nesting depth on this thread. A leased job that calls another leased
    /// helper (an export's second pass, a stitch inside an import) must not
    /// queue behind itself.
    static HELD: Cell<usize> = const { Cell::new(0) };

    /// The lane a plain [`lease`] on this thread waits in: the foreground, unless the thread
    /// is running inside [`in_background`].
    static LANE: Cell<Priority> = const { Cell::new(Priority::Foreground) };
}

/// The lane a plain [`lease`] on this thread waits in.
pub fn lane() -> Priority {
    LANE.with(|l| l.get())
}

/// Run `f` with every plain [`lease`] on this thread in the **background** lane. Analysis
/// uses it, so the whole-file decodes it reaches into — silence, scenes, loudness, the PCM
/// behind rhythm and whisper — queue behind foreground work and proxies without each of
/// them having to know.
pub fn in_background<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(Priority);
    impl Drop for Restore {
        fn drop(&mut self) {
            LANE.with(|l| l.set(self.0));
        }
    }
    let _restore = Restore(LANE.with(|l| l.replace(Priority::Background)));
    f()
}

/// A heavy-job slot, held until dropped.
pub struct Lease {
    threads: usize,
    /// A nested lease owns no slot; only the outermost releases it.
    nested: bool,
    resource: Resource,
    priority: Priority,
}

impl Lease {
    /// How many CPU threads this job may use.
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Which resource's slot this holds.
    pub fn resource(&self) -> Resource {
        self.resource
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        HELD.with(|h| h.set(h.get().saturating_sub(1)));
        if self.nested {
            return;
        }
        let gate = gate();
        state(gate).class(self.resource).release(self.priority);
        // Everyone re-checks: which waiter is next depends on the lanes, and a
        // single wake-up could land on one that has to keep waiting.
        gate.free.notify_all();
    }
}

/// What a job waiting for the slot is waiting for — said out loud (an export's progress, an
/// analysis step's stage) so a job queued behind another does not look hung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// A preview proxy is being built (or queued ahead of it).
    Proxy,
    /// Another heavy job holds the slot or is ahead in the queue.
    Job,
}

impl Wait {
    /// The sentence for a progress line.
    pub fn message(self) -> &'static str {
        match self {
            Wait::Proxy => "waiting for the preview proxy",
            Wait::Job => "waiting for another job to finish",
        }
    }
}

/// A queued [`Priority::Proxy`] job's place in front of the background lane, held from the
/// moment it is queued until it takes the slot (or is dropped, which gives the place up).
///
/// Without it a proxy would only outrank analysis once its worker thread had got
/// as far as asking for the slot — after an ffprobe — by which time the analysis
/// of the very file it was queued for had taken the slot first.
#[must_use = "a dropped reservation gives its place up"]
pub struct Reservation {
    held: bool,
}

/// Put a proxy in front of the background lane now, and take the slot with
/// [`Reservation::lease`] when it is its turn.
pub fn reserve() -> Reservation {
    state(gate()).cpu.proxy += 1;
    Reservation { held: true }
}

impl Reservation {
    /// Wait for the slot and take it, ahead of every background job (tests; a job with a cancel
    /// flag uses [`Reservation::lease_waiting`]).
    #[cfg(test)]
    pub fn lease(mut self) -> Lease {
        self.held = false;
        settled(acquire(Resource::Cpu, Priority::Proxy, true, &mut |_| {}, &|| false))
    }

    /// [`Reservation::lease`] that says why it waits and gives up when `cancel` turns true
    /// — the place is released either way.
    pub fn lease_waiting(mut self, on_wait: &mut dyn FnMut(Wait), cancel: &dyn Fn() -> bool) -> Result<Lease> {
        self.held = false;
        acquire(Resource::Cpu, Priority::Proxy, true, on_wait, cancel)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.held {
            let gate = gate();
            let mut s = state(gate);
            s.cpu.proxy = s.cpu.proxy.saturating_sub(1);
            drop(s);
            gate.free.notify_all();
        }
    }
}

/// Whether a proxy is queued or waiting — what a background job that is about to
/// ask for the slot would be made to wait behind.
pub fn high_pending() -> bool {
    state(gate()).cpu.proxy > 0
}

/// Whether jobs that are hardware from end to end may take the hardware's slot instead of the
/// cores': on unless `KERF_GPU_LANE` is `off` / `0` / `false`, or [`set_gpu_lane`] turned it off.
pub fn gpu_lane() -> bool {
    match GPU_LANE.load(Ordering::Relaxed) {
        0 => {
            let on = !std::env::var("KERF_GPU_LANE")
                .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "0" | "false" | "no"));
            GPU_LANE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
        1 => true,
        _ => false,
    }
}

/// Turn the hardware lane on or off at runtime. Takes effect on the next job to ask which
/// resource it spends; a job running keeps its slot.
pub fn set_gpu_lane(on: bool) {
    GPU_LANE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
}

/// 0: not resolved yet (the first read seeds it from the environment), 1: on, 2: off.
static GPU_LANE: AtomicU8 = AtomicU8::new(0);

/// Wait for the heavy-job slot and take it, in this thread's lane ([`lane`]).
///
/// Every pass that reads a whole file goes through here, which is what keeps
/// eight concurrent agent analyses from becoming eight concurrent full-file
/// decodes. Callers must not hold the project lock across this — the wait is
/// unbounded by design (a queued job waits out the render ahead of it). A job that has a
/// cancel flag and a way to speak ([`lease_waiting`]) should use that instead: it can say
/// why it waits and be stopped while it does. Every job in the engine does, so this is what the
/// gate's own tests take.
#[cfg(test)]
pub fn lease() -> Lease {
    lease_priority(lane())
}

/// [`lease`] in the given lane.
#[cfg(test)]
pub fn lease_priority(priority: Priority) -> Lease {
    settled(acquire(Resource::Cpu, priority, false, &mut |_| {}, &|| false))
}

/// [`lease`] that says why it is waiting (`on_wait`, called when it first has to wait and
/// again if the reason changes) and gives up with [`Error::Cancelled`] when `cancel` turns
/// true — polled four times a second while it waits. Stop and Cancel must work on a job that
/// is still in the queue.
pub fn lease_waiting(on_wait: &mut dyn FnMut(Wait), cancel: &dyn Fn() -> bool) -> Result<Lease> {
    acquire(Resource::Cpu, lane(), false, on_wait, cancel)
}

/// [`lease_waiting`] in the given lane.
pub fn lease_priority_waiting(priority: Priority, on_wait: &mut dyn FnMut(Wait), cancel: &dyn Fn() -> bool) -> Result<Lease> {
    acquire(Resource::Cpu, priority, false, on_wait, cancel)
}

/// [`lease_waiting`] for a job that is hardware from end to end: the hardware's slot
/// ([`Resource::Gpu`]), in this thread's lane. See [`Resource`] for what qualifies; a job that
/// might not should take the cores' slot.
pub fn lease_gpu_waiting(on_wait: &mut dyn FnMut(Wait), cancel: &dyn Fn() -> bool) -> Result<Lease> {
    acquire(Resource::Gpu, lane(), false, on_wait, cancel)
}

/// [`lease_gpu_waiting`] in the given lane (the gate's tests: an export is the only hardware job,
/// and it is foreground).
#[cfg(test)]
pub fn lease_gpu_priority_waiting(priority: Priority, on_wait: &mut dyn FnMut(Wait), cancel: &dyn Fn() -> bool) -> Result<Lease> {
    acquire(Resource::Gpu, priority, false, on_wait, cancel)
}

/// An acquisition that cannot be cancelled has no `Err` to report.
#[cfg(test)]
fn settled(lease: Result<Lease>) -> Lease {
    match lease {
        Ok(lease) => lease,
        Err(_) => unreachable!("an acquisition that cannot be cancelled was cancelled"),
    }
}

/// A lease for a thread that already holds one: owns no slot, so it cannot wait.
fn nested_lease(resource: Resource, priority: Priority) -> Option<Lease> {
    let depth = HELD.with(|h| h.get());
    HELD.with(|h| h.set(depth + 1));
    (depth > 0).then(|| Lease {
        threads: budget_threads(),
        nested: true,
        resource,
        priority,
    })
}

/// The one place a slot is taken. `reserved`: the caller already counts in the cores' `proxy` (a
/// [`Reservation`]).
fn acquire(
    resource: Resource,
    priority: Priority,
    reserved: bool,
    on_wait: &mut dyn FnMut(Wait),
    cancel: &dyn Fn() -> bool,
) -> Result<Lease> {
    let gate = gate();
    if let Some(nested) = nested_lease(resource, priority) {
        if reserved {
            let mut s = state(gate);
            s.cpu.proxy = s.cpu.proxy.saturating_sub(1);
            drop(s);
            gate.free.notify_all();
        }
        return Ok(nested);
    }
    let depth = HELD.with(|h| h.get()).saturating_sub(1);
    let mut s = state(gate);
    let ticket = s.next_ticket;
    s.next_ticket += 1;
    {
        let class = s.class(resource);
        match priority {
            Priority::Foreground => class.foreground.push_back(ticket),
            Priority::Background => class.background.push_back(ticket),
            Priority::Proxy if !reserved => class.proxy += 1,
            Priority::Proxy => {}
        }
    }
    let mut said: Option<Wait> = None;
    loop {
        if s.class(resource).may_go(priority, ticket) {
            s.class(resource).take(priority);
            break;
        }
        if cancel() {
            s.class(resource).forget(priority, ticket);
            drop(s);
            gate.free.notify_all();
            HELD.with(|h| h.set(depth));
            return Err(Error::Cancelled);
        }
        let reason = s.class(resource).reason(priority);
        if said != Some(reason) {
            said = Some(reason);
            // Not under the gate's lock: the callback is the caller's.
            drop(s);
            on_wait(reason);
            s = state(gate);
            continue;
        }
        s = match gate.free.wait_timeout(s, WAIT_POLL) {
            Ok((guard, _)) => guard,
            Err(e) => e.into_inner().0,
        };
    }
    drop(s);
    Ok(Lease {
        threads: budget_threads(),
        nested: false,
        resource,
        priority,
    })
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

/// Cap an argv that reads **one frame** from sources that need next to no decoding
/// (a preview proxy: one intra frame is a few milliseconds) to a single thread
/// everywhere but the decoders: the filter graph, and the encoder just before the
/// sink. Thread pools are created and torn down per process, which for a frame that
/// cheap is a quarter of the whole run; the share of the machine they would be
/// given has no work to spend them on.
///
/// Holds at any budget, 100% included (like [`cap_args`]): it is the nature of the
/// read, not the user's share, that is being applied. The decoders are the caller's:
/// one `-threads 1` ahead of each `-i` (this cannot know which inputs are cheap).
pub fn single_frame_args(args: &mut Vec<String>) {
    if let Some(sink) = args.len().checked_sub(1) {
        args.splice(sink..sink, ["-threads".to_string(), "1".to_string()]);
    }
    args.splice(
        0..0,
        ["-filter_threads", "1", "-filter_complex_threads", "1"].map(String::from),
    );
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

/// [`limit_args`] for one run that writes several files (`sinks`: the argv index of each
/// output path). The decode is capped once at the front, as ever; the encoders run side by
/// side in the one process, so each output's `-threads` is its share of the budget
/// ([`shared_threads`]), written just before its sink. A no-op at 100%, like [`limit_args`].
pub fn limit_outputs_args(args: &mut Vec<String>, threads: usize, sinks: &[usize]) {
    let head = head_flags(threads);
    if head.is_empty() {
        return;
    }
    let each = shared_threads(threads, sinks.len()).to_string();
    let mut sinks = sinks.to_vec();
    sinks.sort_unstable();
    let len = args.len();
    for &sink in sinks.iter().rev().filter(|&&s| s < len) {
        args.splice(sink..sink, ["-threads".to_string(), each.clone()]);
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

/// Drop the calling thread to below-normal scheduling priority: [`background`]'s counterpart for a
/// worker thread of this process (the rasterisers of an animated title), at the same budgets — left
/// alone at 100%. The thread is not restored; call it first thing in a thread that ends with its work.
///
/// Linux lowers the thread's own nice value by 10, **relative to what it has** like [`background`]'s
/// `nice(10)` (a thread already lower stays lower). macOS puts it in the *utility* QoS class — below
/// the interface's, but not the *background* class (`PRIO_DARWIN_BG`), which throttles disk I/O hard
/// and what these threads write is a picture a user is waiting to see. **Windows is left alone**:
/// `SetThreadPriority` needs a binding Kerf does not otherwise carry.
pub fn background_thread() {
    if limited() {
        lower_current_thread();
    }
}

/// [`background_thread`] at any budget.
fn lower_current_thread() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `nice` takes no pointers; Linux keeps a nice value per thread, so it is the calling
        // thread's alone that moves, and lowering a priority needs no privilege (a failure changes nothing).
        unsafe {
            libc::nice(10);
        }
    }
    #[cfg(target_vendor = "apple")]
    {
        extern "C" {
            // `pthread/qos.h`: `qos_class_t` is an `unsigned int`; the call affects the calling thread.
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        const QOS_CLASS_UTILITY: u32 = 0x11;
        // SAFETY: plain integers in, the calling thread's own class changed; a failure changes nothing.
        unsafe {
            pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0);
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
    fn a_graph_runs_the_budgets_threads_where_they_are_capped_and_ffmpegs_default_else() {
        assert_eq!(graph_threads_for(8, 4, true), 4);
        assert_eq!(graph_threads_for(8, 8, true), 9);
        assert_eq!(graph_threads_for(8, 4, false), 9);
        assert_eq!(graph_threads_for(4, 4, false), 5);
        assert_eq!(graph_threads_for(32, 32, false), 16);
        assert_eq!(graph_threads_for(1, 1, true), 1);
    }

    #[test]
    fn threads_scale_with_the_budget() {
        assert_eq!(threads_for(16, 100), 16);
        assert_eq!(threads_for(16, 75), 12);
        assert_eq!(threads_for(16, 50), 8);
        assert_eq!(threads_for(12, 75), 9);
    }

    #[cfg(target_os = "linux")]
    fn nice_of_this_thread() -> i32 {
        // SAFETY: reads the scheduling priority of our own task id.
        unsafe { libc::getpriority(libc::PRIO_PROCESS as _, libc::gettid() as libc::id_t) }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_worker_thread_drops_below_normal_priority_alone() {
        let _serial = exclusive();
        let restore = cpu_percent();
        // The thread, not the process: the thread that asks is lowered, this one is not.
        let before = nice_of_this_thread();
        set_cpu_percent(MIN_CPU_PERCENT);
        let lowered = std::thread::spawn(|| {
            background_thread();
            nice_of_this_thread()
        })
        .join()
        .unwrap();
        assert_eq!(lowered, (before + 10).min(19), "below normal, from {before}");
        assert_eq!(nice_of_this_thread(), before, "the caller's own priority is untouched");
        // Relative, as `background`'s `nice(10)` is: a thread that is already lower is never raised.
        let already = std::thread::spawn(|| {
            // SAFETY: lowers this thread's own priority.
            unsafe {
                libc::nice(12);
            }
            let start = nice_of_this_thread();
            background_thread();
            (start, nice_of_this_thread())
        })
        .join()
        .unwrap();
        assert_eq!(already.1, (already.0 + 10).min(19), "from {}", already.0);
        assert!(already.1 >= already.0, "never raised");
        // At 100% the priority is not touched at all.
        set_cpu_percent(100);
        let left = std::thread::spawn(|| {
            background_thread();
            nice_of_this_thread()
        })
        .join()
        .unwrap();
        assert_eq!(left, before);
        set_cpu_percent(restore);
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
    fn a_run_writing_several_files_shares_the_encoders_threads() {
        let _serial = exclusive();
        let restore = cpu_percent();
        set_cpu_percent(100);
        let original: Vec<String> = ["-i", "in.mp4", "-map", "[a]", "a.mp4", "-map", "[b]", "b.mp4"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut full = original.clone();
        limit_outputs_args(&mut full, 1, &[4, 7]);
        assert_eq!(full, original, "a 100% budget leaves the command line alone");
        set_cpu_percent(MIN_CPU_PERCENT);
        let threads = budget_threads();
        let mut args = original;
        limit_outputs_args(&mut args, threads, &[4, 7]);
        if threads < cores() {
            let n = threads.to_string();
            let each = shared_threads(threads, 2).to_string();
            let mut want: Vec<String> = ["-threads", &n, "-filter_threads", &n, "-filter_complex_threads", &n]
                .map(String::from)
                .to_vec();
            want.extend(
                [
                    "-i", "in.mp4", "-map", "[a]", "-threads", &each, "a.mp4", "-map", "[b]", "-threads", &each, "b.mp4",
                ]
                .map(String::from),
            );
            assert_eq!(args, want);
        }
        set_cpu_percent(restore);
    }

    #[test]
    fn a_single_frame_read_is_one_thread_at_any_budget() {
        let _serial = exclusive();
        let restore = cpu_percent();
        for percent in [MIN_CPU_PERCENT, 100] {
            set_cpu_percent(percent);
            let mut args: Vec<String> = ["-hide_banner", "-threads", "1", "-i", "p.mp4", "-frames:v", "1", "pipe:1"]
                .iter()
                .map(|s| s.to_string())
                .collect();
            single_frame_args(&mut args);
            assert_eq!(
                args.join(" "),
                "-filter_threads 1 -filter_complex_threads 1 -hide_banner -threads 1 -i p.mp4 -frames:v 1 -threads 1 pipe:1",
                "at {percent}%"
            );
        }
        // Nothing to cap in an empty argv, and nothing to find a sink in.
        let mut none: Vec<String> = Vec::new();
        single_frame_args(&mut none);
        assert_eq!(none, ["-filter_threads", "1", "-filter_complex_threads", "1"]);
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

    /// Run a thread that takes the slot in `priority`, reporting `label` on `order` once it
    /// holds the lease and releasing it straight away.
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

    /// How many foreground and background jobs are waiting for the slot.
    fn queued() -> (usize, usize) {
        let s = state(gate());
        (s.cpu.foreground.len(), s.cpu.background.len())
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
    fn the_foreground_goes_before_a_queued_proxy_and_a_proxy_before_background_work() {
        let _serial = exclusive();
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        let held = lease();
        // Background work waits first, then a proxy (queued with its place reserved), then an
        // export: the export, which arrived last, goes first.
        let analysis = waiter("analysis", Priority::Background, &order);
        wait_until("the analysis to queue", || queued() == (0, 1));
        let reservation = reserve();
        let proxy = {
            let order = order.clone();
            std::thread::spawn(move || {
                let _lease = reservation.lease();
                order.lock().unwrap().push("proxy");
            })
        };
        wait_until("the proxy to queue", high_pending);
        let export = waiter("export", Priority::Foreground, &order);
        wait_until("the export to queue", || queued() == (1, 1));
        drop(held);
        for t in [analysis, proxy, export] {
            t.join().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), ["export", "proxy", "analysis"]);
    }

    #[test]
    fn each_lane_takes_the_slot_in_the_order_it_asked() {
        let _serial = exclusive();
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        let held = lease();
        let mut threads = Vec::new();
        for (i, label) in ["a", "b", "c", "d"].into_iter().enumerate() {
            threads.push(waiter(label, Priority::Background, &order));
            wait_until("the waiter to queue", || queued() == (0, i + 1));
        }
        drop(held);
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), ["a", "b", "c", "d"]);

        let held = lease();
        let mut threads = Vec::new();
        for (i, label) in ["e", "f", "g"].into_iter().enumerate() {
            threads.push(waiter(label, Priority::Foreground, &order));
            wait_until("the waiter to queue", || queued() == (i + 1, 0));
        }
        drop(held);
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(order.lock().unwrap()[4..], ["e", "f", "g"]);
    }

    #[test]
    fn a_reservation_keeps_background_work_out_but_not_the_foreground() {
        let _serial = exclusive();
        // The slot is free, but a proxy is queued: an analysis that starts now must not slip in
        // ahead of it just because the proxy's worker is still probing…
        let reservation = reserve();
        assert!(high_pending());
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        let analysis = waiter("analysis", Priority::Background, &order);
        wait_until("the analysis to queue", || queued() == (0, 1));
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(order.lock().unwrap().is_empty(), "the analysis ran ahead of a queued proxy");
        // …while an export is not held up by it: the user is waiting on the export.
        let export = waiter("export", Priority::Foreground, &order);
        export.join().unwrap();
        assert_eq!(*order.lock().unwrap(), ["export"]);
        let proxy = reservation.lease();
        order.lock().unwrap().push("proxy");
        // …and the analysis does not run beside the proxy either: one heavy job at a time.
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(*order.lock().unwrap(), ["export", "proxy"]);
        drop(proxy);
        analysis.join().unwrap();
        assert_eq!(*order.lock().unwrap(), ["export", "proxy", "analysis"]);
        assert!(!high_pending());
    }

    #[test]
    fn a_dropped_reservation_lets_the_queue_move() {
        let _serial = exclusive();
        let reservation = reserve();
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        let analysis = waiter("analysis", Priority::Background, &order);
        wait_until("the analysis to queue", || queued() == (0, 1));
        drop(reservation);
        analysis.join().unwrap();
        assert_eq!(*order.lock().unwrap(), ["analysis"]);
        assert!(!high_pending());
    }

    #[test]
    fn a_lease_inside_a_lease_does_not_wait_for_itself_in_any_lane() {
        let _serial = exclusive();
        let outer = lease_priority(Priority::Background);
        // A proxy made inside a leased job, an export started under an analysis…
        let inner = reserve().lease();
        assert_eq!(inner.threads(), outer.threads());
        assert!(!high_pending(), "the reservation was spent");
        let fg = lease_priority(Priority::Foreground);
        drop(inner);
        drop(fg);
        drop(outer);
        let _next = lease();
    }

    #[test]
    fn plain_leases_follow_the_threads_lane() {
        let _serial = exclusive();
        assert_eq!(lane(), Priority::Foreground);
        in_background(|| {
            assert_eq!(lane(), Priority::Background);
            in_background(|| assert_eq!(lane(), Priority::Background));
            assert_eq!(lane(), Priority::Background);
        });
        assert_eq!(lane(), Priority::Foreground, "the lane is restored");
        // A panic inside it restores the lane too.
        let _ = std::panic::catch_unwind(|| in_background(|| panic!("boom")));
        assert_eq!(lane(), Priority::Foreground);
    }

    #[test]
    fn a_waiting_job_says_why_and_can_be_stopped_while_it_waits() {
        let _serial = exclusive();
        let held = lease();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let said = std::sync::Arc::new(Mutex::new(Vec::<Wait>::new()));
        let worker = {
            let (cancel, said) = (cancel.clone(), said.clone());
            std::thread::spawn(move || {
                lease_priority_waiting(Priority::Background, &mut |w| said.lock().unwrap().push(w), &|| {
                    cancel.load(Ordering::SeqCst)
                })
                .map(|_| ())
            })
        };
        wait_until("the job to say it waits", || !said.lock().unwrap().is_empty());
        assert_eq!(*said.lock().unwrap(), [Wait::Job], "another job has the slot");
        assert_eq!(queued(), (0, 1));
        // Stop while queued: it returns Cancelled, promptly, and leaves the queue.
        let stopped = std::time::Instant::now();
        cancel.store(true, Ordering::SeqCst);
        let result = worker.join().unwrap();
        assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
        assert!(stopped.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(queued(), (0, 0), "a cancelled waiter does not stay queued");
        // Nothing is wedged: the slot still goes to the next job, and the depth is right.
        drop(held);
        let _next = lease();
    }

    #[test]
    fn a_proxy_that_is_stopped_while_queued_gives_its_place_up() {
        let _serial = exclusive();
        let held = lease();
        let reservation = reserve();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker = {
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                reservation
                    .lease_waiting(&mut |_| {}, &|| cancel.load(Ordering::SeqCst))
                    .map(|_| ())
            })
        };
        wait_until("the proxy to wait", high_pending);
        cancel.store(true, Ordering::SeqCst);
        assert!(matches!(worker.join().unwrap(), Err(Error::Cancelled)));
        assert!(!high_pending(), "the stopped proxy no longer holds background work back");
        drop(held);
    }

    #[test]
    fn who_a_job_waits_for_depends_on_its_lane() {
        let _serial = exclusive();
        let said = |priority: Priority, held: Lease| {
            let said = std::sync::Arc::new(Mutex::new(Vec::<Wait>::new()));
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let worker = {
                let (cancel, said) = (cancel.clone(), said.clone());
                std::thread::spawn(move || {
                    let _ = lease_priority_waiting(priority, &mut |w| said.lock().unwrap().push(w), &|| {
                        cancel.load(Ordering::SeqCst)
                    });
                })
            };
            wait_until("the job to say it waits", || !said.lock().unwrap().is_empty());
            cancel.store(true, Ordering::SeqCst);
            worker.join().unwrap();
            drop(held);
            let said = said.lock().unwrap().clone();
            said
        };
        // An export behind a running proxy is waiting for that proxy; behind another export, a job.
        assert_eq!(said(Priority::Foreground, lease_priority(Priority::Proxy)), [Wait::Proxy]);
        assert_eq!(said(Priority::Foreground, lease_priority(Priority::Foreground)), [Wait::Job]);
        // Analysis behind a queued proxy is waiting for it, even while an export runs.
        let reservation = reserve();
        assert_eq!(
            said(Priority::Background, lease_priority(Priority::Foreground)),
            [Wait::Proxy]
        );
        drop(reservation);
        assert_eq!(said(Priority::Background, lease_priority(Priority::Foreground)), [Wait::Job]);
        assert!(Wait::Proxy.message().contains("proxy"));
    }

    // ---- the hardware's slot -----------------------------------------------------------------
    //
    // A lease taken on a thread that already holds one is nested (it owns no slot and cannot
    // wait: see `a_nested_lease_does_not_wait_for_itself`), so a test that needs two slots held
    // holds each on a thread of its own.

    /// How many foreground and background jobs are waiting for a hardware slot.
    fn gpu_queued() -> (usize, usize) {
        let s = state(gate());
        (s.gpu.foreground.len(), s.gpu.background.len())
    }

    fn gpu_lease(priority: Priority) -> Lease {
        settled(lease_gpu_priority_waiting(priority, &mut |_| {}, &|| false))
    }

    /// A slot of `resource` held on a thread of its own until [`Held::release`].
    struct Held {
        release: Option<std::sync::mpsc::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    fn hold(resource: Resource, priority: Priority) -> Held {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (ready, ready_rx) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let _lease = match resource {
                Resource::Cpu => lease_priority(priority),
                Resource::Gpu => gpu_lease(priority),
            };
            ready.send(()).unwrap();
            let _ = wait.recv();
        });
        ready_rx.recv().unwrap();
        Held {
            release: Some(release),
            thread: Some(thread),
        }
    }

    impl Held {
        fn release(mut self) {
            drop(self.release.take());
            self.thread.take().unwrap().join().unwrap();
        }
    }

    impl Drop for Held {
        fn drop(&mut self) {
            drop(self.release.take());
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    /// Whether a lease of `resource` is to be had within a second: a test that expects a slot
    /// must fail, not hang, when it is not there. Taken on a thread of its own.
    fn can_have(resource: Resource, priority: Priority) -> bool {
        std::thread::spawn(move || {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
            let cancel = || std::time::Instant::now() >= until;
            match resource {
                Resource::Cpu => lease_priority_waiting(priority, &mut |_| {}, &cancel).is_ok(),
                Resource::Gpu => lease_gpu_priority_waiting(priority, &mut |_| {}, &cancel).is_ok(),
            }
        })
        .join()
        .unwrap()
    }

    fn gpu_waiter(
        label: &'static str,
        priority: Priority,
        order: &std::sync::Arc<Mutex<Vec<&'static str>>>,
    ) -> std::thread::JoinHandle<()> {
        let order = order.clone();
        std::thread::spawn(move || {
            let _lease = gpu_lease(priority);
            order.lock().unwrap().push(label);
        })
    }

    #[test]
    fn a_hardware_job_runs_beside_the_one_cpu_job_and_does_not_take_its_slot() {
        let _serial = exclusive();
        let cores = hold(Resource::Cpu, Priority::Foreground);
        assert!(
            can_have(Resource::Gpu, Priority::Foreground),
            "the hardware is free while the cores are busy"
        );
        // The cores are still one job's: another software job waits behind the first.
        assert!(!can_have(Resource::Cpu, Priority::Foreground));
        cores.release();
        assert!(can_have(Resource::Cpu, Priority::Foreground));
    }

    #[test]
    fn a_software_job_does_not_wait_for_the_hardware_either() {
        let _serial = exclusive();
        let (a, b) = (
            hold(Resource::Gpu, Priority::Foreground),
            hold(Resource::Gpu, Priority::Foreground),
        );
        assert!(
            !can_have(Resource::Gpu, Priority::Foreground),
            "both hardware slots are taken"
        );
        assert!(can_have(Resource::Cpu, Priority::Foreground), "the cores' is free");
        a.release();
        b.release();
    }

    #[test]
    fn no_more_than_the_limit_of_hardware_jobs_run_together() {
        let _serial = exclusive();
        assert_eq!(MAX_GPU_JOBS, 2);
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        let (a, b) = (
            hold(Resource::Gpu, Priority::Foreground),
            hold(Resource::Gpu, Priority::Foreground),
        );
        let third = gpu_waiter("third", Priority::Foreground, &order);
        wait_until("the third to queue", || gpu_queued() == (1, 0));
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(order.lock().unwrap().is_empty(), "a third ran beside two");
        a.release();
        third.join().unwrap();
        assert_eq!(*order.lock().unwrap(), ["third"]);
        b.release();
    }

    #[test]
    fn the_hardware_slot_has_the_same_three_lanes() {
        let _serial = exclusive();
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        let (a, b) = (
            hold(Resource::Gpu, Priority::Foreground),
            hold(Resource::Gpu, Priority::Foreground),
        );
        // Arrival order: background, proxy, foreground. The foreground arrived last and goes first;
        // the proxy goes before the background. One slot is freed, so they go one at a time.
        let background = gpu_waiter("background", Priority::Background, &order);
        wait_until("the background job to queue", || gpu_queued() == (0, 1));
        let proxy = gpu_waiter("proxy", Priority::Proxy, &order);
        wait_until("the proxy to queue", || state(gate()).gpu.proxy == 1);
        let foreground = gpu_waiter("foreground", Priority::Foreground, &order);
        wait_until("the foreground job to queue", || gpu_queued() == (1, 1));
        a.release();
        for t in [background, proxy, foreground] {
            t.join().unwrap();
        }
        assert_eq!(*order.lock().unwrap(), ["foreground", "proxy", "background"]);
        b.release();
    }

    #[test]
    fn a_lease_taken_inside_a_lease_never_waits_whichever_slot_it_asks_for() {
        let _serial = exclusive();
        let (a, b) = (
            hold(Resource::Gpu, Priority::Foreground),
            hold(Resource::Gpu, Priority::Foreground),
        );
        // Both hardware slots are held elsewhere. A thread that already holds a lease is nested in
        // it, and does not queue behind itself or anyone: an export's second pass, a stitch.
        let outer = lease();
        let inner = gpu_lease(Priority::Foreground);
        assert_eq!(inner.threads(), outer.threads());
        drop((inner, outer));
        a.release();
        b.release();
    }

    #[test]
    fn the_hardware_lane_can_be_turned_off() {
        let _serial = exclusive();
        let was = gpu_lane();
        set_gpu_lane(false);
        assert!(!gpu_lane());
        set_gpu_lane(true);
        assert!(gpu_lane());
        set_gpu_lane(was);
    }
}
