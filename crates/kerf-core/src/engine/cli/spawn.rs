//! Spawning `ffmpeg` / `ffprobe`: the binaries, the commands, and the bounded runs that wait on them.

use super::*;

pub(in crate::engine) fn ffmpeg_bin() -> String {
    #[cfg(test)]
    if let Some(bin) = FFMPEG_OVERRIDE.with(|c| c.borrow().clone()) {
        return bin;
    }
    std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string())
}

#[cfg(test)]
thread_local! {
    /// The `ffmpeg` this thread's runs use while a test pins it (a stand-in that is not on `PATH`).
    static FFMPEG_OVERRIDE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with every `ffmpeg` this thread starts being `bin`: a test's stand-in for hardware the machine
/// does not have. Only this thread's runs: another test's are untouched.
#[cfg(test)]
#[cfg_attr(not(unix), allow(dead_code))]
pub(super) fn with_ffmpeg<R>(bin: &str, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<String>);
    impl Drop for Reset {
        fn drop(&mut self) {
            FFMPEG_OVERRIDE.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(FFMPEG_OVERRIDE.with(|c| c.borrow_mut().replace(bin.to_string())));
    f()
}

/// The `ffmpeg` binary Kerf drives (`KERF_FFMPEG`, else `ffmpeg` on `PATH`) — for
/// callers outside the engine that run it themselves, like the GPU compositor's
/// frame decoder, so they resolve it the way every other run here does.
pub fn ffmpeg_path() -> String {
    ffmpeg_bin()
}

/// A `Command` for the `ffmpeg` binary, set up like every run inside the engine
/// (on Windows `CREATE_NO_WINDOW`, so a GUI app spawning it does not flash a
/// console). Normal priority: it is for interactive reads, which should land now.
pub fn ffmpeg_command() -> Command {
    command(&ffmpeg_bin())
}

/// Apply the CPU budget's thread cap to an `ffmpeg` argv about to be spawned —
/// at spawn time, never in a builder, so the builders keep describing exactly
/// what ffmpeg is handed. `share` is how many such processes run side by side:
/// the budget is the *machine's* share for one job, so each of `share` parallel
/// decodes gets its fraction (at least one thread), even at a full budget. A lone
/// process (`share` 1) at the default 100% is untouched, which is what keeps
/// every run Kerf always issued byte-identical.
pub fn limit_ffmpeg_args(args: &mut Vec<String>, share: usize) {
    cpu::limit_args_shared(args, share);
}

pub(in crate::engine) fn ffprobe_bin() -> String {
    std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string())
}

/// Run `cmd` to completion, but give up on it after `limit`: the child is killed
/// and `None` returned. stdin is closed so nothing can wait on a keypress.
///
/// For probes whose *only* job is to answer "does this work here", where a
/// driver that wedges instead of failing would otherwise take the caller with it
/// (the encoder probe runs inside a `OnceLock` every other thread waits on).
pub(in crate::engine) fn status_within(
    cmd: &mut Command,
    limit: std::time::Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let mut child = cmd.stdin(Stdio::null()).spawn()?;
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

pub(in crate::engine) fn launch_err(bin: &str, e: std::io::Error) -> Error {
    Error::Engine(format!("failed to launch `{bin}` ({e}); is FFmpeg installed and on PATH?"))
}

/// The environment variables that make an OpenMP runtime bind its process's threads as it loads: removed from the environment of every
/// child built by [`command`], so that `av_cpu_count` in FFmpeg's process reads the affinity Kerf's plan read. Which of them bind, what
/// was measured, what is kept and why are in `engine-cli.md` ("The slice threads a graph runs on").
pub(super) const OPENMP_BINDING_VARS: [&str; 4] = ["OMP_PROC_BIND", "OMP_PLACES", "GOMP_CPU_AFFINITY", "KMP_AFFINITY"];

/// Build a `Command` for an ffmpeg/ffprobe binary. On Windows this sets
/// `CREATE_NO_WINDOW` so spawning the console subprocess doesn't flash a
/// terminal window over the GUI; on other platforms it's a plain `Command`.
/// Every child the engine and the GPU compositor start is built here (and so
/// by [`ffmpeg_command`] and [`bg_command`]), which is why this is where the
/// environment is made the one the plan assumed: [`OPENMP_BINDING_VARS`] are
/// removed from the child's. The argv is not touched.
pub(in crate::engine) fn command(bin: &str) -> Command {
    let mut cmd = Command::new(bin);
    for var in OPENMP_BINDING_VARS {
        cmd.env_remove(var);
    }
    #[cfg(windows)]
    let cmd = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = cmd;
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    };
    cmd
}

/// A `Command` for a **background** ffmpeg run: everything [`command`] does,
/// plus below-normal scheduling priority so a render or an analysis pass never
/// out-prioritizes the window the user is actually looking at. Interactive
/// decodes (a scrubbed frame, the preview stream) keep normal priority — they
/// are short, and the whole point of them is to land now.
pub(in crate::engine) fn bg_command(bin: &str) -> Command {
    let mut cmd = command(bin);
    cpu::background(&mut cmd);
    cmd
}

/// Run `cmd` with `input` on its stdin and return its stdout if it exits
/// successfully before `deadline` — killed, and `None`, if it does not. The pipes
/// are served from side threads, so a child that stops reading or never exits
/// cannot wedge the caller. **Nothing here joins a thread that a child can keep
/// alive**: a wrapper script that spawned the real ffmpeg keeps the pipes open
/// past the kill (and past its own exit), so on a timeout the side threads are
/// left to finish on their own, and after a clean exit the output is waited
/// for no longer than the deadline (a short grace past it) either.
pub fn run_piped_until(cmd: &mut Command, input: Vec<u8>, deadline: Instant) -> Option<Vec<u8>> {
    use std::io::{Read, Write};
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?;
    // The input is a few kilobytes and the child may close the pipe before reading
    // it all, which is not the feeder's problem. (Not joined: a descendant that holds
    // the pipe open and does not read keeps a long input's write blocked.)
    std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let (sent, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        let _ = sent.send(out);
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(4)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    // The child has exited, so the pipe is at its end unless a descendant of it holds the other side: the output is
    // there already, and a descendant that outlives the deadline is not waited for.
    let wait = deadline
        .saturating_duration_since(Instant::now())
        .max(std::time::Duration::from_millis(250));
    let out = output.recv_timeout(wait).ok()?;
    status.success().then_some(out)
}

/// How long a whole-file read ([`scene_pass`], [`contact_sheet`]) may go without a word from
/// ffmpeg before it is killed, at the least (the same floor the waveform decode and the
/// filmstrip give theirs). `-progress` reports every [`PROGRESS_ON_STDERR`] second, but **only
/// once a frame has reached the output**: a read that outputs nothing until its last frame is
/// silent for the whole decode, so it gets either a frame to report on early (the scene pass
/// selects frame 0) or a bound that grows with the work ([`stall_for_work`], the sheet's).
pub(in crate::engine) const READ_STALL: std::time::Duration = std::time::Duration::from_secs(60);

/// The most a read that is silent until it ends is given before it counts as wedged.
const READ_STALL_CEILING: std::time::Duration = std::time::Duration::from_secs(2 * 60 * 60);

/// Silence a read of `work_secs` of footage that outputs only at its end is allowed (pure): `per_sec`
/// per second of footage — a decode slower than `1 / per_sec` of real time is a wedge, not a slow
/// machine — between `floor`, which covers start-up and the first seek, and `ceiling`, which keeps
/// even a wedged hour-long read finite. Clamped in seconds first: `from_secs_f64` panics on
/// infinity, and a container can report an absurd duration.
pub(super) fn stall_for_work(
    work_secs: f64,
    per_sec: f64,
    floor: std::time::Duration,
    ceiling: std::time::Duration,
) -> std::time::Duration {
    let secs = if work_secs.is_finite() {
        (work_secs * per_sec).clamp(0.0, ceiling.as_secs_f64())
    } else {
        0.0
    };
    std::time::Duration::from_secs_f64(secs).max(floor)
}

/// The stall bound of a contact sheet of a `window_secs` window: the whole window is decoded
/// before `tile` outputs the one frame, so the read says nothing until it ends. Thirty seconds of
/// silence per second of footage, at least [`READ_STALL`], at most [`READ_STALL_CEILING`].
pub(super) fn sheet_stall(window_secs: f64) -> std::time::Duration {
    stall_for_work(window_secs, 30.0, READ_STALL, READ_STALL_CEILING)
}

/// The `-progress` flags a bounded read adds **at spawn time** (like the thread caps, never in
/// a pure builder): the report goes to stderr, which stdout-carrying reads (a contact sheet's
/// JPEG) leave free, and is what [`output_unless_stalled`] listens for.
pub(in crate::engine) const PROGRESS_ON_STDERR: [&str; 4] = ["-progress", "pipe:2", "-stats_period", "1"];

/// Whether `line` is one of the `key=value` lines of ffmpeg's `-progress` report.
pub(super) fn is_progress_line(line: &str) -> bool {
    let Some((key, _)) = line.trim().split_once('=') else {
        return false;
    };
    key.starts_with("stream_")
        || matches!(
            key,
            "frame"
                | "fps"
                | "bitrate"
                | "total_size"
                | "out_time_us"
                | "out_time_ms"
                | "out_time"
                | "dup_frames"
                | "drop_frames"
                | "speed"
                | "progress"
        )
}

/// What [`output_unless_stalled`] collected.
pub(in crate::engine) struct Collected {
    pub(in crate::engine) status: std::process::ExitStatus,
    pub(in crate::engine) stdout: Vec<u8>,
    /// stderr without the `-progress` report lines (what had arrived, when the read stalled).
    pub(in crate::engine) stderr: String,
    /// ffmpeg said nothing for the stall bound and was killed.
    pub(in crate::engine) stalled: bool,
    /// The caller's cancel tripped and ffmpeg was killed ([`output_supervised`]).
    pub(in crate::engine) cancelled: bool,
    /// The run outlasted its `total` bound and was killed.
    pub(in crate::engine) timed_out: bool,
}

impl Collected {
    pub(in crate::engine) fn succeeded(&self) -> bool {
        self.status.success() && !self.stalled && !self.cancelled && !self.timed_out
    }

    /// The last lines of stderr, for an error message.
    pub(in crate::engine) fn stderr_tail(&self) -> String {
        let lines: Vec<&str> = self.stderr.lines().rev().take(12).collect();
        lines.into_iter().rev().collect::<Vec<_>>().join("\n").trim().to_string()
    }
}

/// [`Command::output`] that cannot wait forever: `cmd` is given `-progress pipe:2`
/// ([`PROGRESS_ON_STDERR`]) and is killed once its stderr has been silent for `stall`. Both
/// pipes are drained on side threads, so neither a flood of warnings nor a large stdout can fill
/// a pipe and wedge the child. A stalled read returns the stderr that had arrived and no stdout.
/// The child is always reaped, whatever fails on the way.
#[cfg(test)]
pub(super) fn output_unless_stalled(cmd: Command, stall: std::time::Duration) -> std::io::Result<Collected> {
    output_supervised(cmd, stall, None, &|| false)
}

/// [`output_unless_stalled`] for a job someone can stop: `cancel` is polled about fifty times a
/// second and, once true, kills the child and returns what had arrived with `cancelled` set.
/// The caller maps that to [`Error::Cancelled`] (it is not a failure to report). `total`, when
/// given, is a wall-clock bound on the whole run (`timed_out`) for the reads that print no
/// `-progress` (pass `Duration::MAX` as `stall` for those: stderr silence then means nothing).
pub(in crate::engine) fn output_supervised(
    cmd: Command,
    stall: std::time::Duration,
    total: Option<std::time::Duration>,
    cancel: &dyn Fn() -> bool,
) -> std::io::Result<Collected> {
    output_supervised_sized(cmd, stall, total, cancel, 0)
}

/// [`output_supervised`] that reads stdout into a buffer of `capacity` bytes allocated up front,
/// for a read whose size is known (the scopes' raw frames), instead of doubling through it.
pub(in crate::engine) fn output_supervised_sized(
    mut cmd: Command,
    stall: std::time::Duration,
    total: Option<std::time::Duration>,
    cancel: &dyn Fn() -> bool,
    capacity: usize,
) -> std::io::Result<Collected> {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::{Arc, Mutex};

    /// When stderr was last heard from, and what it said so far.
    struct Heard {
        at: Instant,
        text: String,
    }

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let heard = Arc::new(Mutex::new(Heard {
        at: Instant::now(),
        text: String::new(),
    }));
    let out_handle = std::thread::spawn(move || {
        let mut bytes = Vec::with_capacity(capacity);
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err_handle = {
        let heard = Arc::clone(&heard);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).split(b'\n') {
                let Ok(line) = line else { break };
                let line = String::from_utf8_lossy(&line);
                if let Ok(mut heard) = heard.lock() {
                    heard.at = Instant::now();
                    if !is_progress_line(&line) {
                        heard.text.push_str(&line);
                        heard.text.push('\n');
                    }
                }
            }
        })
    };
    let started = Instant::now();
    let (mut stalled, mut cancelled, mut timed_out) = (false, false, false);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
        if cancel() {
            cancelled = true;
            let _ = child.kill();
            break child.wait()?;
        }
        if total.is_some_and(|total| started.elapsed() > total) {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?;
        }
        if heard.lock().is_ok_and(|heard| heard.at.elapsed() > stall) {
            stalled = true;
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let text = |heard: &Mutex<Heard>| heard.lock().map(|heard| heard.text.clone()).unwrap_or_default();
    if stalled || cancelled || timed_out {
        // The side threads end on their own when the pipes close; a wrapper that spawned the real
        // ffmpeg keeps them open past the kill, and the caller must not wait for that.
        return Ok(Collected {
            status,
            stdout: Vec::new(),
            stderr: text(&heard),
            stalled,
            cancelled,
            timed_out,
        });
    }
    let stdout = out_handle.join().unwrap_or_default();
    let _ = err_handle.join();
    Ok(Collected {
        status,
        stdout,
        stderr: text(&heard),
        stalled,
        cancelled,
        timed_out,
    })
}

/// How long a child that has closed its pipes (or been killed) is given to exit before it is
/// killed: ffmpeg flushes a muxer on the way out, which on a dying network share can block.
const REAP_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

/// Wait for `child` to exit — for [`REAP_GRACE`] at most, then kill it. The bounded `wait()`:
/// every place that reads a child's pipes to the end and then waits on it would otherwise hang
/// for good on one that closed stdout and never exited.
pub(in crate::engine) fn reap(child: &mut std::process::Child) -> Result<std::process::ExitStatus> {
    reap_within(child, REAP_GRACE)
}

pub(super) fn reap_within(child: &mut std::process::Child, grace: std::time::Duration) -> Result<std::process::ExitStatus> {
    let wait_failed = |e: std::io::Error| Error::Engine(format!("ffmpeg wait failed: {e}"));
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                return child.wait().map_err(wait_failed);
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(2)),
            Err(e) => return Err(wait_failed(e)),
        }
    }
}

