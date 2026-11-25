//! Composited stills: the one-shot runs that draw them and where they are written.

use super::*;

/// Composite a single still of the `timeline` at timeline time `t` and return
/// it as JPEG bytes (`quality` = `-q:v`), the canvas downscaled so it is at most
/// `max_width` px wide. Lets an LLM *see the cut it is assembling* (which footage
/// is on screen, framing, picture-in-picture placement, crop, color) rather than
/// reasoning about timestamps blind.
///
/// Gives up with [`Error::Cancelled`] once `cancel` is true.
pub fn timeline_frame_cancellable(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    quality: u8,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    run_still(
        timeline,
        assets,
        opts,
        t,
        max_width,
        None,
        &StillOutput::JpegPipe { quality },
        cancel,
    )
}

/// The composited still at `t` as its Y'CbCr samples: limited-range 4:4:4 planes (Y, then Cb,
/// then Cr) of the frame [`timeline_frame_cancellable`] would encode, with its size. The scopes read this
/// rather than an RGB conversion, whose matrix is the composite's and not the delivery's.
/// Niced like every read the UI draws from, killed past [`STILL_TIMEOUT`], and gives up with
/// [`Error::Cancelled`] once `cancel` is true.
pub fn timeline_frame_yuv_cancellable(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    cancel: &dyn Fn() -> bool,
) -> Result<(u32, u32, Vec<u8>)> {
    let fmt = export_format(&timeline.for_render(), assets, opts);
    let (w, h) = still_size(fmt.width, fmt.height, max_width);
    let planes = run_still(
        timeline,
        assets,
        opts,
        t,
        max_width,
        None,
        &StillOutput::Yuv444Pipe { width: w, height: h },
        cancel,
    )?;
    if planes.len() != (w as usize) * (h as usize) * 3 {
        return Err(Error::Engine(format!(
            "could not render timeline frame at {t:.3}s: {} bytes for a {w}x{h} frame",
            planes.len()
        )));
    }
    Ok((w, h, planes))
}

/// [`timeline_frame`] of one `region` of the composited canvas. The canvas is
/// rendered large enough that the region alone comes out `max_width` wide
/// (capped at the delivery frame, so nothing is invented), then cropped — a
/// zoom into the cut rather than a screenshot of it.
/// Gives up with [`Error::Cancelled`] once `cancel` is true.
#[allow(clippy::too_many_arguments)]
pub fn timeline_frame_region_cancellable(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    region: Region,
    max_width: u32,
    quality: u8,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    let region = (!region.is_full()).then_some(region);
    run_still(
        timeline,
        assets,
        opts,
        t,
        max_width,
        region,
        &StillOutput::JpegPipe { quality },
        cancel,
    )
}

/// Write the composited still at timeline time `t` to `path` as a **cover
/// frame**: full delivery resolution (no preview downscale) in `format`.
///
/// A cover is the picture a platform shows before anyone presses play, and this
/// renders it through the very graph the export uses — so the cover is literally
/// a frame of the video it fronts, at the project's delivery shape, rather than
/// a screenshot that has to be cropped back into agreement.
pub fn export_still(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    path: &Path,
    format: ImageFormat,
    quality: u8,
) -> Result<PathBuf> {
    export_still_cancellable(timeline, assets, opts, t, path, format, quality, &|| false)
}

/// [`export_still`] that gives up with [`Error::Cancelled`] once `cancel` is true (polled about
/// fifty times a second while ffmpeg runs; the render is killed). A cover that fails or is
/// cancelled removes what it wrote ([`discard_partial`]), leaving an earlier file at `path`
/// alone when this run never touched it.
#[allow(clippy::too_many_arguments)]
pub fn export_still_cancellable(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    path: &Path,
    format: ImageFormat,
    quality: u8,
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let before = file_mtime(path);
    let out = StillOutput::File {
        path: path.to_string_lossy().into_owned(),
        format,
        quality,
    };
    // `u32::MAX` asks for no cap: `build_still_args` clamps to the delivery
    // width, which is exactly the cover size.
    if let Err(e) = run_still(timeline, assets, opts, t, u32::MAX, None, &out, cancel) {
        discard_partial(path, before);
        return Err(e);
    }
    Ok(path.to_path_buf())
}

