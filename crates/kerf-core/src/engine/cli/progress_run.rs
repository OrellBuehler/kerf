//! Running `ffmpeg` with `-progress`: the bar, the stall watch and the cancel poll.

use super::*;

/// Where one ffmpeg invocation's reported `out_time` maps onto the overall
/// export bar: `[offset, offset+width]` of `[0,1]`, against an output of `total`
/// seconds, timed from `start`. (A single-pass export is the whole bar; a
/// two-pass export splits it into two halves.)
#[derive(Clone, Copy)]
pub(super) struct Bar {
    pub(super) total: f64,
    pub(super) offset: f64,
    pub(super) width: f64,
    pub(super) start: std::time::Instant,
}

/// How often a running export checks whether it has been cancelled when ffmpeg
/// has said nothing in the meantime.
pub(super) const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// How long an export may go without moving — not one frame more, not one later output position
/// in its `-progress` stream — before it is treated as hung ([`run_progress_stalling`]). A run that
/// moves is never stopped, so the bound is generous: killing a render that was merely slow is the
/// worse error.
/// (It reports nothing while it opens the footage and decodes up to a range's start, so this is
/// also the most a seek into a recording with no keyframes to land on may take.)
pub(super) const EXPORT_STALL: std::time::Duration = std::time::Duration::from_secs(300);

/// How often a running export that has written nothing yet repeats that it is starting.
const STARTING_BEAT: std::time::Duration = std::time::Duration::from_secs(1);

/// Spawn the `ffmpeg` binary with `args`, streaming `-progress` from stdout to
/// map elapsed render time onto `bar`, and polling `cancel` between updates
/// (killing ffmpeg when it trips). stderr is drained on a side thread so a
/// warning flood can't deadlock the stdout read, and its tail still surfaces in
/// a failure's error.
pub(super) fn run_ffmpeg_progress(
    args: &[String],
    output: &Path,
    bar: Bar,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    run_ffmpeg_progress_as(args, output, bar, cpu::Resource::Cpu, progress, cancel)
}

/// [`run_ffmpeg_progress`] for a run that spends `resource`: the cores' slot for any software
/// run, the hardware's for one that is hardware from end to end ([`export_resource`]).
pub(super) fn run_ffmpeg_progress_as(
    args: &[String],
    output: &Path,
    bar: Bar,
    resource: cpu::Resource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    run_ffmpeg_progress_into(args, &[], output, bar, resource, progress, cancel)
}

/// [`run_ffmpeg_progress_as`] for a run that writes several files: `sinks` is the argv index of
/// each output path, so each encoder is capped to its share of the budget
/// ([`cpu::limit_outputs_args`]). Empty is one output, the last argument ([`cpu::limit_args`]).
pub(super) fn run_ffmpeg_progress_into(
    args: &[String],
    sinks: &[usize],
    output: &Path,
    bar: Bar,
    resource: cpu::Resource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    run_progress_with(&ffmpeg_bin(), args, sinks, output, bar, resource, progress, cancel)
}

/// [`run_ffmpeg_progress_into`] that also says how many frames the run had written when it ended
/// (ffmpeg's `frame=` in the `-progress` stream), for a run whose count is part of what it promises.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_ffmpeg_progress_counted(
    args: &[String],
    sinks: &[usize],
    output: &Path,
    bar: Bar,
    resource: cpu::Resource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
    frames: &mut u64,
) -> Result<RenderStatus> {
    run_progress_counting(&ffmpeg_bin(), args, sinks, output, bar, resource, progress, cancel, frames)
}

/// [`run_ffmpeg_progress_into`] with the binary named (a test runs a script that talks like ffmpeg).
#[allow(clippy::too_many_arguments)]
pub(super) fn run_progress_with(
    bin: &str,
    args: &[String],
    sinks: &[usize],
    output: &Path,
    bar: Bar,
    resource: cpu::Resource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    run_progress_counting(bin, args, sinks, output, bar, resource, progress, cancel, &mut 0)
}

