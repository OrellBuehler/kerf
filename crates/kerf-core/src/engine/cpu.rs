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
//!   — never queues, so the UI stays live behind a running render.
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
    let gate = gate();
    let _held = gate.busy.lock();
    gate.free.notify_all();
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

struct Gate {
    busy: Mutex<bool>,
    free: Condvar,
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| Gate {
        busy: Mutex::new(false),
        free: Condvar::new(),
    })
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
        if let Ok(mut busy) = gate.busy.lock() {
            *busy = false;
            gate.free.notify_one();
        }
    }
}

/// Wait for the heavy-job slot and take it.
///
/// Every pass that reads a whole file goes through here, which is what keeps
/// eight concurrent agent analyses from becoming eight concurrent full-file
/// decodes. Callers must not hold the project lock across this — the wait is
/// unbounded by design (a queued job waits out the render ahead of it).
pub fn lease() -> Lease {
    let depth = HELD.with(|h| h.get());
    HELD.with(|h| h.set(depth + 1));
    if depth > 0 {
        return Lease {
            threads: budget_threads(),
            nested: true,
        };
    }
    let gate = gate();
    match gate.busy.lock() {
        Ok(mut busy) => {
            while *busy {
                busy = match gate.free.wait(busy) {
                    Ok(g) => g,
                    // A panicking job poisoned the gate; take the slot rather
                    // than wedging every later job for the rest of the session.
                    Err(e) => e.into_inner(),
                };
            }
            *busy = true;
        }
        // Same: a poisoned mutex must not stop the engine working.
        Err(e) => *e.into_inner() = true,
    }
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
    if !limited() || threads == 0 || threads >= cores() {
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
    let head = head_flags(threads);
    if head.is_empty() {
        return;
    }
    if let Some(sink) = args.len().checked_sub(1) {
        args.splice(sink..sink, ["-threads".to_string(), threads.to_string()]);
    }
    args.splice(0..0, head);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget is process-global, so the tests that move it cannot run
    /// beside each other (cargo runs them on threads of one process).
    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
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
}