/// Write the picture file `input` again as `output`, encoded with `codec` (`-c:v mjpeg -q:v 2`, …): one
/// frame, killed past [`STILL_TIMEOUT`] or once `cancel` is true ([`Error::Cancelled`]). A run that fails
/// removes what it wrote ([`discard_partial`]).
pub fn transcode_image(input: &Path, output: &Path, codec: &[&str], cancel: &dyn Fn() -> bool) -> Result<()> {
    let before = file_mtime(output);
    let bin = ffmpeg_bin();
    let mut cmd = command(&bin);
    cmd.args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args(["-frames:v", "1"])
        .args(codec)
        .args(["-f", "image2", "-update", "1", "-y"])
        .arg(output);
    let result = match output_supervised(cmd, std::time::Duration::MAX, Some(STILL_TIMEOUT), cancel) {
        Err(e) => Err(launch_err(&bin, e)),
        Ok(out) if out.cancelled => Err(Error::Cancelled),
        Ok(out) if out.timed_out => Err(Error::Engine(format!(
            "could not write {}: ffmpeg did not answer in {} s and was stopped",
            output.display(),
            STILL_TIMEOUT.as_secs()
        ))),
        Ok(out) if !out.status.success() => Err(Error::Engine(format!(
            "could not write {}: {}",
            output.display(),
            out.stderr.trim()
        ))),
        Ok(_) => Ok(()),
    };
    if result.is_err() {
        discard_partial(output, before);
    }
    result
}

/// Run a composited still through ffmpeg, retrying in software if hardware
/// decode was asked for and failed. Returns stdout (empty for a file sink).
#[allow(clippy::too_many_arguments)]
pub(super) fn run_still(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    region: Option<Region>,
    out: &StillOutput,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    // The scopes are a measurement, not the frame the user is waiting to see: niced.
    let spawn = StillSpawn {
        share: 1,
        background: matches!(out, StillOutput::Yuv444Pipe { .. }),
    };
    run_still_as(timeline, assets, opts, t, max_width, region, out, spawn, cancel)
}

/// How a still's ffmpeg is spawned: the share of the budget it takes among stills drawn side by
/// side ([`tune_still_args_shared`]), and whether it runs niced.
#[derive(Debug, Clone, Copy)]
pub(super) struct StillSpawn {
    pub(super) share: usize,
    pub(super) background: bool,
}

/// [`run_still`] spawned as `spawn` says.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_still_as(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    region: Option<Region>,
    out: &StillOutput,
    spawn: StillSpawn,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    let piping = matches!(out, StillOutput::JpegPipe { .. });
    // The scopes' output size is known, so the read buffer is allocated once instead of doubling
    // through ~3 MB.
    let capacity = match out {
        StillOutput::Yuv444Pipe { width, height } => (*width as usize) * (*height as usize) * 3,
        _ => 0,
    };
    let background = spawn.background;
    let hw_used = std::cell::Cell::new(false);
    // The titles' pictures, on disk and kept for the run (and its software retry).
    let _titles = still_titles(timeline, assets, opts, t, max_width, region)?;
    let run = |o: &ExportOptions| -> Result<Vec<u8>> {
        let mut args = build_still_args(timeline, assets, o, t, max_width, region, out)?;
        hw_used.set(tune_still_args_shared(&mut args, assets, HostOs::CURRENT, spawn.share));
        let _script = externalize_filter_complex(&mut args, "still")?;
        let bin = ffmpeg_bin();
        let mut cmd = if background { bg_command(&bin) } else { command(&bin) };
        cmd.args(&args);
        // No `-progress` on a one-frame render, so no stall bound: a wall-clock one and the cancel.
        let output = output_supervised_sized(cmd, std::time::Duration::MAX, Some(STILL_TIMEOUT), cancel, capacity)
            .map_err(|e| launch_err(&bin, e))?;
        if output.cancelled {
            return Err(Error::Cancelled);
        }
        if output.timed_out {
            return Err(Error::Engine(format!(
                "could not render timeline frame at {t:.3}s: ffmpeg did not answer in {} s and was stopped",
                STILL_TIMEOUT.as_secs()
            )));
        }
        if !output.status.success() || (piping && output.stdout.is_empty()) {
            return Err(Error::Engine(format!(
                "could not render timeline frame at {t:.3}s: {}",
                output.stderr.trim()
            )));
        }
        Ok(output.stdout)
    };
    match run(opts) {
        Err(Error::Cancelled) => Err(Error::Cancelled),
        // Mirror `decode_frame`'s fallback: a software retry that succeeds means
        // `-hwaccel` is the culprit here, so stop asking for it. Only a run that
        // still asked for hardware after the spawn-time tuning can blame it.
        Err(hw_err) if hw_used.get() => match run(&ExportOptions {
            hwaccel: None,
            ..opts.clone()
        }) {
            Ok(bytes) => {
                HWACCEL_OK.store(false, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!("hardware decode failed for the timeline still ({hw_err}); using software decode");
                Ok(bytes)
            }
            Err(_) => Err(hw_err),
        },
        result => result,
    }
}