/// [`run_progress_with`] that also counts the frames written (`frames`).
#[allow(clippy::too_many_arguments)]
fn run_progress_counting(
    bin: &str,
    args: &[String],
    sinks: &[usize],
    output: &Path,
    bar: Bar,
    resource: cpu::Resource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
    frames: &mut u64,
) -> Result<RenderStatus> {
    run_progress_stalling(
        bin,
        args,
        sinks,
        output,
        bar,
        resource,
        progress,
        cancel,
        frames,
        EXPORT_STALL,
    )
}

/// [`run_progress_counting`] that gives up on a run that has not moved for `stall`
/// ([`EXPORT_STALL`] in production): **moved** is a frame more or a later output position in the
/// `-progress` stream, not a line. FFmpeg 7 and later print a block every `-stats_period` whatever
/// the graph is doing, so a run whose graph had stopped (a 45 MP still decoded and resampled per
/// frame under memory pressure) went on "reporting" the same frame for hours; a run that is slow
/// but moving is never stopped, however slow.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_progress_stalling(
    bin: &str,
    args: &[String],
    sinks: &[usize],
    output: &Path,
    bar: Bar,
    resource: cpu::Resource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
    frames: &mut u64,
    stall: std::time::Duration,
) -> Result<RenderStatus> {
    use std::io::{BufRead, BufReader, Read};
    use std::process::Stdio;

    // The heaviest thing the engine does. The lease is reentrant, so an export's
    // second pass and a stitch inside an import do not queue behind themselves. It is the
    // foreground lane — ahead of any proxy still queued — and a render that has to wait says
    // so through `progress` and can be cancelled while it does.
    let mut on_wait = |wait: cpu::Wait| {
        progress(ExportProgress::queued(
            bar.offset,
            bar.start.elapsed().as_secs_f64(),
            wait.message(),
        ));
    };
    let leased = match resource {
        cpu::Resource::Cpu => cpu::lease_waiting(&mut on_wait, cancel),
        cpu::Resource::Gpu => cpu::lease_gpu_waiting(&mut on_wait, cancel),
    };
    let cpu = match leased {
        Ok(lease) => lease,
        Err(Error::Cancelled) => return Ok(RenderStatus::Cancelled),
        Err(e) => return Err(e),
    };
    let mut args = args.to_vec();
    spawn_caps(&mut args, cpu.budget(), sinks);
    let args = &args[..];
    tracing::info!(output = %output.display(), resource = ?cpu.resource(), "exporting timeline");
    tracing::debug!(command = %format!("{bin} {}", args.join(" ")), "ffmpeg export command");

    // `-progress pipe:1` writes machine-readable key=value blocks to stdout;
    // `-stats_period` bounds how often, and thus the cancel-poll latency.
    let mut child = bg_command(bin)
        .arg("-progress")
        .arg("pipe:1")
        .arg("-stats_period")
        .arg("0.5")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(bin, e))?;

    let stderr = child.stderr.take().expect("stderr piped");
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut buf);
        buf
    });

    let mut cancelled = false;
    let mut stalled = false;
    let stdout = child.stdout.take().expect("stdout piped");
    // Progress lines are read on a side thread so the loop below keeps polling
    // `cancel` (and watching for a silent ffmpeg) even when none arrive: a
    // blocking `lines()` read meant a render that stopped reporting could neither
    // be cancelled nor ever be given up on.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    // ffmpeg opens the footage before it writes anything — and decodes up to a range's start,
    // which on a long-GOP source takes as long as it takes, with the output file empty. Say so
    // now: it also replaces whatever the stage before this one (a queue, a metering pass) last said.
    let say = |progress: &mut dyn FnMut(ExportProgress), fraction: f64, eta_secs: Option<f64>, stage: ExportStage| {
        progress(ExportProgress {
            fraction,
            elapsed_secs: bar.start.elapsed().as_secs_f64(),
            eta_secs,
            waiting: None,
            stage: Some(stage),
            part: None,
        });
    };
    say(progress, bar.offset, None, ExportStage::Starting);
    let mut reader = ProgressReader::default();
    // The wall clock and the place on the bar at the first output: the rate (and so the time
    // left) is judged from there, not from the spawn.
    let mut first_output: Option<(Instant, f64)> = None;
    // When the run last moved, and how far it had got: frames written, output seconds.
    let mut last_moved = Instant::now();
    let mut furthest = (0u64, f64::NEG_INFINITY);
    let mut last_beat = Instant::now();
    loop {
        if last_moved.elapsed() > stall {
            let _ = child.kill();
            stalled = true;
            break;
        }
        let line = match rx.recv_timeout(CANCEL_POLL) {
            Ok(line) => line,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if cancel() {
                    let _ = child.kill();
                    cancelled = true;
                    break;
                }
                // ffmpeg prints no `-progress` block while it opens the footage and decodes up
                // to a range's start (measured on 4.4: the first block comes only once the first
                // packet is written), so the silence of that stretch is the engine's to keep
                // alive: a beat a second, which moves the elapsed time.
                if first_output.is_none() && last_beat.elapsed() >= STARTING_BEAT {
                    last_beat = Instant::now();
                    say(progress, bar.offset, None, ExportStage::Starting);
                }
                continue;
            }
        };
        if let Some(n) = line.strip_prefix("frame=").and_then(|v| v.trim().parse::<u64>().ok()) {
            *frames = n;
            if n > furthest.0 {
                furthest.0 = n;
                last_moved = Instant::now();
            }
        }
        let block = reader.feed(&line);
        if let Some(Block::Working(Some(position)) | Block::End(Some(position))) = block {
            if position > furthest.1 {
                furthest.1 = position;
                last_moved = Instant::now();
            }
        }
        match block {
            // The output has been written to: its position against the length of the cut (the
            // output starts at 0 whatever the range's offset — the input seek rebases the
            // timestamps — so a range is measured against its own length). The last block is told
            // too: it carries the end of the output, which no periodic one was late enough to.
            Some(Block::Working(Some(position)) | Block::End(Some(position))) => {
                let (pass, stage) = pass_position(position, bar.total);
                let fraction = (bar.offset + bar.width * pass).clamp(0.0, 1.0);
                let (at, from) = *first_output.get_or_insert((Instant::now(), fraction));
                let eta = eta_secs(at.elapsed().as_secs_f64(), fraction - from, 1.0 - fraction);
                say(progress, fraction, eta, stage);
            }
            // Nothing written yet: ffmpeg is still opening and seeking. A beat, so the elapsed
            // time moves and the stage stays honest.
            Some(Block::Working(None)) if first_output.is_none() => say(progress, bar.offset, None, ExportStage::Starting),
            _ => {}
        }
        // A cancel that arrives with the last block still counts (the file is removed, as for any
        // other): the check comes before the end of the stream ends the loop.
        if cancel() {
            let _ = child.kill();
            cancelled = true;
            break;
        }
        if matches!(block, Some(Block::End(_))) {
            break;
        }
    }

    let status = reap(&mut child)?;
    let stderr_text = stderr_handle.join().unwrap_or_default();

    if cancelled {
        tracing::info!(output = %output.display(), "export cancelled");
        return Ok(RenderStatus::Cancelled);
    }
    if stalled {
        let mut tail: Vec<&str> = stderr_text.lines().rev().take(20).collect();
        tail.reverse();
        let at = if furthest.1.is_finite() {
            format!(" (stuck at frame {}, {:.2} s of output)", furthest.0, furthest.1)
        } else {
            " (nothing written yet)".to_string()
        };
        return Err(Error::Engine(format!(
            "ffmpeg stopped reporting progress for {}s{at} and was stopped: {}",
            stall.as_secs(),
            tail.join("\n").trim()
        )));
    }
    if !status.success() {
        let mut tail: Vec<&str> = stderr_text.lines().rev().take(20).collect();
        tail.reverse();
        let tail = tail.join("\n");
        tracing::error!(status = %status, "ffmpeg export failed:\n{tail}");
        return Err(Error::Engine(format!("ffmpeg exited with {}: {}", status, tail.trim())));
    }
    tracing::info!(output = %output.display(), "export complete");
    Ok(RenderStatus::Completed)
}
