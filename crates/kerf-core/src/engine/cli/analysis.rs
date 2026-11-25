//! Silence, scene changes and salience read out of a file.

use super::*;

// ---- silence / scene analysis ---------------------------------------------

/// Detect silent spans using the `silencedetect` filter.
///
/// `noise_db` is the threshold in dBFS (e.g. `-30.0`); `min_silence` is the
/// shortest span to report, in seconds.
///
/// Gives up with [`Error::Cancelled`] once `cancel` is true (while it waits for the heavy-job
/// slot or while ffmpeg runs), and fails — rather than answering "no silence" — when the decode
/// does: a run that exits non-zero, or says nothing for [`READ_STALL`], is an error with
/// ffmpeg's last words. A file with no audio stream is the one non-zero exit that is an answer
/// (no silence), as the `0:a:0?` map always meant.
pub fn detect_silence_cancellable(
    path: &Path,
    noise_db: f64,
    min_silence: f64,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<TimeRange>> {
    let bin = ffmpeg_bin();
    let filter = format!("silencedetect=noise={noise_db}dB:d={min_silence}");
    // A whole-file decode: takes the heavy-job slot and the budget's threads.
    let cpu = cpu::lease_waiting(&mut |_| {}, cancel)?;
    // A free slot is taken without a look at the cancel: do not start a pass that was stopped.
    if cancel() {
        return Err(Error::Cancelled);
    }
    let mut cmd = bg_command(&bin);
    cpu::limit_cmd(&mut cmd, cpu.budget());
    cmd.args(PROGRESS_ON_STDERR)
        .args(["-hide_banner", "-nostats"])
        .arg("-i")
        .arg(path)
        .args(["-map", "0:a:0?", "-af", &filter, "-f", "null", "-"]);
    let output = output_supervised(cmd, READ_STALL, None, cancel).map_err(|e| launch_err(&bin, e))?;
    silence_from_run(&output, path)
}

/// What a finished `silencedetect` run says (pure over [`Collected`], unit-tested): the spans
/// it printed, or why there are none.
pub(super) fn silence_from_run(output: &Collected, path: &Path) -> Result<Vec<TimeRange>> {
    if output.cancelled {
        return Err(Error::Cancelled);
    }
    if output.stalled {
        return Err(Error::Engine(format!(
            "silence detection stalled: ffmpeg said nothing for {}s reading {}: {}",
            READ_STALL.as_secs(),
            path.display(),
            output.stderr_tail()
        )));
    }
    if !output.status.success() {
        if output.stderr.contains("does not contain any stream") {
            return Ok(Vec::new());
        }
        return Err(Error::Engine(format!(
            "silence detection failed ({}) reading {}: {}",
            output.status,
            path.display(),
            output.stderr_tail()
        )));
    }
    Ok(parse_silence(&output.stderr))
}

pub(super) fn parse_silence(stderr: &str) -> Vec<TimeRange> {
    let mut ranges = Vec::new();
    let mut pending_start: Option<f64> = None;
    for line in stderr.lines() {
        if let Some(v) = field_after(line, "silence_start:") {
            pending_start = Some(v);
        } else if let Some(end) = field_after(line, "silence_end:") {
            if let Some(start) = pending_start.take() {
                if end > start {
                    ranges.push(TimeRange { start, end });
                }
            }
        }
    }
    ranges
}

/// Width the scene detector runs at. The scene score is a mean absolute frame
/// difference normalized by pixel count, so it is stable under downscale — and
/// computing it on a 640px frame instead of 4K makes the per-frame diff ~20x
/// cheaper while the decode (hardware-accelerated when available) dominates.
pub(super) const SCENE_DETECT_WIDTH: u32 = 640;

/// Detect scene-change timestamps using `select='gt(scene,threshold)'`.
///
/// Decodes with `-hwaccel` when configured (the whole file is decoded, which is
/// the expensive part for 4K sources); a failed accelerated run retries in
/// software, mirroring [`decode_frame`]'s fallback.
#[cfg(test)]
pub fn detect_scenes(path: &Path, threshold: f64) -> Result<Vec<f64>> {
    detect_scenes_cancellable(path, threshold, &|| false)
}

/// [`detect_scenes`] that gives up with [`Error::Cancelled`] once `cancel` is true, and that
/// fails — rather than answering "no cuts" — when the decode does (a file with no video stream
/// is the one non-zero exit that is an answer: no cuts).
pub fn detect_scenes_cancellable(path: &Path, threshold: f64, cancel: &dyn Fn() -> bool) -> Result<Vec<f64>> {
    let pass = scene_pass(path, threshold, cancel)?;
    if !pass.ok && !pass.no_video {
        return Err(Error::Engine(format!(
            "scene detection failed reading {}: {}",
            path.display(),
            pass.why
        )));
    }
    Ok(pass.times)
}

/// [`detect_scenes`] for the file at `original`, reading its preview `proxy` instead when one
/// is given: a small all-intra decode in place of a whole 4K long-GOP one.
///
/// The proxy is only ever a faster way to the *same answer*, so a proxy that does not read
/// (a truncated or foreign file, a decoder that dies on it) is not a failed analysis: the
/// original is read instead, as if there had been none. The cut times themselves come out
/// the same because a proxy answers `-ss T` with the frame the original does (see
/// [`build_proxy_args`]; a late-starting video is padded so its frames keep their
/// timestamps), and the detector scores at [`SCENE_DETECT_WIDTH`] either way.
///
/// Gives up with [`Error::Cancelled`] once `cancel` is true (a stop on the proxy read does not
/// fall back to reading the original).
pub fn detect_scenes_preferring_cancellable(
    original: &Path,
    proxy: Option<&Path>,
    threshold: f64,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<f64>> {
    if let Some(proxy) = proxy {
        match scene_pass(proxy, threshold, cancel) {
            Ok(pass) if pass.ok => return Ok(pass.times),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            _ => {}
        }
        tracing::warn!(proxy = %proxy.display(), "scene detection could not read the proxy; reading the original");
    }
    detect_scenes_cancellable(original, threshold, cancel)
}

/// The `-vf` chain of a scene pass (pure). Frame 0 is selected too (`eq(n,0)`) and dropped again
/// by [`parse_scenes`]: a pass with no cut outputs nothing otherwise, and ffmpeg reports no
/// progress until a frame has reached the output, so a long single take would look wedged from
/// its first second.
pub(super) fn scene_filter(threshold: f64) -> String {
    format!("scale='min({SCENE_DETECT_WIDTH},iw)':-2:flags=bilinear,select='gt(scene,{threshold})+eq(n,0)',showinfo")
}

/// What one scene-detection pass found.
struct ScenePass {
    /// The cut times in the log.
    times: Vec<f64>,
    /// ffmpeg ended cleanly.
    ok: bool,
    /// The input has no video stream to score (`-map 0:v:0?` left nothing to write).
    no_video: bool,
    /// The last of ffmpeg's words, for the error of a run that did not end cleanly.
    why: String,
}

/// One scene-detection pass over `path`: the cut times and whether ffmpeg ended cleanly; the
/// flag is what lets a caller with somewhere else to read (the original, behind a proxy) tell.
/// A run that goes [`READ_STALL`] without a word is killed and is an error, not a short list of
/// cuts.
fn scene_pass(path: &Path, threshold: f64, cancel: &dyn Fn() -> bool) -> Result<ScenePass> {
    use std::sync::atomic::Ordering;

    let bin = ffmpeg_bin();
    let filter = scene_filter(threshold);
    // Probed before queueing: a hung probe must not hold the one heavy-job slot.
    let hw = decode_hwaccel_for(path, Reading::Whole);
    // The whole file is decoded, so this queues behind any other heavy job.
    let cpu = cpu::lease_waiting(&mut |_| {}, cancel)?;
    if cancel() {
        return Err(Error::Cancelled);
    }
    let run = |hw: Option<&str>| {
        let mut args: Vec<String> = vec!["-hide_banner".into(), "-nostats".into()];
        if let Some(hw) = hw {
            args.extend(["-hwaccel".into(), hw.into()]);
        }
        args.extend(["-i".into(), path.to_string_lossy().into_owned()]);
        args.extend(["-map", "0:v:0?", "-vf", &filter, "-f", "null", "-"].map(String::from));
        cpu::limit_args(&mut args, cpu.budget());
        let mut cmd = bg_command(&bin);
        cmd.args(PROGRESS_ON_STDERR).args(&args);
        let out = output_supervised(cmd, READ_STALL, None, cancel).map_err(|e| launch_err(&bin, e))?;
        if out.cancelled {
            return Err(Error::Cancelled);
        }
        Ok(out)
    };
    let output = match hw.as_deref() {
        Some(h) => {
            let out = run(Some(h))?;
            if out.succeeded() {
                out
            } else {
                let sw = run(None)?;
                if sw.succeeded() {
                    HWACCEL_OK.store(false, Ordering::Relaxed);
                    tracing::warn!("hardware decode failed during scene detection; using software decode");
                }
                sw
            }
        }
        None => run(None)?,
    };
    if output.stalled {
        return Err(Error::Engine(format!(
            "scene detection stalled: ffmpeg said nothing for {}s reading {}: {}",
            READ_STALL.as_secs(),
            path.display(),
            output.stderr_tail()
        )));
    }
    Ok(ScenePass {
        times: parse_scenes(&output.stderr),
        ok: output.status.success(),
        no_video: output.stderr.contains("does not contain any stream"),
        why: output.stderr_tail(),
    })
}

/// The cut times in a scene pass's `showinfo` log. Its first line (`n:0`, showinfo's own count of
/// the frames that reached it) is frame 0, which the pass lets through to keep ffmpeg reporting
/// and which never scores as a cut: not a cut.
pub(super) fn parse_scenes(stderr: &str) -> Vec<f64> {
    let mut times = Vec::new();
    for line in stderr.lines() {
        if field_after(line, "] n:") == Some(0.0) {
            continue;
        }
        if let Some(t) = field_after(line, "pts_time:") {
            times.push(t);
        }
    }
    times.sort_by(f64::total_cmp);
    times.dedup();
    times
}

/// Parse the number that immediately follows `key` on a log line, tolerating a
/// leading space (`"silence_start: 12.5"`).
fn field_after(line: &str, key: &str) -> Option<f64> {
    let rest = line.split(key).nth(1)?.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

// ---- salience sampling (smart crop) ----------------------------------------

/// Grid the salience sampler decodes at. Coarse on purpose: the answer is a
/// crop window, not a mask, and 64x36 gray pixels is 2.3 KB a frame — small
/// enough that hundreds of samples cost less than one preview still.
pub const SALIENCE_COLS: usize = 64;
pub const SALIENCE_ROWS: usize = 36;

/// Frames sampled across the window. Enough to average out a blink or a
/// handheld wobble without decoding the whole clip's worth of pictures.
const SALIENCE_SAMPLES: usize = 48;

/// Weight of *motion* against *detail* in a cell's score. Motion is the stronger
/// signal when there is any — a subject who moves is the subject — but a
/// locked-off talking head has none at all, which is why detail carries the
/// floor rather than being a tie-break.
const SALIENCE_MOTION_WEIGHT: f64 = 3.0;

/// Sample where the content of `path`'s `[start, end)` window sits, as a coarse
/// [`SalienceMap`].
///
/// One ffmpeg pass decodes a few dozen tiny grayscale frames; each cell scores
/// the picture's edge energy plus how much it changed since the last sample.
/// Hardware-accelerated like [`detect_scenes`], with the same software retry —
/// the decode is the expensive half on 4K footage, and the arithmetic here runs
/// on 2 KB frames.
///
/// Gives up with [`Error::Cancelled`] once `cancel` is true (queued for the slot or decoding),
/// and is killed after [`READ_STALL`] without a word from ffmpeg.
pub fn salience_map_cancellable(path: &Path, start: f64, end: f64, cancel: &dyn Fn() -> bool) -> Result<SalienceMap> {
    use std::sync::atomic::Ordering;

    let bin = ffmpeg_bin();
    let hw = decode_hwaccel_for(path, Reading::Whole);
    let cpu = cpu::lease_waiting(&mut |_| {}, cancel)?;
    if cancel() {
        return Err(Error::Cancelled);
    }
    let mut args = build_salience_args(path, start, end);
    cpu::limit_args(&mut args, cpu.budget());
    let run = |hw: Option<&str>| -> Result<Collected> {
        let mut cmd = bg_command(&bin);
        cmd.args(PROGRESS_ON_STDERR);
        if let Some(hw) = hw {
            cmd.args(["-hwaccel", hw]);
        }
        cmd.args(&args);
        let out = output_supervised(cmd, READ_STALL, None, cancel).map_err(|e| launch_err(&bin, e))?;
        if out.cancelled {
            return Err(Error::Cancelled);
        }
        Ok(out)
    };
    let output = match hw.as_deref() {
        Some(h) => {
            let out = run(Some(h))?;
            if out.succeeded() && !out.stdout.is_empty() {
                out
            } else {
                let sw = run(None)?;
                if sw.succeeded() {
                    HWACCEL_OK.store(false, Ordering::Relaxed);
                    tracing::warn!("hardware decode failed while sampling salience; using software decode");
                }
                sw
            }
        }
        None => run(None)?,
    };
    if output.stalled {
        return Err(Error::Engine(format!(
            "could not sample the shot: ffmpeg said nothing for {}s: {}",
            READ_STALL.as_secs(),
            output.stderr_tail()
        )));
    }
    if !output.succeeded() {
        return Err(Error::Engine(format!("could not sample the shot: {}", output.stderr.trim())));
    }
    Ok(score_salience(&output.stdout))
}

/// Pure arg builder for [`salience_map_cancellable`] (no I/O, unit-tested). Fast-seeks to
/// the window, decodes at most [`SALIENCE_SAMPLES`] frames spread across it, and
/// writes them as raw gray at the analysis grid. `-an` because nothing here
/// looks at sound, and `fps` before `scale` so the scaler runs on the frames
/// that survive rather than on every one.
pub(super) fn build_salience_args(path: &Path, start: f64, end: f64) -> Vec<String> {
    let start = start.max(0.0);
    let window = (end - start).max(0.04);
    // Spread the samples across the window, but never ask for more frames per
    // second than a sane source has — a 0.2s clip wants every frame it has, not
    // 240 duplicated ones.
    let fps = (SALIENCE_SAMPLES as f64 / window).clamp(0.2, 30.0);
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostats"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.push("-ss".into());
    args.push(format!("{start:.3}"));
    args.push("-t".into());
    args.push(format!("{window:.3}"));
    args.push("-i".into());
    args.push(path.to_string_lossy().into_owned());
    args.push("-an".into());
    args.push("-map".into());
    args.push("0:v:0?".into());
    args.push("-vf".into());
    args.push(format!(
        "fps={fps:.4},scale={SALIENCE_COLS}:{SALIENCE_ROWS}:flags=bilinear,format=gray"
    ));
    args.push("-frames:v".into());
    args.push(SALIENCE_SAMPLES.to_string());
    args.push("-f".into());
    args.push("rawvideo".into());
    args.push("-pix_fmt".into());
    args.push("gray".into());
    args.push("pipe:1".into());
    args
}

/// Score a run of raw gray [`SALIENCE_COLS`]x[`SALIENCE_ROWS`] frames into a
/// [`SalienceMap`]: per cell, the local edge energy of every frame plus the
/// frame-to-frame change, averaged over the frames actually decoded. Pure over
/// the decoded bytes, so the scoring is unit-testable without ffmpeg.
pub(super) fn score_salience(raw: &[u8]) -> SalienceMap {
    let (w, h) = (SALIENCE_COLS, SALIENCE_ROWS);
    let stride = w * h;
    let frames = raw.len() / stride;
    if frames == 0 {
        return SalienceMap::default();
    }
    let mut cells = vec![0.0f64; stride];
    let mut prev: Option<&[u8]> = None;
    for f in 0..frames {
        let frame = &raw[f * stride..(f + 1) * stride];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let p = frame[i] as f64;
                // Edge energy: how much this pixel differs from its right and
                // lower neighbours. Texture and outlines score, flat sky doesn't.
                let dx = if x + 1 < w { (frame[i + 1] as f64 - p).abs() } else { 0.0 };
                let dy = if y + 1 < h { (frame[i + w] as f64 - p).abs() } else { 0.0 };
                let motion = prev.map_or(0.0, |q| (q[i] as f64 - p).abs());
                cells[i] += dx + dy + SALIENCE_MOTION_WEIGHT * motion;
            }
        }
        prev = Some(frame);
    }
    let scale = 1.0 / (frames as f64 * 255.0);
    SalienceMap::new(w, h, cells.into_iter().map(|c| (c * scale) as f32).collect())
}