/// How long one composited still may run before it is killed. A still of a 4K 4:2:2 original is
/// a second or two; this is for the one that hangs (a hardware decoder stuck in a driver), which
/// would otherwise hold the caller, and the Preview's one frame in flight, for good.
const STILL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// `Command::output()` with a deadline: run `cmd` with its stdin closed and both output pipes
/// read, and kill it if it has not finished in `limit` (`ErrorKind::TimedOut`). The calling thread
/// reads stdout to its end (into a buffer of `capacity` bytes up front), a watchdog kills the process at the deadline (which ends that read),
/// and nothing waits on the child while the watchdog could need it.
pub(in crate::engine) fn output_bounded(
    cmd: &mut Command,
    limit: std::time::Duration,
    capacity: usize,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{channel, RecvTimeoutError};
    use std::sync::Arc;

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));
    let timed_out = Arc::new(AtomicBool::new(false));
    let (done, finished) = channel::<()>();
    let watchdog = {
        let child = Arc::clone(&child);
        let timed_out = Arc::clone(&timed_out);
        std::thread::spawn(move || {
            if matches!(finished.recv_timeout(limit), Err(RecvTimeoutError::Timeout)) {
                timed_out.store(true, Ordering::SeqCst);
                let _ = child.lock().unwrap_or_else(|e| e.into_inner()).kill();
            }
        })
    };
    let err_reader = std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_end(&mut text);
        }
        text
    });
    let mut out = Vec::with_capacity(capacity);
    let read = match stdout.as_mut() {
        Some(o) => o.read_to_end(&mut out).map(|_| ()),
        None => Ok(()),
    };
    // The output has ended, so the process is exiting; poll rather than block in `wait`, which
    // would hold the child's lock against the watchdog's kill.
    let started = Instant::now();
    let status = loop {
        let mut c = child.lock().unwrap_or_else(|e| e.into_inner());
        match c.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() > limit => {
                timed_out.store(true, Ordering::SeqCst);
                let _ = c.kill();
                break c.wait();
            }
            Ok(None) => {}
            Err(e) => break Err(e),
        }
        drop(c);
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    let _ = done.send(());
    let _ = watchdog.join();
    let stderr = err_reader.join().unwrap_or_default();
    if timed_out.load(Ordering::SeqCst) {
        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the process timed out"));
    }
    read?;
    Ok(std::process::Output {
        status: status?,
        stdout: out,
        stderr,
    })
}

/// [`output_bounded`] that a caller's `cancel` can also stop: `Ok(None)` is a run killed because
/// `cancel` went true (the caller says [`Error::Cancelled`]); a run past `limit` is the same
/// `TimedOut` error. With no `cancel` it *is* [`output_bounded`] — the interactive reads (a
/// scrubbed frame) keep the path that has no polling interval in it.
pub(in crate::engine) fn output_bounded_cancellable(
    mut cmd: Command,
    limit: std::time::Duration,
    cancel: Option<&dyn Fn() -> bool>,
) -> std::io::Result<Option<std::process::Output>> {
    let Some(cancel) = cancel else {
        return output_bounded(&mut cmd, limit, 0).map(Some);
    };
    // These reads print no `-progress`, so stderr silence means nothing: only the wall clock bounds them.
    let run = output_supervised(cmd, std::time::Duration::MAX, Some(limit), cancel)?;
    if run.cancelled {
        return Ok(None);
    }
    if run.timed_out {
        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the process timed out"));
    }
    Ok(Some(std::process::Output {
        status: run.status,
        stdout: run.stdout,
        stderr: run.stderr.into_bytes(),
    }))
}

/// Tune a built still argv for the process about to be spawned: each input's `-hwaccel` and
/// threads per `decode_policy` (a proxy is decoded on one thread without a hardware session; a
/// still whose every input is a proxy runs its graph and encoder on one thread too), the budget's
/// threads otherwise. Returns whether any input still asks for hardware decode. At spawn time,
/// not in [`build_still_args`], so the builder keeps describing exactly what was asked for.
#[cfg(test)]
pub(super) fn tune_still_args(args: &mut Vec<String>, assets: &[Asset], os: HostOs) -> bool {
    tune_still_args_shared(args, assets, os, 1)
}

