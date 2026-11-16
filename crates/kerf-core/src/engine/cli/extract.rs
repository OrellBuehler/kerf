//! Frames, contact sheets, waveform peaks and decoded audio read out of a source.

use super::*;

/// A small process-global LRU of decoded single frames. Decoded frames are a
/// pure function of (source path, time, filter, codec, quality), so caching is
/// always safe for immutable source media — and it turns scrubbing back over a
/// region, pausing, or replaying into cache hits instead of a fresh `ffmpeg`
/// spawn each time. Bounded so it never grows without limit.
struct FrameCache {
    map: HashMap<String, (u64, Vec<u8>)>,
    tick: u64,
    cap: usize,
    /// Sum of cached frame bytes, kept under [`FRAME_CACHE_MAX_BYTES`]. The
    /// entry cap alone is no memory bound — 96 full-width PNGs can run to
    /// hundreds of megabytes.
    bytes: usize,
}

/// Byte budget for the frame cache (the entry cap still applies too).
const FRAME_CACHE_MAX_BYTES: usize = 64 << 20;

impl FrameCache {
    fn get(&mut self, key: &str) -> Option<Vec<u8>> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.map.get_mut(key)?;
        entry.0 = tick; // mark recently used
        Some(entry.1.clone())
    }

    fn put(&mut self, key: String, value: Vec<u8>) {
        self.tick += 1;
        if let Some((_, old)) = self.map.remove(&key) {
            self.bytes -= old.len();
        }
        // Evict least-recently-used entries until both the entry and byte caps
        // hold with the new frame counted in.
        while !self.map.is_empty() && (self.map.len() >= self.cap || self.bytes + value.len() > FRAME_CACHE_MAX_BYTES) {
            if let Some(oldest) = self.map.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone()) {
                if let Some((_, evicted)) = self.map.remove(&oldest) {
                    self.bytes -= evicted.len();
                }
            }
        }
        self.bytes += value.len();
        self.map.insert(key, (self.tick, value));
    }
}

fn frame_cache() -> &'static Mutex<FrameCache> {
    static CACHE: OnceLock<Mutex<FrameCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(FrameCache {
            map: HashMap::new(),
            tick: 0,
            cap: 96,
            bytes: 0,
        })
    })
}

// ---- frame / waveform extraction ------------------------------------------

/// Decode a single frame at `time_secs` and return it as PNG bytes, scaled to
/// at most `max_width` pixels wide.
pub fn frame_at(path: &Path, time_secs: f64, max_width: u32) -> Result<Vec<u8>> {
    let scale = format!("scale='min({max_width},iw)':-2");
    decode_frame(path, time_secs, &scale, "png", None, true, None)
}

/// A rectangle of a frame to look at more closely, as fractions of the full
/// frame: `left`/`top` place its corner, `width`/`height` size it.
///
/// This is the *zoom* half of "look, then look closer": a vision model spends
/// its image budget on whatever it is handed, so a quarter of the frame cropped
/// out and scaled to the same width shows four times the detail of the whole
/// frame for the same cost — a face, a caption, a mask edge. Nothing here is an
/// edit; it is only how a frame is presented.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl Region {
    /// The smallest side a region may have, as a fraction of the frame: below
    /// this a crop of a 640 px preview is a handful of pixels.
    pub const MIN_SIDE: f64 = 0.02;

    /// The whole frame, which is what "no region" means.
    pub const FULL: Region = Region {
        left: 0.0,
        top: 0.0,
        width: 1.0,
        height: 1.0,
    };

    /// Clamp into the frame: sides at least [`Region::MIN_SIDE`], the corner
    /// inside, and the far edge pulled back to 1.0 by shrinking the size, not
    /// by moving the corner — the corner is what the caller pointed at.
    pub fn normalized(self) -> Region {
        let n = |v: f64| if v.is_finite() { v } else { 0.0 };
        let left = n(self.left).clamp(0.0, 1.0 - Self::MIN_SIDE);
        let top = n(self.top).clamp(0.0, 1.0 - Self::MIN_SIDE);
        let width = n(self.width).clamp(Self::MIN_SIDE, 1.0 - left);
        let height = n(self.height).clamp(Self::MIN_SIDE, 1.0 - top);
        Region {
            left,
            top,
            width,
            height,
        }
    }

    /// Whether this is (within rounding) the whole frame — then no crop is
    /// added, so a caller passing the full region gets the byte-identical
    /// invocation it always got.
    pub fn is_full(&self) -> bool {
        let r = self.normalized();
        r.left < 1e-6 && r.top < 1e-6 && r.width > 1.0 - 1e-6 && r.height > 1.0 - 1e-6
    }

    /// The `crop` filter for this region against whatever frame it is applied
    /// to, in `iw`/`ih` terms so it needs no knowledge of the frame size. The
    /// width and height are kept even: MJPEG is 4:2:0 and an odd crop would be
    /// realigned by the filter to a size the scale after it cannot predict.
    pub(super) fn crop_filter(&self) -> String {
        let r = self.normalized();
        format!(
            "crop=2*trunc(iw*{w:.4}/2):2*trunc(ih*{h:.4}/2):iw*{l:.4}:ih*{t:.4}",
            w = r.width,
            h = r.height,
            l = r.left,
            t = r.top
        )
    }
}