/// How a streamed ffmpeg run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::engine) enum StreamOutcome {
    /// ffmpeg exited on its own (successfully or not — see the status).
    Ended,
    /// The cancel callback tripped and ffmpeg was killed.
    Cancelled,
    /// ffmpeg said nothing for [`EXPORT_STALL`] and was killed.
    Stalled,
}

/// The result of [`run_ffmpeg_streamed`].
pub(in crate::engine) struct Streamed {
    pub(in crate::engine) status: std::process::ExitStatus,
    pub(in crate::engine) stderr: String,
    pub(in crate::engine) outcome: StreamOutcome,
}

/// Run `ffmpeg` with `args`, reading its `-progress` stream: `progress` gets the
/// encoder's position as a fraction of `duration` (when it is known), `cancel` is
/// polled between reports and while ffmpeg is silent, and a run that says nothing
/// for `stall` ([`EXPORT_STALL`] in production) is killed. A bounded cousin of `Command::output()` for the
/// whole-file jobs that outlive a glance — stderr is drained on a side thread so a
/// warning flood cannot fill its pipe and wedge the child.
pub(super) fn run_ffmpeg_streamed(
    bin: &str,
    args: &[String],
    duration: Option<f64>,
    stall: std::time::Duration,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> bool,
) -> Result<Streamed> {
    run_ffmpeg_streamed_in(bin, args, None, duration, stall, progress, cancel)
}