/// [`tune_still_args`] for one of `share` stills drawn side by side (a sheet of the cut): what is
/// not cheap takes its share of the budget's threads ([`cpu::limit_args_shared`]; a lone still is
/// [`cpu::limit_args`] at the budget exactly).
fn tune_still_args_shared(args: &mut Vec<String>, assets: &[Asset], os: HostOs, share: usize) -> bool {
    let tuning = tune_inputs(args, os, Reading::Frame, &facts_from_assets(assets));
    if tuning.all_cheap {
        cpu::single_frame_args(args);
    } else {
        cpu::limit_args_shared(args, share);
    }
    tuning.hw
}

/// The JPEG-pipe still args — [`timeline_frame`]'s shape, and what every still
/// test asserts against.
#[cfg(test)]
pub(super) fn build_timeline_frame_args(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    quality: u8,
) -> Result<Vec<String>> {
    build_still_args(timeline, assets, opts, t, max_width, None, &StillOutput::JpegPipe { quality })
}

/// Where a composited still is written, and in what image format.
///
/// The still compositor serves two callers with the same graph: the preview /
/// agent path wants JPEG bytes on stdout, and a **cover frame** wants a real
/// file at full delivery resolution — so only the sink differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StillOutput {
    /// MJPEG on stdout (`quality` = `-q:v`) — the preview and agent path.
    JpegPipe { quality: u8 },
    /// An image file. JPEG honors `quality`; PNG is lossless and ignores it.
    File { path: String, format: ImageFormat, quality: u8 },
    /// Raw `rgb24` on stdout — only for [`composite_color_policy`]'s probe, which
    /// wants the converted pixels themselves.
    RgbPipe,
    /// Raw planar `yuv444p` on stdout — [`timeline_frame_yuv_cancellable`], for the scopes. The frame's size
    /// sizes the read buffer; the argv does not carry it (the graph scales to `max_width`).
    Yuv444Pipe { width: u32, height: u32 },
}

/// The image formats a cover frame can be written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Jpeg,
    Png,
}

impl ImageFormat {
    /// The customary file extension (no dot).
    pub fn ext(self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Png => "png",
        }
    }

    /// The format a path's extension asks for, defaulting to JPEG — the format
    /// every platform accepts as a cover.
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("png") => ImageFormat::Png,
            _ => ImageFormat::Jpeg,
        }
    }

    fn encoder(self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "mjpeg",
            ImageFormat::Png => "png",
        }
    }
}

impl StillOutput {
    /// The trailing output arguments: one frame, encoded and sent to the sink.
    pub(super) fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec!["-frames:v".to_string(), "1".to_string()];
        match self {
            StillOutput::JpegPipe { quality } => {
                a.extend([
                    "-q:v".to_string(),
                    quality.to_string(),
                    "-f".to_string(),
                    "image2pipe".to_string(),
                    "-vcodec".to_string(),
                    "mjpeg".to_string(),
                    "pipe:1".to_string(),
                ]);
            }
            StillOutput::RgbPipe => {
                a.extend(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"].map(String::from));
            }
            StillOutput::Yuv444Pipe { .. } => {
                a.extend(["-f", "rawvideo", "-pix_fmt", "yuv444p", "pipe:1"].map(String::from));
            }
            StillOutput::File { path, format, quality } => {
                if *format == ImageFormat::Jpeg {
                    a.extend(["-q:v".to_string(), quality.to_string()]);
                }
                a.extend([
                    "-f".to_string(),
                    "image2".to_string(),
                    "-vcodec".to_string(),
                    format.encoder().to_string(),
                    "-y".to_string(),
                    path.clone(),
                ]);
            }
        }
        a
    }
}

/// How a seek that has to land on one particular frame is spelled: `-ss` to the
/// microsecond.
///
/// `-ss` is an exact tick on a fine time base, so milliseconds are not enough: with a
/// frame at 1.0006 s, `-ss 1.0006` returns it and the spelling `1.001` skips to the
/// next. The composited still and `kerf-gpu`'s decode of the same layer both call
/// this, so the two cannot drift apart and show different frames.
pub fn seek_arg(seconds: f64) -> String {
    format!("{seconds:.6}")
}

/// How the export spells a clip's input `-ss`: the shortest text of the `f64`, which FFmpeg
/// reads and truncates to whole microseconds. [`seek_arg`] rounds to the nearest instead, so
/// for a window start like 2/30 s on a one-microsecond time base the two land on frames a tick
/// apart; whatever has to see the frames the export sees (`FpsPick::seek_arg`, the frame pick's
/// model of the seek) takes this one.
pub fn export_seek_arg(seconds: f64) -> String {
    format!("{seconds}")
}