/// Decode a single frame at `time_secs` as **JPEG** bytes, scaled to at most
/// `max_width` pixels wide, at `quality` (ffmpeg `-q:v`, 2 = best … 31 = worst).
/// JPEG is dramatically smaller than the PNG of [`frame_at`], which matters when
/// the frame is handed to an LLM as an image content block rather than rendered
/// in the GUI. `accurate = false` snaps to the nearest keyframe (fast scrubbing);
/// see [`decode_frame`].
pub fn frame_jpeg(path: &Path, time_secs: f64, max_width: u32, quality: u8, accurate: bool) -> Result<Vec<u8>> {
    let scale = format!("scale='min({max_width},iw)':-2");
    decode_frame(path, time_secs, &scale, "mjpeg", Some(quality), accurate, None)
}

/// [`frame_jpeg`] that gives up with [`Error::Cancelled`] once `cancel` is true.
pub fn frame_jpeg_cancellable(
    path: &Path,
    time_secs: f64,
    max_width: u32,
    quality: u8,
    accurate: bool,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    let scale = format!("scale='min({max_width},iw)':-2");
    decode_frame(path, time_secs, &scale, "mjpeg", Some(quality), accurate, Some(cancel))
}

/// [`frame_jpeg`] of one `region` of the frame: the region is cropped out of
/// the decoded frame *first* and then scaled to at most `max_width`, so the
/// width budget is spent on the region rather than on the whole picture. Never
/// upscaled past the source's own pixels — a zoom shows real detail or none.
/// A full region is exactly [`frame_jpeg`].
pub fn frame_jpeg_region(
    path: &Path,
    time_secs: f64,
    region: Region,
    max_width: u32,
    quality: u8,
    accurate: bool,
) -> Result<Vec<u8>> {
    if region.is_full() {
        return frame_jpeg(path, time_secs, max_width, quality, accurate);
    }
    let vf = region_frame_filter(region, max_width);
    decode_frame(path, time_secs, &vf, "mjpeg", Some(quality), accurate, None)
}

/// [`frame_jpeg_region`] that gives up with [`Error::Cancelled`] once `cancel` is true.
pub fn frame_jpeg_region_cancellable(
    path: &Path,
    time_secs: f64,
    region: Region,
    max_width: u32,
    quality: u8,
    accurate: bool,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    if region.is_full() {
        return frame_jpeg_cancellable(path, time_secs, max_width, quality, accurate, cancel);
    }
    let vf = region_frame_filter(region, max_width);
    decode_frame(path, time_secs, &vf, "mjpeg", Some(quality), accurate, Some(cancel))
}

/// Pure filter chain for [`frame_jpeg_region`] (unit-tested): crop, then scale
/// to an even width no wider than `max_width` or the crop itself.
pub(super) fn region_frame_filter(region: Region, max_width: u32) -> String {
    format!(
        "{crop},scale='2*trunc(min({max_width},iw)/2)':-2",
        crop = region.crop_filter()
    )
}