/// [`run_ffmpeg_streamed`] with ffmpeg's working directory set to `cwd` (the whisper filter
/// is handed bare file names so that no path has to survive filter-argument escaping).
pub(in crate::engine) fn run_ffmpeg_streamed_in(
    bin: &str,
    args: &[String],
    cwd: Option<&Path>,
    duration: Option<f64>,
    stall: std::time::Duration,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> bool,
) -> Result<Streamed> {
    use std::io::{BufRead, BufReader, Read};

    let mut command = bg_command(bin);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .args(["-progress", "pipe:1", "-stats_period", "0.5"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(bin, e))?;
    let stderr = child.stderr.take().expect("stderr piped");
    let stderr_handle = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut text);
        text
    });
    let stdout = child.stdout.take().expect("stdout piped");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let total = duration.filter(|d| d.is_finite() && *d > 0.0);
    let mut outcome = StreamOutcome::Ended;
    let mut last_output = Instant::now();
    loop {
        match rx.recv_timeout(CANCEL_POLL) {
            Ok(line) => {
                last_output = Instant::now();
                if line == "progress=end" {
                    break;
                }
                if let (Some(total), Some(us)) = (
                    total,
                    line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<i64>().ok()),
                ) {
                    // Held under 1: the file is not in place until it is renamed.
                    progress((us.max(0) as f64 / 1_000_000.0 / total).clamp(0.0, 0.99));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if last_output.elapsed() > stall {
                    outcome = StreamOutcome::Stalled;
                    break;
                }
            }
        }
        if cancel() {
            outcome = StreamOutcome::Cancelled;
            break;
        }
    }
    if outcome != StreamOutcome::Ended {
        let _ = child.kill();
    }
    let status = reap(&mut child)?;
    let stderr = stderr_handle.join().unwrap_or_default();
    Ok(Streamed { status, stderr, outcome })
}