/// Seek to `time_secs`, run the `-vf` chain on a single frame and pipe it out in
/// the given image codec (`png` / `mjpeg`); `quality`, when set, becomes `-q:v`.
/// Shared by [`frame_at`] and [`frame_jpeg`]. `-ss` is input-side (fast). With
/// `accurate` ffmpeg decodes forward from the keyframe to the exact frame; with
/// `accurate = false` it snaps to the keyframe (`-noaccurate_seek`, no forward
/// decode) — tens of ms even on long-GOP 4K, for responsive scrubbing. Decode is
/// hardware-accelerated per [`hwaccel`]. With a `cancel` the decode is killed when it goes true and
/// the answer is [`Error::Cancelled`] — never a reason to retry in software or to switch hardware
/// decode off; without one (the scrub, which is never cancelled, only superseded) the decode waits
/// on nothing but its own deadline.
pub(super) fn decode_frame(
    path: &Path,
    time_secs: f64,
    vf: &str,
    vcodec: &str,
    quality: Option<u8>,
    accurate: bool,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<Vec<u8>> {
    if cancel.is_some_and(|cancel| cancel()) {
        return Err(Error::Cancelled);
    }
    let time_secs = time_secs.max(0.0);
    // HDR footage is tone-mapped after the caller's own scale/crop, so the
    // expensive float stage runs on the small frame rather than the 4K one.
    let tonemapped;
    let vf = match source_hdr(path) {
        Some(hdr) => {
            tonemapped = format!("{vf},{}", tonemap_filter(hdr));
            tonemapped.as_str()
        }
        None => vf,
    };
    // Cache key captures everything that determines the bytes (path, time,
    // filter — which includes the target width — codec, quality and whether the
    // seek was exact or keyframe-snapped).
    let key = format!("{}|{:.3}|{vf}|{vcodec}|{quality:?}|{accurate}", path.display(), time_secs);
    if let Some(hit) = frame_cache().lock().ok().and_then(|mut c| c.get(&key)) {
        return Ok(hit);
    }

    use std::sync::atomic::Ordering;
    let bin = ffmpeg_bin();
    // A proxy is one intra frame of a small H.264: no hardware session, one thread.
    let proxy = is_cache_proxy(&path.to_string_lossy());
    let hw = decode_hwaccel_for(path, Reading::Frame);
    let use_hw = hw.is_some();

    let bytes = if use_hw {
        match run_frame_decode(
            &bin,
            path,
            time_secs,
            vf,
            vcodec,
            quality,
            accurate,
            hw.as_deref(),
            proxy,
            cancel,
        ) {
            Ok(b) => b,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(hw_err) => {
                // The accelerated decode failed — retry in software. If *that*
                // works, `-hwaccel` is the culprit on this machine, so disable it
                // for subsequent frames; if it fails too, the error is genuine.
                match run_frame_decode(&bin, path, time_secs, vf, vcodec, quality, accurate, None, proxy, cancel) {
                    Ok(b) => {
                        HWACCEL_OK.store(false, Ordering::Relaxed);
                        tracing::warn!("hardware decode failed ({hw_err}); using software decode for previews");
                        b
                    }
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(_) => return Err(hw_err),
                }
            }
        }
    } else {
        run_frame_decode(&bin, path, time_secs, vf, vcodec, quality, accurate, None, proxy, cancel)?
    };

    if let Ok(mut c) = frame_cache().lock() {
        c.put(key, bytes.clone());
    }
    Ok(bytes)
}

/// How long one single-frame decode may run before it is killed (a hardware decoder stuck in a
/// driver would otherwise hold the preview's one frame in flight for good). The software retry
/// that follows is a second chance, not a second wait.
const FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Run one `ffmpeg` single-frame decode (optionally with `-hwaccel hw`) and
/// return the encoded image bytes. Split out of [`decode_frame`] so the same
/// invocation can be retried in software when an accelerated attempt fails.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_frame_decode(
    bin: &str,
    path: &Path,
    time_secs: f64,
    vf: &str,
    vcodec: &str,
    quality: Option<u8>,
    accurate: bool,
    hw: Option<&str>,
    single_thread: bool,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<Vec<u8>> {
    let mut cmd = command(bin);
    // Interactive: never gated and never de-prioritized — a scrubbed frame is
    // wanted now — but still held to the budget's threads, or to one when the
    // frame is read from a proxy (the thread pools would outlast the decode).
    if single_thread {
        cmd.args(["-threads", "1", "-filter_threads", "1", "-filter_complex_threads", "1"]);
    } else {
        cpu::limit_cmd(&mut cmd, cpu::budget());
    }
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    if let Some(hw) = hw {
        cmd.args(["-hwaccel", hw]);
    }
    if !accurate {
        cmd.arg("-noaccurate_seek");
    }
    // No seek to the start: it reads the same first frame, and on a JPEG still (the `image2`
    // demuxer) any `-ss`, 0 included, makes ffmpeg exit 0 having decoded nothing (6.1 and 9.0).
    if let Some(at) = seek_unless_start(time_secs) {
        cmd.arg("-ss").arg(at);
    }
    cmd.arg("-i").arg(path).args(["-frames:v", "1", "-vf", vf]);
    if let Some(q) = quality {
        cmd.args(["-q:v", q.to_string().as_str()]);
    }
    cmd.args(["-f", "image2pipe", "-vcodec", vcodec]);
    if single_thread {
        cmd.args(["-threads", "1"]);
    }
    cmd.arg("pipe:1");
    let output = output_bounded_cancellable(cmd, FRAME_TIMEOUT, cancel)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::TimedOut => Error::Engine(format!(
                "could not extract frame at {time_secs:.3}s: ffmpeg did not answer in {} s and was stopped",
                FRAME_TIMEOUT.as_secs()
            )),
            _ => launch_err(bin, e),
        })?
        .ok_or(Error::Cancelled)?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(Error::Engine(format!(
            "could not extract frame at {time_secs:.3}s: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

/// The `-ss` a single-frame read or a sheet seeks with (`{:.3}`), or `None` at the start, which
/// seeking to would only break a JPEG still: there `-ss 0` decodes no frame and ffmpeg still exits 0.
fn seek_unless_start(at: f64) -> Option<String> {
    let at = format!("{:.3}", at.max(0.0));
    (at != "0.000").then_some(at)
}

/// [`contact_sheet_cancellable`] with nothing to cancel it.
#[cfg(test)]
pub fn contact_sheet(
    path: &Path,
    start: f64,
    end: f64,
    columns: u32,
    rows: u32,
    cell_width: u32,
    quality: u8,
) -> Result<(Vec<u8>, Vec<f64>)> {
    contact_sheet_cancellable(path, start, end, columns, rows, cell_width, quality, &|| false)
}

/// Build a **contact sheet** of `path`: `columns`×`rows` frames sampled evenly
/// across `[start, end)`, each cell scaled to `cell_width` px wide and tiled into
/// one JPEG (`quality` = `-q:v`). Returns the montage bytes plus the per-cell
/// timestamps in row-major order, so the caller can tell an LLM which moment each
/// cell shows. One ffmpeg pass — lets the model skim a long clip cheaply.
///
/// Gives up with [`Error::Cancelled`] once `cancel` is true (polled about fifty times a second; the
/// sampling ffmpeg is killed) — an agent that dropped its request does not leave a many-seek
/// decode running.
#[allow(clippy::too_many_arguments)]
pub fn contact_sheet_cancellable(
    path: &Path,
    start: f64,
    end: f64,
    columns: u32,
    rows: u32,
    cell_width: u32,
    quality: u8,
    cancel: &dyn Fn() -> bool,
) -> Result<(Vec<u8>, Vec<f64>)> {
    let path = path
        .to_str()
        .ok_or_else(|| Error::Engine("asset path is not valid UTF-8".to_string()))?;
    let tonemap = source_hdr(Path::new(path)).map(tonemap_filter);
    let (mut args, times) = build_contact_sheet_args(path, start, end, columns, rows, cell_width, quality, tonemap.as_deref());
    let bin = ffmpeg_bin();
    // Deliberately ungated: this is how an agent *looks* at the footage, and
    // making it wait out a ten-minute render would read as a hung server. It
    // still runs thread-capped and at background priority.
    cpu::limit_args(&mut args, cpu::budget());
    let mut cmd = bg_command(&bin);
    cmd.args(PROGRESS_ON_STDERR).args(&args);
    // `tile` outputs the one frame when the window has been decoded, and ffmpeg reports nothing
    // before: the bound grows with the window (see `sheet_stall`).
    let stall = sheet_stall(end - start);
    let output = output_supervised(cmd, stall, None, cancel).map_err(|e| launch_err(&bin, e))?;
    if output.cancelled {
        return Err(Error::Cancelled);
    }
    if !output.succeeded() || output.stdout.is_empty() {
        let why = if output.stalled {
            format!("ffmpeg said nothing for {}s: {}", stall.as_secs(), output.stderr_tail())
        } else {
            output.stderr.trim().to_string()
        };
        return Err(Error::Engine(format!("could not build contact sheet: {why}")));
    }
    Ok((output.stdout, times))
}

/// The source timestamp each cell of a `columns`×`rows` [`contact_sheet`] over
/// `[start, end)` shows, row-major: the start of each of the equal slices the
/// window is cut into. Public so a caller holding a sheet can turn "cell 7"
/// back into a moment to look at more closely without re-sampling the sheet.
pub fn contact_sheet_times(start: f64, end: f64, columns: u32, rows: u32) -> Vec<f64> {
    let cells = (columns.max(1) * rows.max(1)) as usize;
    let start = start.max(0.0);
    let window = (end - start).max(0.0);
    let step = if window > 0.0 { window / cells as f64 } else { 0.0 };
    (0..cells).map(|k| start + step * k as f64).collect()
}

/// Pure arg builder for [`contact_sheet`] (no I/O, unit-tested): the ffmpeg
/// argument list and the row-major per-cell timestamps. Frames are sampled at
/// the start of each of `columns*rows` equal slices of the window via the `fps`
/// filter over an `-ss`/`-t` window, then `tile`d into the single output frame.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_contact_sheet_args(
    path: &str,
    start: f64,
    end: f64,
    columns: u32,
    rows: u32,
    cell_width: u32,
    quality: u8,
    tonemap: Option<&str>,
) -> (Vec<String>, Vec<f64>) {
    let columns = columns.max(1);
    let rows = rows.max(1);
    let cells = (columns * rows) as usize;
    let start = start.max(0.0);
    let window = (end - start).max(0.0);
    let times = contact_sheet_times(start, end, columns, rows);
    // `fps` = one frame per slice over the seeked window; `tile` packs them and
    // `-frames:v 1` emits the single sheet. A degenerate window falls back to 1.
    let rate = if window > 0.0 { cells as f64 / window } else { 1.0 };
    // HDR sources are tone-mapped per cell, after the downscale, before the tile.
    let tone = tonemap.map(|t| format!("{t},")).unwrap_or_default();
    // A head-padded proxy opens with a clone of its first frame, which the original has no
    // counterpart for: read from the very start (no seek, which keeps it) it would take a
    // slot of its own and push every cell one over. Any later seek has skipped it already.
    let unpad = if start < 0.0005 && is_head_padded_proxy(path) {
        "trim=start_frame=1,"
    } else {
        ""
    };
    let vf = format!("{unpad}fps={rate},scale={cell_width}:-2:flags=bilinear,{tone}tile={columns}x{rows}");
    let mut args = vec!["-hide_banner".to_string(), "-loglevel".to_string(), "error".to_string()];
    if let Some(at) = seek_unless_start(start) {
        args.extend(["-ss".to_string(), at]);
    }
    args.extend([
        "-t".to_string(),
        format!("{window:.3}"),
        "-i".to_string(),
        path.to_string(),
        "-frames:v".to_string(),
        "1".to_string(),
        "-vf".to_string(),
        vf,
        "-q:v".to_string(),
        quality.to_string(),
        "-f".to_string(),
        "image2pipe".to_string(),
        "-vcodec".to_string(),
        "mjpeg".to_string(),
        "pipe:1".to_string(),
    ]);
    (args, times)
}

/// Decode the first audio stream to mono f32 PCM at `sample_rate` Hz and reduce
/// it to `buckets` peak magnitudes in `0.0..=1.0` (for waveform rendering).
pub fn waveform(path: &Path, buckets: usize, sample_rate: u32) -> Result<Vec<f32>> {
    waveform_cancellable(path, buckets, sample_rate, &|| false)
}

/// [`waveform`] that gives up with [`Error::Cancelled`] once `cancel` is true (the decode is killed).
pub fn waveform_cancellable(path: &Path, buckets: usize, sample_rate: u32, cancel: &dyn Fn() -> bool) -> Result<Vec<f32>> {
    let buckets = buckets.max(1);
    // Stream the decoded PCM through a bounded peak-downsampler instead of
    // buffering the whole signal: a 1h file at 8 kHz mono is ~115 MB of f32
    // otherwise (held twice — raw stdout then the parsed Vec). Here memory is
    // O(buckets) regardless of length, and ffmpeg's decode is the only cost.
    let bin = ffmpeg_bin();
    // Ungated like the other reads the timeline draws from — a clip's waveform
    // appearing is not worth queueing behind an export — but capped and niced.
    let mut cmd = bg_command(&bin);
    cpu::limit_cmd(&mut cmd, cpu::budget());
    let child = cmd
        .args(["-hide_banner", "-loglevel", "error"])
        .arg("-i")
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-ac",
            "1",
            "-ar",
            &sample_rate.to_string(),
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    let mut down = PeakDownsampler::new(buckets);
    // The pump kills a decode that stops producing for `READ_STALL`, which a bare read of its
    // stdout never did.
    super::peaks::pump_pcm_cancellable(child, 4, READ_STALL, cancel, &mut |samples| {
        for s in samples {
            down.push(s.abs());
        }
    })?;
    Ok(down.finish())
}

/// Folds an unbounded stream of sample magnitudes into exactly `buckets` peak
/// values, holding only `2*buckets` floats. Each incoming sample updates the
/// running peak of the current bucket; once the buffer fills it halves
/// resolution (merging adjacent buckets, doubling samples-per-bucket) and keeps
/// going — so the output is length-independent without knowing the total up
/// front. [`finish`] resamples the filled region down to `buckets`.
pub(super) struct PeakDownsampler {
    buf: Vec<f32>,
    buckets: usize,
    write: usize,
    samples_per_bucket: u64,
    in_bucket: u64,
}

impl PeakDownsampler {
    pub(super) fn new(buckets: usize) -> Self {
        let buckets = buckets.max(1);
        Self {
            buf: vec![0.0; buckets * 2],
            buckets,
            write: 0,
            samples_per_bucket: 1,
            in_bucket: 0,
        }
    }

    pub(super) fn push(&mut self, magnitude: f32) {
        let a = magnitude.clamp(0.0, 1.0);
        self.buf[self.write] = self.buf[self.write].max(a);
        self.in_bucket += 1;
        if self.in_bucket < self.samples_per_bucket {
            return;
        }
        self.in_bucket = 0;
        self.write += 1;
        if self.write == self.buf.len() {
            // Buffer full: merge each adjacent pair into the first half (peak of
            // peaks), clear the rest, and halve the resolution.
            for i in 0..self.buckets {
                self.buf[i] = self.buf[2 * i].max(self.buf[2 * i + 1]);
            }
            for slot in self.buf[self.buckets..].iter_mut() {
                *slot = 0.0;
            }
            self.write = self.buckets;
            self.samples_per_bucket *= 2;
        }
    }

    pub(super) fn finish(self) -> Vec<f32> {
        // `write` complete buckets plus an in-progress partial one, all at the
        // current resolution; collapse to exactly `buckets`.
        let len = self.write + if self.in_bucket > 0 { 1 } else { 0 };
        peaks(&self.buf[..len], self.buckets)
    }
}

pub(super) fn peaks(samples: &[f32], buckets: usize) -> Vec<f32> {
    if samples.is_empty() {
        return vec![0.0; buckets];
    }
    let mut out = Vec::with_capacity(buckets);
    for b in 0..buckets {
        let lo = b * samples.len() / buckets;
        let hi = ((b + 1) * samples.len() / buckets).max(lo + 1).min(samples.len());
        let peak = samples[lo..hi].iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        out.push(peak.clamp(0.0, 1.0));
    }
    out
}

/// Decode the first audio stream to 16 kHz mono f32 PCM (Whisper's input shape); gives up with
/// [`Error::Cancelled`] once `cancel` is true.
#[cfg(feature = "whisper")]
pub fn decode_audio_16k_mono_cancellable(path: &Path, cancel: &dyn Fn() -> bool) -> Result<Vec<f32>> {
    decode_audio_mono_f32(path, 16_000, cancel)
}

/// Decode the first audio stream to mono f32 PCM at `sample_rate`, into memory. A whole-file
/// decode, so it takes the heavy-job slot (and says "cancelled" if `cancel` trips while it
/// waits for it), and it is killed after [`READ_STALL`] without a byte of audio.
pub(in crate::engine) fn decode_audio_mono_f32(path: &Path, sample_rate: u32, cancel: &dyn Fn() -> bool) -> Result<Vec<f32>> {
    let bin = ffmpeg_bin();
    // Decodes the whole stream *into memory* — eight of these at once is the
    // several gigabytes an unsupervised agent used to cost — so it is gated.
    let cpu = cpu::lease_waiting(&mut |_| {}, cancel)?;
    if cancel() {
        return Err(Error::Cancelled);
    }
    let mut cmd = bg_command(&bin);
    cpu::limit_cmd(&mut cmd, cpu.budget());
    let child = cmd
        .args(["-hide_banner", "-loglevel", "error"])
        .arg("-i")
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-ac",
            "1",
            "-ar",
            &sample_rate.to_string(),
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    let mut samples = Vec::new();
    super::peaks::pump_pcm_cancellable(child, 4, READ_STALL, cancel, &mut |frames| {
        samples.extend_from_slice(frames);
    })?;
    Ok(samples)
}

/// How long one [`audio_pcm`] window may take before it is killed: a window is seconds of sound
/// behind a fast seek, so a minute is a read that went wrong.
const AUDIO_PCM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Decode a window of the first audio stream to mono s16le PCM at
/// `sample_rate` — the GUI's Web Audio preview playback. Raw s16le (no
/// container) so the webview can build an `AudioBuffer` directly without
/// codec support; the input-side `-ss` fast-seek keeps a window deep in a
/// long source cheap to extract.
pub fn audio_pcm(path: &Path, start: f64, duration: f64, sample_rate: u32, filters: Option<&str>) -> Result<Vec<u8>> {
    audio_pcm_with(path, start, duration, sample_rate, filters, None)
}

/// [`audio_pcm`] that gives up with [`Error::Cancelled`] once `cancel` is true (the decode is
/// killed), for a caller that is itself a job someone can stop.
pub fn audio_pcm_cancellable(
    path: &Path,
    start: f64,
    duration: f64,
    sample_rate: u32,
    filters: Option<&str>,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    audio_pcm_with(path, start, duration, sample_rate, filters, Some(cancel))
}

fn audio_pcm_with(
    path: &Path,
    start: f64,
    duration: f64,
    sample_rate: u32,
    filters: Option<&str>,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<Vec<u8>> {
    if cancel.is_some_and(|cancel| cancel()) {
        return Err(Error::Cancelled);
    }
    let bin = ffmpeg_bin();
    let mut cmd = command(&bin);
    // Interactive, like the frame decode: the preview's playback is waiting on it.
    cpu::limit_cmd(&mut cmd, cpu::budget());
    cmd.args(["-hide_banner", "-loglevel", "error"])
        .args(["-ss", &start.max(0.0).to_string()])
        .arg("-i")
        .arg(path)
        .args([
            "-t",
            &duration.max(0.0).to_string(),
            "-map",
            "0:a:0",
            "-ac",
            "1",
            "-ar",
            &sample_rate.to_string(),
        ]);
    // The clip's own effect chain, so the monitor hears the EQ / compressor /
    // gate the export will render rather than the dry source.
    if let Some(filters) = filters.filter(|f| !f.is_empty()) {
        cmd.args(["-af", filters]);
    }
    cmd.args(["-f", "s16le", "pipe:1"]);
    let output = output_bounded_cancellable(cmd, AUDIO_PCM_TIMEOUT, cancel)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::TimedOut => Error::Engine(format!(
                "could not decode audio: ffmpeg did not answer in {} s and was stopped",
                AUDIO_PCM_TIMEOUT.as_secs()
            )),
            _ => launch_err(&bin, e),
        })?
        .ok_or(Error::Cancelled)?;
    if !output.status.success() {
        return Err(Error::Engine(format!(
            "could not decode audio: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}
