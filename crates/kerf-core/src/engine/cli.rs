//! CLI-driven media engine: probing, analysis, frame/waveform extraction and
//! export by invoking the system `ffmpeg` / `ffprobe` binaries.
//!
//! Unlike [`super::ffmpeg`] (in-process libav, gated behind the `ffmpeg`
//! feature and the FFmpeg *development* libraries), everything here only needs
//! the FFmpeg *binaries* on `PATH`, so it compiles and runs in the
//! `--no-default-features` build. The binaries can be overridden with the
//! `KERF_FFMPEG` / `KERF_FFPROBE` environment variables.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use super::ProbeResult;
use crate::error::{Error, Result};
use crate::model::{
    Asset, AudioEffect, Clip, Color, Projection, Reframe, ReframeKeyframe, ResolvedReframe, StreamInfo, StreamKind, TextOverlay,
    TimeRange, Timeline, Transform, TransitionKind, VideoEffect,
};

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

pub(super) fn ffmpeg_bin() -> String {
    std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string())
}

fn ffprobe_bin() -> String {
    std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string())
}

/// The decode hardware-acceleration to request for preview frames. Defaults to
/// ffmpeg's `auto` (D3D11VA on Windows, VAAPI / VideoToolbox elsewhere; falls
/// back to software when none is usable), which offloads 4K decode off the CPU.
/// Set `KERF_HWACCEL=none` (or empty) to force software decoding.
fn hwaccel() -> Option<String> {
    match std::env::var("KERF_HWACCEL") {
        Ok(v) if v.is_empty() || v.eq_ignore_ascii_case("none") => None,
        Ok(v) => Some(v),
        Err(_) => Some("auto".to_string()),
    }
}

/// Cleared the first time an accelerated preview decode fails while the software
/// retry succeeds — i.e. `-hwaccel` is broken on this machine (a misconfigured
/// VAAPI / D3D11VA, an unsupported codec for the chosen accelerator). Once
/// cleared, later preview frames skip the accelerated attempt so a broken
/// `-hwaccel auto` doesn't double every frame's latency.
static HWACCEL_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Cleared the first time a hardware *encode* (proxy / stitch) fails while the
/// software retry succeeds, so a broken GPU encoder doesn't double every later
/// background encode.
static HW_ENCODE_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The `-hwaccel` for the engine's own decodes (preview stills and streaming,
/// proxy generation, stitching, scene detection): the configured accel unless
/// an earlier fallback proved it broken on this machine.
pub fn decode_hwaccel() -> Option<String> {
    if HWACCEL_OK.load(std::sync::atomic::Ordering::Relaxed) {
        hwaccel()
    } else {
        None
    }
}

/// Whether hardware encoding may be used at all. `KERF_HW_ENCODE=none` (or
/// empty, or `0`) forces every internal encode onto the software encoders.
fn hw_encode_enabled() -> bool {
    match std::env::var("KERF_HW_ENCODE") {
        Ok(v) => !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("none")),
        Err(_) => true,
    }
}

/// Every hardware encoder the export surface knows how to drive, in family
/// preference order (NVENC, QuickSync, VideoToolbox, AMF). VAAPI encoders are
/// deliberately absent: they only accept frames already uploaded to the GPU,
/// which the software filtergraph never produces, whereas these all take system
/// memory frames directly.
const HW_ENCODER_CANDIDATES: [&str; 10] = [
    "h264_nvenc",
    "hevc_nvenc",
    "av1_nvenc",
    "h264_qsv",
    "hevc_qsv",
    "av1_qsv",
    "h264_videotoolbox",
    "hevc_videotoolbox",
    "h264_amf",
    "hevc_amf",
];

/// The hardware video encoders this machine's ffmpeg can actually use, probed
/// once per process and cached. `-encoders` listing an encoder is not proof —
/// an nvenc-enabled build without a usable NVIDIA driver still lists it and
/// then fails at open — so each compiled-in candidate is exercised with a
/// one-frame test encode and only the ones that succeed are reported. Ordered
/// by [`HW_ENCODER_CANDIDATES`]. `KERF_HW_ENCODE=none` reports none.
pub fn hw_encoders() -> &'static [String] {
    static ENCODERS: OnceLock<Vec<String>> = OnceLock::new();
    ENCODERS.get_or_init(|| {
        if !hw_encode_enabled() {
            return Vec::new();
        }
        let bin = ffmpeg_bin();
        let listed = match command(&bin)
            .args(["-hide_banner", "-v", "error", "-encoders"])
            .stderr(Stdio::null())
            .output()
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            _ => return Vec::new(),
        };
        // `-encoders` prints ` V....D h264_nvenc  NVIDIA NVENC…` — the name is
        // the second whitespace-separated token.
        let compiled: std::collections::HashSet<&str> = listed.lines().filter_map(|l| l.split_whitespace().nth(1)).collect();
        let found: Vec<String> = HW_ENCODER_CANDIDATES
            .iter()
            .filter(|enc| compiled.contains(**enc))
            .filter(|enc| {
                // 256x256 clears every family's minimum-dimension floor; nv12 is
                // the input format they all accept.
                command(&bin)
                    .args(["-hide_banner", "-v", "error", "-f", "lavfi", "-i", "color=black:s=256x256:r=30:d=0.2"])
                    .args(["-frames:v", "1", "-pix_fmt", "nv12", "-c:v", enc, "-f", "null", "-"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            })
            .map(|s| s.to_string())
            .collect();
        tracing::info!(encoders = ?found, "hardware video encoders detected");
        found
    })
}

pub(super) fn launch_err(bin: &str, e: std::io::Error) -> Error {
    Error::Engine(format!("failed to launch `{bin}` ({e}); is FFmpeg installed and on PATH?"))
}

/// Build a `Command` for an ffmpeg/ffprobe binary. On Windows this sets
/// `CREATE_NO_WINDOW` so spawning the console subprocess doesn't flash a
/// terminal window over the GUI; on other platforms it's a plain `Command`.
pub(super) fn command(bin: &str) -> Command {
    let cmd = Command::new(bin);
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

// ---- probe -----------------------------------------------------------------

#[derive(serde::Deserialize)]
struct ProbeJson {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    #[serde(default)]
    format: Option<ProbeFormat>,
}

#[derive(serde::Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

#[derive(serde::Deserialize)]
struct ProbeStream {
    index: u32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u16>,
    duration: Option<String>,
    /// Stream-level side data, which is where a spherical mapping surfaces. The
    /// mov demuxer fills this from the `sv3d` box (and the legacy Google
    /// spatial-media `uuid` blob), and `-show_streams` already prints it — no
    /// extra ffprobe flag is needed. (`-export_side_data` is a *decoder* option
    /// for film grain / motion vectors and is unrelated.)
    #[serde(default)]
    side_data_list: Option<Vec<ProbeSideData>>,
}

#[derive(serde::Deserialize)]
struct ProbeSideData {
    side_data_type: Option<String>,
    projection: Option<String>,
}

/// Probe a media file via `ffprobe -of json`.
// In a full `ffmpeg` build the in-process libav probe is used instead.
#[cfg_attr(feature = "ffmpeg", allow(dead_code))]
pub fn probe(path: &Path) -> Result<ProbeResult> {
    let bin = ffprobe_bin();
    let output = command(&bin)
        .args(["-v", "error", "-show_format", "-show_streams", "-of", "json"])
        .arg(path)
        .output()
        .map_err(|e| launch_err(&bin, e))?;
    if !output.status.success() {
        return Err(Error::Engine(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let parsed: ProbeJson =
        serde_json::from_slice(&output.stdout).map_err(|e| Error::Engine(format!("could not parse ffprobe output: {e}")))?;
    Ok(probe_from_json(parsed, Some(path)))
}

fn probe_from_json(parsed: ProbeJson, path: Option<&Path>) -> ProbeResult {
    let format_dur = parsed
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .and_then(|d| d.parse::<f64>().ok());

    // A still image probes as a lone video stream in an image codec with no
    // playable duration and no audio. We mark that stream so the engine loops it
    // for the clip length on export and decodes its single frame without seeking.
    // A still has no real timeline; we treat a sub-second duration as "none" so the
    // probe is robust whether the demuxer reports N/A (ffprobe) or a single frame's
    // worth (libav). The image-codec guard keeps short *videos* from being misread.
    let video_count = parsed
        .streams
        .iter()
        .filter(|s| s.codec_type.as_deref() == Some("video"))
        .count();
    let has_audio = parsed.streams.iter().any(|s| s.codec_type.as_deref() == Some("audio"));
    let still = video_count == 1
        && !has_audio
        && format_dur.unwrap_or(0.0) < STILL_MAX_DURATION
        && parsed
            .streams
            .iter()
            .any(|s| s.codec_type.as_deref() == Some("video") && is_still_codec(s.codec_name.as_deref()));

    let mut streams = Vec::new();
    let mut max_stream_dur = 0.0_f64;
    for s in &parsed.streams {
        let kind = match s.codec_type.as_deref() {
            Some("video") => StreamKind::Video,
            Some("audio") => StreamKind::Audio,
            Some("subtitle") => StreamKind::Subtitle,
            _ => StreamKind::Data,
        };
        if let Some(d) = s.duration.as_deref().and_then(|d| d.parse::<f64>().ok()) {
            max_stream_dur = max_stream_dur.max(d);
        }
        streams.push(StreamInfo {
            index: s.index,
            kind,
            codec: s.codec_name.clone().unwrap_or_default(),
            width: s.width,
            height: s.height,
            fps: s.r_frame_rate.as_deref().and_then(parse_rational),
            sample_rate: s.sample_rate.as_deref().and_then(|r| r.parse().ok()),
            channels: s.channels,
            image: still && kind == StreamKind::Video,
            projection: if kind == StreamKind::Video {
                detect_projection(path, s)
            } else {
                None
            },
        });
    }
    let duration = format_dur.unwrap_or(max_stream_dur).max(0.0);
    ProbeResult { duration, streams }
}

/// FFmpeg `codec_name`s for single-frame still images. Animated containers
/// (gif/webp/apng) only land here when they probe with no playable duration —
/// i.e. they really are one frame; an animated one keeps a real duration and is
/// treated as ordinary video.
/// A probed duration below this (seconds) counts as "no real duration" when
/// deciding whether an image-codec stream is a still vs. an animated/looping one.
pub(crate) const STILL_MAX_DURATION: f64 = 1.0;

pub(crate) fn is_still_codec(codec: Option<&str>) -> bool {
    matches!(
        codec,
        Some(
            "png"
                | "mjpeg"
                | "jpeg"
                | "jpegls"
                | "bmp"
                | "gif"
                | "webp"
                | "tiff"
                | "ppm"
                | "pgm"
                | "pgmyuv"
                | "pam"
                | "targa"
                | "tga"
                | "qoi"
                | "apng"
                | "jpeg2000"
                | "j2k"
                | "heif"
                | "heic"
        )
    )
}

/// Decide whether a probed video stream is 360 footage, and in which projection.
///
/// Two signals, strongest first:
///
/// 1. A `Spherical Mapping` side-data entry declaring an equirectangular
///    projection. This is authoritative — it is what a stitched export (Insta360
///    Studio, the Google spatial-media injector, YouTube-ready files) writes.
/// 2. An Insta360 `.insv` whose frame is two squares side by side — the raw
///    dual-fisheye shape (a 5.7K capture probes as 5760x2880, i.e. two 2880x2880
///    circular hemispheres).
///
/// Deliberately **no** bare aspect-ratio guess. A 2:1 frame is a real and common
/// shape for anamorphic masters, ultrawide edits and ordinary stitched panoramas,
/// and the costs are lopsided: a missed detection costs one click in the
/// Inspector, whereas a false positive silently reprojects ordinary footage and
/// changes the export resolution out from under the user. Everything past these
/// two signals is a manual override.
fn detect_projection(path: Option<&Path>, s: &ProbeStream) -> Option<Projection> {
    if let Some(list) = &s.side_data_list {
        for sd in list {
            if sd.side_data_type.as_deref() == Some("Spherical Mapping") {
                return spherical_side_data_projection(sd.projection.as_deref());
            }
        }
    }
    projection_from_shape(path, s.width, s.height)
}

/// Signal 1: map the `projection` field of a `Spherical Mapping` side-data entry.
/// `None` for the field covers older ffmpeg builds that name the side data but not
/// its projection — equirect is the only mono-360 projection Insta360 and the
/// spatial-media spec actually emit. Cubemap / mesh projections exist but are not
/// something we can reframe faithfully, so they stay flat rather than being
/// reprojected wrongly.
pub(crate) fn spherical_side_data_projection(projection: Option<&str>) -> Option<Projection> {
    match projection {
        Some("equirectangular") | Some("half equirectangular") | None => Some(Projection::Equirect),
        _ => None,
    }
}

/// Signal 2: an Insta360 capture (`.insv` / `.insp`) whose frame is exactly two
/// squares side by side — the raw dual-fisheye packing (a 5.7K capture probes as
/// 5760x2880, i.e. two 2880x2880 circular hemispheres). Shared with the libav
/// probe backend so both agree on the geometry rule.
pub(crate) fn projection_from_shape(path: Option<&Path>, width: Option<u32>, height: Option<u32>) -> Option<Projection> {
    let insta360 = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("insv") || e.eq_ignore_ascii_case("insp"));
    match (insta360, width, height) {
        (true, Some(w), Some(h)) if h > 0 && w == h * 2 => Some(Projection::DualFisheye),
        _ => None,
    }
}

/// Parse an FFmpeg rational like `"30000/1001"` into an `f64`.
fn parse_rational(s: &str) -> Option<f64> {
    let (num, den) = s.split_once('/')?;
    let num: f64 = num.trim().parse().ok()?;
    let den: f64 = den.trim().parse().ok()?;
    if den == 0.0 {
        None
    } else {
        Some(num / den)
    }
}

// ---- silence / scene analysis ---------------------------------------------

/// Detect silent spans using the `silencedetect` filter.
///
/// `noise_db` is the threshold in dBFS (e.g. `-30.0`); `min_silence` is the
/// shortest span to report, in seconds.
pub fn detect_silence(path: &Path, noise_db: f64, min_silence: f64) -> Result<Vec<TimeRange>> {
    let bin = ffmpeg_bin();
    let filter = format!("silencedetect=noise={noise_db}dB:d={min_silence}");
    let output = command(&bin)
        .args(["-hide_banner", "-nostats"])
        .arg("-i")
        .arg(path)
        .args(["-map", "0:a:0?", "-af", &filter, "-f", "null", "-"])
        .stdout(Stdio::null())
        .output()
        .map_err(|e| launch_err(&bin, e))?;
    // silencedetect prints to stderr regardless of exit status.
    Ok(parse_silence(&String::from_utf8_lossy(&output.stderr)))
}

fn parse_silence(stderr: &str) -> Vec<TimeRange> {
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
const SCENE_DETECT_WIDTH: u32 = 640;

/// Detect scene-change timestamps using `select='gt(scene,threshold)'`.
///
/// Decodes with `-hwaccel` when configured (the whole file is decoded, which is
/// the expensive part for 4K sources); a failed accelerated run retries in
/// software, mirroring [`decode_frame`]'s fallback.
pub fn detect_scenes(path: &Path, threshold: f64) -> Result<Vec<f64>> {
    use std::sync::atomic::Ordering;

    let bin = ffmpeg_bin();
    let filter = format!("scale='min({SCENE_DETECT_WIDTH},iw)':-2:flags=bilinear,select='gt(scene,{threshold})',showinfo");
    let run = |hw: Option<&str>| {
        let mut cmd = command(&bin);
        cmd.args(["-hide_banner", "-nostats"]);
        if let Some(hw) = hw {
            cmd.args(["-hwaccel", hw]);
        }
        cmd.arg("-i")
            .arg(path)
            .args(["-map", "0:v:0?", "-vf", &filter, "-f", "null", "-"])
            .stdout(Stdio::null())
            .output()
            .map_err(|e| launch_err(&bin, e))
    };
    let hw = decode_hwaccel();
    let output = match hw.as_deref() {
        Some(h) => {
            let out = run(Some(h))?;
            if out.status.success() {
                out
            } else {
                let sw = run(None)?;
                if sw.status.success() {
                    HWACCEL_OK.store(false, Ordering::Relaxed);
                    tracing::warn!("hardware decode failed during scene detection; using software decode");
                }
                sw
            }
        }
        None => run(None)?,
    };
    Ok(parse_scenes(&String::from_utf8_lossy(&output.stderr)))
}

fn parse_scenes(stderr: &str) -> Vec<f64> {
    let mut times = Vec::new();
    for line in stderr.lines() {
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

// ---- frame / waveform extraction ------------------------------------------

/// Decode a single frame at `time_secs` and return it as PNG bytes, scaled to
/// at most `max_width` pixels wide.
pub fn frame_at(path: &Path, time_secs: f64, max_width: u32) -> Result<Vec<u8>> {
    let scale = format!("scale='min({max_width},iw)':-2");
    decode_frame(path, time_secs, &scale, "png", None, true)
}

/// Decode a single frame at `time_secs` as **JPEG** bytes, scaled to at most
/// `max_width` pixels wide, at `quality` (ffmpeg `-q:v`, 2 = best … 31 = worst).
/// JPEG is dramatically smaller than the PNG of [`frame_at`], which matters when
/// the frame is handed to an LLM as an image content block rather than rendered
/// in the GUI. `accurate = false` snaps to the nearest keyframe (fast scrubbing);
/// see [`decode_frame`].
pub fn frame_jpeg(path: &Path, time_secs: f64, max_width: u32, quality: u8, accurate: bool) -> Result<Vec<u8>> {
    let scale = format!("scale='min({max_width},iw)':-2");
    decode_frame(path, time_secs, &scale, "mjpeg", Some(quality), accurate)
}

/// Seek to `time_secs`, run the `-vf` chain on a single frame and pipe it out in
/// the given image codec (`png` / `mjpeg`); `quality`, when set, becomes `-q:v`.
/// Shared by [`frame_at`] and [`frame_jpeg`]. `-ss` is input-side (fast). With
/// `accurate` ffmpeg decodes forward from the keyframe to the exact frame; with
/// `accurate = false` it snaps to the keyframe (`-noaccurate_seek`, no forward
/// decode) — tens of ms even on long-GOP 4K, for responsive scrubbing. Decode is
/// hardware-accelerated per [`hwaccel`].
fn decode_frame(path: &Path, time_secs: f64, vf: &str, vcodec: &str, quality: Option<u8>, accurate: bool) -> Result<Vec<u8>> {
    let time_secs = time_secs.max(0.0);
    // Cache key captures everything that determines the bytes (path, time,
    // filter — which includes the target width — codec, quality and whether the
    // seek was exact or keyframe-snapped).
    let key = format!("{}|{:.3}|{vf}|{vcodec}|{quality:?}|{accurate}", path.display(), time_secs);
    if let Some(hit) = frame_cache().lock().ok().and_then(|mut c| c.get(&key)) {
        return Ok(hit);
    }

    use std::sync::atomic::Ordering;
    let bin = ffmpeg_bin();
    let hw = hwaccel();
    let use_hw = hw.is_some() && HWACCEL_OK.load(Ordering::Relaxed);

    let bytes = if use_hw {
        match run_frame_decode(&bin, path, time_secs, vf, vcodec, quality, accurate, hw.as_deref()) {
            Ok(b) => b,
            Err(hw_err) => {
                // The accelerated decode failed — retry in software. If *that*
                // works, `-hwaccel` is the culprit on this machine, so disable it
                // for subsequent frames; if it fails too, the error is genuine.
                match run_frame_decode(&bin, path, time_secs, vf, vcodec, quality, accurate, None) {
                    Ok(b) => {
                        HWACCEL_OK.store(false, Ordering::Relaxed);
                        tracing::warn!("hardware decode failed ({hw_err}); using software decode for previews");
                        b
                    }
                    Err(_) => return Err(hw_err),
                }
            }
        }
    } else {
        run_frame_decode(&bin, path, time_secs, vf, vcodec, quality, accurate, None)?
    };

    if let Ok(mut c) = frame_cache().lock() {
        c.put(key, bytes.clone());
    }
    Ok(bytes)
}

/// Run one `ffmpeg` single-frame decode (optionally with `-hwaccel hw`) and
/// return the encoded image bytes. Split out of [`decode_frame`] so the same
/// invocation can be retried in software when an accelerated attempt fails.
#[allow(clippy::too_many_arguments)]
fn run_frame_decode(
    bin: &str,
    path: &Path,
    time_secs: f64,
    vf: &str,
    vcodec: &str,
    quality: Option<u8>,
    accurate: bool,
    hw: Option<&str>,
) -> Result<Vec<u8>> {
    let mut cmd = command(bin);
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    if let Some(hw) = hw {
        cmd.args(["-hwaccel", hw]);
    }
    if !accurate {
        cmd.arg("-noaccurate_seek");
    }
    cmd.arg("-ss")
        .arg(format!("{time_secs:.3}"))
        .arg("-i")
        .arg(path)
        .args(["-frames:v", "1", "-vf", vf]);
    if let Some(q) = quality {
        cmd.args(["-q:v", q.to_string().as_str()]);
    }
    cmd.args(["-f", "image2pipe", "-vcodec", vcodec, "pipe:1"])
        .stderr(Stdio::piped());
    let output = cmd.output().map_err(|e| launch_err(bin, e))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(Error::Engine(format!(
            "could not extract frame at {time_secs:.3}s: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

/// Build a **contact sheet** of `path`: `columns`×`rows` frames sampled evenly
/// across `[start, end)`, each cell scaled to `cell_width` px wide and tiled into
/// one JPEG (`quality` = `-q:v`). Returns the montage bytes plus the per-cell
/// timestamps in row-major order, so the caller can tell an LLM which moment each
/// cell shows. One ffmpeg pass — lets the model skim a long clip cheaply.
pub fn contact_sheet(
    path: &Path,
    start: f64,
    end: f64,
    columns: u32,
    rows: u32,
    cell_width: u32,
    quality: u8,
) -> Result<(Vec<u8>, Vec<f64>)> {
    let path = path
        .to_str()
        .ok_or_else(|| Error::Engine("asset path is not valid UTF-8".to_string()))?;
    let (args, times) = build_contact_sheet_args(path, start, end, columns, rows, cell_width, quality);
    let bin = ffmpeg_bin();
    let output = command(&bin)
        .args(&args)
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| launch_err(&bin, e))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(Error::Engine(format!(
            "could not build contact sheet: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok((output.stdout, times))
}

/// Pure arg builder for [`contact_sheet`] (no I/O, unit-tested): the ffmpeg
/// argument list and the row-major per-cell timestamps. Frames are sampled at
/// the start of each of `columns*rows` equal slices of the window via the `fps`
/// filter over an `-ss`/`-t` window, then `tile`d into the single output frame.
fn build_contact_sheet_args(
    path: &str,
    start: f64,
    end: f64,
    columns: u32,
    rows: u32,
    cell_width: u32,
    quality: u8,
) -> (Vec<String>, Vec<f64>) {
    let columns = columns.max(1);
    let rows = rows.max(1);
    let cells = (columns * rows) as usize;
    let start = start.max(0.0);
    let window = (end - start).max(0.0);
    let step = if window > 0.0 { window / cells as f64 } else { 0.0 };
    let times: Vec<f64> = (0..cells).map(|k| start + step * k as f64).collect();
    // `fps` = one frame per slice over the seeked window; `tile` packs them and
    // `-frames:v 1` emits the single sheet. A degenerate window falls back to 1.
    let rate = if window > 0.0 { cells as f64 / window } else { 1.0 };
    let vf = format!("fps={rate},scale={cell_width}:-2:flags=bilinear,tile={columns}x{rows}");
    let args = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-ss".to_string(),
        format!("{start:.3}"),
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
    ];
    (args, times)
}

/// Decode the first audio stream to mono f32 PCM at `sample_rate` Hz and reduce
/// it to `buckets` peak magnitudes in `0.0..=1.0` (for waveform rendering).
pub fn waveform(path: &Path, buckets: usize, sample_rate: u32) -> Result<Vec<f32>> {
    use std::io::Read;

    let buckets = buckets.max(1);
    // Stream the decoded PCM through a bounded peak-downsampler instead of
    // buffering the whole signal: a 1h file at 8 kHz mono is ~115 MB of f32
    // otherwise (held twice — raw stdout then the parsed Vec). Here memory is
    // O(buckets) regardless of length, and ffmpeg's decode is the only cost.
    let bin = ffmpeg_bin();
    let mut child = command(&bin)
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;

    let stderr = child.stderr.take().expect("stderr piped");
    let stderr_handle = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::BufReader::new(stderr).read_to_string(&mut s);
        s
    });

    let mut down = PeakDownsampler::new(buckets);
    let mut stdout = child.stdout.take().expect("stdout piped");
    // Read in fixed blocks and parse 4-byte f32le samples, carrying any straddling
    // tail bytes (a pipe read need not land on a sample boundary) into the next read.
    let mut block = [0u8; 1 << 16];
    let mut leftover: Vec<u8> = Vec::with_capacity(4);
    loop {
        let n = stdout
            .read(&mut block)
            .map_err(|e| Error::Engine(format!("waveform read failed: {e}")))?;
        if n == 0 {
            break;
        }
        leftover.extend_from_slice(&block[..n]);
        for s in leftover.chunks_exact(4) {
            down.push(f32::from_le_bytes([s[0], s[1], s[2], s[3]]).abs());
        }
        let consumed = (leftover.len() / 4) * 4;
        leftover.copy_within(consumed.., 0);
        leftover.truncate(leftover.len() - consumed);
    }

    let status = child.wait().map_err(|e| Error::Engine(format!("ffmpeg wait failed: {e}")))?;
    if !status.success() {
        let err = stderr_handle.join().unwrap_or_default();
        return Err(Error::Engine(format!("could not decode audio: {}", err.trim())));
    }
    Ok(down.finish())
}

/// Folds an unbounded stream of sample magnitudes into exactly `buckets` peak
/// values, holding only `2*buckets` floats. Each incoming sample updates the
/// running peak of the current bucket; once the buffer fills it halves
/// resolution (merging adjacent buckets, doubling samples-per-bucket) and keeps
/// going — so the output is length-independent without knowing the total up
/// front. [`finish`] resamples the filled region down to `buckets`.
struct PeakDownsampler {
    buf: Vec<f32>,
    buckets: usize,
    write: usize,
    samples_per_bucket: u64,
    in_bucket: u64,
}

impl PeakDownsampler {
    fn new(buckets: usize) -> Self {
        let buckets = buckets.max(1);
        Self {
            buf: vec![0.0; buckets * 2],
            buckets,
            write: 0,
            samples_per_bucket: 1,
            in_bucket: 0,
        }
    }

    fn push(&mut self, magnitude: f32) {
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

    fn finish(self) -> Vec<f32> {
        // `write` complete buckets plus an in-progress partial one, all at the
        // current resolution; collapse to exactly `buckets`.
        let len = self.write + if self.in_bucket > 0 { 1 } else { 0 };
        peaks(&self.buf[..len], self.buckets)
    }
}

fn peaks(samples: &[f32], buckets: usize) -> Vec<f32> {
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

/// Decode the first audio stream to 16 kHz mono f32 PCM (Whisper's input shape).
#[cfg(feature = "whisper")]
pub fn decode_audio_16k_mono(path: &Path) -> Result<Vec<f32>> {
    decode_audio_mono_f32(path, 16_000)
}

pub(super) fn decode_audio_mono_f32(path: &Path, sample_rate: u32) -> Result<Vec<f32>> {
    let bin = ffmpeg_bin();
    let output = command(&bin)
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
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| launch_err(&bin, e))?;
    if !output.status.success() {
        return Err(Error::Engine(format!(
            "could not decode audio: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// Decode a window of the first audio stream to mono s16le PCM at
/// `sample_rate` — the GUI's Web Audio preview playback. Raw s16le (no
/// container) so the webview can build an `AudioBuffer` directly without
/// codec support; the input-side `-ss` fast-seek keeps a window deep in a
/// long source cheap to extract.
pub fn audio_pcm(path: &Path, start: f64, duration: f64, sample_rate: u32, filters: Option<&str>) -> Result<Vec<u8>> {
    let bin = ffmpeg_bin();
    let mut cmd = command(&bin);
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
    let output = cmd
        .args(["-f", "s16le", "pipe:1"])
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| launch_err(&bin, e))?;
    if !output.status.success() {
        return Err(Error::Engine(format!(
            "could not decode audio: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

// ---- preview proxies -------------------------------------------------------

/// Cap on the proxy's width, in pixels (`scale='min(W,iw)'`). ~720p: small
/// enough that one frame decodes in a few ms, large enough to preview framing.
const PROXY_MAX_WIDTH: u32 = 1280;

/// Cap on the proxy's width for spherical sources. A reframed preview crops
/// roughly a 100° window out of a 360° picture, so only about a quarter of the
/// proxy's width ever reaches the screen: at 1280 that leaves ~355 px of real
/// detail — visible mush. 3072 puts ~850 px across the shot while an all-intra
/// frame still decodes fast enough to scrub.
const PROXY_MAX_WIDTH_SPHERICAL: u32 = 3072;

/// The proxy width to render (and look up) an asset at. 360 footage is preserved
/// at a larger size because reframing throws most of the frame away.
pub fn proxy_width(projection: Option<Projection>) -> u32 {
    if projection.is_some_and(|p| p.is_spherical()) {
        PROXY_MAX_WIDTH_SPHERICAL
    } else {
        PROXY_MAX_WIDTH
    }
}

/// FNV-1a over `s`. A small, dependency-free, deterministic hash for naming a
/// source's proxy file — stability across sessions is what lets a re-import
/// reuse the cached proxy (a non-deterministic hasher would orphan it).
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A content key for `src` that changes whenever the file is replaced: its path
/// plus size and modified time. Hashed into a cache file name so re-imports of
/// the same source reuse the cached artifact, while a swapped-out source
/// regenerates one. Shared by the proxy cache and the Insta360 stitch cache.
fn source_key(src: &Path) -> String {
    let meta = std::fs::metadata(src).ok();
    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}|{len}|{mtime}", src.display())
}

/// The on-disk path of `src`'s preview proxy at `width` (whether or not it
/// exists yet): `<cache>/kerf/proxies/<hash>.mp4`. `None` when no OS cache
/// directory is resolvable (a proxy simply can't be cached — preview falls back
/// to the original).
///
/// The width is part of the key, so an asset that is later marked as 360 (or
/// stops being one) looks up a different file and regenerates instead of
/// silently reusing a proxy rendered at the wrong size.
pub fn proxy_path(src: &Path, width: u32) -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("kerf").join("proxies");
    Some(dir.join(format!("{:016x}.mp4", fnv1a(&format!("{}|{width}", source_key(src))))))
}

/// The proxy for `src` at `width` **if it has already been generated** (the file
/// exists), for a preview path to decode from instead of the original; `None`
/// otherwise. Preview must never block on generation, so this is a pure
/// existence check.
pub fn ready_proxy(src: &Path, width: u32) -> Option<PathBuf> {
    proxy_path(src, width).filter(|p| p.is_file())
}

/// How many CPU threads a single preview-proxy encode may use. Capped to leave
/// at least one core free so the GUI and a working agent stay responsive while a
/// proxy transcodes in the background (an uncapped `libx264` grabs every core).
/// Override with `KERF_PROXY_THREADS` (clamped to >= 1).
fn proxy_threads() -> usize {
    if let Some(n) = std::env::var("KERF_PROXY_THREADS").ok().and_then(|v| v.parse::<usize>().ok()) {
        return n.max(1);
    }
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1))
        .unwrap_or(1)
}

/// Constant-quality flags for encoder `vc` at software-CRF-scale `crf` — the
/// proxy / stitch analogue of the export's `push_video_opts`, spelling the same
/// intent per hardware family (each names its quality knob differently).
fn quality_args(vc: &str, crf: u32) -> Vec<String> {
    let s = |v: &str| v.to_string();
    match enc_family(vc) {
        EncFamily::Software => vec![s("-preset"), s("veryfast"), s("-crf"), crf.to_string()],
        EncFamily::Nvenc => vec![s("-rc"), s("vbr"), s("-cq"), crf.to_string(), s("-b:v"), s("0")],
        EncFamily::Qsv => vec![s("-global_quality"), crf.to_string()],
        EncFamily::VideoToolbox => vec![s("-q:v"), crf_to_vt_quality(crf).to_string()],
        EncFamily::Amf => {
            let qp = crf.to_string();
            vec![s("-rc"), s("cqp"), s("-qp_i"), qp.clone(), s("-qp_p"), qp]
        }
    }
}

/// Encoder input pixel format: the hardware encoders all take `nv12` (some take
/// nothing else), the software path keeps its historical `yuv420p`. Both are
/// 4:2:0, so the decoded proxy looks the same either way.
fn encode_pix_fmt(vc: &str) -> &'static str {
    if enc_family(vc) == EncFamily::Software {
        "yuv420p"
    } else {
        "nv12"
    }
}

/// The hardware H.264 encoder to generate proxies with, when one is available
/// and hardware encoding hasn't been disabled or found broken. H.264 rather
/// than HEVC because every proxy width (≤3072) sits inside even H.264 NVENC's
/// 4096-wide ceiling, and H.264 decodes cheapest — which is the whole point of
/// a scrubbing proxy.
fn proxy_hw_encoder() -> Option<&'static str> {
    if !HW_ENCODE_OK.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    HW_ENCODER_CANDIDATES
        .iter()
        .find(|e| e.starts_with("h264_") && hw_encoders().iter().any(|h| h == *e))
        .copied()
}

/// Build the ffmpeg argument list (pure, unit-tested) that transcodes `src` into
/// an all-intra, audio-less preview proxy at `dst`, capped to `width` pixels
/// across (see [`proxy_width`]) and using at most `threads` CPU threads.
/// `encoder` is `libx264` or a detected hardware encoder (which offloads the
/// whole background transcode to the GPU); `hw_decode` adds an input-side
/// `-hwaccel`. `-g 1` makes every frame a keyframe, so a seek decodes
/// exactly one frame (instant scrub even on long-GOP 4K/HEVC). fps and duration
/// are left untouched — no `-r`, no `-t`, no trim — so a source time maps 1:1
/// onto the proxy and a preview seek lands on the same frame the export (which
/// always reads the original) would.
fn build_proxy_args(src: &str, dst: &str, threads: usize, width: u32, encoder: &str, hw_decode: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec!["-hide_banner".to_string(), "-loglevel".to_string(), "error".to_string(), "-y".to_string()];
    if let Some(hw) = hw_decode {
        args.push("-hwaccel".to_string());
        args.push(hw.to_string());
    }
    args.extend([
        "-i".to_string(),
        src.to_string(),
        "-an".to_string(),
        "-vf".to_string(),
        format!("scale='min({width},iw)':-2:flags=bilinear"),
        "-c:v".to_string(),
        encoder.to_string(),
    ]);
    args.extend(quality_args(encoder, 24));
    args.extend([
        "-g".to_string(),
        "1".to_string(),
        "-threads".to_string(),
        threads.max(1).to_string(),
        "-pix_fmt".to_string(),
        encode_pix_fmt(encoder).to_string(),
        // The encode writes a `.part` temp file, whose extension tells ffmpeg
        // nothing — name the muxer instead of letting it guess, or it exits
        // with "unable to find a suitable output format" before decoding a frame.
        "-f".to_string(),
        "mp4".to_string(),
        dst.to_string(),
    ]);
    args
}

/// Generate the preview proxy for `src` if it isn't cached yet, returning its
/// path. A cache hit (the proxy already on disk) returns immediately — so this
/// is cheap to call for every asset on project open. The encode writes to a
/// per-process temp file and atomically renames it into place, so a partial /
/// interrupted / concurrent encode never leaves a half-written file that
/// [`ready_proxy`] would mistake for a finished proxy. Blocking; callers run it
/// off the project lock (e.g. on a background thread).
pub fn generate_proxy(src: &Path, width: u32) -> Result<PathBuf> {
    let dst =
        proxy_path(src, width).ok_or_else(|| Error::Engine("no cache directory available for preview proxies".to_string()))?;
    if dst.is_file() {
        return Ok(dst);
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Engine(format!("could not create proxy cache dir: {e}")))?;
    }
    let src_str = src
        .to_str()
        .ok_or_else(|| Error::Engine("asset path is not valid UTF-8".to_string()))?;
    let tmp = dst.with_extension(format!("{}.part", std::process::id()));
    let tmp_str = tmp
        .to_str()
        .ok_or_else(|| Error::Engine("proxy temp path is not valid UTF-8".to_string()))?;
    let bin = ffmpeg_bin();
    let run = |encoder: &str, hw_decode: Option<&str>| -> Result<std::process::Output> {
        let args = build_proxy_args(src_str, tmp_str, proxy_threads(), width, encoder, hw_decode);
        command(&bin)
            .args(&args)
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| launch_err(&bin, e))
    };
    // GPU encode (and decode) when available — a background proxy transcode
    // then costs the CPU almost nothing. A failure falls back to the software
    // pipeline once, and a hardware-*encoder* failure whose software retry
    // succeeds disables hardware encodes for the rest of the process.
    let hw_enc = proxy_hw_encoder();
    let hw_dec = decode_hwaccel();
    let mut output = run(hw_enc.unwrap_or("libx264"), hw_dec.as_deref())?;
    if !output.status.success() && (hw_enc.is_some() || hw_dec.is_some()) {
        let _ = std::fs::remove_file(&tmp);
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        output = run("libx264", None)?;
        if output.status.success() {
            if hw_enc.is_some() {
                HW_ENCODE_OK.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            tracing::warn!(error = %err, "hardware-accelerated proxy encode failed; using software");
        }
    }
    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Engine(format!(
            "could not generate preview proxy: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    // Another generator may have finished the same proxy while we encoded; if so
    // ours is redundant — drop the temp and use the existing file.
    if dst.is_file() {
        let _ = std::fs::remove_file(&tmp);
        return Ok(dst);
    }
    std::fs::rename(&tmp, &dst).map_err(|e| Error::Engine(format!("could not finalize preview proxy: {e}")))?;
    Ok(dst)
}

// ---- insta360 dual-lens stitching ------------------------------------------

/// The equirect frame a lens pair is stitched into. Each lens is a square
/// fisheye covering [`STITCH_FOV`] degrees, so a 3072x3072 lens carries about
/// 16 px per degree — roughly 5800 px around the full circle. 5760x2880 is
/// therefore the size that keeps the capture's detail without inventing any
/// (and is what Insta360 Studio itself exports a 5.7K capture at).
const STITCH_WIDTH: u32 = 5760;
const STITCH_HEIGHT: u32 = 2880;

/// Per-lens field of view, in degrees. Insta360's lenses overshoot the
/// hemisphere so the two circles overlap at the seam; `v360` needs the real
/// figure or the halves meet with a gap.
const STITCH_FOV: u32 = 190;

/// The lens token of an Insta360 capture file name and the name of its other
/// lens: a capture is written as a *pair* of files whose second-to-last
/// underscore-separated token is `00` (front) or `10` (rear) —
/// `VID_20220625_140410_00_008.mp4` / `..._10_008.mp4`.
///
/// Matched token-wise rather than by searching for `_00_`, so a capture whose
/// date or time happens to contain those digits can't be misread.
pub(crate) fn insta360_lens(file_name: &str) -> Option<(&'static str, String)> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    if !ext.eq_ignore_ascii_case("mp4") || !stem.starts_with("VID_") {
        return None;
    }
    // `VID_<date>_<time>_<lens>_<sequence>` — anything shorter, or with an empty
    // trailing sequence, is not a capture file whatever its middle tokens say.
    let mut tokens: Vec<&str> = stem.split('_').collect();
    if tokens.len() < 4 || tokens.last().is_none_or(|t| t.is_empty()) {
        return None;
    }
    let lens_at = tokens.len() - 2;
    let (lens, other) = match tokens[lens_at] {
        "00" => ("00", "10"),
        "10" => ("10", "00"),
        _ => return None,
    };
    tokens[lens_at] = other;
    Some((lens, format!("{}.{ext}", tokens.join("_"))))
}

/// The display name for a stitched pair: the capture name with the lens token
/// dropped, so the bin shows one `VID_20220625_140410_008.mp4` rather than
/// whichever lens file happened to be imported.
pub(crate) fn insta360_pair_name(file_name: &str) -> Option<String> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    insta360_lens(file_name)?;
    let mut tokens: Vec<&str> = stem.split('_').collect();
    tokens.remove(tokens.len() - 2);
    Some(format!("{}.{ext}", tokens.join("_")))
}

/// The `(front, rear)` lens files of the Insta360 capture `path` belongs to, if
/// it is one: a square video frame (one fisheye circle per file) whose sibling
/// lens is on disk next to it. Either half resolves to the same canonical pair,
/// so importing either file stitches — and caches — the same sphere.
pub fn insta360_pair(path: &Path, width: Option<u32>, height: Option<u32>) -> Option<(PathBuf, PathBuf)> {
    let (w, h) = (width?, height?);
    if w == 0 || w != h {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    let (lens, sibling_name) = insta360_lens(name)?;
    let sibling = path.with_file_name(sibling_name);
    if !sibling.is_file() {
        return None;
    }
    let this = path.to_path_buf();
    Some(if lens == "00" { (this, sibling) } else { (sibling, this) })
}

/// Cache key for a stitched pair: both lens files' identity plus a version salt,
/// so changing the stitch recipe below invalidates everything stitched by an
/// older build instead of silently reusing it.
fn stitch_key(front: &Path, rear: &Path) -> String {
    format!("{}||{}||v1", source_key(front), source_key(rear))
}

/// Where the stitched equirect for a lens pair lives (whether or not it exists
/// yet): `<cache>/kerf/stitched/<hash>.mp4`. Alongside the proxy cache rather
/// than next to the originals — capture media is routinely on a read-only or
/// ejected SD card, and a deterministic key lets a re-import reuse the stitch
/// instead of spending minutes re-encoding it.
pub fn stitched_path(front: &Path, rear: &Path) -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("kerf").join("stitched");
    Some(dir.join(format!("{:016x}.mp4", fnv1a(&stitch_key(front, rear)))))
}

/// Build the ffmpeg argument list (pure, unit-tested) that stitches the two
/// fisheye lens files into one equirectangular video at `dst`.
///
/// `hstack` packs the pair into the dual-fisheye layout `v360` expects (front
/// left), `roll=180` corrects the sensor orientation — Insta360 records both
/// lenses upside down — and `shortest` resolves the few-frames-different
/// durations the two files are written with. The front file's audio is copied
/// through untouched; its telemetry/subtitle stream is dropped.
///
/// CRF 15 rather than a lossless or visually-lossy setting: this file becomes
/// the effective source for every later reframe and export, so it must not be
/// the quality floor, while `veryfast` keeps a capture's import to minutes.
/// With a hardware `encoder` the same quality intent is mapped per family (see
/// [`quality_args`]) and the multi-minute re-encode moves onto the GPU;
/// `hw_decode` accelerates the two lens decodes the same way.
pub(crate) fn build_stitch_args(front: &str, rear: &str, dst: &str, encoder: &str, hw_decode: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec!["-hide_banner".to_string(), "-loglevel".to_string(), "error".to_string(), "-y".to_string()];
    // `-hwaccel` is an input option: emit it before each lens file's `-i`.
    for lens in [front, rear] {
        if let Some(hw) = hw_decode {
            args.push("-hwaccel".to_string());
            args.push(hw.to_string());
        }
        args.push("-i".to_string());
        args.push(lens.to_string());
    }
    args.extend([
        "-filter_complex".to_string(),
        format!(
            "[0:v][1:v]hstack=shortest=1,v360=dfisheye:e:ih_fov={STITCH_FOV}:iv_fov={STITCH_FOV}:roll=180:w={STITCH_WIDTH}:h={STITCH_HEIGHT}[v]"
        ),
        "-map".to_string(),
        "[v]".to_string(),
        "-map".to_string(),
        "0:a?".to_string(),
        "-c:v".to_string(),
        encoder.to_string(),
    ]);
    args.extend(quality_args(encoder, 15));
    args.extend([
        "-pix_fmt".to_string(),
        encode_pix_fmt(encoder).to_string(),
        "-c:a".to_string(),
        "copy".to_string(),
        "-shortest".to_string(),
        // The encode writes a `.part` temp file, whose extension tells ffmpeg
        // nothing — name the muxer instead of letting it guess.
        "-f".to_string(),
        "mp4".to_string(),
        dst.to_string(),
    ]);
    args
}

/// The hardware encoder for stitching, when available: an **HEVC** one, because
/// the 5760-wide equirect frame exceeds H.264 NVENC's 4096-wide ceiling while
/// every HEVC hardware encoder handles 8K. `None` falls back to libx264.
fn stitch_hw_encoder() -> Option<&'static str> {
    if !HW_ENCODE_OK.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    HW_ENCODER_CANDIDATES
        .iter()
        .find(|e| e.starts_with("hevc_") && hw_encoders().iter().any(|h| h == *e))
        .copied()
}

/// Serializes stitches of the same pair. Importing both lens files at once (the
/// obvious thing to do in a file dialog) would otherwise run the same
/// multi-minute encode twice and throw one away; the loser of the race waits
/// here and then finds the finished file in the cache.
fn stitch_locks() -> &'static Mutex<HashMap<PathBuf, std::sync::Arc<Mutex<()>>>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, std::sync::Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Stitch an Insta360 lens pair into a single equirectangular file, returning
/// its cached path; a pair already stitched returns immediately. `duration_hint`
/// (the capture's length in seconds) scales the `progress` bar. Blocking and
/// slow — a full re-encode — so callers run it off the project lock.
///
/// Like the proxy cache, the encode writes a per-process temp file and renames
/// it into place, so an interrupted or concurrent stitch never leaves a
/// half-written file behind for the next import to mistake for a finished one.
pub fn stitch_insta360(
    front: &Path,
    rear: &Path,
    duration_hint: f64,
    progress: &mut dyn FnMut(ExportProgress),
) -> Result<PathBuf> {
    let dst = stitched_path(front, rear)
        .ok_or_else(|| Error::Engine("no cache directory available for stitched 360 media".to_string()))?;
    if dst.is_file() {
        return Ok(dst);
    }

    let gate = {
        let mut locks = stitch_locks()
            .lock()
            .map_err(|_| Error::Engine("stitch lock poisoned".to_string()))?;
        std::sync::Arc::clone(locks.entry(dst.clone()).or_default())
    };
    let _held = gate.lock().map_err(|_| Error::Engine("stitch lock poisoned".to_string()))?;
    // Another stitch of this pair may have finished while we waited for the gate.
    if dst.is_file() {
        return Ok(dst);
    }

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Engine(format!("could not create stitch cache dir: {e}")))?;
    }
    let (front_str, rear_str) = match (front.to_str(), rear.to_str()) {
        (Some(f), Some(r)) => (f, r),
        _ => return Err(Error::Engine("lens file path is not valid UTF-8".to_string())),
    };
    let tmp = dst.with_extension(format!("{}.part", std::process::id()));
    let tmp_str = tmp
        .to_str()
        .ok_or_else(|| Error::Engine("stitch temp path is not valid UTF-8".to_string()))?;

    tracing::info!(front = %front.display(), rear = %rear.display(), "stitching insta360 lens pair");
    let attempt = |encoder: &str, hw_decode: Option<&str>, progress: &mut dyn FnMut(ExportProgress)| {
        let args = build_stitch_args(front_str, rear_str, tmp_str, encoder, hw_decode);
        run_ffmpeg_progress(
            &args,
            &dst,
            Bar {
                total: duration_hint.max(1e-9),
                offset: 0.0,
                width: 1.0,
                start: std::time::Instant::now(),
            },
            progress,
            &|| false,
        )
    };
    // GPU-encode the stitch when a verified HEVC hardware encoder exists (the
    // full re-encode drops from minutes towards realtime); one failure falls
    // back to the software pipeline and disables hardware encodes.
    let hw_enc = stitch_hw_encoder();
    let hw_dec = decode_hwaccel();
    let mut result = attempt(hw_enc.unwrap_or("libx264"), hw_dec.as_deref(), progress);
    if result.is_err() && (hw_enc.is_some() || hw_dec.is_some()) {
        tracing::warn!(error = ?result.as_ref().err(), "hardware-accelerated stitch failed; retrying in software");
        if hw_enc.is_some() {
            HW_ENCODE_OK.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        let _ = std::fs::remove_file(&tmp);
        result = attempt("libx264", None, progress);
    }
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    if dst.is_file() {
        let _ = std::fs::remove_file(&tmp);
        return Ok(dst);
    }
    std::fs::rename(&tmp, &dst).map_err(|e| Error::Engine(format!("could not finalize stitched 360 media: {e}")))?;
    Ok(dst)
}

// ---- export ----------------------------------------------------------------

/// Output container / muxer. Authoritative over the output path extension; it
/// gates the codec allow-lists, faststart, the gif palette pipeline and whether
/// a video / audio stream is produced at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    #[default]
    Mp4,
    Mov,
    Mkv,
    Webm,
    Gif,
    Mp3,
    M4a,
    Wav,
    Flac,
}

impl Container {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
            Self::Gif => "gif",
            Self::Mp3 => "mp3",
            Self::M4a => "m4a",
            Self::Wav => "wav",
            Self::Flac => "flac",
        }
    }
    /// mp4 / mov / m4a benefit from a front-loaded moov atom; nothing else does.
    pub fn supports_faststart(self) -> bool {
        matches!(self, Self::Mp4 | Self::Mov | Self::M4a)
    }
    /// Audio-only containers never carry a video stream.
    pub fn is_audio_only(self) -> bool {
        matches!(self, Self::Mp3 | Self::M4a | Self::Wav | Self::Flac)
    }
    /// Gif is the only video-only container (no audio stream).
    pub fn is_video_only(self) -> bool {
        matches!(self, Self::Gif)
    }
    pub fn video_codecs(self) -> &'static [&'static str] {
        // Hardware encoders sit alongside the software ones — they emit the same
        // h264 / hevc / av1 bitstreams, so a container accepts a codec's HW
        // variants wherever it accepts the software one.
        match self {
            Self::Mp4 => &[
                "libx264",
                "libx265",
                "libsvtav1",
                "h264_nvenc",
                "hevc_nvenc",
                "av1_nvenc",
                "h264_qsv",
                "hevc_qsv",
                "av1_qsv",
                "h264_videotoolbox",
                "hevc_videotoolbox",
                "h264_amf",
                "hevc_amf",
            ],
            Self::Mov => &[
                "prores_ks",
                "libx264",
                "libx265",
                "h264_nvenc",
                "hevc_nvenc",
                "h264_qsv",
                "hevc_qsv",
                "h264_videotoolbox",
                "hevc_videotoolbox",
                "h264_amf",
                "hevc_amf",
            ],
            Self::Mkv => &[
                "libx264",
                "libx265",
                "libvpx-vp9",
                "libsvtav1",
                "h264_nvenc",
                "hevc_nvenc",
                "av1_nvenc",
                "h264_qsv",
                "hevc_qsv",
                "av1_qsv",
                "h264_videotoolbox",
                "hevc_videotoolbox",
                "h264_amf",
                "hevc_amf",
            ],
            Self::Webm => &["libvpx-vp9", "libsvtav1", "av1_nvenc", "av1_qsv"],
            Self::Gif => &["gif"],
            _ => &[],
        }
    }
    pub fn audio_codecs(self) -> &'static [&'static str] {
        match self {
            Self::Mp4 => &["aac", "alac"],
            Self::Mov => &["aac", "alac", "pcm_s16le", "pcm_s24le"],
            Self::Mkv => &["aac", "libopus", "libmp3lame", "flac", "pcm_s16le"],
            Self::Webm => &["libopus"],
            Self::Mp3 => &["libmp3lame"],
            Self::M4a => &["aac", "alac"],
            Self::Wav => &["pcm_s16le", "pcm_s24le"],
            Self::Flac => &["flac"],
            Self::Gif => &[],
        }
    }
    pub fn video_ok(self, codec: &str) -> bool {
        self.video_codecs().contains(&codec)
    }
    pub fn audio_ok(self, codec: &str) -> bool {
        self.audio_codecs().contains(&codec)
    }
}

/// Which video rate-control branch [`build_export_args`] emits. Ignored for
/// `prores_ks` (driven by the ProRes profile) and `gif` (palette pipeline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RateControl {
    /// Constant quality: `-crf N` (libvpx-vp9 also gets `-b:v 0`). The default.
    #[default]
    Crf,
    /// Single-pass average bitrate: `-b:v X` (+ optional `-maxrate`/`-bufsize`).
    Bitrate,
    /// Two-pass average bitrate (two ffmpeg runs sharing a passlog).
    TwoPass,
    /// Per-codec lossless: x264/x265/svt-av1 `-crf 0`; libvpx-vp9 `-lossless 1`.
    Lossless,
}

pub use crate::model::Fit;


/// Which ffmpeg invocation [`build_export_args`] is emitting for. Injected so
/// the builder stays pure (no knowledge of the platform null device or the temp
/// passlog file — [`render_with`] supplies those).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PassPhase {
    /// One-shot encode (every mode except two-pass).
    #[default]
    Single,
    /// Two-pass analysis pass: `-pass 1`, video-only, discarded output.
    First,
    /// Two-pass final pass: `-pass 2`, real output.
    Second,
}

/// Everything the export menu can drive. `Default` reproduces the original
/// hard-coded behaviour byte-for-byte (no `-c:v`/`-c:a`/`-crf`, no faststart),
/// so the legacy [`render`] path and the existing unit tests are unaffected.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", default)]
pub struct ExportOptions {
    /// Target container / muxer.
    pub container: Container,

    /// `-c:v` value. `None` lets ffmpeg pick the encoder from the container
    /// (legacy behaviour); audio-only containers ignore it.
    pub video_codec: Option<String>,
    /// `-c:a` value. `None` lets ffmpeg pick from the container.
    pub audio_codec: Option<String>,

    /// Video rate-control mode.
    pub rate_control: RateControl,
    /// `-crf N` (Crf / Lossless modes). `None` keeps the encoder default.
    pub crf: Option<u32>,
    /// `-b:v` token, e.g. "8M" / "2500k". Required for Bitrate / TwoPass.
    pub video_bitrate: Option<String>,
    /// `-maxrate` VBV cap (bitrate modes).
    pub max_rate: Option<String>,
    /// `-bufsize` VBV buffer (pairs with `max_rate`).
    pub buf_size: Option<String>,

    /// `-preset` for x264/x265 (named) and svt-av1 (numeric); reinterpreted as
    /// `-cpu-used` for libvpx-vp9.
    pub preset: Option<String>,
    /// ProRes quality `-profile:v 0..5` (prores_ks only).
    pub prores_profile: Option<u8>,
    /// `-tune` (x264 / x265 only).
    pub tune: Option<String>,
    /// `-profile:v` for h264 / hevc (not ProRes).
    pub profile_v: Option<String>,

    /// `-pix_fmt` AND the filtergraph terminal `format=` (dual-write). `None`
    /// keeps the yuv420p path. yuv420p requires even dimensions.
    pub pix_fmt: Option<String>,

    /// `-hwaccel` for input **decode** on export ("auto", "cuda", "vaapi",
    /// "videotoolbox", "qsv", "d3d11va", …). `None` / "none" decodes in software
    /// (the default, byte-for-byte as before). Independent of the encoder: GPU
    /// decode composes with a software encode. Frames are downloaded to system
    /// memory (no `-hwaccel_output_format`) so the CPU filtergraph still runs.
    pub hwaccel: Option<String>,

    /// Output WxH, baked into the filtergraph. Even-clamped.
    pub resolution: Option<(u32, u32)>,
    /// How footage of a different shape is fitted to that frame — letterboxed
    /// (the default) or filled and cropped. Only matters when the two differ.
    pub fit: Fit,
    /// Output fps, baked into the filtergraph; never emits `-r`.
    pub fps: Option<f64>,
    /// `scale=…:flags=` scaler (bicubic / bilinear / lanczos / neighbor / spline).
    pub scaler: Option<String>,
    /// Forced audio sample rate, via the graph `aformat` (not `-ar`).
    pub audio_sample_rate: Option<u32>,
    /// Forced channel count, via the graph `aformat` (not `-ac`).
    pub audio_channels: Option<u16>,

    /// `-b:a` token (lossy codecs only).
    pub audio_bitrate: Option<String>,
    /// `-compression_level` for flac.
    pub flac_compression: Option<u8>,
    /// When false the audio map is dropped and `-an` emitted.
    pub include_audio: bool,

    /// `-movflags +faststart` (mp4 / mov / m4a only).
    pub faststart: bool,
    /// `paletteuse=dither=` for gif.
    pub gif_dither: Option<String>,
    /// gif `-loop 0` (true, infinite) vs `-loop -1` (false, play once).
    pub gif_loop: bool,
    /// `-metadata title=`.
    pub metadata_title: Option<String>,
    /// Render only this timeline span (seconds), e.g. the GUI's in/out marks;
    /// the output starts at `range.start`. `None` renders the whole timeline.
    pub range: Option<crate::model::TimeRange>,
    /// Normalize the final mix to -14 LUFS (single-pass `loudnorm`, the
    /// streaming-platform target) before encoding.
    pub loudnorm: bool,
}

impl Default for ExportOptions {
    // Reproduces the pre-existing argv exactly: no codecs, no crf, no faststart.
    fn default() -> Self {
        Self {
            container: Container::Mp4,
            video_codec: None,
            audio_codec: None,
            rate_control: RateControl::Crf,
            crf: None,
            video_bitrate: None,
            max_rate: None,
            buf_size: None,
            preset: None,
            prores_profile: None,
            tune: None,
            profile_v: None,
            pix_fmt: None,
            hwaccel: None,
            resolution: None,
            fit: Fit::Contain,
            fps: None,
            scaler: None,
            audio_sample_rate: None,
            audio_channels: None,
            audio_bitrate: None,
            flac_compression: None,
            include_audio: true,
            faststart: false,
            gif_dither: None,
            gif_loop: true,
            metadata_title: None,
            range: None,
            loudnorm: false,
        }
    }
}

/// The validated export range from `opts`, clamped to the timeline. `None`
/// when absent or empty after clamping (which falls back to a full export).
fn effective_range(timeline: &Timeline, opts: &ExportOptions) -> Option<(f64, f64)> {
    let r = opts.range?;
    let start = r.start.max(0.0);
    let end = r.end.min(timeline.duration());
    (end - start > 1e-9).then_some((start, end))
}

/// Whether a bitrate token like "8M" / "2500k" / "800000" is well-formed.
fn valid_bitrate(s: &str) -> bool {
    let s = s.trim();
    let digits = match s.char_indices().find(|(_, c)| !(c.is_ascii_digit() || *c == '.')) {
        Some((i, c)) => {
            // The only allowed trailing char is a single k/K/M unit suffix.
            if !matches!(c, 'k' | 'K' | 'm' | 'M') || i + c.len_utf8() != s.len() {
                return false;
            }
            &s[..i]
        }
        None => s,
    };
    !digits.is_empty() && digits.parse::<f64>().map(|v| v > 0.0).unwrap_or(false)
}

/// The `-tune` values each encoder accepts. x265 notably lacks x264's `film` /
/// `stillimage`; feeding an unknown tune makes the encoder fail to initialise.
fn video_tunes(vc: &str) -> &'static [&'static str] {
    match vc {
        "libx264" => &[
            "film",
            "animation",
            "grain",
            "stillimage",
            "zerolatency",
            "fastdecode",
            "psnr",
            "ssim",
        ],
        "libx265" => &["psnr", "ssim", "grain", "zerolatency", "fastdecode", "animation"],
        _ => &[],
    }
}

/// The hardware-encoder family a `-c:v` value belongs to, read from its ffmpeg
/// suffix. Software encoders (libx264 / libx265 / libsvtav1 / libvpx-vp9) and
/// the prores / gif pipelines are [`EncFamily::Software`]. The family decides how
/// the constant-quality knob, VBV caps and speed preset are spelled — each HW
/// encoder names the same intent differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncFamily {
    Software,
    Nvenc,
    Qsv,
    VideoToolbox,
    Amf,
}

fn enc_family(vc: &str) -> EncFamily {
    if vc.ends_with("_nvenc") {
        EncFamily::Nvenc
    } else if vc.ends_with("_qsv") {
        EncFamily::Qsv
    } else if vc.ends_with("_videotoolbox") {
        EncFamily::VideoToolbox
    } else if vc.ends_with("_amf") {
        EncFamily::Amf
    } else {
        EncFamily::Software
    }
}

/// Whether `vc` produces an H.264 bitstream (software or any HW family).
fn is_h264(vc: &str) -> bool {
    vc == "libx264" || vc.starts_with("h264_")
}

/// Whether `vc` produces an HEVC bitstream (software or any HW family) — these
/// need the `hvc1` tag in mp4 / mov so QuickTime / iOS will play them.
fn is_hevc(vc: &str) -> bool {
    vc == "libx265" || vc.starts_with("hevc_")
}

/// Map a 0..51 CRF (lower = better) onto VideoToolbox's 1..100 constant-quality
/// scale (higher = better). VideoToolbox has no CRF; this preserves the intent
/// so the same `crf` field drives every encoder.
fn crf_to_vt_quality(crf: u32) -> u32 {
    let crf = crf.min(51) as f64;
    (((1.0 - crf / 51.0) * 100.0).round() as u32).clamp(1, 100)
}

/// Validate an option set against the timeline's available streams, returning a
/// list of human-readable problems (empty = OK). Pure; called by the pre-launch
/// guard in [`render_with`] and mirrored client-side by the export dialog.
pub fn validate_export(opts: &ExportOptions, has_video: bool, has_audio: bool) -> Vec<String> {
    let mut issues = Vec::new();
    let c = opts.container;
    let want_video = has_video && !c.is_audio_only();
    let want_audio = has_audio && !c.is_video_only() && opts.include_audio;

    if c.is_audio_only() && !has_audio {
        issues.push(format!(
            "{} is audio-only, but the timeline has no audio.",
            c.ext().to_uppercase()
        ));
    }
    if c.is_video_only() && !has_video {
        issues.push("GIF export needs video, but the timeline has no video.".to_string());
    }
    if !want_video && !want_audio {
        issues.push("These settings would export nothing.".to_string());
    }
    if want_video {
        if let Some(vc) = opts.video_codec.as_deref() {
            if !c.video_ok(vc) {
                issues.push(format!("{vc} can't go in a .{} file.", c.ext()));
            }
        }
        let rate_mode = !matches!(opts.video_codec.as_deref(), Some("prores_ks") | Some("gif"));
        if rate_mode && matches!(opts.rate_control, RateControl::Bitrate | RateControl::TwoPass) && opts.video_bitrate.is_none() {
            issues.push("A target video bitrate is required for bitrate / two-pass.".to_string());
        }
        if let Some(vc) = opts.video_codec.as_deref() {
            if matches!(opts.rate_control, RateControl::TwoPass) && enc_family(vc) != EncFamily::Software {
                issues.push(format!(
                    "Two-pass encoding isn't supported for hardware encoder {vc}; use crf or bitrate."
                ));
            }
        }
        if let (Some(vc), Some(t)) = (opts.video_codec.as_deref(), opts.tune.as_deref()) {
            if matches!(vc, "libx264" | "libx265") && !t.is_empty() && !video_tunes(vc).contains(&t) {
                issues.push(format!("tune \"{t}\" is not valid for {vc}."));
            }
        }
    }
    if let Some(b) = opts.video_bitrate.as_deref() {
        if !valid_bitrate(b) {
            issues.push(format!("Invalid video bitrate \"{b}\"."));
        }
    }
    for (label, v) in [("max rate", &opts.max_rate), ("buffer size", &opts.buf_size)] {
        if let Some(b) = v.as_deref() {
            if !valid_bitrate(b) {
                issues.push(format!("Invalid {label} \"{b}\"."));
            }
        }
    }
    if want_audio {
        if let Some(ac) = opts.audio_codec.as_deref() {
            if !c.audio_ok(ac) {
                issues.push(format!("{ac} can't go in a .{} file.", c.ext()));
            }
        }
        if let Some(b) = opts.audio_bitrate.as_deref() {
            if !valid_bitrate(b) {
                issues.push(format!("Invalid audio bitrate \"{b}\"."));
            }
        }
    }
    issues
}

/// The single output shape every clip is normalized to before `concat`. The
/// `concat` filter requires identical resolution / frame rate / sample format
/// across its inputs, and `concat`'s `a=1` requires every segment to carry
/// audio — so clips from a video-only asset get synthesized silence.
#[derive(Debug, Clone)]
struct ExportFormat {
    width: u32,
    height: u32,
    fps: f64,
    sample_rate: u32,
    channels: u16,
    /// Terminal pixel format: argv `-pix_fmt` and the filtergraph terminal
    /// `format=` are kept in sync through this single field.
    pix_fmt: String,
    /// Optional `scale=…:flags=` scaler.
    scaler: Option<String>,
    /// Letterbox or fill-and-crop when the footage and the frame differ in shape.
    fit: Fit,
}

impl Default for ExportFormat {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30.0,
            sample_rate: 48_000,
            channels: 2,
            pix_fmt: "yuv420p".to_string(),
            scaler: None,
            fit: Fit::Contain,
        }
    }
}

impl ExportFormat {
    fn channel_layout(&self) -> &'static str {
        if self.channels <= 1 {
            "mono"
        } else {
            "stereo"
        }
    }
    /// The `:flags=…` suffix to append to a `scale` filter, or empty.
    fn scale_flags(&self) -> String {
        match &self.scaler {
            Some(s) => format!(":flags={s}"),
            None => String::new(),
        }
    }
}

/// Derive the output shape from the first clip (across all tracks) that carries
/// a video stream and the first that carries audio, falling back to 1080p30
/// stereo defaults. When `opts` carries resolution or fps overrides those win.
fn export_format(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> ExportFormat {
    let stream_of = |clip: &crate::model::Clip, kind: StreamKind| {
        assets
            .iter()
            .find(|a| a.id == clip.asset_id)
            .and_then(|a| a.streams.iter().find(|s| s.kind == kind))
    };
    let clips = || timeline.tracks.iter().flat_map(|t| t.clips.iter());

    let mut fmt = ExportFormat::default();
    if let Some((clip, v)) = clips().find_map(|c| stream_of(c, StreamKind::Video).map(|s| (c, s))) {
        // A reframed clip's source dimensions describe the *sphere*, not the
        // deliverable — inheriting them would export a 16:9 reframe of a 5.7K
        // Insta360 capture at 5760x2880. Keep the 1080p default instead and let
        // `opts.resolution` override as usual. Keyed off the reframe rather than
        // the projection, so an un-reframed 360 clip still behaves as before.
        let reframes_to_flat = clip
            .reframe
            .as_ref()
            .is_some_and(|r| r.output == crate::model::Projection::Flat);
        if let (false, Some(w), Some(h)) = (reframes_to_flat, v.width, v.height) {
            fmt.width = w;
            fmt.height = h;
        }
        if let Some(f) = v.fps.filter(|f| *f > 0.0) {
            fmt.fps = f;
        }
    }
    if let Some(a) = clips().find_map(|c| stream_of(c, StreamKind::Audio)) {
        if let Some(r) = a.sample_rate.filter(|r| *r > 0) {
            fmt.sample_rate = r;
        }
        if let Some(c) = a.channels.filter(|c| *c > 0) {
            fmt.channels = c;
        }
    }
    // The project's delivery frame sits between the footage and an explicit
    // export resolution: it overrides "whatever the first clip is" so the
    // preview, the still and the export agree on the shape being cut for, and
    // yields to a resolution typed into the export dialog so a one-off render
    // at a different size still works.
    if let Some(d) = timeline.format {
        fmt.width = d.width;
        fmt.height = d.height;
    }
    if let Some((w, h)) = opts.resolution {
        fmt.width = w;
        fmt.height = h;
    }
    if let Some(f) = opts.fps.filter(|f| *f > 0.0) {
        fmt.fps = f;
    }
    if let Some(r) = opts.audio_sample_rate.filter(|r| *r > 0) {
        fmt.sample_rate = r;
    }
    if let Some(c) = opts.audio_channels.filter(|c| *c > 0) {
        fmt.channels = c;
    }
    // libopus only encodes at 48 kHz; force it regardless of source / override.
    if opts.audio_codec.as_deref() == Some("libopus") {
        fmt.sample_rate = 48_000;
    }
    // yuv420p (and most 4:2:0 formats) require even dimensions, so clamp both
    // source-derived and custom sizes before they reach `scale=`/`color=s=`.
    fmt.width = (fmt.width & !1).max(2);
    fmt.height = (fmt.height & !1).max(2);
    if let Some(pf) = opts.pix_fmt.clone() {
        fmt.pix_fmt = pf;
    } else if opts.video_codec.as_deref() == Some("prores_ks") {
        // ProRes cannot encode 4:2:0; default a None pix_fmt to 10-bit 4:2:2 (or
        // 4:4:4 for the 4444 profiles) so the graph terminal doesn't silently
        // decimate a 10-bit / 4:2:2 source to 8-bit 4:2:0 before the encode.
        fmt.pix_fmt = if matches!(opts.prores_profile, Some(4) | Some(5)) {
            "yuva444p10le".to_string()
        } else {
            "yuv422p10le".to_string()
        };
    }
    fmt.scaler = opts.scaler.clone();
    // Same precedence for the fit: a Cover delivery crops in the preview exactly
    // as it will on export, but an explicit non-default `opts.fit` still wins.
    fmt.fit = match (timeline.format, opts.fit) {
        (Some(d), Fit::Contain) => d.fit,
        (_, f) => f,
    };
    fmt
}

/// Build the complete argument list for `ffmpeg` (everything after the binary
/// name) that renders the whole `timeline` to `output_path` with `opts`.
///
/// One input (`-i`) is added per clip, in track-then-clip order; the filtergraph
/// (see [`build_filter_complex`]) references those inputs by the same index. The
/// `[outv]` / `[outa]` maps — and the codec / rate-control / muxer flags that
/// follow them — are emitted only for the streams the chosen container actually
/// carries, kept in lockstep with the graph so no produced pad is left unmapped.
///
/// The function is pure — it performs no I/O and does not spawn ffmpeg —
/// which makes it unit-testable without the binary being present. The actual
/// render call feeds the returned `Vec<String>` straight to `Command::args`.
pub fn build_export_args(timeline: &Timeline, assets: &[Asset], output_path: &str, opts: &ExportOptions) -> Result<Vec<String>> {
    build_export_args_phase(timeline, assets, output_path, opts, PassPhase::Single, "", "")
}

/// Maps each timeline clip (in track-then-storage order, the flat indexing the
/// graph and `fx` already use) onto an ffmpeg input, **deduplicating** clips that
/// would emit byte-identical `-i` arguments — the same asset fast-seeked to the
/// same point — so a source decoded for several clips (e.g. composited on two
/// tracks, or a duplicated clip) is opened and decoded **once** and fanned out
/// with `split`. Clips with *different* seeks deliberately stay separate: the
/// per-input `-ss` already decodes only each one's kept region, and collapsing
/// them would force decoding the whole span between the first and last cut. Still
/// images are never shared (their input encodes a per-clip `-t` window).
struct InputPlan {
    /// The representative clip flat-index for each unique input, in `-i` order.
    representatives: Vec<usize>,
    /// Per clip flat-index → its unique input index (position in `representatives`).
    clip_input: Vec<usize>,
}

fn plan_inputs(timeline: &Timeline, assets: &[Asset], fx: &[ClipFx]) -> InputPlan {
    let image_of = |id| assets.iter().find(|a| a.id == id).is_some_and(|a| a.is_image());
    let mut representatives: Vec<usize> = Vec::new();
    let mut clip_input: Vec<usize> = Vec::new();
    let mut seen: std::collections::HashMap<(uuid::Uuid, String), usize> = std::collections::HashMap::new();
    for (flat, clip) in timeline.tracks.iter().flat_map(|t| t.clips.iter()).enumerate() {
        let input = if image_of(clip.asset_id) {
            let i = representatives.len();
            representatives.push(flat);
            i
        } else {
            let (start, _) = clip_source_window(clip, &fx[flat]);
            // Key on the exact emitted seek string: two clips share an input iff
            // their `-i` arguments (path is implied by asset_id) are identical.
            let key = (clip.asset_id, format!("{}", clip_seek(start)));
            *seen.entry(key).or_insert_with(|| {
                let i = representatives.len();
                representatives.push(flat);
                i
            })
        };
        clip_input.push(input);
    }
    InputPlan {
        representatives,
        clip_input,
    }
}

/// Push the `-i` inputs for every clip in `timeline` and return the plan the
/// filtergraph indexes by.
///
/// Extracted so export and the live preview stream ([`build_preview_args`])
/// decode their sources *identically* — same deduplication, same per-input
/// fast-seek, same still-image looping — and only differ at the two ends of the
/// pipeline (which files go in, what muxer comes out).
fn push_inputs(
    timeline: &Timeline,
    assets: &[Asset],
    fmt: &ExportFormat,
    opts: &ExportOptions,
    args: &mut Vec<String>,
) -> Result<InputPlan> {
    let path_of = |id: uuid::Uuid| assets.iter().find(|a| a.id == id).map(|a| a.path.as_str());
    let image_of = |id: uuid::Uuid| assets.iter().find(|a| a.id == id).is_some_and(|a| a.is_image());
    let fx = transition_fx(timeline, assets);
    // Deduplicate inputs: clips whose `-i` args are identical (same asset, same
    // fast-seek) share one decoded input, fanned out in the graph with `split`.
    let plan = plan_inputs(timeline, assets, &fx);
    let clips: Vec<&crate::model::Clip> = timeline.tracks.iter().flat_map(|t| t.clips.iter()).collect();
    for &rep in &plan.representatives {
        let clip = clips[rep];
        let path = path_of(clip.asset_id).ok_or(Error::AssetNotFound(clip.asset_id))?;
        let (start, end) = clip_source_window(clip, &fx[rep]);
        if image_of(clip.asset_id) {
            // A still has no timeline of its own: loop the single frame and read it
            // for the clip's whole source window. The in-graph trim (with the seek
            // forced to 0 for images) then carves the clip's duration out of it.
            // No `-ss` — seeking into a one-frame input decodes nothing.
            args.push("-loop".to_string());
            args.push("1".to_string());
            args.push("-framerate".to_string());
            args.push(format!("{}", fmt.fps));
            args.push("-t".to_string());
            args.push(format!("{}", end.max(1.0 / fmt.fps)));
        } else {
            // Hardware-accelerated decode for this input when requested. `-hwaccel`
            // is an input option (applies to the next `-i`), so it's emitted
            // per-input and only for real media — a still gains nothing. Frames
            // are downloaded to system memory (no `-hwaccel_output_format`), so the
            // per-input `-ss` fast-seek and the CPU filtergraph still work.
            if let Some(hw) = opts
                .hwaccel
                .as_deref()
                .filter(|h| !h.is_empty() && !h.eq_ignore_ascii_case("none"))
            {
                args.push("-hwaccel".to_string());
                args.push(hw.to_string());
            }
            let seek = clip_seek(start);
            if seek > 0.0 {
                args.push("-ss".to_string());
                args.push(format!("{seek}"));
            }
        }
        args.push("-i".to_string());
        args.push(path.to_string());
    }
    Ok(plan)
}

/// [`build_export_args`] parameterised by the two-pass [`PassPhase`]. `null_sink`
/// is the platform null device (`/dev/null` / `NUL`) used as the first-pass
/// output, and `passlog` is the shared `-passlogfile` prefix — both injected by
/// [`render_with`] so this builder stays pure. Single-pass callers pass
/// `(Single, "", "")`.
fn build_export_args_phase(
    timeline: &Timeline,
    assets: &[Asset],
    output_path: &str,
    opts: &ExportOptions,
    pass: PassPhase,
    null_sink: &str,
    passlog: &str,
) -> Result<Vec<String>> {
    // Drop muted / solo-shadowed tracks and disabled clips up front, so the rest
    // of the builder never has to reason about them.
    let rendered = timeline.for_render();
    let timeline = &rendered;

    // Range export: build the graph against the sliced sub-timeline, so trims,
    // fades, keyframes and overlays all see the same shifted geometry.
    let sliced;
    let timeline = match effective_range(timeline, opts) {
        Some((s, e)) => {
            sliced = timeline.slice(s, e);
            &sliced
        }
        None => timeline,
    };

    // Stream gating: decide what the graph emits and what we `-map`, in lockstep.
    let timeline_has_video = timeline
        .tracks
        .iter()
        .any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    let timeline_has_audio = timeline.tracks.iter().flat_map(|t| t.clips.iter()).any(|c| {
        assets
            .iter()
            .find(|a| a.id == c.asset_id)
            .is_some_and(|a| a.streams.iter().any(|s| s.kind == StreamKind::Audio))
    });
    let c = opts.container;
    let want_video = timeline_has_video && !c.is_audio_only();
    let want_audio = timeline_has_audio && !c.is_video_only() && opts.include_audio && pass != PassPhase::First;

    // `-hide_banner -nostats` keep the captured stderr to genuine warnings/errors
    // (matching the probe/frame calls); without `-nostats` the per-frame progress
    // lines would accumulate unbounded in memory for a long export.
    let mut args: Vec<String> = vec!["-y".to_string(), "-hide_banner".to_string(), "-nostats".to_string()];
    // Per-input fast-seek: an input-side `-ss` to each clip's source-window start
    // so ffmpeg decodes only the kept region instead of everything from t=0 — a
    // 20-subclip cut from a 1h source no longer decodes the hour 20 times over.
    // `-ss` before `-i` is keyframe-accurate-seek (decode+discard up to the point)
    // and resets timestamps to ~0, so the in-graph trim is expressed relative to
    // this same seek (see `video_clip_chain` / `audio_clip_chain`) and the two
    // stay frame-accurate. Inputs are added in storage order, matching how
    // `build_filter_complex` indexes them and how `fx` is indexed.
    let fmt = export_format(timeline, assets, opts);
    let plan = push_inputs(timeline, assets, &fmt, opts, &mut args)?;

    let total = timeline.duration();
    let graph = build_filter_complex(timeline, assets, &fmt, total, opts, want_video, want_audio, &plan);
    args.push("-filter_complex".to_string());
    args.push(graph.filter);
    if graph.has_video {
        args.push("-map".to_string());
        args.push("[outv]".to_string());
    }
    if graph.has_audio {
        args.push("-map".to_string());
        args.push("[outa]".to_string());
    }

    // ---- video output options (only when a codec is explicitly chosen; a bare
    // default still maps [outv] and lets ffmpeg pick the encoder, as before) ----
    if graph.has_video {
        if let Some(vc) = opts.video_codec.as_deref() {
            args.push("-c:v".to_string());
            args.push(vc.to_string());
            push_video_opts(&mut args, opts, vc, pass, passlog);
            // `-pix_fmt` must equal the graph terminal `format=`; gif is pal8.
            if vc != "gif" {
                args.push("-pix_fmt".to_string());
                args.push(fmt.pix_fmt.clone());
            }
        }
    }

    // ---- audio output options ----
    if graph.has_audio {
        if let Some(ac) = opts.audio_codec.as_deref() {
            args.push("-c:a".to_string());
            args.push(ac.to_string());
            match ac {
                "aac" | "libmp3lame" | "libopus" => {
                    if let Some(b) = &opts.audio_bitrate {
                        args.push("-b:a".to_string());
                        args.push(b.clone());
                    }
                }
                "flac" => {
                    if let Some(lvl) = opts.flac_compression {
                        args.push("-compression_level".to_string());
                        args.push(lvl.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    // Explicit mute: the timeline has audio but the user dropped it (distinct
    // from a timeline that simply has no audio).
    if timeline_has_audio && !want_audio && pass != PassPhase::First && !c.is_video_only() {
        args.push("-an".to_string());
    }

    // ---- muxer / misc (skipped on the two-pass analysis pass, whose output is
    // the null muxer — it rejects mov/gif muxer options like -movflags) ----
    if pass != PassPhase::First {
        if opts.faststart && c.supports_faststart() {
            args.push("-movflags".to_string());
            args.push("+faststart".to_string());
        }
        if c == Container::Gif {
            args.push("-loop".to_string());
            args.push(if opts.gif_loop { "0" } else { "-1" }.to_string());
        }
        if let Some(title) = opts.metadata_title.as_deref().filter(|t| !t.is_empty()) {
            // One argv token via Command::args — no shell quoting; spaces/= are safe.
            args.push("-metadata".to_string());
            args.push(format!("title={title}"));
        }
    }

    if pass == PassPhase::First {
        args.push("-an".to_string());
        args.push("-f".to_string());
        args.push("null".to_string());
        args.push(null_sink.to_string());
    } else {
        args.push(output_path.to_string());
    }
    Ok(args)
}

/// Append the `-c:v`-private options for `vc`: rate control, speed preset,
/// tune / profile and the HEVC `hvc1` tag. Must run after `-c:v` is pushed or
/// ffmpeg silently drops these.
fn push_video_opts(args: &mut Vec<String>, opts: &ExportOptions, vc: &str, pass: PassPhase, passlog: &str) {
    // ProRes and gif drive quality elsewhere (profile / palette), not rate control.
    if vc == "prores_ks" {
        args.push("-profile:v".to_string());
        args.push(opts.prores_profile.unwrap_or(3).to_string());
        return;
    }
    if vc == "gif" {
        return;
    }

    let fam = enc_family(vc);

    // ---- rate control / quality (spelled per encoder family) ----
    match opts.rate_control {
        RateControl::Crf => match fam {
            EncFamily::Software => {
                if let Some(n) = opts.crf {
                    args.push("-crf".to_string());
                    args.push(n.to_string());
                }
                // VP9 constant-quality requires -crf paired with -b:v 0.
                if vc == "libvpx-vp9" {
                    args.push("-b:v".to_string());
                    args.push("0".to_string());
                }
            }
            // NVENC: VBR steered by a constant-quality target (`-cq`), with no
            // average-bitrate target so quality (not size) drives the encode.
            EncFamily::Nvenc => {
                args.push("-rc".to_string());
                args.push("vbr".to_string());
                if let Some(n) = opts.crf {
                    args.push("-cq".to_string());
                    args.push(n.to_string());
                }
                args.push("-b:v".to_string());
                args.push("0".to_string());
            }
            // QSV: `-global_quality` is its CRF analogue (ICQ mode).
            EncFamily::Qsv => {
                if let Some(n) = opts.crf {
                    args.push("-global_quality".to_string());
                    args.push(n.to_string());
                }
            }
            // VideoToolbox has no CRF — map onto its 1..100 quality scale.
            EncFamily::VideoToolbox => {
                if let Some(n) = opts.crf {
                    args.push("-q:v".to_string());
                    args.push(crf_to_vt_quality(n).to_string());
                }
            }
            // AMF: constant QP.
            EncFamily::Amf => {
                args.push("-rc".to_string());
                args.push("cqp".to_string());
                if let Some(n) = opts.crf {
                    let qp = n.to_string();
                    args.push("-qp_i".to_string());
                    args.push(qp.clone());
                    args.push("-qp_p".to_string());
                    args.push(qp);
                }
            }
        },
        RateControl::Bitrate => {
            if let Some(b) = &opts.video_bitrate {
                args.push("-b:v".to_string());
                args.push(b.clone());
            }
            // VBV caps: x264 / x265 and NVENC honour -maxrate/-bufsize; the other
            // HW families ignore or reject them, so only emit where they apply.
            if matches!(fam, EncFamily::Software | EncFamily::Nvenc) {
                if let Some(m) = &opts.max_rate {
                    args.push("-maxrate".to_string());
                    args.push(m.clone());
                }
                if let Some(b) = &opts.buf_size {
                    args.push("-bufsize".to_string());
                    args.push(b.clone());
                }
            }
        }
        // Two-pass is gated to software encoders (validate_export rejects it for
        // HW families, whose multi-pass uses different flags).
        RateControl::TwoPass => {
            if let Some(b) = &opts.video_bitrate {
                args.push("-b:v".to_string());
                args.push(b.clone());
            }
            args.push("-pass".to_string());
            args.push(if pass == PassPhase::First { "1" } else { "2" }.to_string());
            if !passlog.is_empty() {
                args.push("-passlogfile".to_string());
                args.push(passlog.to_string());
            }
        }
        RateControl::Lossless => match fam {
            EncFamily::Software => match vc {
                "libx264" | "libx265" | "libsvtav1" => {
                    args.push("-crf".to_string());
                    args.push("0".to_string());
                }
                "libvpx-vp9" => {
                    args.push("-lossless".to_string());
                    args.push("1".to_string());
                }
                _ => {}
            },
            // The extreme of each HW family's quality knob (visually lossless,
            // not necessarily bit-exact).
            EncFamily::Nvenc => {
                args.push("-rc".to_string());
                args.push("constqp".to_string());
                args.push("-qp".to_string());
                args.push("0".to_string());
            }
            EncFamily::Qsv => {
                args.push("-global_quality".to_string());
                args.push("1".to_string());
            }
            EncFamily::VideoToolbox => {
                args.push("-q:v".to_string());
                args.push("100".to_string());
            }
            EncFamily::Amf => {
                args.push("-rc".to_string());
                args.push("cqp".to_string());
                args.push("-qp_i".to_string());
                args.push("0".to_string());
                args.push("-qp_p".to_string());
                args.push("0".to_string());
            }
        },
    }

    // ---- speed preset ----
    match fam {
        EncFamily::Software => match vc {
            // Named for x264 / x265 / svt-av1, -cpu-used for libvpx-vp9.
            "libx264" | "libx265" | "libsvtav1" => {
                if let Some(p) = &opts.preset {
                    args.push("-preset".to_string());
                    args.push(p.clone());
                }
            }
            "libvpx-vp9" => {
                args.push("-cpu-used".to_string());
                args.push(opts.preset.clone().unwrap_or_else(|| "4".to_string()));
                args.push("-deadline".to_string());
                args.push("good".to_string());
                args.push("-row-mt".to_string());
                args.push("1".to_string());
            }
            _ => {}
        },
        // NVENC (p1..p7 / named) and QSV (veryfast..veryslow) take `-preset`.
        EncFamily::Nvenc | EncFamily::Qsv => {
            if let Some(p) = &opts.preset {
                args.push("-preset".to_string());
                args.push(p.clone());
            }
        }
        // VideoToolbox / AMF have no `-preset` knob in this shape.
        EncFamily::VideoToolbox | EncFamily::Amf => {}
    }

    // -tune: software x264 / x265 only. Emit only a tune the encoder accepts
    // (x265 lacks film/stillimage) so a stale value never fails encoder open.
    if matches!(vc, "libx264" | "libx265") {
        if let Some(t) = opts.tune.as_deref().filter(|t| video_tunes(vc).contains(t)) {
            args.push("-tune".to_string());
            args.push(t.to_string());
        }
    }
    // -profile:v applies to every h264 / hevc encoder (software and hardware).
    if is_h264(vc) || is_hevc(vc) {
        if let Some(p) = &opts.profile_v {
            args.push("-profile:v".to_string());
            args.push(p.clone());
        }
    }
    // HEVC in mp4/mov needs the hvc1 tag or QuickTime / iOS refuse to play it.
    if is_hevc(vc) && matches!(opts.container, Container::Mp4 | Container::Mov) {
        args.push("-tag:v".to_string());
        args.push("hvc1".to_string());
    }
}

// ---- live preview streaming -------------------------------------------------

/// One composited frame of the live preview.
pub struct PreviewFrame {
    /// The timeline time this frame shows, in seconds.
    pub time: f64,
    /// The frame as JPEG bytes.
    pub jpeg: Vec<u8>,
}

/// Cap on the live preview's width. Playback has to composite, encode and ship a
/// frame every ~40 ms, so it renders smaller than the drill-in still does — the
/// preview pane is well under this on a normal window anyway.
const PREVIEW_STREAM_WIDTH: u32 = 960;

/// JPEG quality (`-q:v`) for streamed frames: 2 is best, 31 worst. 6 is visually
/// clean while keeping a 960px frame near 40 KB, so 30 fps costs ~1.2 MB/s over
/// the IPC channel instead of the ~4 MB/s that q=2 would.
const PREVIEW_STREAM_QUALITY: u8 = 6;

/// The preview's render size: the export geometry scaled down to at most
/// `max_width`, keeping the aspect and staying even (yuv420 needs it).
fn preview_resolution(timeline: &Timeline, assets: &[Asset], max_width: u32) -> (u32, u32) {
    let natural = export_format(timeline, assets, &ExportOptions::default());
    let even = |v: u32| v.max(2) & !1;
    if natural.width <= max_width {
        return (even(natural.width), even(natural.height));
    }
    let scale = max_width as f64 / natural.width.max(1) as f64;
    (even(max_width), even((natural.height as f64 * scale).round() as u32))
}

/// Build the ffmpeg argument list that renders the timeline from `start` onwards
/// as a stream of JPEG frames on stdout. Pure, so the graph can be unit-tested.
///
/// Playback composites through **the same filtergraph the export builds** — from
/// a `Timeline::slice` starting at the playhead — so what plays is what renders:
/// every track, effect, transform, keyframe and overlay, not just the raw clip
/// under the playhead. Only the two ends differ from an export: proxy sources go
/// in (the caller substitutes those paths) and MJPEG comes out on a pipe.
fn build_preview_args(
    timeline: &Timeline,
    assets: &[Asset],
    start: f64,
    fps: f64,
    max_width: u32,
    quality: u8,
) -> Result<Vec<String>> {
    // Same gate as the export: muted / solo-shadowed tracks and disabled clips
    // never reach the graph, so what plays is what would render.
    let timeline = &timeline.for_render();
    let end = timeline.duration();
    // `is_finite` first so a NaN playhead is rejected here rather than becoming a
    // nonsensical `-ss` argument.
    if !start.is_finite() || start >= end {
        return Err(Error::Engine("nothing to play past the playhead".to_string()));
    }
    let sliced = timeline.slice(start, end);
    let has_video = sliced
        .tracks
        .iter()
        .any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    if !has_video {
        return Err(Error::Engine("no video on the timeline from here".to_string()));
    }

    let opts = ExportOptions {
        include_audio: false,
        fps: Some(fps),
        resolution: Some(preview_resolution(&sliced, assets, max_width)),
        // mjpeg is a full-range JPEG codec: matching the graph's terminal format
        // to it keeps ffmpeg from inserting a range conversion per frame.
        pix_fmt: Some("yuvj420p".to_string()),
        // Bilinear over the export's default: at preview size the difference is
        // invisible and the scaler runs on every frame of every clip.
        scaler: Some("bilinear".to_string()),
        hwaccel: decode_hwaccel(),
        ..ExportOptions::default()
    };
    let fmt = export_format(&sliced, assets, &opts);

    let mut args: Vec<String> = ["-hide_banner", "-nostats", "-nostdin", "-loglevel", "error"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let plan = push_inputs(&sliced, assets, &fmt, &opts, &mut args)?;
    let graph = build_filter_complex(&sliced, assets, &fmt, sliced.duration(), &opts, true, false, &plan);
    args.push("-filter_complex".to_string());
    args.push(graph.filter);
    args.push("-map".to_string());
    args.push("[outv]".to_string());
    args.push("-an".to_string());
    args.push("-c:v".to_string());
    args.push("mjpeg".to_string());
    args.push("-q:v".to_string());
    args.push(quality.clamp(2, 31).to_string());
    args.push("-f".to_string());
    args.push("image2pipe".to_string());
    args.push("pipe:1".to_string());
    Ok(args)
}

/// Find the first complete JPEG in `buf` — its `FFD8` start-of-image through the
/// byte after its `FFD9` end-of-image — or `None` while one is still arriving.
///
/// Scanning for the markers is safe here: inside JPEG entropy-coded data every
/// `FF` byte is followed by `00` (byte stuffing) or a restart marker
/// (`FFD0`..`FFD7`), so `FFD9` only ever appears as the real end of image — and
/// ffmpeg's mjpeg encoder writes no embedded thumbnail that could nest one.
fn next_jpeg(buf: &[u8]) -> Option<(usize, usize)> {
    let start = buf.windows(2).position(|w| w == [0xFF, 0xD8])?;
    let end = buf[start + 2..].windows(2).position(|w| w == [0xFF, 0xD9])?;
    Some((start, start + 2 + end + 2))
}

/// Play the timeline from `start`, calling `on_frame` with each composited frame
/// in turn; returning `false` from it stops playback and kills ffmpeg.
///
/// This is the difference between the preview being a slideshow and being video.
/// Frames used to come one `ffmpeg` process at a time — spawn, seek, decode,
/// exit, repeat — which caps out well below frame rate however fast the machine
/// is. One long-lived process decoding sequentially instead amortizes all of
/// that, and the all-intra proxies keep each decode cheap.
///
/// Frames are paced to `fps` against the wall clock rather than pushed as fast
/// as they render, which keeps the pipe (and so ffmpeg itself) throttled to real
/// time instead of racing ahead and buffering the whole timeline. Each frame
/// carries its timeline time so a caller following the audio clock can drop one
/// that arrived too late to be worth showing.
pub fn stream_preview(
    timeline: &Timeline,
    assets: &[Asset],
    start: f64,
    fps: f64,
    on_frame: &mut dyn FnMut(PreviewFrame) -> bool,
) -> Result<()> {
    use std::io::Read;

    let fps = fps.clamp(1.0, 60.0);
    let mut args = build_preview_args(timeline, assets, start, fps, PREVIEW_STREAM_WIDTH, PREVIEW_STREAM_QUALITY)?;
    // The composited graph outgrows argv just as the export's does.
    let _script = externalize_filter_complex(&mut args, "preview")?;

    let bin = ffmpeg_bin();
    tracing::debug!(start, fps, "starting preview stream");
    let mut child = command(&bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;

    // Drain stderr on a side thread so a warning flood can't deadlock the frame
    // read, keeping only the tail for a failure message.
    let stderr = child.stderr.take().expect("stderr piped");
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::BufReader::new(stderr).read_to_string(&mut buf);
        buf
    });

    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut index: u64 = 0;
    let mut origin: Option<std::time::Instant> = None;
    let mut stopped = false;

    'read: loop {
        while let Some((s, e)) = next_jpeg(&buf) {
            let jpeg = buf[s..e].to_vec();
            buf.drain(..e);
            // Anchor the clock to the *first* frame: the graph takes a moment to
            // set up, and pacing from before that would make every later frame
            // look overdue and play the whole stream at a sprint.
            let t0 = *origin.get_or_insert_with(std::time::Instant::now);
            let due = std::time::Duration::from_secs_f64(index as f64 / fps);
            if let Some(wait) = due.checked_sub(t0.elapsed()) {
                std::thread::sleep(wait);
            }
            let frame = PreviewFrame {
                time: start + index as f64 / fps,
                jpeg,
            };
            index += 1;
            if !on_frame(frame) {
                stopped = true;
                break 'read;
            }
        }
        let n = stdout
            .read(&mut chunk)
            .map_err(|e| Error::Engine(format!("preview stream read failed: {e}")))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }

    if stopped {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| Error::Engine(format!("ffmpeg wait failed: {e}")))?;
    let stderr_text = stderr_handle.join().unwrap_or_default();
    if !stopped && !status.success() {
        let mut tail: Vec<&str> = stderr_text.lines().rev().take(12).collect();
        tail.reverse();
        return Err(Error::Engine(format!("preview stream failed: {}", tail.join("\n").trim())));
    }
    Ok(())
}

/// Render the timeline by driving the `ffmpeg` binary with a generated
/// `filter_complex` (trim + per-clip volume + normalize + concat).
// With the `libav-render` feature the in-process libav executor is used instead.
#[cfg_attr(feature = "libav-render", allow(dead_code))]
pub fn render(timeline: &Timeline, assets: &[Asset], output: &Path, _format: &str) -> Result<()> {
    render_with(timeline, assets, output, &ExportOptions::default())
}

/// Progress emitted during an export: `fraction` in `0.0..=1.0`, wall-clock
/// `elapsed_secs`, and an `eta_secs` estimate once enough has rendered to
/// extrapolate. Derived from ffmpeg's `-progress` stream.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ExportProgress {
    pub fraction: f64,
    pub elapsed_secs: f64,
    pub eta_secs: Option<f64>,
}

/// Whether an export ran to completion or was stopped by the cancel callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStatus {
    Completed,
    Cancelled,
}

/// Like [`render`] but with explicit export options. Validates the options
/// against the timeline's available streams before launching, and runs ffmpeg
/// twice for [`RateControl::TwoPass`]. The no-op-callback wrapper over
/// [`render_with_progress`], so both share one code path.
#[cfg_attr(feature = "libav-render", allow(dead_code))]
pub fn render_with(timeline: &Timeline, assets: &[Asset], output: &Path, opts: &ExportOptions) -> Result<()> {
    render_with_progress(timeline, assets, output, opts, &mut |_| {}, &|| false).map(|_| ())
}

/// Like [`render_with`] but streams [`ExportProgress`] to `progress` and polls
/// `cancel` between updates — returning [`RenderStatus::Cancelled`] (and leaving
/// the partial output for the caller to remove) when it trips.
///
/// When the options request hardware decode (`opts.hwaccel`) and the render
/// fails, it is retried once fully in software — so defaulting exports to GPU
/// decode can never lose a render that plain software decoding would have
/// produced. (`-hwaccel auto` already falls back at init; this covers the rarer
/// mid-stream decoder failure.)
#[cfg_attr(feature = "libav-render", allow(dead_code))]
pub fn render_with_progress(
    timeline: &Timeline,
    assets: &[Asset],
    output: &Path,
    opts: &ExportOptions,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    let hw_requested = opts
        .hwaccel
        .as_deref()
        .is_some_and(|h| !h.is_empty() && !h.eq_ignore_ascii_case("none"));
    match render_attempt(timeline, assets, output, opts, progress, cancel) {
        Err(e) if hw_requested => {
            tracing::warn!(error = %e, "export with hardware decode failed; retrying with software decode");
            let sw = ExportOptions {
                hwaccel: None,
                ..opts.clone()
            };
            render_attempt(timeline, assets, output, &sw, progress, cancel)
        }
        result => result,
    }
}

/// One export run with `opts` exactly as given (no fallback).
fn render_attempt(
    timeline: &Timeline,
    assets: &[Asset],
    output: &Path,
    opts: &ExportOptions,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    if !timeline.tracks.iter().any(|t| !t.clips.is_empty()) {
        return Err(Error::InvalidArgument("timeline has no clips to export".to_string()));
    }

    let has_video = timeline
        .tracks
        .iter()
        .any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    let has_audio = timeline.tracks.iter().flat_map(|t| t.clips.iter()).any(|c| {
        assets
            .iter()
            .find(|a| a.id == c.asset_id)
            .is_some_and(|a| a.streams.iter().any(|s| s.kind == StreamKind::Audio))
    });
    let issues = validate_export(opts, has_video, has_audio);
    if !issues.is_empty() {
        return Err(Error::InvalidArgument(issues.join(" ")));
    }

    let output_str = output
        .to_str()
        .ok_or_else(|| Error::InvalidArgument(format!("non-UTF-8 output path: {}", output.display())))?;

    let two_pass = matches!(opts.rate_control, RateControl::TwoPass)
        && has_video
        && !opts.container.is_audio_only()
        && matches!(opts.video_codec.as_deref(), Some(vc) if vc != "prores_ks" && vc != "gif" && enc_family(vc) == EncFamily::Software);

    let total = match effective_range(timeline, opts) {
        Some((s, e)) => e - s,
        None => timeline.duration(),
    };
    let start = std::time::Instant::now();

    if two_pass {
        let null_sink = if cfg!(windows) { "NUL" } else { "/dev/null" };
        // ffmpeg appends "-N.log" to the passlog prefix; scope it to this process.
        let passlog = std::env::temp_dir()
            .join(format!("kerf-2pass-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let cleanup = || {
            for suffix in ["-0.log", "-0.log.mbtree", ".log", ".log.mbtree"] {
                let _ = std::fs::remove_file(format!("{passlog}{suffix}"));
            }
        };
        // Analysis pass fills the first half of the bar, the encode the second.
        let mut a1 = build_export_args_phase(timeline, assets, output_str, opts, PassPhase::First, null_sink, &passlog)?;
        let _g1 = externalize_filter_complex(&mut a1, "p1")?;
        let s1 = run_ffmpeg_progress(
            &a1,
            output,
            Bar {
                total,
                offset: 0.0,
                width: 0.5,
                start,
            },
            progress,
            cancel,
        )?;
        if s1 == RenderStatus::Cancelled {
            cleanup();
            return Ok(RenderStatus::Cancelled);
        }
        let mut a2 = build_export_args_phase(timeline, assets, output_str, opts, PassPhase::Second, null_sink, &passlog)?;
        let _g2 = externalize_filter_complex(&mut a2, "p2")?;
        let res = run_ffmpeg_progress(
            &a2,
            output,
            Bar {
                total,
                offset: 0.5,
                width: 0.5,
                start,
            },
            progress,
            cancel,
        );
        cleanup();
        res
    } else {
        let mut args = build_export_args(timeline, assets, output_str, opts)?;
        let _g = externalize_filter_complex(&mut args, "s")?;
        run_ffmpeg_progress(
            &args,
            output,
            Bar {
                total,
                offset: 0.0,
                width: 1.0,
                start,
            },
            progress,
            cancel,
        )
    }
}

/// Longest `-filter_complex` we are willing to hand over as an argv string.
///
/// An animated reframe's `sendcmd` list dwarfs an ordinary graph — a single
/// channel at 30 fps runs to roughly 50 KB a minute — and it is *argv*, not
/// ffmpeg, that gives out first: Linux caps one argument at 128 KiB
/// (`MAX_ARG_STRLEN`) and Windows caps the whole command line at 32767
/// characters, which is about ten seconds of animation. The threshold sits well
/// below both, since spilling to a file costs nothing.
const GRAPH_ARG_MAX: usize = 8192;

/// Index of the `-filter_complex` *value* when it is too long to pass in argv.
fn oversized_graph_index(args: &[String]) -> Option<usize> {
    args.iter()
        .position(|a| a == "-filter_complex")
        .map(|i| i + 1)
        .filter(|&i| args.get(i).is_some_and(|g| g.len() > GRAPH_ARG_MAX))
}

/// A filtergraph spilled to a script file, removed when the render is done.
struct GraphScript(Option<PathBuf>);

impl Drop for GraphScript {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Move an oversized filtergraph out of argv into a script file, pointing ffmpeg
/// at it with `-filter_complex_script`. Leaves ordinary exports untouched, so
/// their argv stays byte-identical (and every pure arg-builder test with it).
///
/// `-filter_complex_script` takes the path as its own argv token, which is why
/// it beats the obvious alternative of `sendcmd=f=`: that would bury the path
/// *inside* a filtergraph value, where `\` escapes and `:` separates options, so
/// a Windows path would have to be mangled first.
fn externalize_filter_complex(args: &mut [String], tag: &str) -> Result<GraphScript> {
    let Some(i) = oversized_graph_index(args) else {
        return Ok(GraphScript(None));
    };
    let path = std::env::temp_dir().join(format!("kerf-graph-{}-{tag}.txt", std::process::id()));
    std::fs::write(&path, &args[i]).map_err(|e| Error::Engine(format!("could not write the filtergraph script: {e}")))?;
    args[i] = path.to_string_lossy().into_owned();
    args[i - 1] = "-filter_complex_script".to_string();
    Ok(GraphScript(Some(path)))
}

/// Where one ffmpeg invocation's reported `out_time` maps onto the overall
/// export bar: `[offset, offset+width]` of `[0,1]`, against an output of `total`
/// seconds, timed from `start`. (A single-pass export is the whole bar; a
/// two-pass export splits it into two halves.)
#[derive(Clone, Copy)]
struct Bar {
    total: f64,
    offset: f64,
    width: f64,
    start: std::time::Instant,
}

/// Spawn the `ffmpeg` binary with `args`, streaming `-progress` from stdout to
/// map elapsed render time onto `bar`, and polling `cancel` between updates
/// (killing ffmpeg when it trips). stderr is drained on a side thread so a
/// warning flood can't deadlock the stdout read, and its tail still surfaces in
/// a failure's error.
fn run_ffmpeg_progress(
    args: &[String],
    output: &Path,
    bar: Bar,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    use std::io::{BufRead, BufReader, Read};
    use std::process::Stdio;

    let bin = ffmpeg_bin();
    tracing::info!(output = %output.display(), "exporting timeline");
    tracing::debug!(command = %format!("{bin} {}", args.join(" ")), "ffmpeg export command");

    // `-progress pipe:1` writes machine-readable key=value blocks to stdout;
    // `-stats_period` bounds how often, and thus the cancel-poll latency.
    let mut child = command(&bin)
        .arg("-progress")
        .arg("pipe:1")
        .arg("-stats_period")
        .arg("0.5")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;

    let stderr = child.stderr.take().expect("stderr piped");
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut buf);
        buf
    });

    let total = bar.total.max(1e-9);
    let mut cancelled = false;
    let stdout = child.stdout.take().expect("stdout piped");
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if line == "progress=end" {
            break;
        }
        // `out_time_us` is the output timeline position in microseconds (printed
        // as `N/A` before the first frame, which `parse` skips).
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<i64>().ok()) {
            let pass = (us.max(0) as f64 / 1_000_000.0 / total).clamp(0.0, 1.0);
            let fraction = (bar.offset + bar.width * pass).clamp(0.0, 1.0);
            let elapsed = bar.start.elapsed().as_secs_f64();
            let eta = (fraction > 1e-3).then(|| elapsed * (1.0 - fraction) / fraction);
            progress(ExportProgress {
                fraction,
                elapsed_secs: elapsed,
                eta_secs: eta,
            });
        }
        if cancel() {
            let _ = child.kill();
            cancelled = true;
            break;
        }
    }

    let status = child.wait().map_err(|e| Error::Engine(format!("ffmpeg wait failed: {e}")))?;
    let stderr_text = stderr_handle.join().unwrap_or_default();

    if cancelled {
        tracing::info!(output = %output.display(), "export cancelled");
        return Ok(RenderStatus::Cancelled);
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

/// The result of [`build_filter_complex`]: the `-filter_complex` string plus
/// which output pads it produced, so the caller knows which `-map`s to add.
struct FilterGraph {
    filter: String,
    has_video: bool,
    has_audio: bool,
}

/// Build the positional, multi-track `filter_complex`.
///
/// Unlike a flat `concat`, this honors each clip's `timeline_start` and layers
/// the tracks:
///
/// * **Picture** — an opaque black canvas of the whole `total` duration, then
///   every video clip `overlay`'d onto it at its timeline position
///   (`setpts=…+start/TB`, gated by `enable='between(t,start,end)'`). Tracks are
///   composited in list order, so clips that appear later in the timeline's
///   track list (e.g. a B-roll lane added above the interview) render on top,
///   and gaps fall through to black.
/// * **Sound** — every clip that has a real audio stream is trimmed, gained,
///   faded, delayed to its timeline position (`adelay`), and summed with `amix`,
///   so audio from any track (video or audio) is mixed together.
///
/// Each clip indexes the ffmpeg input list by its track-then-clip order, which
/// matches how [`build_export_args`] adds the `-i` inputs. Kept pure (no I/O)
/// so it is unit-testable without the binary present.
///
/// `want_video` / `want_audio` gate stream emission so the graph never produces
/// a pad the caller won't `-map` (e.g. an mp3 export of a video timeline emits
/// no `[outv]`). For a gif container the picture pad is routed through a
/// `palettegen` / `paletteuse` pair and audio is always dropped.
#[allow(clippy::too_many_arguments)]
fn build_filter_complex(
    timeline: &Timeline,
    assets: &[Asset],
    fmt: &ExportFormat,
    total: f64,
    opts: &ExportOptions,
    want_video: bool,
    want_audio: bool,
    plan: &InputPlan,
) -> FilterGraph {
    let has_audio = |clip: &crate::model::Clip| {
        assets
            .iter()
            .find(|a| a.id == clip.asset_id)
            .is_some_and(|a| a.streams.iter().any(|s| s.kind == StreamKind::Audio))
    };
    let is_image = |clip: &crate::model::Clip| assets.iter().find(|a| a.id == clip.asset_id).is_some_and(|a| a.is_image());
    let layout = fmt.channel_layout();

    // Assign each clip its ffmpeg input index (track-then-storage order, matching
    // the `-i` order) and split into composited video clips and mixed audio clips.
    // Within a track the clips are visited in *timeline* order so video overlays
    // composite in timeline order (a later clip on top of an earlier one's tail,
    // e.g. during a crossfade); tracks keep their list order so a later track
    // still composites on top.
    // Each entry is `(flat, input, clip)`: `flat` is the storage-order clip index
    // (keys `fx` and the per-clip pad labels — unique per clip); `input` is the
    // deduplicated ffmpeg input index (may be shared, so the `[input:v]` source is
    // fanned out with `split` below).
    let mut video: Vec<(usize, usize, &crate::model::Clip)> = Vec::new();
    // Audio entries also carry the owning track's `duck` flag for the bus split.
    let mut audio: Vec<(usize, usize, &crate::model::Clip, bool)> = Vec::new();
    let mut base = 0;
    for track in &timeline.tracks {
        let mut order: Vec<usize> = (0..track.clips.len()).collect();
        order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
        for &cj in &order {
            let clip = &track.clips[cj];
            let flat = base + cj;
            let input = plan.clip_input[flat];
            if track.kind == StreamKind::Video {
                video.push((flat, input, clip));
            }
            if has_audio(clip) {
                audio.push((flat, input, clip, track.duck));
            }
        }
        base += track.clips.len();
    }

    // Per-clip transition adjustments (crossfade tail / alpha, dip-to-black
    // fades), computed per track from each clip's `transition_in`.
    let fx = transition_fx(timeline, assets);

    let gif = opts.container == Container::Gif;
    let has_video = want_video && !video.is_empty();
    let has_audio_out = want_audio && !audio.is_empty();

    // How many clips consume each input as video / as audio. An input used by
    // more than one must be fanned out with `split` / `asplit` — ffmpeg forbids
    // reusing an input pad across filters. When nothing is shared (the common
    // case) every count is ≤ 1 and no split is emitted, so the graph is identical.
    let n_inputs = plan.representatives.len();
    let mut vcount = vec![0usize; n_inputs];
    let mut acount = vec![0usize; n_inputs];
    for (_, input, _) in &video {
        vcount[*input] += 1;
    }
    for (_, input, _, _) in &audio {
        acount[*input] += 1;
    }

    let mut chains: Vec<String> = Vec::new();
    if has_video {
        for (i, &cnt) in vcount.iter().enumerate() {
            if cnt > 1 {
                let outs: String = (0..cnt).map(|k| format!("[vsp{i}_{k}]")).collect();
                chains.push(format!("[{i}:v]split={cnt}{outs}"));
            }
        }
    }
    if has_audio_out {
        for (i, &cnt) in acount.iter().enumerate() {
            if cnt > 1 {
                let outs: String = (0..cnt).map(|k| format!("[asp{i}_{k}]")).collect();
                chains.push(format!("[{i}:a]asplit={cnt}{outs}"));
            }
        }
    }
    // A clip's source pad: its own input, or the next `split` output when that
    // input is shared. `vnext`/`anext` hand out the split outputs in clip order.
    let mut vnext = vec![0usize; n_inputs];
    let mut anext = vec![0usize; n_inputs];

    // ---- picture: black base + positioned overlays --------------------------
    if has_video {
        chains.push(format!(
            "color=c=black:s={w}x{h}:r={fps}:d={total},format={pf}[vbase]",
            w = fmt.width,
            h = fmt.height,
            fps = fmt.fps,
            total = total.max(0.0),
            pf = fmt.pix_fmt,
        ));
        let mut cur = "vbase".to_string();
        let draw = !timeline.overlays.is_empty();
        // The composite lands on `vcomp` for gif (palettegen follows), on `vtext`
        // when text overlays will be drawn on top, else directly on `[outv]`.
        let composite_pad = if draw {
            "vtext"
        } else if gif {
            "vcomp"
        } else {
            "outv"
        };
        let last = video.len() - 1;
        for (n, (flat, input, clip)) in video.iter().enumerate() {
            let src = if vcount[*input] > 1 {
                let k = vnext[*input];
                vnext[*input] += 1;
                format!("vsp{input}_{k}")
            } else {
                format!("{input}:v")
            };
            chains.push(format!(
                "[{src}]{chain}[v{flat}]",
                chain = video_clip_chain(clip, fmt, &fx[*flat], is_image(clip), &format!("c{flat}"))
            ));
            let out = if n == last {
                composite_pad.to_string()
            } else {
                format!("vov{n}")
            };
            let end = clip.timeline_end() + fx[*flat].tail;
            let overlay = if clip.is_animated() {
                // Animated picture position: per-frame overlay x / y expressions.
                let kf = clip.sorted_keyframes();
                let xs: Vec<(f64, f64)> = kf.iter().map(|k| (k.time, k.pos_x)).collect();
                let ys: Vec<(f64, f64)> = kf.iter().map(|k| (k.time, k.pos_y)).collect();
                format!(
                    "overlay=x=(W-w)/2+({px})*W:y=(H-h)/2+({py})*H:\
                     eof_action=pass:enable='between(t,{start},{end})'",
                    px = keyframe_expr(&xs, "t", clip.timeline_start),
                    py = keyframe_expr(&ys, "t", clip.timeline_start),
                    start = clip.timeline_start,
                )
            } else if clip.transform.is_identity() {
                format!(
                    "overlay=eof_action=pass:enable='between(t,{start},{end})'",
                    start = clip.timeline_start
                )
            } else {
                let t = &clip.transform;
                format!(
                    "overlay=x=(W-w)/2+({px})*W:y=(H-h)/2+({py})*H:\
                     eof_action=pass:enable='between(t,{start},{end})'",
                    px = t.pos_x,
                    py = t.pos_y,
                    start = clip.timeline_start,
                )
            };
            chains.push(format!("[{cur}][v{flat}]{overlay}[{out}]"));
            cur = out;
        }
        // Text overlays (titles / lower-thirds / captions) drawn on the composited
        // picture in order; the last produces the gif source `vcomp` or `[outv]`.
        if draw {
            let mut tcur = composite_pad.to_string();
            let last_o = timeline.overlays.len() - 1;
            let text_final = if gif { "vcomp" } else { "outv" };
            for (oi, ov) in timeline.overlays.iter().enumerate() {
                let out = if oi == last_o {
                    text_final.to_string()
                } else {
                    format!("vtxt{oi}")
                };
                chains.push(format!("[{tcur}]{f}[{out}]", f = drawtext_export(ov, fmt)));
                tcur = out;
            }
        }
        if gif {
            // A two-stream palette gives far better color than the default 216-color
            // web palette: generate an optimized palette, then map onto it.
            let dither = opts.gif_dither.as_deref().unwrap_or("bayer");
            chains.push("[vcomp]split[gpsrc][gpuse]".to_string());
            chains.push("[gpsrc]palettegen=stats_mode=diff[gpal]".to_string());
            chains.push(format!("[gpuse][gpal]paletteuse=dither={dither}[outv]"));
        }
    }

    // ---- sound: positioned per-clip audio summed with amix ------------------
    if has_audio_out {
        for (flat, input, clip, _) in &audio {
            let src = if acount[*input] > 1 {
                let k = anext[*input];
                anext[*input] += 1;
                format!("asp{input}_{k}")
            } else {
                format!("{input}:a")
            };
            chains.push(format!(
                "[{src}]{chain}[a{flat}]",
                chain = audio_clip_chain(clip, fmt, &fx[*flat], layout)
            ));
        }
        // Optional single-pass loudness normalization on the final mix; loudnorm
        // upsamples to 192 kHz internally, so resample back to the output rate.
        let mix_tail = if opts.loudnorm {
            format!(",loudnorm=I=-14:TP=-1.5:LRA=11,aresample={sr}", sr = fmt.sample_rate)
        } else {
            String::new()
        };
        let pads = |flats: &[usize]| flats.iter().map(|f| format!("[a{f}]")).collect::<String>();
        let ducked: Vec<usize> = audio.iter().filter(|(_, _, _, d)| *d).map(|(f, _, _, _)| *f).collect();
        let keyed: Vec<usize> = audio.iter().filter(|(_, _, _, d)| !*d).map(|(f, _, _, _)| *f).collect();
        if ducked.is_empty() || keyed.is_empty() {
            // No ducking in play (nothing flagged, or nothing to key from): one
            // flat sum of every clip, exactly as before.
            let flats: Vec<usize> = audio.iter().map(|(f, _, _, _)| *f).collect();
            chains.push(format!(
                "{ins}amix=inputs={n}:normalize=0:dropout_transition=0{mix_tail}[outa]",
                ins = pads(&flats),
                n = flats.len(),
            ));
        } else {
            // Mix each group into a bus, dip the ducked bus under the keyed one
            // (sidechain compression — music falls when dialogue speaks), then
            // sum the two buses.
            chains.push(format!(
                "{ins}amix=inputs={n}:normalize=0:dropout_transition=0[akey]",
                ins = pads(&keyed),
                n = keyed.len(),
            ));
            chains.push(format!(
                "{ins}amix=inputs={n}:normalize=0:dropout_transition=0[aduck]",
                ins = pads(&ducked),
                n = ducked.len(),
            ));
            chains.push("[akey]asplit=2[akmix][akside]".to_string());
            chains.push("[aduck][akside]sidechaincompress=threshold=0.05:ratio=8:attack=20:release=400[aducked]".to_string());
            chains.push(format!(
                "[akmix][aducked]amix=inputs=2:normalize=0:dropout_transition=0{mix_tail}[outa]"
            ));
        }
    }

    FilterGraph {
        filter: chains.join(";"),
        has_video,
        has_audio: has_audio_out,
    }
}

/// Per-clip render adjustments derived from transitions. `tail` extends an
/// outgoing clip so it keeps showing under the incoming crossfade; `xfade_in`
/// is the incoming clip's alpha dissolve; `black_in`/`black_out` are the
/// dip-to-black fades on either side of a cut.
#[derive(Clone, Copy, Default)]
struct ClipFx {
    tail: f64,
    xfade_in: f64,
    black_in: f64,
    black_out: f64,
}

/// Compute the [`ClipFx`] for every clip (indexed by ffmpeg input index, i.e.
/// track-then-clip order), resolving each `transition_in` against the clip that
/// precedes it on the same track in timeline order.
fn transition_fx(timeline: &Timeline, assets: &[Asset]) -> Vec<ClipFx> {
    let total_clips: usize = timeline.tracks.iter().map(|t| t.clips.len()).sum();
    let mut fx = vec![ClipFx::default(); total_clips];
    let asset_dur = |id| assets.iter().find(|a| a.id == id).map(|a| a.duration);

    let mut base = 0;
    for track in &timeline.tracks {
        let n = track.clips.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
        for w in 0..n {
            let j = order[w];
            let clip = &track.clips[j];
            let Some(tr) = clip.transition_in else { continue };
            let d = tr.duration.max(0.0);
            if d <= 0.0 {
                continue;
            }
            // The transition partner is the immediately preceding clip on the
            // track — but only when it is actually adjacent (no gap before this
            // clip); otherwise the transition resolves against black.
            let prev = (w > 0)
                .then(|| order[w - 1])
                .filter(|&pj| (track.clips[pj].timeline_end() - clip.timeline_start).abs() < 1e-3);
            match tr.kind {
                TransitionKind::Crossfade => match prev {
                    Some(pj) => {
                        let p = &track.clips[pj];
                        // The tail borrows the outgoing clip's unused source: for a
                        // forward clip that is the handle past source_out, for a
                        // reversed clip the handle below source_in.
                        let avail = if p.is_reversed() {
                            p.source_in / p.speed_mag()
                        } else {
                            asset_dur(p.asset_id).map(|ad| (ad - p.source_out).max(0.0)).unwrap_or(0.0) / p.speed_mag()
                        };
                        // Both sides share the achievable overlap so the dissolve
                        // length matches the tail (no fade-from-black when there is
                        // no handle — it just becomes a hard cut).
                        let overlap = d.min(p.duration()).min(clip.duration()).min(avail.max(0.0));
                        fx[base + j].xfade_in = overlap;
                        fx[base + pj].tail = fx[base + pj].tail.max(overlap);
                    }
                    // No adjacent predecessor: dissolve up from black.
                    None => fx[base + j].xfade_in = d.min(clip.duration()),
                },
                TransitionKind::DipToBlack => {
                    fx[base + j].black_in = (d / 2.0).min(clip.duration());
                    if let Some(pj) = prev {
                        let p = &track.clips[pj];
                        let out = (d / 2.0).min(p.duration());
                        fx[base + pj].black_out = fx[base + pj].black_out.max(out);
                    }
                }
            }
        }
        base += n;
    }
    fx
}

/// The source-time window `[start, end]` a clip needs from its asset, accounting
/// for reverse playback and any crossfade tail (which borrows unused handle past
/// `source_out`, or below `source_in` when reversed). The single source of truth
/// for both the per-input `-ss` fast-seek and the in-graph `trim` / `atrim`, so
/// the seek and the trim window can never drift out of lockstep.
fn clip_source_window(clip: &Clip, fx: &ClipFx) -> (f64, f64) {
    let s = clip.speed_mag();
    if clip.is_reversed() {
        ((clip.source_in - fx.tail * s).max(0.0), clip.source_out)
    } else {
        (clip.source_in, clip.source_out + fx.tail * s)
    }
}

/// The input-side fast-seek for a clip's window start: seek there when it is past
/// the head (so ffmpeg decodes from a nearby keyframe instead of t=0), else `0.0`
/// for no seek — head clips keep byte-identical args. `SEEK_EPS` skips a
/// pointless sub-millisecond seek. Callers must express the in-graph trim
/// relative to this value.
fn clip_seek(window_start: f64) -> f64 {
    const SEEK_EPS: f64 = 1e-3;
    if window_start > SEEK_EPS {
        window_start
    } else {
        0.0
    }
}

/// Format an f64 for an ffmpeg filter argument / expression (Rust's default
/// `{}` avoids scientific notation for the ranges used here; `-0` is normalized).
fn fnum(v: f64) -> String {
    let s = format!("{v}");
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

/// Build a piecewise-linear ffmpeg expression over **clip-local time** for a
/// channel of keyframes. `points` are `(seconds_from_clip_start, value)` and are
/// sorted here. `tvar` is the time variable the target filter exposes (`t` for
/// overlay / scale / rotate, `T` for geq); `start` is the clip's `timeline_start`
/// so the expression reads time relative to the clip. Values hold flat before the
/// first and after the last keyframe.
fn keyframe_expr(points: &[(f64, f64)], tvar: &str, start: f64) -> String {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    if pts.is_empty() {
        return "0".to_string();
    }
    if pts.len() == 1 {
        return fnum(pts[0].1);
    }
    let lt = format!("({tvar}-{})", fnum(start));
    // Fold segments in from the end; `expr` starts as the value held after the
    // last keyframe.
    let mut expr = fnum(pts[pts.len() - 1].1);
    for w in (0..pts.len() - 1).rev() {
        let (t0, v0) = pts[w];
        let (t1, v1) = pts[w + 1];
        let seg = if (t1 - t0).abs() < 1e-9 {
            fnum(v0)
        } else {
            format!(
                "({v0}+({dv})*({lt}-{t0})/({dt}))",
                v0 = fnum(v0),
                dv = fnum(v1 - v0),
                lt = lt,
                t0 = fnum(t0),
                dt = fnum(t1 - t0),
            )
        };
        expr = format!(
            "if(lt({lt},{t1}),{seg},{expr})",
            lt = lt,
            t1 = fnum(t1),
            seg = seg,
            expr = expr
        );
    }
    // Hold the first value before the first keyframe.
    format!(
        "if(lt({lt},{t0}),{v0},{expr})",
        lt = lt,
        t0 = fnum(pts[0].0),
        v0 = fnum(pts[0].1),
        expr = expr
    )
}

fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// The `eq` filter for a clip's color correction, shared by the export and
/// still chains. `temperature` becomes opposing red/blue per-channel gammas —
/// `eq` has no white-balance knob, but shifting midtone gamma per channel
/// warms/cools convincingly; ±1.0 maps to a ±30% gamma split. The channel
/// gammas are omitted at 0 so a temperature-free clip's graph stays
/// byte-identical to before the field existed.
fn eq_filter(c: &Color) -> String {
    let mut f = format!(
        "eq=brightness={}:contrast={}:saturation={}:gamma={}",
        c.brightness, c.contrast, c.saturation, c.gamma
    );
    if c.temperature != 0.0 {
        let t = c.temperature.clamp(-1.0, 1.0);
        f.push_str(&format!(
            ":gamma_r={}:gamma_b={}",
            fnum(1.0 + 0.3 * t),
            fnum(1.0 - 0.3 * t)
        ));
    }
    f
}

/// The filter for a non-alpha video effect, or `None` for chroma key (which
/// establishes alpha and is emitted separately, after the alpha plane exists).
fn video_effect_filter(e: &VideoEffect) -> Option<String> {
    Some(match e {
        VideoEffect::Blur { sigma } => format!("gblur=sigma={}", fnum(*sigma)),
        VideoEffect::Sharpen { amount } => {
            format!("unsharp=luma_msize_x=5:luma_msize_y=5:luma_amount={}", fnum(*amount))
        }
        VideoEffect::Grayscale => "hue=s=0".to_string(),
        VideoEffect::Invert => "negate".to_string(),
        VideoEffect::Vignette => "vignette".to_string(),
        VideoEffect::ChromaKey { .. } => return None,
    })
}

/// The `chromakey` filter for a chroma-key effect, or `None` for any other.
fn chroma_filter(e: &VideoEffect) -> Option<String> {
    match e {
        VideoEffect::ChromaKey {
            color,
            similarity,
            blend,
        } => Some(format!("chromakey={color}:{}:{}", fnum(*similarity), fnum(*blend))),
        _ => None,
    }
}

/// A clip's whole audio effect chain as one comma-joined filter string, or `None`
/// when it has no effects.
///
/// Public because the GUI's preview monitor decodes clip audio through the same
/// chain the export renders — the chain is only *described* once, here, so the
/// two can't drift.
pub fn audio_effects_filter(effects: &[AudioEffect]) -> Option<String> {
    (!effects.is_empty()).then(|| effects.iter().map(audio_effect_filter).collect::<Vec<_>>().join(","))
}

/// The filter for one audio effect. dB thresholds / make-up gain are converted to
/// the linear units ffmpeg's dynamics filters expect.
fn audio_effect_filter(e: &AudioEffect) -> String {
    match e {
        AudioEffect::Highpass { hz } => format!("highpass=f={}", fnum(*hz)),
        AudioEffect::Lowpass { hz } => format!("lowpass=f={}", fnum(*hz)),
        AudioEffect::Equalizer { hz, width, gain_db } => {
            format!(
                "equalizer=f={}:width_type=h:width={}:g={}",
                fnum(*hz),
                fnum(*width),
                fnum(*gain_db)
            )
        }
        AudioEffect::Compressor {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            makeup_db,
        } => format!(
            "acompressor=threshold={}:ratio={}:attack={}:release={}:makeup={}",
            fnum(db_to_linear(*threshold_db)),
            fnum(*ratio),
            fnum(*attack_ms),
            fnum(*release_ms),
            fnum(db_to_linear(*makeup_db)),
        ),
        AudioEffect::Gate { threshold_db } => format!("agate=threshold={}", fnum(db_to_linear(*threshold_db))),
    }
}

/// Escape user text for a single-quoted `drawtext` value inside a filtergraph
/// passed as one argv argument: backslashes (drawtext layer), then apostrophes
/// (filtergraph single-quote layer). Newlines collapse to spaces — drawtext here
/// is single-line.
fn escape_drawtext(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', " ").replace('\'', "'\\''")
}

/// `drawtext` options shared by the export and still paths (text, size, color,
/// font, bold approximation, box) — everything except position / alpha /
/// enable. `frame_h` is the target canvas height (export height, or the
/// still's height).
fn drawtext_common(o: &TextOverlay, frame_h: f64) -> Vec<String> {
    let fontsize = (frame_h * o.size).round().max(1.0);
    let mut parts = vec![
        format!("text='{}'", escape_drawtext(&o.text)),
        format!("fontsize={}", fnum(fontsize)),
        format!("fontcolor={}", o.color),
    ];
    // Resolve a chosen system font to its file on disk; falls through to
    // FFmpeg's drawtext default if unset or no longer installed.
    let resolved_bold = o
        .font
        .as_deref()
        .and_then(|family| crate::fonts::resolve_font_file(family, o.bold))
        .map(|(path, matched_bold)| {
            parts.push(format!("fontfile='{}'", escape_drawtext(&path.to_string_lossy())));
            matched_bold
        });
    if o.bold && resolved_bold != Some(true) {
        // No real bold face available: a same-color border thickens the glyphs.
        parts.push("borderw=2".to_string());
        parts.push(format!("bordercolor={}", o.color));
    }
    if let Some(bg) = &o.bg {
        parts.push("box=1".to_string());
        parts.push(format!("boxcolor={bg}"));
        parts.push("boxborderw=12".to_string());
    }
    parts
}

/// The export-path `drawtext` for an overlay: `enable`-gated to its lifetime,
/// with per-frame position / alpha expressions when it is animated.
fn drawtext_export(o: &TextOverlay, fmt: &ExportFormat) -> String {
    let mut parts = drawtext_common(o, fmt.height as f64);
    if o.keyframes.is_empty() {
        parts.push(format!("x=(w*{}-text_w/2)", fnum(o.pos_x)));
        parts.push(format!("y=(h*{}-text_h/2)", fnum(o.pos_y)));
    } else {
        let xs: Vec<(f64, f64)> = o.keyframes.iter().map(|k| (k.time, k.pos_x)).collect();
        let ys: Vec<(f64, f64)> = o.keyframes.iter().map(|k| (k.time, k.pos_y)).collect();
        let al: Vec<(f64, f64)> = o.keyframes.iter().map(|k| (k.time, k.opacity)).collect();
        parts.push(format!("x=(w*({})-text_w/2)", keyframe_expr(&xs, "t", o.start)));
        parts.push(format!("y=(h*({})-text_h/2)", keyframe_expr(&ys, "t", o.start)));
        parts.push(format!("alpha='{}'", keyframe_expr(&al, "t", o.start)));
    }
    parts.push(format!("enable='between(t,{},{})'", fnum(o.start), fnum(o.end)));
    format!("drawtext={}", parts.join(":"))
}

/// The still-path `drawtext` for an overlay active at time `t`: position / alpha
/// sampled to constants (the still pipeline has no timeline clock), no `enable`.
fn drawtext_still(o: &TextOverlay, frame_h: u32, t: f64) -> String {
    let mut parts = drawtext_common(o, frame_h as f64);
    let (px, py, op) = o.sample(t);
    parts.push(format!("x=(w*{}-text_w/2)", fnum(px)));
    parts.push(format!("y=(h*{}-text_h/2)", fnum(py)));
    if op < 1.0 {
        parts.push(format!("alpha={}", fnum(op)));
    }
    format!("drawtext={}", parts.join(":"))
}

/// `v360` resampling used for export (quality) and for stills / previews (speed).
/// The measured difference in reprojection cost between these is small; the
/// export one buys sharper edges on a wide reframe.
const EXPORT_INTERP: &str = "cubic";
const PREVIEW_INTERP: &str = "line";

/// How far a reframe channel must drift from its last emitted value before it is
/// worth another `sendcmd`, in degrees. Every command makes `v360` rebuild its
/// remap LUT, so this gate is what keeps a held camera from paying per frame.
const REFRAME_CMD_TOLERANCE: f64 = 0.05;

/// The `v360` filter realizing one sampled reframe at output size `w`x`h`.
///
/// `instance` names the filter (`v360@c3`) so a `sendcmd` can target this clip's
/// instance and no other — several reframed clips coexist in one graph. The
/// still path passes `None`: it has no commands to send, so it needs no name.
fn reframe_filter(r: &ResolvedReframe, w: u32, h: u32, interp: &str, instance: Option<&str>) -> String {
    let name = match instance {
        Some(id) => format!("v360@{id}"),
        None => "v360".to_string(),
    };
    // An equirect output must stay 2:1 or the sphere is squashed; size it to the
    // frame width and let the caller's fit/pad letterbox it. A flat output is
    // rendered straight at the frame size, which both skips an oversized
    // intermediate (an 8K source never materializes at 8K) and makes the fit
    // `scale` that follows a no-op.
    let (ow, oh) = match r.output {
        Projection::Equirect => (w, ((w / 2).max(2)) & !1),
        _ => (w, h),
    };
    let mut opts = vec![
        format!("input={}", r.input.v360_name()),
        format!("output={}", r.output.v360_name()),
    ];
    // The lens field of view only describes a physical fisheye; it means nothing
    // for an equirect source, which is already unwrapped.
    if r.input.is_fisheye() {
        opts.push(format!("ih_fov={}", fnum(r.lens_fov)));
        opts.push(format!("iv_fov={}", fnum(r.lens_fov)));
    }
    opts.push(format!("w={ow}"));
    opts.push(format!("h={oh}"));
    opts.push(format!("interp={interp}"));
    opts.push(format!("yaw={}", fnum(r.yaw)));
    opts.push(format!("pitch={}", fnum(r.pitch)));
    opts.push(format!("roll={}", fnum(r.roll)));
    // `d_fov` derives an aspect-correct horizontal/vertical pair on its own,
    // unlike `h_fov`, which needs `v_fov` set in lockstep or the picture
    // stretches. It is meaningless for an equirect output, which always covers
    // the whole sphere.
    if r.output == Projection::Flat {
        opts.push(format!("d_fov={}", fnum(r.fov)));
    }
    format!("{name}={}", opts.join(":"))
}

/// The `sendcmd` command list that drives `target`'s `v360` across an animated
/// clip, or `None` when the camera holds still — a static reframe bakes its pose
/// into the filter's own arguments and costs nothing extra.
///
/// This is deliberately stingy, because each command makes `v360` re-run
/// `config_output` and rebuild its remap LUT (~32 ms per command at 1080p, linear
/// in both command count and output pixels). Two economies do the work: channels
/// that never move are left as static arguments and never appear here at all, and
/// a channel re-emits only once it has drifted past [`REFRAME_CMD_TOLERANCE`],
/// which collapses a long hold to a single command.
fn reframe_commands(clip: &Clip, rf: &Reframe, target: &str, fps: f64, dur: f64) -> Option<String> {
    if !rf.is_animated() || !fps.is_finite() || fps <= 0.0 || !dur.is_finite() || dur <= 0.0 {
        return None;
    }
    let k = rf.sorted_keyframes();
    let moves = |get: fn(&ReframeKeyframe) -> f64| {
        let first = get(&k[0]);
        k.iter().any(|kf| get(kf) != first)
    };
    // (v360 option name, how to read it off a sampled pose, whether it moves)
    type Channel = (&'static str, fn(&ResolvedReframe) -> f64, bool);
    let channels: [Channel; 4] = [
        ("yaw", |r| r.yaw, moves(|kf| kf.yaw)),
        ("pitch", |r| r.pitch, moves(|kf| kf.pitch)),
        ("roll", |r| r.roll, moves(|kf| kf.roll)),
        ("d_fov", |r| r.fov, moves(|kf| kf.fov)),
    ];
    if !channels.iter().any(|(_, _, moving)| *moving) {
        return None;
    }

    let frames = (dur * fps).ceil().max(0.0) as u64;
    let mut cmds: Vec<String> = Vec::new();
    let mut last: [Option<f64>; 4] = [None; 4];
    for i in 0..=frames {
        let local = i as f64 / fps;
        let pose = rf.sample(local);
        // Fire half a frame early so the command is already pending when frame
        // `i` reaches `v360` and cannot be consumed by frame `i-1`. Without the
        // lead, float drift at rates like 29.97 lands commands a frame late.
        let at = (clip.timeline_start + local - 0.5 / fps).max(0.0);
        for (n, (name, get, moving)) in channels.iter().enumerate() {
            if !*moving {
                continue;
            }
            let v = get(&pose);
            if last[n].is_some_and(|prev| (v - prev).abs() < REFRAME_CMD_TOLERANCE) {
                continue;
            }
            last[n] = Some(v);
            // `{:.4}` rather than `fnum`, whose `{}` formatting can spell a
            // rounded value `0.30000000000000004` and triple the graph's size.
            cmds.push(format!("{at:.5} {target} {name} {v:.4}"));
        }
    }
    (!cmds.is_empty()).then(|| cmds.join(";"))
}

/// The video filter chain for one clip (everything between its `[i:v]` input
/// and its `[v{i}]` output): trim, optional reverse / crop / retime, 360
/// reprojection, fit or transform geometry, color correction, per-clip video
/// effects, keyframe animation, fades and transition alpha. With all properties
/// at their defaults this reduces to the original fit-and-letterbox chain.
///
/// `instance` is the clip's unique flat index, used to name its `v360` so
/// `sendcmd` can address it; it is unused for clips that do not reframe.
fn video_clip_chain(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx, is_image: bool, instance: &str) -> String {
    let s = clip.speed_mag();
    let t = &clip.transform;
    let anim = clip.is_animated();
    let kf = clip.sorted_keyframes();
    // Which animated channels actually move (so alpha / per-frame geometry is only
    // forced when needed). A keyframed clip's scale / position always come from the
    // keyframes (a fresh keyframe captures the static transform), so `anim` alone
    // drives the geometry; rotation / opacity additionally need an alpha plane.
    let anim_rotation = anim && kf.iter().any(|k| k.rotation != 0.0);
    let anim_opacity = anim && kf.iter().any(|k| k.opacity < 1.0);
    let chroma = clip.effects.iter().any(|e| e.produces_alpha());
    // Alpha is needed for static opacity/rotation, animated opacity/rotation, a
    // chroma key, or a crossfade dissolve.
    let transform_alpha = (!anim && !t.is_identity() && t.needs_alpha()) || anim_rotation || anim_opacity || chroma;
    let needs_alpha = transform_alpha || fx.xfade_in > 0.0;
    let dur = clip.duration() + fx.tail;
    // A crossfade tail borrows unused source: forward clips extend past source_out,
    // reversed clips extend below source_in (reverse plays high->low, so the visible
    // tail is at the low end).
    let (trim_start, trim_end) = clip_source_window(clip, fx);
    // Relative to the input-side `-ss` (see `build_export_args_phase`): when the
    // window is fast-seeked the trim starts at 0, otherwise it is unchanged. A still
    // image is never seeked (it is `-loop`ed from t=0), so its trim stays absolute.
    let seek = if is_image { 0.0 } else { clip_seek(trim_start) };

    let reframe = clip.reframe.as_ref();
    let crop = t.has_crop().then(|| {
        let cw = (1.0 - t.crop_left - t.crop_right).max(0.0);
        let ch = (1.0 - t.crop_top - t.crop_bottom).max(0.0);
        format!(
            "crop=w=iw*{cw}:h=ih*{ch}:x=iw*{cl}:y=ih*{ct}",
            cl = t.crop_left,
            ct = t.crop_top
        )
    });

    let mut p: Vec<String> = Vec::new();
    p.push(format!("trim=start={}:end={}", trim_start - seek, trim_end - seek));
    if clip.is_reversed() {
        p.push("reverse".to_string());
    }
    // A reframed clip crops *after* reprojection instead: edge fractions of a raw
    // dual-fisheye frame mean nothing, and the user set them against the flat
    // picture they were looking at. `crop` reads no timestamps (its `iw`/`ih`
    // fractions are constants), so moving it past `setpts` is safe.
    if reframe.is_none() {
        p.extend(crop.clone());
    }
    if (s - 1.0).abs() < 1e-9 {
        p.push(format!("setpts=PTS-STARTPTS+{}/TB", clip.timeline_start));
    } else {
        p.push(format!("setpts=(PTS-STARTPTS)/{}+{}/TB", s, clip.timeline_start));
    }
    if let Some(rf) = reframe {
        // Hoisted above `v360` so a 50 fps source exporting at 30 reprojects 30
        // frames a second rather than reprojecting 50 and discarding 20 — each
        // of which would have rebuilt the remap LUT.
        p.push(format!("fps={}", fmt.fps));
        // `sendcmd` sits upstream of `v360`: a command takes effect as a frame
        // passes through, so downstream it would land one frame late. Both sit
        // after `setpts`, which puts the timestamps `sendcmd` matches on the
        // timeline clock — so a keyframe at clip-local `t` is a command at
        // `timeline_start + t`, and `speed` and `reverse` are already folded in.
        let target = format!("v360@{instance}");
        if let Some(cmds) = reframe_commands(clip, rf, &target, fmt.fps, dur) {
            p.push(format!("sendcmd=c='{cmds}'"));
        }
        p.push(reframe_filter(
            &rf.pose(),
            fmt.width,
            fmt.height,
            EXPORT_INTERP,
            Some(instance),
        ));
        p.extend(crop);
    }
    let sf = fmt.scale_flags();
    // A keyframed clip is treated as non-identity so its picture is centered by
    // the overlay (not padded full-frame), and its zoom is re-evaluated per frame.
    let geom_identity = t.is_identity() && !anim;
    match fmt.fit {
        // Fit inside the frame; the identity case then pads out to full size so
        // the overlay lands on a complete canvas.
        Fit::Contain => {
            p.push(format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease{sf}",
                w = fmt.width,
                h = fmt.height
            ));
            if geom_identity {
                p.push(format!("pad={w}:{h}:(ow-iw)/2:(oh-ih)/2", w = fmt.width, h = fmt.height));
            }
        }
        // Fill the frame and cut the overflow, so 16:9 footage delivered at 9:16
        // is a usable vertical shot rather than a strip of picture in a black
        // field. `increase` overshoots on one axis; the crop takes the centre.
        Fit::Cover => {
            p.push(format!(
                "scale={w}:{h}:force_original_aspect_ratio=increase{sf}",
                w = fmt.width,
                h = fmt.height
            ));
            p.push(format!("crop={w}:{h}", w = fmt.width, h = fmt.height));
        }
    }
    // A transformed clip's own zoom rides on top of that base fit.
    if !geom_identity {
        if anim {
            // Per-frame zoom: re-evaluate the scale expression every frame.
            let expr = keyframe_expr(
                &kf.iter().map(|k| (k.time, k.scale)).collect::<Vec<_>>(),
                "t",
                clip.timeline_start,
            );
            p.push(format!("scale=w='iw*({expr})':h='ih*({expr})':eval=frame{sf}"));
        } else if (t.scale - 1.0).abs() > 1e-9 {
            p.push(format!("scale=iw*{sc}:ih*{sc}{sf}", sc = t.scale));
        }
    }
    p.push("setsar=1".to_string());
    if reframe.is_none() {
        p.push(format!("fps={}", fmt.fps));
    }
    // Color correction must run BEFORE any alpha plane is established: ffmpeg's `eq`
    // has no alpha-capable input format, so the graph would otherwise auto-insert a
    // conversion that drops the alpha (silently disabling opacity / rotation).
    if !clip.color.is_identity() {
        p.push(eq_filter(&clip.color));
    }
    // Color-space video effects (blur / sharpen / grayscale / invert / vignette),
    // applied in author order, before any alpha plane.
    for e in &clip.effects {
        if let Some(f) = video_effect_filter(e) {
            p.push(f);
        }
    }
    // Establish alpha once, before any alpha-producing step (chroma key, opacity,
    // rotation fill, crossfade dissolve).
    if needs_alpha {
        p.push("format=yuva420p".to_string());
    }
    // Chroma key (color → transparency) after alpha is available.
    for e in &clip.effects {
        if let Some(f) = chroma_filter(e) {
            p.push(f);
        }
    }
    // Opacity: animated via a per-frame geq alpha (geq's time var is `T`), else a
    // constant alpha mix.
    if anim_opacity {
        let expr = keyframe_expr(
            &kf.iter().map(|k| (k.time, k.opacity)).collect::<Vec<_>>(),
            "T",
            clip.timeline_start,
        );
        p.push(format!(
            "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='({expr})*alpha(X,Y)'"
        ));
    } else if !anim && t.opacity < 1.0 {
        p.push(format!("colorchannelmixer=aa={}", t.opacity));
    }
    // Rotation: animated angle expression (degrees → radians), else a constant
    // rotate. Animated rotation uses a fixed bounding box (the frame diagonal).
    if anim_rotation {
        let expr = keyframe_expr(
            &kf.iter().map(|k| (k.time, k.rotation)).collect::<Vec<_>>(),
            "t",
            clip.timeline_start,
        );
        p.push(format!(
            "rotate=a='({expr})*PI/180':fillcolor=none:ow='hypot(iw,ih)':oh='hypot(iw,ih)'"
        ));
    } else if !anim && t.rotation != 0.0 {
        let rad = t.rotation.to_radians();
        p.push(format!("rotate={rad}:fillcolor=none:ow=rotw({rad}):oh=roth({rad})"));
    }
    let fi = clip.fade_in + fx.black_in;
    let fo = clip.fade_out + fx.black_out;
    if fi > 0.0 {
        p.push(format!("fade=t=in:st=0:d={}", fi.clamp(0.0, dur)));
    }
    if fo > 0.0 {
        p.push(format!("fade=t=out:st={}:d={}", (dur - fo).max(0.0), fo.clamp(0.0, dur)));
    }
    if fx.xfade_in > 0.0 {
        // The alpha plane is already established above (xfade implies needs_alpha).
        p.push(format!("fade=t=in:st=0:d={}:alpha=1", fx.xfade_in.clamp(0.0, dur)));
    }
    if !needs_alpha {
        // Terminal pixel format — kept equal to argv `-pix_fmt` so a 10-bit /
        // 4:2:2 selection isn't silently bottlenecked back through 8-bit.
        p.push(format!("format={}", fmt.pix_fmt));
    }
    p.join(",")
}

/// Composite a single still of the `timeline` at timeline time `t` and return
/// it as JPEG bytes (`quality` = `-q:v`), the canvas downscaled so it is at most
/// `max_width` px wide. Lets an LLM *see the cut it is assembling* (which footage
/// is on screen, framing, picture-in-picture placement, crop, color) rather than
/// reasoning about timestamps blind.
pub fn timeline_frame(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    quality: u8,
) -> Result<Vec<u8>> {
    let run = |o: &ExportOptions| -> Result<Vec<u8>> {
        let args = build_timeline_frame_args(timeline, assets, o, t, max_width, quality)?;
        let bin = ffmpeg_bin();
        let output = command(&bin)
            .args(&args)
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| launch_err(&bin, e))?;
        if !output.status.success() || output.stdout.is_empty() {
            return Err(Error::Engine(format!(
                "could not render timeline frame at {t:.3}s: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout)
    };
    let hw = opts
        .hwaccel
        .as_deref()
        .is_some_and(|h| !h.is_empty() && !h.eq_ignore_ascii_case("none"));
    match run(opts) {
        // Mirror `decode_frame`'s fallback: a software retry that succeeds means
        // `-hwaccel` is the culprit here, so stop asking for it.
        Err(hw_err) if hw => match run(&ExportOptions {
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

/// Pure arg builder for [`timeline_frame`] (no I/O, unit-tested).
///
/// Every video clip whose timeline span contains `t` is decoded at its
/// corresponding source time (`-ss` input seek), put through the same geometry /
/// color chain the export uses ([`still_clip_chain`] mirrors [`video_clip_chain`]
/// minus the time-domain `trim`/`setpts`/`fps`/fade steps), then `overlay`d onto
/// a black canvas in **track-then-timeline order** — so later tracks composite on
/// top and gaps fall through to black, matching export framing. The output canvas
/// keeps the export aspect ratio capped to `max_width`. Static blends
/// (mid-crossfade dissolve, dip-to-black) are intentionally *not* reproduced; the
/// still shows the frame each visible clip contributes at `t`.
fn build_timeline_frame_args(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    quality: u8,
) -> Result<Vec<String>> {
    // Same gate as the export, so the still shows the cut that would render.
    let rendered = timeline.for_render();
    let timeline = &rendered;

    let fmt = export_format(timeline, assets, opts);
    // Output canvas: export aspect ratio, capped to `max_width`, even dimensions.
    let ow = (max_width.min(fmt.width).max(2)) & !1;
    let oh = ((((ow as u64) * (fmt.height as u64)) / (fmt.width.max(1) as u64)) as u32).max(2) & !1;
    let t = t.max(0.0);
    let asset_of = |id| assets.iter().find(|a: &&Asset| a.id == id);

    // Active video clips at `t`, in composite order (tracks in list order, clips
    // within a track in timeline order), paired with their source time.
    let mut active: Vec<(&Clip, f64)> = Vec::new();
    for track in &timeline.tracks {
        if track.kind != StreamKind::Video {
            continue;
        }
        let mut order: Vec<usize> = (0..track.clips.len()).collect();
        order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
        for &ci in &order {
            let clip = &track.clips[ci];
            if t < clip.timeline_start || t >= clip.timeline_end() {
                continue;
            }
            let off = (t - clip.timeline_start) * clip.speed_mag();
            let raw = if clip.is_reversed() {
                clip.source_out - off
            } else {
                clip.source_in + off
            };
            let dur = asset_of(clip.asset_id).map(|a| a.duration).unwrap_or(clip.source_out);
            active.push((clip, raw.clamp(0.0, dur.max(0.0))));
        }
    }

    let mut args: Vec<String> = vec!["-hide_banner".to_string(), "-loglevel".to_string(), "error".to_string()];
    for (clip, src) in &active {
        let asset = asset_of(clip.asset_id).ok_or(Error::AssetNotFound(clip.asset_id))?;
        // A still has a single frame at t=0 (`trim=end_frame=1` in the chain picks
        // it up); seeking into it decodes nothing, so skip the `-ss` (and any
        // decode acceleration) for images.
        if !asset.is_image() {
            if let Some(hw) = opts
                .hwaccel
                .as_deref()
                .filter(|h| !h.is_empty() && !h.eq_ignore_ascii_case("none"))
            {
                args.push("-hwaccel".to_string());
                args.push(hw.to_string());
            }
            args.push("-ss".to_string());
            args.push(format!("{src:.3}"));
        }
        args.push("-i".to_string());
        args.push(asset.path.clone());
    }

    // Black base + each active clip's still chain (its transform sampled at `t`,
    // so a keyframed clip shows its pose), overlaid in order, then the text
    // overlays live at `t`. A trailing `null` makes the final label always
    // `[outv]` (so an empty timeline still maps cleanly).
    let live: Vec<&TextOverlay> = timeline.overlays.iter().filter(|o| t >= o.start && t < o.end).collect();
    let canvas = StillCanvas {
        w: ow,
        h: oh,
        fit: fmt.fit,
        sf: fmt.scale_flags(),
    };
    let mut chains: Vec<String> = vec![format!("color=c=black:s={ow}x{oh}:d=0.1[base]")];
    let mut cur = "base".to_string();
    for (n, (clip, _)) in active.iter().enumerate() {
        let local = (t - clip.timeline_start).max(0.0);
        let tf = clip.transform_at(local);
        let rf = clip.reframe_at(local);
        chains.push(format!(
            "[{n}:v]{chain}[v{n}]",
            chain = still_clip_chain(&tf, &clip.color, &clip.effects, rf.as_ref(), &canvas)
        ));
        let out = format!("ov{n}");
        chains.push(format!("[{cur}][v{n}]{overlay}[{out}]", overlay = still_overlay(&tf)));
        cur = out;
    }
    for (oi, ov) in live.iter().enumerate() {
        let out = format!("txt{oi}");
        chains.push(format!("[{cur}]{f}[{out}]", f = drawtext_still(ov, oh, t)));
        cur = out;
    }
    chains.push(format!("[{cur}]null[outv]"));
    let filter = chains.join(";");

    args.extend([
        "-filter_complex".to_string(),
        filter,
        "-map".to_string(),
        "[outv]".to_string(),
        "-frames:v".to_string(),
        "1".to_string(),
        "-q:v".to_string(),
        quality.to_string(),
        "-f".to_string(),
        "image2pipe".to_string(),
        "-vcodec".to_string(),
        "mjpeg".to_string(),
        "pipe:1".to_string(),
    ]);
    Ok(args)
}

/// The frame a [`timeline_frame`] composite renders into: the delivery aspect
/// capped to the caller's preview width, how footage of another shape meets it,
/// and the scaler flags. Bundled because these four always travel together —
/// every one of them comes from the same `ExportFormat`.
struct StillCanvas {
    w: u32,
    h: u32,
    fit: Fit,
    sf: String,
}

/// The still video chain for one clip in a [`timeline_frame`] composite: take a
/// single decoded frame, then apply the same 360 reprojection / crop /
/// fit-or-transform / color / opacity / rotation geometry as
/// [`video_clip_chain`], minus every time-domain step (trim/setpts/fps/fades)
/// since the `-ss` input seek already positioned it.
///
/// `reframe` is the clip's camera already **sampled** at the requested instant.
/// The still pipeline has no timeline clock to run `sendcmd` against, so an
/// animated reframe resolves to a constant here — which is exactly what the
/// export chain's commands will have set `v360` to at the same timestamp.
fn still_clip_chain(
    tf: &Transform,
    color: &Color,
    effects: &[VideoEffect],
    reframe: Option<&ResolvedReframe>,
    canvas: &StillCanvas,
) -> String {
    let StillCanvas { w: ow, h: oh, fit, sf } = canvas;
    let (ow, oh, fit) = (*ow, *oh, *fit);
    let chroma = effects.iter().any(|e| e.produces_alpha());
    let needs_alpha = (!tf.is_identity() && tf.needs_alpha()) || chroma;
    let mut p: Vec<String> = vec!["trim=end_frame=1".to_string(), "setpts=PTS-STARTPTS".to_string()];
    let crop = tf.has_crop().then(|| {
        let cw = (1.0 - tf.crop_left - tf.crop_right).max(0.0);
        let ch = (1.0 - tf.crop_top - tf.crop_bottom).max(0.0);
        format!(
            "crop=w=iw*{cw}:h=ih*{ch}:x=iw*{cl}:y=ih*{ct}",
            cl = tf.crop_left,
            ct = tf.crop_top
        )
    });
    // Mirrors `video_clip_chain`: crop follows reprojection. There is no `setpts`
    // to step around here, so one order serves both cases.
    if let Some(r) = reframe {
        p.push(reframe_filter(r, ow, oh, PREVIEW_INTERP, None));
    }
    p.extend(crop);
    // The same base fit `video_clip_chain` applies, so a Cover delivery crops in
    // the scrubbed still exactly as it will in the file. Letterboxing here while
    // the export cropped meant the one frame you look at while cutting was the
    // one shape you were never going to ship.
    match fit {
        Fit::Contain => p.push(format!("scale={ow}:{oh}:force_original_aspect_ratio=decrease{sf}")),
        Fit::Cover => {
            p.push(format!("scale={ow}:{oh}:force_original_aspect_ratio=increase{sf}"));
            p.push(format!("crop={ow}:{oh}"));
        }
    }
    if tf.is_identity() {
        // Cover already fills the frame; padding it would be a no-op that still
        // costs a filter, so only the letterboxed path needs it.
        if fit == Fit::Contain {
            p.push(format!("pad={ow}:{oh}:(ow-iw)/2:(oh-ih)/2"));
        }
    } else if (tf.scale - 1.0).abs() > 1e-9 {
        p.push(format!("scale=iw*{sc}:ih*{sc}{sf}", sc = tf.scale));
    }
    p.push("setsar=1".to_string());
    if !color.is_identity() {
        p.push(eq_filter(color));
    }
    for e in effects {
        if let Some(f) = video_effect_filter(e) {
            p.push(f);
        }
    }
    if needs_alpha {
        p.push("format=yuva420p".to_string());
    }
    for e in effects {
        if let Some(f) = chroma_filter(e) {
            p.push(f);
        }
    }
    if tf.opacity < 1.0 {
        p.push(format!("colorchannelmixer=aa={}", tf.opacity));
    }
    if tf.rotation != 0.0 {
        let rad = tf.rotation.to_radians();
        p.push(format!("rotate={rad}:fillcolor=none:ow=rotw({rad}):oh=roth({rad})"));
    }
    p.join(",")
}

/// The `overlay` placement for a clip in a [`timeline_frame`] composite: a full
/// frame for an identity transform, else centered with the clip's fractional
/// `pos_x`/`pos_y` offset (matching the export overlay positions).
fn still_overlay(t: &Transform) -> String {
    if t.is_identity() {
        "overlay=(W-w)/2:(H-h)/2".to_string()
    } else {
        format!("overlay=x=(W-w)/2+({px})*W:y=(H-h)/2+({py})*H", px = t.pos_x, py = t.pos_y)
    }
}

/// The audio filter chain for one clip (between `[i:a]` and `[a{i}]`): trim,
/// optional reverse / tempo, gain, fades (including transition cross-fades) and
/// delay to the clip's timeline position. Defaults reduce to the original chain.
fn audio_clip_chain(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx, layout: &str) -> String {
    let s = clip.speed_mag();
    let dur = clip.duration() + fx.tail;
    // Mirror the video crossfade tail (extends below source_in when reversed) and
    // the same input-side `-ss` fast-seek, so the atrim is relative to the seek.
    let (trim_start, trim_end) = clip_source_window(clip, fx);
    let seek = clip_seek(trim_start);
    let delay_ms = (clip.timeline_start * 1000.0).round().max(0.0) as i64;
    let fi = clip.fade_in + fx.black_in + fx.xfade_in;
    let fo = clip.fade_out + fx.black_out + fx.tail;

    let mut p: Vec<String> = Vec::new();
    p.push(format!("atrim=start={}:end={}", trim_start - seek, trim_end - seek));
    p.push("asetpts=PTS-STARTPTS".to_string());
    if clip.is_reversed() {
        p.push("areverse".to_string());
    }
    if (s - 1.0).abs() > 1e-9 {
        p.push(atempo_chain(s));
    }
    p.push(format!("volume={}", clip.volume));
    // Per-clip audio effects (EQ / compressor / gate / filters) in author order,
    // after the clip gain.
    for e in &clip.audio {
        p.push(audio_effect_filter(e));
    }
    if fi > 0.0 {
        p.push(format!("afade=t=in:st=0:d={}", fi.clamp(0.0, dur)));
    }
    if fo > 0.0 {
        p.push(format!("afade=t=out:st={}:d={}", (dur - fo).max(0.0), fo.clamp(0.0, dur)));
    }
    p.push(format!(
        "aformat=sample_rates={sr}:channel_layouts={layout}",
        sr = fmt.sample_rate
    ));
    p.push(format!("adelay={delay_ms}:all=1"));
    p.join(",")
}

/// Decompose a tempo change into `atempo` steps each within ffmpeg's supported
/// `[0.5, 2.0]` range (e.g. 4× → `atempo=2.0,atempo=2.0`).
fn atempo_chain(speed: f64) -> String {
    let mut s = speed;
    let mut parts: Vec<String> = Vec::new();
    while s > 2.0 {
        parts.push("atempo=2.0".to_string());
        s /= 2.0;
    }
    while s < 0.5 {
        parts.push("atempo=0.5".to_string());
        s *= 2.0;
    }
    parts.push(format!("atempo={s}"));
    parts.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Asset, Clip, Delivery, StreamInfo, StreamKind, Timeline, Track};
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn parses_silence_pairs() {
        let log = "\
[silencedetect @ 0x1] silence_start: 12.5
[silencedetect @ 0x1] silence_end: 14.0 | silence_duration: 1.5
[silencedetect @ 0x1] silence_start: 60
[silencedetect @ 0x1] silence_end: 63.2 | silence_duration: 3.2
";
        let ranges = parse_silence(log);
        assert_eq!(ranges.len(), 2);
        assert!((ranges[0].start - 12.5).abs() < 1e-9);
        assert!((ranges[0].end - 14.0).abs() < 1e-9);
        assert!((ranges[1].end - 63.2).abs() < 1e-9);
    }

    #[test]
    fn unterminated_silence_is_dropped() {
        let ranges = parse_silence("silence_start: 5.0\n");
        assert!(ranges.is_empty());
    }

    #[test]
    fn parses_scene_times() {
        let log = "\
[Parsed_showinfo_1 @ 0x1] n:0 pts:0 pts_time:0 duration_time:0.04
[Parsed_showinfo_1 @ 0x1] n:1 pts:720 pts_time:30.0 duration_time:0.04
[Parsed_showinfo_1 @ 0x1] n:2 pts:1800 pts_time:75.5 duration_time:0.04
";
        let scenes = parse_scenes(log);
        assert_eq!(scenes, vec![0.0, 30.0, 75.5]);
    }

    #[test]
    fn parses_rational_fps() {
        assert!((parse_rational("30000/1001").unwrap() - 29.97).abs() < 0.01);
        assert_eq!(parse_rational("30/1"), Some(30.0));
        assert_eq!(parse_rational("25/0"), None);
    }

    #[test]
    fn peaks_have_requested_length_and_range() {
        let samples: Vec<f32> = (0..1000).map(|i| ((i as f32) / 1000.0) - 0.5).collect();
        let p = peaks(&samples, 16);
        assert_eq!(p.len(), 16);
        assert!(p.iter().all(|&v| (0.0..=1.0).contains(&v)));
    }

    #[test]
    fn peak_downsampler_is_length_independent_and_keeps_the_peak() {
        // Stream far more samples than 2*buckets so the halving path runs many
        // times, with one clipping spike buried in the middle.
        let buckets = 16;
        let mut down = PeakDownsampler::new(buckets);
        for i in 0..100_000u32 {
            let s = if i == 40_000 { 2.0 } else { ((i % 7) as f32) / 50.0 };
            down.push(s);
        }
        let out = down.finish();
        // Exactly `buckets` long regardless of how many samples streamed through.
        assert_eq!(out.len(), buckets);
        assert!(out.iter().all(|&v| (0.0..=1.0).contains(&v)));
        // The clipping spike survives the downsample, clamped into range.
        let max = out.iter().cloned().fold(0.0_f32, f32::max);
        assert!((max - 1.0).abs() < 1e-6, "peak should be preserved, got {max}");
    }

    #[test]
    fn peak_downsampler_handles_fewer_samples_than_buckets() {
        let mut down = PeakDownsampler::new(16);
        for &s in &[0.1, 0.9, 0.3] {
            down.push(s);
        }
        let out = down.finish();
        assert_eq!(out.len(), 16);
        let max = out.iter().cloned().fold(0.0_f32, f32::max);
        assert!((max - 0.9).abs() < 1e-6, "got {max}");
    }

    #[test]
    fn proxy_args_are_all_intra_audioless_and_keep_timing() {
        let args = build_proxy_args("/in.mov", "/out.mp4", 3, PROXY_MAX_WIDTH, "libx264", None);
        // All-intra: every frame a keyframe, so a preview seek decodes one frame.
        let gop = args.iter().position(|a| a == "-g").expect("-g present");
        assert_eq!(args[gop + 1], "1");
        // Thread-capped so a background proxy encode leaves cores for the GUI/agent.
        let threads = args.iter().position(|a| a == "-threads").expect("-threads present");
        assert_eq!(args[threads + 1], "3");
        // Audio is dropped — the proxy is only ever decoded for video frames.
        assert!(args.contains(&"-an".to_string()));
        // Downscale only: the proxy must NOT retime, trim or seek, or a source
        // time would no longer map 1:1 onto it (the invariant preview seek math
        // and the shared clip source-window math both rely on).
        assert!(!args.contains(&"-r".to_string()), "proxy must not change fps");
        assert!(!args.contains(&"-t".to_string()), "proxy must not trim duration");
        assert!(!args.contains(&"-ss".to_string()), "proxy must not seek");
        assert!(args.iter().any(|a| a.contains("scale='min(1280,iw)':-2")));
        assert!(args.contains(&"libx264".to_string()));
        // The output is a `.part` temp file the muxer can't be inferred from, so
        // the format must be stated or ffmpeg refuses to start.
        assert!(
            args.windows(2).any(|w| w[0] == "-f" && w[1] == "mp4"),
            "muxer must be explicit"
        );
        // The source is the input; the proxy is the (final) output.
        let input = args.iter().position(|a| a == "-i").expect("-i present");
        assert_eq!(args[input + 1], "/in.mov");
        assert_eq!(args.last().unwrap(), "/out.mp4");
    }

    #[test]
    fn proxy_args_with_hw_encoder_spell_quality_per_family_and_stay_all_intra() {
        // NVENC: CRF intent becomes -rc vbr -cq, input format nv12, and the
        // all-intra / no-retime invariants hold exactly as in software.
        let args = build_proxy_args("/in.mov", "/out.mp4", 3, PROXY_MAX_WIDTH, "h264_nvenc", Some("auto"));
        assert!(args.windows(2).any(|w| w[0] == "-hwaccel" && w[1] == "auto"));
        assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "h264_nvenc"));
        assert!(args.windows(2).any(|w| w[0] == "-cq" && w[1] == "24"));
        assert!(!args.contains(&"-crf".to_string()), "hw encoders have no -crf");
        assert!(args.windows(2).any(|w| w[0] == "-pix_fmt" && w[1] == "nv12"));
        assert!(args.windows(2).any(|w| w[0] == "-g" && w[1] == "1"));
        assert!(!args.contains(&"-ss".to_string()) && !args.contains(&"-r".to_string()));
        // The `-hwaccel` is an input option: it must precede the `-i`.
        let hw = args.iter().position(|a| a == "-hwaccel").unwrap();
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert!(hw < input);
    }

    #[test]
    fn stitch_args_with_hw_encoder_accelerate_both_lens_decodes() {
        let args = build_stitch_args("/dcim/f_00_.mp4", "/dcim/r_10_.mp4", "/cache/out.mp4", "hevc_nvenc", Some("auto"));
        // One `-hwaccel` per lens input, each before its `-i`.
        assert_eq!(args.iter().filter(|a| *a == "-hwaccel").count(), 2);
        assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "hevc_nvenc"));
        assert!(args.windows(2).any(|w| w[0] == "-cq" && w[1] == "15"));
        assert!(args.windows(2).any(|w| w[0] == "-pix_fmt" && w[1] == "nv12"));
        // The stitch geometry is untouched by the encoder choice.
        let graph = args
            .iter()
            .position(|a| a == "-filter_complex")
            .map(|i| args[i + 1].clone())
            .expect("-filter_complex present");
        assert!(graph.contains("hstack=shortest=1"));
        assert!(graph.contains("w=5760:h=2880"));
    }

    #[test]
    fn quality_args_map_the_same_intent_per_family() {
        assert_eq!(quality_args("libx264", 24).join(" "), "-preset veryfast -crf 24");
        assert_eq!(quality_args("h264_qsv", 24).join(" "), "-global_quality 24");
        assert_eq!(quality_args("h264_amf", 24).join(" "), "-rc cqp -qp_i 24 -qp_p 24");
        // VideoToolbox flips the scale: lower CRF must become higher quality.
        let vt15: u32 = quality_args("hevc_videotoolbox", 15)[1].parse().unwrap();
        let vt28: u32 = quality_args("hevc_videotoolbox", 28)[1].parse().unwrap();
        assert!(vt15 > vt28);
    }

    #[test]
    fn timeline_frame_hwaccel_is_per_input_and_opt_in() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let assets = vec![asset.clone()];
        let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
        // Default stays byte-identical: no -hwaccel.
        let plain = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 1.0, 960, 4).unwrap();
        assert!(!plain.contains(&"-hwaccel".to_string()));
        // Requested: emitted as an input option (before the -i it applies to).
        let hw = ExportOptions {
            hwaccel: Some("auto".to_string()),
            ..ExportOptions::default()
        };
        let args = build_timeline_frame_args(&timeline, &assets, &hw, 1.0, 960, 4).unwrap();
        let at = args.iter().position(|a| a == "-hwaccel").expect("-hwaccel present");
        assert_eq!(args[at + 1], "auto");
        assert!(at < args.iter().position(|a| a == "-i").unwrap());
    }

    #[test]
    fn insta360_lens_pairs_the_two_capture_files() {
        // Either lens resolves to the other, and to one shared display name.
        let front = "VID_20220625_140410_00_008.mp4";
        let rear = "VID_20220625_140410_10_008.mp4";
        assert_eq!(insta360_lens(front), Some(("00", rear.to_string())));
        assert_eq!(insta360_lens(rear), Some(("10", front.to_string())));
        assert_eq!(insta360_pair_name(front).as_deref(), Some("VID_20220625_140410_008.mp4"));
        assert_eq!(insta360_pair_name(rear).as_deref(), Some("VID_20220625_140410_008.mp4"));
    }

    #[test]
    fn insta360_lens_ignores_everything_else() {
        // The lens token is matched positionally, so digits elsewhere in the
        // name — a time ending in 10, a clip numbered 00 — are not lens tokens.
        for name in [
            "VID_20220625_141000_008.mp4",
            "VID_20220625_140410_20_008.mp4",
            "VID_20220625_140410_00_008.mov",
            "MVI_20220625_140410_00_008.mp4",
            "holiday.mp4",
            "VID_00_.mp4",
        ] {
            assert_eq!(insta360_lens(name), None, "{name} must not read as a lens file");
        }
    }

    #[test]
    fn stitch_args_reproject_the_lens_pair_to_equirect() {
        let args = build_stitch_args("/dcim/front_00_.mp4", "/dcim/rear_10_.mp4", "/cache/out.mp4", "libx264", None);
        // Front lens is input 0 — hstack packs it into the left half, which is
        // the front hemisphere `v360=dfisheye` expects.
        let inputs: Vec<&String> = args
            .iter()
            .enumerate()
            .filter(|(i, _)| *i > 0 && args[i - 1] == "-i")
            .map(|(_, a)| a)
            .collect();
        assert_eq!(inputs, vec!["/dcim/front_00_.mp4", "/dcim/rear_10_.mp4"]);
        let graph = args
            .iter()
            .position(|a| a == "-filter_complex")
            .map(|i| args[i + 1].clone())
            .expect("-filter_complex present");
        assert!(graph.contains("hstack=shortest=1"));
        assert!(graph.contains("v360=dfisheye:e"));
        assert!(graph.contains("ih_fov=190"), "the lenses overshoot the hemisphere");
        assert!(graph.contains("roll=180"), "both lenses record upside down");
        assert!(graph.contains("w=5760:h=2880"));
        // The capture's audio rides along untouched; the output is the stitch.
        assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "0:a?"));
        assert!(args.windows(2).any(|w| w[0] == "-c:a" && w[1] == "copy"));
        assert_eq!(args.last().unwrap(), "/cache/out.mp4");
    }

    #[test]
    fn stitched_path_is_shared_by_both_lens_orders() {
        // The pair is keyed by both files, so whichever lens was imported the
        // cached stitch is the same file (the second import is a cache hit).
        if let (Some(a), Some(b), Some(other)) = (
            stitched_path(Path::new("/dcim/a_00_.mp4"), Path::new("/dcim/a_10_.mp4")),
            stitched_path(Path::new("/dcim/a_00_.mp4"), Path::new("/dcim/a_10_.mp4")),
            stitched_path(Path::new("/dcim/b_00_.mp4"), Path::new("/dcim/b_10_.mp4")),
        ) {
            assert_eq!(a, b);
            assert_ne!(a, other);
            assert!(a.to_string_lossy().contains("stitched"));
        }
    }

    #[test]
    fn insta360_pair_needs_square_frames_and_a_sibling_on_disk() {
        // Non-square (already stitched, or ordinary video) is never a lens file,
        // and a lone lens file with no sibling next to it can't be stitched.
        assert_eq!(
            insta360_pair(Path::new("/dcim/VID_1_2_00_3.mp4"), Some(5760), Some(2880)),
            None
        );
        assert_eq!(
            insta360_pair(Path::new("/dcim/VID_1_2_00_3.mp4"), Some(3072), Some(3072)),
            None,
            "no sibling on disk"
        );
    }

    #[test]
    fn proxy_path_is_deterministic_and_distinct_per_source() {
        // Same source → same proxy path on every call (so a re-import / new
        // session reuses the cached proxy); different sources → different files.
        // Skipped when the platform exposes no cache directory.
        if let (Some(a1), Some(a2), Some(b)) = (
            proxy_path(Path::new("/media/a.mov"), PROXY_MAX_WIDTH),
            proxy_path(Path::new("/media/a.mov"), PROXY_MAX_WIDTH),
            proxy_path(Path::new("/media/b.mov"), PROXY_MAX_WIDTH),
        ) {
            assert_eq!(a1, a2);
            assert_ne!(a1, b);
            assert!(a1.to_string_lossy().contains("proxies"));
            assert_eq!(a1.extension().and_then(|e| e.to_str()), Some("mp4"));
        }
    }

    #[test]
    fn spherical_sources_proxy_larger_and_under_their_own_key() {
        // Reframing crops ~100° out of 360, so a 360 proxy keeps more pixels.
        assert_eq!(proxy_width(None), PROXY_MAX_WIDTH);
        assert_eq!(proxy_width(Some(Projection::Flat)), PROXY_MAX_WIDTH);
        assert_eq!(proxy_width(Some(Projection::Equirect)), PROXY_MAX_WIDTH_SPHERICAL);
        assert_eq!(proxy_width(Some(Projection::DualFisheye)), PROXY_MAX_WIDTH_SPHERICAL);
        assert!(build_proxy_args("/in.mp4", "/out.mp4", 1, PROXY_MAX_WIDTH_SPHERICAL, "libx264", None)
            .iter()
            .any(|a| a.contains("scale='min(3072,iw)':-2")));
        // Marking an asset as 360 must not silently reuse the small proxy that
        // was rendered while it looked flat, so the width is part of the key.
        if let (Some(flat), Some(sphere)) = (
            proxy_path(Path::new("/media/a.mov"), PROXY_MAX_WIDTH),
            proxy_path(Path::new("/media/a.mov"), PROXY_MAX_WIDTH_SPHERICAL),
        ) {
            assert_ne!(flat, sphere);
        }
    }

    fn test_asset(streams: Vec<StreamInfo>) -> Asset {
        Asset {
            id: Uuid::new_v4(),
            path: "/x.mp4".into(),
            name: "x.mp4".into(),
            duration: 100.0,
            streams,
            imported_at: Utc::now(),
            source_paths: Vec::new(),
        }
    }

    fn video_stream(w: u32, h: u32, fps: f64) -> StreamInfo {
        StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(w),
            height: Some(h),
            fps: Some(fps),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
        }
    }

    fn audio_stream(rate: u32, channels: u16) -> StreamInfo {
        StreamInfo {
            index: 1,
            kind: StreamKind::Audio,
            codec: "aac".into(),
            width: None,
            height: None,
            fps: None,
            sample_rate: Some(rate),
            channels: Some(channels),
            image: false,
            projection: None,
        }
    }

    fn image_stream(w: u32, h: u32) -> StreamInfo {
        StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "png".into(),
            width: Some(w),
            height: Some(h),
            fps: None,
            sample_rate: None,
            channels: None,
            image: true,
            projection: None,
        }
    }

    #[test]
    fn filter_complex_positions_and_overlays_clips() {
        // One clip with audio, one from a video-only asset, on one video track.
        let with_audio = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
        let video_only = test_asset(vec![video_stream(3840, 2160, 24.0)]);
        let assets = vec![with_audio.clone(), video_only.clone()];

        let mut first = make_clip(with_audio.id, 0.0, 5.0, 0.0);
        first.volume = 0.5;
        let timeline = single(vec![first, make_clip(video_only.id, 2.0, 4.0, 5.0)]);

        let opts = ExportOptions::default();
        let fmt = export_format(&timeline, &assets, &opts);
        // Output shape comes from the first video/audio-bearing clips.
        assert_eq!((fmt.width, fmt.height), (1920, 1080));
        assert_eq!(fmt.sample_rate, 48_000);

        let g = build_filter_complex(
            &timeline,
            &assets,
            &fmt,
            timeline.duration(),
            &ExportOptions::default(),
            true,
            true,
            &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
        );
        assert!(g.has_video && g.has_audio);
        let f = g.filter;
        // A black canvas spanning the whole timeline, then one positioned
        // overlay per video clip; the last overlay writes [outv].
        assert!(f.contains("color=c=black:s=1920x1080"));
        assert!(f.contains("overlay=eof_action=pass:enable='between(t,0,5)'"));
        assert!(f.contains("enable='between(t,5,7)'")); // second clip: start 5, dur 2
        assert!(f.contains("[outv]"));
        assert!(f.contains("volume=0.5"));
        assert!(f.contains("[0:v]trim=start=0:end=5"));
        assert!(f.contains("setpts=PTS-STARTPTS+5/TB")); // second clip positioned at 5s
                                                         // Every video segment is scaled/padded to the common resolution.
        assert_eq!(f.matches("scale=1920:1080").count(), 2);
        assert!(f.contains("format=yuv420p"));
        // Only the audio-bearing clip contributes audio; it is summed via amix
        // (no synthesized silence for the video-only clip any more).
        assert!(f.contains("[0:a]atrim=start=0:end=5"));
        assert!(f.contains("aformat=sample_rates=48000:channel_layouts=stereo"));
        assert!(f.contains("amix=inputs=1:normalize=0"));
        assert!(!f.contains("[1:a]"));
        assert!(!f.contains("anullsrc"));
    }

    #[test]
    fn filter_complex_layers_multiple_tracks() {
        // Interview on V1 (video+audio), B-roll over it on V2 (video only).
        let interview = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
        let broll = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let assets = vec![interview.clone(), broll.clone()];

        let timeline = timeline_of(vec![
            video_track(vec![make_clip(interview.id, 0.0, 20.0, 0.0)]),
            video_track(vec![make_clip(broll.id, 0.0, 6.0, 4.0)]), // overlaps 4..10
        ]);
        let fmt = export_format(&timeline, &assets, &ExportOptions::default());
        let g = build_filter_complex(
            &timeline,
            &assets,
            &fmt,
            timeline.duration(),
            &ExportOptions::default(),
            true,
            true,
            &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
        );
        let f = g.filter;
        // Two overlays: B-roll (input 1) composites on top of the interview.
        assert_eq!(f.matches("overlay=eof_action=pass").count(), 2);
        assert!(f.contains("[1:v]trim=start=0:end=6"));
        assert!(f.contains("enable='between(t,4,10)'"));
        // Only the interview carries audio, so the mix has one input.
        assert!(g.has_audio);
        assert!(f.contains("amix=inputs=1"));
        assert!(!f.contains("[1:a]"));
    }

    #[test]
    fn filter_complex_audio_only_timeline_has_no_video() {
        let audio_asset = test_asset(vec![audio_stream(44_100, 2)]);
        let assets = vec![audio_asset.clone()];
        let timeline = timeline_of(vec![audio_track(vec![make_clip(audio_asset.id, 0.0, 10.0, 3.0)])]);
        let fmt = export_format(&timeline, &assets, &ExportOptions::default());
        let g = build_filter_complex(
            &timeline,
            &assets,
            &fmt,
            timeline.duration(),
            &ExportOptions::default(),
            true,
            true,
            &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
        );
        assert!(!g.has_video);
        assert!(g.has_audio);
        // Positioned at 3s on the timeline via adelay; no picture canvas.
        assert!(g.filter.contains("adelay=3000:all=1"));
        assert!(!g.filter.contains("color=c=black"));
    }

    #[test]
    fn filter_complex_applies_fades_to_picture_and_audio() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
        let assets = vec![asset.clone()];
        let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        clip.fade_in = 0.5;
        clip.fade_out = 1.0;
        let timeline = single(vec![clip]);

        let fmt = export_format(&timeline, &assets, &ExportOptions::default());
        let f = build_filter_complex(
            &timeline,
            &assets,
            &fmt,
            timeline.duration(),
            &ExportOptions::default(),
            true,
            true,
            &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
        )
        .filter;
        // Picture fades sit just before the pixel-format normalize.
        assert!(f.contains("fade=t=in:st=0:d=0.5,fade=t=out:st=9:d=1,format=yuv420p"));
        // Audio fades sit just before the audio-format normalize. The out fade
        // starts at (duration - fade_out) = 9s.
        assert!(f.contains("afade=t=in:st=0:d=0.5,afade=t=out:st=9:d=1,aformat"));
    }

    #[test]
    fn filter_complex_omits_fades_when_zero() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
        let assets = vec![asset.clone()];
        let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
        let fmt = export_format(&timeline, &assets, &ExportOptions::default());
        let f = build_filter_complex(
            &timeline,
            &assets,
            &fmt,
            timeline.duration(),
            &ExportOptions::default(),
            true,
            true,
            &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
        )
        .filter;
        assert!(!f.contains("fade="), "no fade filter when fades are zero");
        assert!(!f.contains("afade="), "no afade filter when fades are zero");
    }

    #[test]
    fn export_format_falls_back_to_defaults() {
        let timeline = Timeline {
            tracks: Vec::new(),
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
        };
        let fmt = export_format(&timeline, &[], &ExportOptions::default());
        assert_eq!((fmt.width, fmt.height), (1920, 1080));
        assert_eq!(fmt.channel_layout(), "stereo");
    }

    #[test]
    fn export_args_reference_the_original_source_not_a_proxy() {
        // Hard invariant: export always reads the original asset; preview proxies
        // are a preview-only optimisation. The export builder has no proxy
        // knowledge, so its `-i` inputs are the asset paths verbatim and never a
        // cached file under .../proxies/.
        let asset = test_asset(vec![video_stream(3840, 2160, 60.0), audio_stream(48_000, 2)]);
        let assets = vec![asset.clone()];
        let timeline = single(vec![make_clip(asset.id, 1.0, 5.0, 0.0)]);
        let args = build_export_args(&timeline, &assets, "/out.mp4", &ExportOptions::default()).unwrap();
        let input = args.iter().position(|a| a == "-i").expect("-i present");
        assert_eq!(args[input + 1], asset.path);
        assert!(
            !args.iter().any(|a| a.contains("proxies")),
            "export must not reference a proxy"
        );
    }

    fn make_clip(asset_id: uuid::Uuid, source_in: f64, source_out: f64, timeline_start: f64) -> Clip {
        Clip::new(asset_id, source_in, source_out, timeline_start)
    }

    fn video_track(clips: Vec<Clip>) -> Track {
        Track {
            clips,
            ..Track::new(StreamKind::Video, "V1")
        }
    }

    fn audio_track(clips: Vec<Clip>) -> Track {
        Track {
            clips,
            ..Track::new(StreamKind::Audio, "A1")
        }
    }

    fn timeline_of(tracks: Vec<Track>) -> Timeline {
        Timeline {
            tracks,
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
        }
    }

    /// A timeline with a single video track holding `clips`.
    fn single(clips: Vec<Clip>) -> Timeline {
        timeline_of(vec![video_track(clips)])
    }

    // ---- mute / solo / clip-enable gating ----------------------------------

    /// The graph builders take the timeline through `Timeline::for_render`, so a
    /// muted track or a disabled clip must never reach argv. Tested here rather
    /// than only on the model, because the failure mode is a *missing call*.
    #[test]
    fn muted_tracks_and_disabled_clips_never_reach_the_export_args() {
        let keep = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
        // Distinct paths: `test_asset` reuses one, which would make the assertion vacuous.
        let drop = Asset {
            path: "/gated.mp4".into(),
            ..test_asset(vec![video_stream(1920, 1080, 30.0)])
        };
        let assets = vec![keep.clone(), drop.clone()];

        let mut disabled = make_clip(drop.id, 0.0, 4.0, 10.0);
        disabled.enabled = false;
        let timeline = timeline_of(vec![
            video_track(vec![make_clip(keep.id, 0.0, 5.0, 0.0), disabled]),
            Track {
                muted: true,
                ..video_track(vec![make_clip(drop.id, 0.0, 6.0, 0.0)])
            },
        ]);

        let args = build_export_args(&timeline, &assets, "/out.mp4", &ExportOptions::default()).unwrap();
        let argv = args.join(" ");
        assert!(argv.contains(&keep.path), "the kept clip's input is missing");
        assert!(
            !argv.contains(&drop.path),
            "a muted track and a disabled clip both still reached argv: {argv}"
        );
    }

    /// Soloing gates by kind, and the still path must agree with the export.
    #[test]
    fn solo_gates_the_timeline_still_by_kind() {
        let a = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let b = Asset {
            path: "/soloed.mp4".into(),
            ..test_asset(vec![video_stream(1920, 1080, 30.0)])
        };
        let assets = vec![a.clone(), b.clone()];
        let timeline = timeline_of(vec![
            video_track(vec![make_clip(a.id, 0.0, 5.0, 0.0)]),
            Track {
                solo: true,
                ..video_track(vec![make_clip(b.id, 0.0, 5.0, 0.0)])
            },
        ]);

        let args = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 2.0, 960, 4).unwrap();
        let argv = args.join(" ");
        assert!(argv.contains(&b.path), "the soloed track should be the one shown");
        assert!(!argv.contains(&a.path), "an unsoloed track leaked into the still: {argv}");
    }

    // ---- per-clip video / audio effects, keyframes, text overlays -----------

    #[test]
    fn video_effects_render_with_chroma_keeping_alpha() {
        let fmt = ExportFormat::default();
        let mut clip = make_clip(Uuid::new_v4(), 0.0, 5.0, 0.0);
        clip.effects = vec![
            VideoEffect::Blur { sigma: 8.0 },
            VideoEffect::ChromaKey {
                color: "green".into(),
                similarity: 0.1,
                blend: 0.0,
            },
        ];
        let chain = video_clip_chain(&clip, &fmt, &ClipFx::default(), false, "c0");
        // Color-space blur runs before the alpha plane is established, chroma key
        // after it; the terminal yuv420p flatten is suppressed so alpha survives.
        let gi = chain.find("gblur=sigma=8").expect("blur");
        let yi = chain.find("format=yuva420p").expect("alpha plane");
        let ci = chain.find("chromakey=green:0.1:0").expect("chroma key");
        assert!(gi < yi && yi < ci, "order: blur < yuva < chroma in {chain}");
        assert!(!chain.contains("format=yuv420p"), "alpha must not be flattened: {chain}");
    }

    #[test]
    fn audio_effects_chain_after_gain_in_order() {
        let fmt = ExportFormat::default();
        let mut clip = make_clip(Uuid::new_v4(), 0.0, 5.0, 1.0);
        clip.audio = vec![
            AudioEffect::Highpass { hz: 80.0 },
            AudioEffect::Compressor {
                threshold_db: -18.0,
                ratio: 3.0,
                attack_ms: 20.0,
                release_ms: 250.0,
                makeup_db: 6.0,
            },
        ];
        let chain = audio_clip_chain(&clip, &fmt, &ClipFx::default(), "stereo");
        let vi = chain.find("volume=").expect("gain");
        let hi = chain.find("highpass=f=80").expect("highpass");
        let ai = chain.find("acompressor=").expect("compressor");
        assert!(vi < hi && hi < ai, "effects follow the gain in author order: {chain}");
        assert!(chain.contains("ratio=3"));
    }

    #[test]
    fn keyframe_expr_is_piecewise_linear_and_clamped() {
        let e = keyframe_expr(&[(0.0, 10.0), (4.0, 20.0)], "t", 2.0);
        // Local time is (t - start); the first value is held before the first key.
        assert!(e.contains("(t-2)"), "local time relative to clip start: {e}");
        assert!(e.starts_with("if(lt((t-2),0),10,"), "holds first value before t0: {e}");
        assert!(e.contains("10+(10)*"), "linear segment v0 + dv*…: {e}");
        // A single keyframe degenerates to a constant.
        assert_eq!(keyframe_expr(&[(1.0, 0.5)], "t", 0.0), "0.5");
    }

    #[test]
    fn keyframed_clip_animates_scale_and_position() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let assets = vec![asset.clone()];
        let mut clip = make_clip(asset.id, 0.0, 10.0, 2.0);
        clip.keyframes = vec![
            crate::model::Keyframe {
                time: 0.0,
                scale: 1.0,
                pos_x: -0.3,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
            },
            crate::model::Keyframe {
                time: 4.0,
                scale: 1.5,
                pos_x: 0.3,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
            },
        ];
        // Per-frame zoom is in the clip chain…
        let chain = video_clip_chain(&clip, &ExportFormat::default(), &ClipFx::default(), false, "c0");
        assert!(
            chain.contains("scale=w='iw*(") && chain.contains("eval=frame"),
            "animated zoom: {chain}"
        );
        // …and the animated position is an expression on the overlay.
        let timeline = single(vec![clip]);
        let g = build_filter_complex(
            &timeline,
            &assets,
            &ExportFormat::default(),
            timeline.duration(),
            &ExportOptions::default(),
            true,
            false,
            &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
        );
        assert!(
            g.filter.contains("overlay=x=(W-w)/2+(if(lt((t-2)"),
            "animated overlay x: {}",
            g.filter
        );
    }

    #[test]
    fn text_overlay_drawn_over_composite() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let assets = vec![asset.clone()];
        let mut timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
        timeline.overlays = vec![TextOverlay::new("Hello", 1.0, 4.0)];
        let g = build_filter_complex(
            &timeline,
            &assets,
            &ExportFormat::default(),
            timeline.duration(),
            &ExportOptions::default(),
            true,
            false,
            &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
        );
        let f = g.filter;
        // The composite lands on `vtext`, then drawtext writes the final `[outv]`.
        assert!(f.contains("[vtext]"), "composite pad before text: {f}");
        assert!(f.contains("drawtext=") && f.contains("text='Hello'"), "{f}");
        assert!(f.contains("enable='between(t,1,4)'"), "gated to its lifetime: {f}");
        assert!(f.contains("[outv]"));
    }

    #[test]
    fn drawtext_escapes_apostrophes() {
        // close-quote, escaped quote, reopen — the ffmpeg-safe single-quote escape.
        assert_eq!(escape_drawtext("a'b"), "a'\\''b");
    }

    #[test]
    fn drawtext_falls_back_when_font_unknown() {
        let mut o = TextOverlay::new("Hi", 0.0, 1.0);
        o.font = Some("Definitely Not An Installed Font XYZ123".to_string());
        let f = drawtext_export(&o, &ExportFormat::default());
        assert!(!f.contains("fontfile="), "unresolvable font omits fontfile: {f}");
    }

    #[test]
    fn drawtext_bold_without_font_uses_border_fallback() {
        let mut o = TextOverlay::new("Hi", 0.0, 1.0);
        o.bold = true;
        let f = drawtext_export(&o, &ExportFormat::default());
        assert!(
            f.contains("borderw=2"),
            "bold with no font resolved approximates via border: {f}"
        );
    }

    #[test]
    fn drawtext_resolves_installed_font_to_fontfile() {
        // Environment-dependent: skip if this machine has no fonts installed
        // at all, rather than hardcoding a family that may be absent on some
        // CI runner OS.
        let Some(family) = crate::fonts::list_system_fonts().into_iter().next() else {
            return;
        };
        let mut o = TextOverlay::new("Hi", 0.0, 1.0);
        o.font = Some(family);
        let f = drawtext_export(&o, &ExportFormat::default());
        assert!(f.contains("fontfile='"), "installed font resolves to a fontfile: {f}");
    }

    #[test]
    fn still_frame_samples_keyframes_and_draws_overlays() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let assets = vec![asset.clone()];
        let mut timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        timeline.overlays = vec![TextOverlay::new("Caption", 0.0, 10.0)];
        let args = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 3.0, 640, 4).unwrap();
        let joined = args.join(" ");
        assert!(
            joined.contains("drawtext=") && joined.contains("text='Caption'"),
            "still draws overlays: {joined}"
        );
        assert!(joined.contains("null[outv]"), "still terminates on [outv]: {joined}");
    }

    #[test]
    fn srt_export_formats_timecodes() {
        use crate::model::TranscriptSegment;
        let srt = crate::model::transcript_to_srt(&[
            TranscriptSegment {
                start: 1.5,
                end: 3.0,
                text: "first".into(),
            },
            TranscriptSegment {
                start: 3.0,
                end: 4.25,
                text: "second".into(),
            },
        ]);
        assert!(srt.contains("1\n00:00:01,500 --> 00:00:03,000\nfirst"), "{srt}");
        assert!(srt.contains("2\n00:00:03,000 --> 00:00:04,250\nsecond"), "{srt}");
    }

    #[test]
    fn contact_sheet_samples_evenly_and_tiles() {
        let (args, times) = build_contact_sheet_args("/media/clip.mp4", 0.0, 40.0, 4, 4, 240, 5);
        let joined = args.join(" ");
        // 16 cells across 40s -> one frame every 2.5s, row-major.
        assert_eq!(times.len(), 16);
        assert!((times[0] - 0.0).abs() < 1e-9);
        assert!((times[1] - 2.5).abs() < 1e-9);
        assert!((times[15] - 37.5).abs() < 1e-9);
        // Seek/limit to the window, sample with fps, scale cells, tile to one sheet.
        assert!(joined.contains("-ss 0.000"));
        assert!(joined.contains("-t 40.000"));
        assert!(joined.contains("fps=0.4")); // 16 / 40
        assert!(joined.contains("scale=240:-2"));
        assert!(joined.contains("tile=4x4"));
        assert!(joined.contains("-vcodec mjpeg"));
        assert!(joined.contains("-q:v 5"));
        assert!(joined.ends_with("pipe:1"));
    }

    #[test]
    fn contact_sheet_respects_a_subrange() {
        let (args, times) = build_contact_sheet_args("/x.mp4", 10.0, 20.0, 2, 2, 160, 3);
        let joined = args.join(" ");
        assert_eq!(times.len(), 4);
        assert!((times[0] - 10.0).abs() < 1e-9); // window starts at `start`
        assert!((times[3] - 17.5).abs() < 1e-9); // step = 10 / 4 = 2.5
        assert!(joined.contains("-ss 10.000"));
        assert!(joined.contains("-t 10.000"));
        assert!(joined.contains("tile=2x2"));
    }

    #[test]
    fn timeline_frame_composites_the_active_clip() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let assets = vec![asset.clone()];
        // Source 5..15 at timeline 0; at t=2 the mapped source time is 7.
        let timeline = single(vec![make_clip(asset.id, 5.0, 15.0, 0.0)]);
        let args = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 2.0, 640, 4).unwrap();
        let joined = args.join(" ");
        assert_eq!(joined.matches("-i /x.mp4").count(), 1);
        assert!(joined.contains("-ss 7.000"));
        // 16:9 export shape capped to max_width 640 -> 640x360.
        assert!(joined.contains("color=c=black:s=640x360"));
        assert!(joined.contains("[0:v]trim=end_frame=1"));
        assert!(joined.contains("scale=640:360:force_original_aspect_ratio=decrease"));
        // The composite overlays onto a pad, then a trailing `null` names [outv].
        assert!(joined.contains("overlay=(W-w)/2:(H-h)/2[ov0]"));
        assert!(joined.contains("null[outv]"));
        assert!(joined.contains("-vcodec mjpeg"));
    }

    #[test]
    fn timeline_frame_renders_black_on_a_gap() {
        let asset = test_asset(vec![video_stream(1280, 720, 30.0)]);
        let assets = vec![asset.clone()];
        let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]); // covers 0..5
        let args = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 8.0, 640, 4).unwrap();
        let joined = args.join(" ");
        // Nothing visible at t=8 -> no inputs, bare black canvas renamed to [outv].
        assert!(!joined.contains("-i "));
        assert!(joined.contains("color=c=black:s=640x360:d=0.1[base]"));
        assert!(joined.contains("[base]null[outv]"));
    }

    #[test]
    fn timeline_frame_layers_tracks_with_the_last_on_top() {
        let base = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let pip = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let assets = vec![base.clone(), pip.clone()];
        let mut top = make_clip(pip.id, 0.0, 10.0, 0.0);
        top.transform.scale = 0.5;
        top.transform.pos_x = 0.25;
        let timeline = timeline_of(vec![
            video_track(vec![make_clip(base.id, 0.0, 10.0, 0.0)]),
            video_track(vec![top]),
        ]);
        let args = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 1.0, 960, 4).unwrap();
        let joined = args.join(" ");
        // Both clips visible at t=1 -> two inputs; the V2 picture-in-picture is the
        // second input, scaled down, offset, and overlaid last onto [outv].
        assert_eq!(joined.matches("-i /x.mp4").count(), 2);
        assert!(joined.contains("scale=iw*0.5:ih*0.5"));
        assert!(joined.contains("[base][v0]overlay=(W-w)/2:(H-h)/2[ov0]"));
        assert!(joined.contains("[v1]overlay=x=(W-w)/2+(0.25)*W:y=(H-h)/2+(0)*H[ov1]"));
        assert!(joined.contains("[ov1]null[outv]"));
    }

    #[test]
    fn build_export_args_single_video_clip() {
        let asset = Asset {
            id: Uuid::new_v4(),
            path: "/media/clip.mp4".into(),
            name: "clip.mp4".into(),
            duration: 10.0,
            streams: vec![video_stream(1280, 720, 25.0), audio_stream(44_100, 2)],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
        };
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let assets = vec![asset];
        let opts = ExportOptions::default();

        let args = build_export_args(&timeline, &assets, "/out/result.mp4", &opts).unwrap();

        assert_eq!(args[0], "-y");
        assert!(
            args.contains(&"-nostats".to_string()),
            "progress stats suppressed so stderr stays bounded"
        );
        let i_pos = args.iter().position(|a| a == "-i").expect("an input flag");
        assert_eq!(args[i_pos + 1], "/media/clip.mp4");
        assert!(args.contains(&"-filter_complex".to_string()));
        let fc_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
        let filter = &args[fc_pos + 1];
        assert!(filter.contains("trim=start=0:end=10"));
        assert!(filter.contains("overlay=eof_action=pass"));
        assert!(filter.contains("[outv]"));
        assert!(filter.contains("[outa]"));
        assert!(args.contains(&"-map".to_string()));
        assert!(args.contains(&"[outv]".to_string()));
        assert!(args.contains(&"[outa]".to_string()));
        assert_eq!(args.last().unwrap(), "/out/result.mp4");
        // Default opts: no explicit codec or crf flags.
        assert!(!args.contains(&"-c:v".to_string()));
        assert!(!args.contains(&"-c:a".to_string()));
        assert!(!args.contains(&"-crf".to_string()));
    }

    #[test]
    fn build_export_args_two_clips_two_inputs() {
        let a1 = Asset {
            id: Uuid::new_v4(),
            path: "/media/a.mp4".into(),
            name: "a.mp4".into(),
            duration: 20.0,
            streams: vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
        };
        let a2 = Asset {
            id: Uuid::new_v4(),
            path: "/media/b.mp4".into(),
            name: "b.mp4".into(),
            duration: 10.0,
            streams: vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
        };
        let timeline = single(vec![make_clip(a1.id, 0.0, 20.0, 0.0), make_clip(a2.id, 0.0, 10.0, 20.0)]);
        let assets = vec![a1, a2.clone()];
        let opts = ExportOptions::default();

        let args = build_export_args(&timeline, &assets, "/out/out.mp4", &opts).unwrap();

        // Two -i flags for the two clips.
        let input_count = args.windows(2).filter(|w| w[0] == "-i").count();
        assert_eq!(input_count, 2);
        let fc_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
        let filter = &args[fc_pos + 1];
        // One overlay per clip, both audio streams summed.
        assert_eq!(filter.matches("overlay=eof_action=pass").count(), 2);
        assert!(filter.contains("amix=inputs=2"));
        assert_eq!(args.last().unwrap(), "/out/out.mp4");
    }

    #[test]
    fn build_export_args_video_only_has_no_audio_map() {
        let video_only = Asset {
            id: Uuid::new_v4(),
            path: "/media/vo.mp4".into(),
            name: "vo.mp4".into(),
            duration: 5.0,
            streams: vec![video_stream(1920, 1080, 30.0)],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
        };
        let timeline = single(vec![make_clip(video_only.id, 0.0, 5.0, 0.0)]);
        let assets = vec![video_only];
        let opts = ExportOptions::default();

        let args = build_export_args(&timeline, &assets, "/out/vo.mp4", &opts).unwrap();

        let fc_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
        let filter = &args[fc_pos + 1];
        assert!(filter.contains("overlay=eof_action=pass"));
        assert!(!filter.contains("[0:a]"), "no real audio stream should be trimmed");
        assert!(!filter.contains("amix"), "nothing to mix with no audio");
        // A timeline with no audio yields a video map but no [outa] map.
        assert!(args.contains(&"[outv]".to_string()));
        assert!(!args.contains(&"[outa]".to_string()));
    }

    #[test]
    fn build_export_args_with_codec_and_crf_options() {
        let asset = Asset {
            id: Uuid::new_v4(),
            path: "/media/clip.mp4".into(),
            name: "clip.mp4".into(),
            duration: 10.0,
            streams: vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
        };
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let assets = vec![asset];
        let opts = ExportOptions {
            video_codec: Some("libx264".to_string()),
            audio_codec: Some("aac".to_string()),
            crf: Some(23),
            ..Default::default()
        };

        let args = build_export_args(&timeline, &assets, "/out/result.mp4", &opts).unwrap();

        let cv_pos = args.iter().position(|a| a == "-c:v").expect("-c:v must be present");
        assert_eq!(args[cv_pos + 1], "libx264");
        let ca_pos = args.iter().position(|a| a == "-c:a").expect("-c:a must be present");
        assert_eq!(args[ca_pos + 1], "aac");
        let crf_pos = args.iter().position(|a| a == "-crf").expect("-crf must be present");
        assert_eq!(args[crf_pos + 1], "23");
    }

    #[test]
    fn build_export_args_resolution_override() {
        let asset = Asset {
            id: Uuid::new_v4(),
            path: "/media/4k.mp4".into(),
            name: "4k.mp4".into(),
            duration: 10.0,
            streams: vec![video_stream(3840, 2160, 60.0), audio_stream(48_000, 2)],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
        };
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let assets = vec![asset];
        let opts = ExportOptions {
            resolution: Some((1920, 1080)),
            fps: Some(30.0),
            ..Default::default()
        };

        let args = build_export_args(&timeline, &assets, "/out/downscaled.mp4", &opts).unwrap();

        let fc_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
        let filter = &args[fc_pos + 1];
        // Override forces 1920x1080 even though the source is 4K.
        assert!(filter.contains("scale=1920:1080"), "resolution override must apply");
        assert!(filter.contains("fps=30"), "fps override must apply");
    }

    #[test]
    fn build_export_args_error_on_missing_asset() {
        let timeline = single(vec![make_clip(Uuid::new_v4(), 0.0, 5.0, 0.0)]);
        let result = build_export_args(&timeline, &[], "/out/result.mp4", &ExportOptions::default());
        assert!(matches!(result, Err(Error::AssetNotFound(_))));
    }

    fn av_asset(id: Uuid, duration: f64) -> Asset {
        Asset {
            id,
            path: "/media/clip.mp4".into(),
            name: "clip.mp4".into(),
            duration,
            streams: vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
        }
    }

    fn img_asset(id: Uuid) -> Asset {
        Asset {
            id,
            path: "/media/title.png".into(),
            name: "title.png".into(),
            duration: crate::model::DEFAULT_IMAGE_DURATION,
            streams: vec![image_stream(1920, 1080)],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
        }
    }

    /// A raw Insta360 5.7K capture: one dual-fisheye video stream plus audio.
    fn insv_asset(id: Uuid, duration: f64) -> Asset {
        let mut v = video_stream(5760, 2880, 30.0);
        v.codec = "hevc".into();
        v.projection = Some(Projection::DualFisheye);
        Asset {
            id,
            path: "/media/VID_20260801_120000_10_001.insv".into(),
            name: "VID_20260801_120000_10_001.insv".into(),
            duration,
            streams: vec![v, audio_stream(48_000, 2)],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
        }
    }

    /// A clip of `asset` that reframes to flat, optionally animated.
    fn reframed_clip(asset: &Asset, keyframes: Vec<crate::model::ReframeKeyframe>) -> Clip {
        let mut clip = Clip::for_asset(asset, 0.0, 8.0, 2.0);
        let rf = clip.reframe.as_mut().expect("a 360 asset reframes by default");
        rf.pitch = -8.0;
        rf.keyframes = keyframes;
        clip
    }

    fn rkf(time: f64, yaw: f64) -> crate::model::ReframeKeyframe {
        crate::model::ReframeKeyframe {
            time,
            yaw,
            pitch: -8.0,
            roll: 0.0,
            fov: 100.0,
        }
    }

    fn fmt_1080p() -> ExportFormat {
        ExportFormat {
            width: 1920,
            height: 1080,
            fps: 30.0,
            sample_rate: 48_000,
            channels: 2,
            pix_fmt: "yuv420p".to_string(),
            scaler: None,
            fit: Fit::Contain,
        }
    }

    fn graph_of(timeline: &Timeline, assets: &[Asset]) -> String {
        build_filter_complex(
            timeline,
            assets,
            &fmt_1080p(),
            timeline.duration(),
            &ExportOptions::default(),
            true,
            true,
            &plan_inputs(timeline, assets, &transition_fx(timeline, assets)),
        )
        .filter
    }

    // ---- 360 / reframe -----------------------------------------------------

    #[test]
    fn reframe_chain_inserts_v360_after_setpts_before_the_fit() {
        let asset = insv_asset(Uuid::new_v4(), 30.0);
        let clip = reframed_clip(&asset, vec![]);
        let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c3");

        let setpts = chain.find("setpts=").expect("setpts");
        let v360 = chain.find("v360@c3=").expect("v360");
        let fit = chain.find("scale=1920:1080:force_original_aspect_ratio").expect("fit");
        assert!(setpts < v360 && v360 < fit, "order: setpts < v360 < fit in {chain}");
        // Reprojected straight to the export frame, so the fit that follows is a
        // no-op and an 8K sphere never materializes at 8K.
        assert!(chain.contains("w=1920:h=1080"), "v360 renders at frame size: {chain}");
        assert!(chain.contains("input=dfisheye"), "dual-fisheye source: {chain}");
        assert!(chain.contains("output=flat"), "flat deliverable: {chain}");
        assert!(chain.contains("d_fov=100"), "aspect-correct fov knob: {chain}");
        // `:h_fov=`, not `h_fov=` — `ih_fov` (the input lens) contains it.
        assert!(!chain.contains(":h_fov="), "h_fov would stretch the picture: {chain}");
        assert!(chain.contains("ih_fov=190"), "lens fov for a fisheye source: {chain}");
    }

    #[test]
    fn reframe_hoists_fps_above_v360_and_does_not_repeat_it() {
        let asset = insv_asset(Uuid::new_v4(), 30.0);
        let clip = reframed_clip(&asset, vec![]);
        let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c0");
        assert_eq!(chain.matches("fps=30").count(), 1, "exactly one fps: {chain}");
        assert!(
            chain.find("fps=30").unwrap() < chain.find("v360@c0=").unwrap(),
            "reproject at the output rate, not the source rate: {chain}"
        );
    }

    #[test]
    fn reframe_crops_after_reprojection() {
        let asset = insv_asset(Uuid::new_v4(), 30.0);
        let mut clip = reframed_clip(&asset, vec![]);
        clip.transform.crop_left = 0.1;
        let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c0");
        assert!(
            chain.find("v360@c0=").unwrap() < chain.find("crop=").unwrap(),
            "edge crops mean nothing on a raw fisheye frame: {chain}"
        );
    }

    #[test]
    fn an_ordinary_clip_keeps_its_original_chain() {
        // The reframe branch reorders crop and fps; a non-360 clip must not move.
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let mut clip = make_clip(asset.id, 0.0, 8.0, 2.0);
        clip.transform.crop_left = 0.1;
        let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c0");
        assert!(!chain.contains("v360"), "no reprojection: {chain}");
        assert!(
            chain.find("crop=").unwrap() < chain.find("setpts=").unwrap(),
            "crop stays ahead of setpts: {chain}"
        );
        assert!(
            chain.find("setsar=1").unwrap() < chain.find("fps=30").unwrap(),
            "fps stays after setsar: {chain}"
        );
    }

    #[test]
    fn a_static_reframe_emits_no_sendcmd() {
        let asset = insv_asset(Uuid::new_v4(), 30.0);
        let clip = reframed_clip(&asset, vec![]);
        let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c0");
        assert!(
            !chain.contains("sendcmd"),
            "a held camera must not pay a LUT rebuild per frame: {chain}"
        );
        assert!(chain.contains("yaw=0"), "the pose is baked into the args: {chain}");
    }

    #[test]
    fn an_animated_reframe_sends_commands_upstream_of_v360() {
        let asset = insv_asset(Uuid::new_v4(), 30.0);
        let clip = reframed_clip(&asset, vec![rkf(0.0, 0.0), rkf(4.0, 60.0)]);
        let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c3");

        let send = chain.find("sendcmd").expect("sendcmd");
        let v360 = chain.find("v360@c3=").expect("v360");
        assert!(send < v360, "a command must reach v360 with its own frame: {chain}");
        assert!(chain.contains("v360@c3 yaw"), "commands target this clip's instance");

        // Timestamps are on the timeline clock (the clip starts at 2.0) and lead
        // by half a frame so frame `i` cannot be served by frame `i-1`'s value.
        let first = chain.split("sendcmd=c='").nth(1).unwrap().split(' ').next().unwrap();
        let expected = 2.0 - 0.5 / 30.0;
        assert!(
            (first.parse::<f64>().unwrap() - expected).abs() < 1e-4,
            "first command at {first}, want {expected}"
        );
    }

    #[test]
    fn reframe_commands_skip_channels_that_never_move() {
        let asset = insv_asset(Uuid::new_v4(), 30.0);
        let clip = reframed_clip(&asset, vec![rkf(0.0, 0.0), rkf(4.0, 60.0)]);
        let rf = clip.reframe.as_ref().unwrap();
        let cmds = reframe_commands(&clip, rf, "v360@c0", 30.0, clip.duration()).expect("commands");
        assert!(cmds.contains("yaw"), "yaw moves: {cmds}");
        for still in ["pitch", "roll", "d_fov"] {
            assert!(!cmds.contains(still), "{still} is static and must stay an arg: {cmds}");
        }
        // …and the static pitch is still applied, via the filter's own arguments.
        let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c0");
        assert!(chain.contains("pitch=-8"), "static pitch survives: {chain}");
    }

    #[test]
    fn reframe_commands_wrap_yaw_into_range() {
        // A pan across the ±180 seam. `v360` *silently discards* an out-of-range
        // command — the frames render as if uncommanded — so every emitted value
        // has to land inside [-180, 180].
        let asset = insv_asset(Uuid::new_v4(), 30.0);
        let clip = reframed_clip(&asset, vec![rkf(0.0, 170.0), rkf(4.0, -170.0)]);
        let rf = clip.reframe.as_ref().unwrap();
        let cmds = reframe_commands(&clip, rf, "v360@c0", 30.0, clip.duration()).expect("commands");

        let mut seen = 0;
        for c in cmds.split(';') {
            let v: f64 = c.rsplit(' ').next().unwrap().parse().unwrap();
            assert!((-180.0..=180.0).contains(&v), "{v} is out of v360's range: {c}");
            seen += 1;
        }
        assert!(seen > 1, "the pan should emit more than one command");
        // Shortest arc: 170 -> -170 travels 20° forward through 180, never back
        // through 0. Halfway is therefore 180/-180, not 0.
        let mid = rf.sample(2.0).yaw;
        assert!(mid.abs() > 179.0, "midpoint {mid} should be at the seam, not near 0");
    }

    #[test]
    fn reframe_commands_collapse_a_held_camera() {
        // Keyframes that pin the same pose twice: nothing moves after the first
        // sample, so the tolerance gate should leave a single command at most.
        let asset = insv_asset(Uuid::new_v4(), 60.0);
        let mut clip = reframed_clip(&asset, vec![rkf(0.0, 30.0), rkf(50.0, 30.0)]);
        clip.source_out = 50.0;
        let rf = clip.reframe.as_ref().unwrap();
        let cmds = reframe_commands(&clip, rf, "v360@c0", 30.0, clip.duration());
        assert!(cmds.is_none(), "a motionless camera needs no commands at all: {cmds:?}");
    }

    #[test]
    fn export_format_ignores_a_reframed_clips_source_size() {
        let asset = insv_asset(Uuid::new_v4(), 30.0);
        let clip = reframed_clip(&asset, vec![]);
        let tl = single(vec![clip]);
        let fmt = export_format(&tl, std::slice::from_ref(&asset), &ExportOptions::default());
        assert_eq!(
            (fmt.width, fmt.height),
            (1920, 1080),
            "a 16:9 reframe must not inherit the sphere's 5760x2880"
        );
        assert_eq!(fmt.fps, 30.0, "frame rate still comes from the source");

        // An explicit override still wins.
        let opts = ExportOptions {
            resolution: Some((3840, 2160)),
            ..Default::default()
        };
        assert_eq!(export_format(&tl, &[asset], &opts).width, 3840);
    }

    #[test]
    fn the_still_path_samples_the_reframe_statically() {
        let asset = insv_asset(Uuid::new_v4(), 30.0);
        let clip = reframed_clip(&asset, vec![rkf(0.0, 0.0), rkf(4.0, 60.0)]);
        let tl = single(vec![clip]);
        // t = 4.0 is 2.0s into a clip starting at 2.0, i.e. halfway through the pan.
        let args = build_timeline_frame_args(&tl, &[asset], &ExportOptions::default(), 4.0, 960, 4).expect("args");
        let graph = args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone();
        assert!(!graph.contains("sendcmd"), "a still has no clock to command against: {graph}");
        assert!(graph.contains("v360=input=dfisheye"), "unnamed instance: {graph}");
        assert!(graph.contains("yaw=30"), "the pose is sampled to a constant: {graph}");
        assert!(graph.contains("w=960"), "reproject at the preview size: {graph}");
    }

    #[test]
    fn slice_resamples_reframe_keyframes() {
        let asset = insv_asset(Uuid::new_v4(), 30.0);
        // Clip spans timeline 2..10, panning 0 -> 60 over its 8 seconds.
        let clip = reframed_clip(&asset, vec![rkf(0.0, 0.0), rkf(8.0, 60.0)]);
        let pose_at_cut = clip.reframe_at(2.0).unwrap();
        let sliced = single(vec![clip]).slice(4.0, 10.0);

        let c = &sliced.tracks[0].clips[0];
        assert_eq!(c.timeline_start, 0.0);
        let kfs = &c.reframe.as_ref().unwrap().keyframes;
        assert_eq!(kfs[0].time, 0.0, "a pinned keyframe opens the sliced clip");
        assert!(
            (kfs[0].yaw - pose_at_cut.yaw).abs() < 1e-9,
            "the pin carries the pose the cut landed on: {} vs {}",
            kfs[0].yaw,
            pose_at_cut.yaw
        );
    }

    #[test]
    fn an_ordinary_graph_stays_in_argv_but_a_long_pan_spills_to_a_script() {
        let asset = insv_asset(Uuid::new_v4(), 300.0);

        // A static reframe: no commands, so the graph stays small.
        let still = single(vec![reframed_clip(&asset, vec![])]);
        let args = build_export_args(&still, std::slice::from_ref(&asset), "/out.mp4", &ExportOptions::default()).unwrap();
        assert_eq!(oversized_graph_index(&args), None, "a normal graph rides in argv");

        // A four-minute pan: the sendcmd list alone outgrows what a single argv
        // string may hold on Windows, and eventually on Linux too.
        let mut clip = reframed_clip(&asset, vec![rkf(0.0, 0.0), rkf(240.0, 170.0)]);
        clip.source_out = 240.0;
        let long = single(vec![clip]);
        let args = build_export_args(&long, &[asset], "/out.mp4", &ExportOptions::default()).unwrap();
        let i = oversized_graph_index(&args).expect("a long pan must spill out of argv");
        assert!(args[i].len() > GRAPH_ARG_MAX);

        let mut spilled = args.clone();
        let guard = externalize_filter_complex(&mut spilled, "test").unwrap();
        assert_eq!(spilled[i - 1], "-filter_complex_script");
        let path = std::path::PathBuf::from(&spilled[i]);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            args[i],
            "the graph is written verbatim"
        );
        drop(guard);
        assert!(!path.exists(), "the script is cleaned up after the render");
    }

    #[test]
    fn probe_reads_a_spherical_mapping() {
        let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","width":3840,"height":1920,"r_frame_rate":"30/1","side_data_list":[{"side_data_type":"Spherical Mapping","projection":"equirectangular"}]}],"format":{"duration":"20.0"}}"#;
        let r = probe_from_json(serde_json::from_str(json).unwrap(), None);
        assert_eq!(r.streams[0].projection, Some(Projection::Equirect));
    }

    #[test]
    fn probe_reads_insta360_dual_fisheye_geometry() {
        let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","width":5760,"height":2880,"r_frame_rate":"30/1"}],"format":{"duration":"20.0"}}"#;
        let r = probe_from_json(
            serde_json::from_str(json).unwrap(),
            Some(Path::new("/media/VID_20260801_120000_10_001.insv")),
        );
        assert_eq!(r.streams[0].projection, Some(Projection::DualFisheye));
    }

    #[test]
    fn probe_does_not_guess_360_from_aspect_alone() {
        // 2:1 at 4K is an ordinary shape (anamorphic, ultrawide, panoramas). A
        // false positive would silently reproject real footage, so only an
        // Insta360 extension unlocks the geometry signal.
        let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":5760,"height":2880,"r_frame_rate":"30/1"}],"format":{"duration":"20.0"}}"#;
        let r = probe_from_json(serde_json::from_str(json).unwrap(), Some(Path::new("/media/ultrawide.mp4")));
        assert_eq!(r.streams[0].projection, None);
    }

    #[test]
    fn probe_leaves_ordinary_video_flat() {
        let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"r_frame_rate":"30/1"}],"format":{"duration":"12.0"}}"#;
        let r = probe_from_json(serde_json::from_str(json).unwrap(), Some(Path::new("/media/a.mp4")));
        assert_eq!(r.streams[0].projection, None);
    }

    #[test]
    fn speed_retimes_picture_and_sound() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        clip.speed = 2.0;
        // Source span 10s at 2x => 5s on the timeline.
        assert!((clip.duration() - 5.0).abs() < 1e-9);
        let g = graph_of(&single(vec![clip]), &[asset]);
        assert!(g.contains("setpts=(PTS-STARTPTS)/2+0/TB"), "{g}");
        assert!(g.contains("atempo=2"), "{g}");
    }

    #[test]
    fn negative_speed_reverses() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        clip.speed = -1.0;
        let g = graph_of(&single(vec![clip]), &[asset]);
        assert!(g.contains(",reverse,"), "{g}");
        assert!(g.contains("areverse"), "{g}");
        // |speed| == 1, so the picture is not retimed.
        assert!(g.contains("setpts=PTS-STARTPTS+0/TB"), "{g}");
    }

    #[test]
    fn transform_pip_positions_a_scaled_overlay() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        clip.transform = crate::model::Transform {
            scale: 0.5,
            pos_x: 0.25,
            ..Default::default()
        };
        let g = graph_of(&single(vec![clip]), &[asset]);
        assert!(g.contains("scale=iw*0.5:ih*0.5"), "{g}");
        assert!(g.contains("overlay=x=(W-w)/2+(0.25)*W:y=(H-h)/2+(0)*H"), "{g}");
        // A transformed clip is positioned by overlay, not letterbox-padded.
        assert!(!g.contains("pad=1920:1080"), "{g}");
    }

    #[test]
    fn opacity_uses_an_alpha_channel() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        clip.transform = crate::model::Transform {
            opacity: 0.5,
            ..Default::default()
        };
        let g = graph_of(&single(vec![clip]), &[asset]);
        assert!(g.contains("format=yuva420p"), "{g}");
        assert!(g.contains("colorchannelmixer=aa=0.5"), "{g}");
    }

    #[test]
    fn color_correction_adds_an_eq_filter() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        clip.color = crate::model::Color {
            brightness: 0.1,
            contrast: 1.2,
            ..Default::default()
        };
        let g = graph_of(&single(vec![clip]), &[asset]);
        assert!(g.contains("eq=brightness=0.1:contrast=1.2:saturation=1:gamma=1"), "{g}");
        // No temperature → no channel gammas, so pre-temperature graphs are
        // reproduced byte-for-byte.
        assert!(!g.contains("gamma_r"), "{g}");
    }

    #[test]
    fn color_temperature_warms_via_opposing_channel_gammas() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        clip.color = crate::model::Color {
            temperature: 0.5,
            ..Default::default()
        };
        let g = graph_of(&single(vec![clip]), &[asset]);
        assert!(g.contains("gamma_r=1.15"), "{g}");
        assert!(g.contains("gamma_b=0.85"), "{g}");
    }

    #[test]
    fn slice_cuts_clips_and_shifts_to_zero() {
        let asset_id = Uuid::new_v4();
        let a = make_clip(asset_id, 0.0, 10.0, 0.0);
        let b = make_clip(asset_id, 0.0, 10.0, 10.0);
        let s = single(vec![a, b]).slice(8.0, 12.0);
        let clips = &s.tracks[0].clips;
        assert_eq!(clips.len(), 2);
        // A keeps its last 2 source seconds, landing at t=0.
        assert!((clips[0].source_in - 8.0).abs() < 1e-9, "{}", clips[0].source_in);
        assert!((clips[0].source_out - 10.0).abs() < 1e-9);
        assert!(clips[0].timeline_start.abs() < 1e-9);
        // B keeps its first 2 source seconds, landing right after.
        assert!(clips[1].source_in.abs() < 1e-9);
        assert!((clips[1].source_out - 2.0).abs() < 1e-9);
        assert!((clips[1].timeline_start - 2.0).abs() < 1e-9);
    }

    #[test]
    fn slice_drops_outside_clips_and_honors_speed() {
        let asset_id = Uuid::new_v4();
        let mut a = make_clip(asset_id, 0.0, 4.0, 0.0);
        a.speed = 2.0; // 2 timeline seconds
        let b = make_clip(asset_id, 0.0, 4.0, 6.0);
        let s = single(vec![a, b]).slice(1.0, 3.0);
        let clips = &s.tracks[0].clips;
        assert_eq!(clips.len(), 1);
        // One timeline second cut from the front = two source seconds at 2×.
        assert!((clips[0].source_in - 2.0).abs() < 1e-9, "{}", clips[0].source_in);
        assert!(clips[0].timeline_start.abs() < 1e-9);
    }

    #[test]
    fn range_export_builds_the_graph_from_the_sliced_timeline() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        let tl = single(vec![clip]);
        let opts = ExportOptions {
            range: Some(crate::model::TimeRange { start: 2.0, end: 6.0 }),
            ..Default::default()
        };
        let args = build_export_args(&tl, &[asset], "out.mp4", &opts).unwrap();
        let joined = args.join(" ");
        // The kept span is source 2..6 fast-sought to 2, so the in-graph trim
        // is seek-relative 0..4 — the graph really was built from the slice.
        assert!(joined.contains("trim=start=0:end=4"), "{joined}");
    }

    #[test]
    fn ducked_track_sidechains_under_the_rest() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let voice = make_clip(asset.id, 0.0, 10.0, 0.0);
        let music = make_clip(asset.id, 0.0, 10.0, 0.0);
        let mut music_track = audio_track(vec![music]);
        music_track.duck = true;
        let tl = timeline_of(vec![video_track(vec![voice]), music_track]);
        let g = graph_of(&tl, &[asset]);
        assert!(g.contains("sidechaincompress"), "{g}");
        assert!(g.contains("[akmix][aducked]amix=inputs=2"), "{g}");
    }

    #[test]
    fn duck_flag_without_other_audio_keeps_the_flat_mix() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let music = make_clip(asset.id, 0.0, 10.0, 0.0);
        let mut t = audio_track(vec![music]);
        t.duck = true;
        let g = graph_of(&timeline_of(vec![t]), &[asset]);
        assert!(!g.contains("sidechaincompress"), "{g}");
        assert!(g.contains("amix=inputs=1"), "{g}");
    }

    #[test]
    fn loudnorm_option_appends_to_the_final_mix() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        let opts = ExportOptions {
            loudnorm: true,
            ..Default::default()
        };
        let args = build_export_args(&single(vec![clip]), &[asset], "out.mp4", &opts).unwrap();
        let joined = args.join(" ");
        assert!(joined.contains("loudnorm=I=-14:TP=-1.5:LRA=11,aresample="), "{joined}");
    }

    #[test]
    fn crossfade_extends_the_outgoing_tail_and_dissolves_the_incoming() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let a = make_clip(asset.id, 0.0, 10.0, 0.0);
        let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
        b.transition_in = Some(crate::model::Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let g = graph_of(&single(vec![a, b]), &[asset]);
        // Outgoing clip A renders one extra second of source under the dissolve.
        assert!(g.contains("trim=start=0:end=11"), "{g}");
        // Incoming clip B fades up via alpha.
        assert!(g.contains("fade=t=in:st=0:d=1:alpha=1"), "{g}");
    }

    #[test]
    fn color_eq_runs_before_alpha_is_established() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        clip.transform = crate::model::Transform {
            opacity: 0.5,
            ..Default::default()
        };
        clip.color = crate::model::Color {
            brightness: 0.1,
            ..Default::default()
        };
        let g = graph_of(&single(vec![clip]), &[asset]);
        let eq = g.find("eq=").expect("eq present");
        let alpha = g.find("format=yuva420p").expect("alpha present");
        // eq cannot carry alpha, so it must precede the alpha conversion or the
        // opacity (colorchannelmixer) would be silently dropped.
        assert!(eq < alpha, "eq must come before alpha: {g}");
        assert!(g.contains("colorchannelmixer=aa=0.5"), "{g}");
    }

    #[test]
    fn crossfade_without_source_handle_is_a_hard_cut() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let a = make_clip(asset.id, 0.0, 20.0, 0.0); // uses the whole asset — no handle to borrow
        let mut b = make_clip(asset.id, 0.0, 10.0, 20.0);
        b.transition_in = Some(crate::model::Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let g = graph_of(&single(vec![a, b]), &[asset]);
        assert!(!g.contains(":alpha=1"), "no fade-from-black when there is no handle: {g}");
        assert!(g.contains("trim=start=0:end=20"), "outgoing tail must not be extended: {g}");
    }

    #[test]
    fn crossfade_across_a_gap_dissolves_from_black_without_bleeding_the_partner() {
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let a = make_clip(asset.id, 0.0, 10.0, 0.0);
        let mut b = make_clip(asset.id, 0.0, 10.0, 15.0); // 5s gap after a
        b.transition_in = Some(crate::model::Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let g = graph_of(&single(vec![a, b]), &[asset]);
        assert!(
            g.contains("trim=start=0:end=10"),
            "outgoing clip must not bleed across the gap: {g}"
        );
        assert!(g.contains("fade=t=in:st=0:d=1:alpha=1"), "incoming dissolves from black: {g}");
    }

    #[test]
    fn reversed_crossfade_extends_the_low_source_end() {
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let mut a = make_clip(asset.id, 5.0, 15.0, 0.0);
        a.speed = -1.0; // reversed, with 5s of handle below source_in
        let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
        b.transition_in = Some(crate::model::Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let g = graph_of(&single(vec![a, b]), &[asset]);
        // Window [4,15] is fast-seeked: the input gets `-ss 4` and the trim is
        // expressed relative to it (start 0, 11s long).
        assert!(
            g.contains("trim=start=0:end=11"),
            "reversed tail extends below source_in: {g}"
        );
        assert!(g.contains(",reverse,"), "{g}");
    }

    #[test]
    fn fast_seek_emits_input_ss_and_makes_the_trim_relative() {
        let asset = av_asset(Uuid::new_v4(), 60.0);
        // A subclip deep into the source: 30s..33s.
        let timeline = single(vec![make_clip(asset.id, 30.0, 33.0, 0.0)]);
        let args = build_export_args(&timeline, &[asset], "/out/x.mp4", &ExportOptions::default()).unwrap();
        // `-ss 30` precedes the input so ffmpeg decodes from ~30s, not from 0.
        let ss = args.iter().position(|a| a == "-ss").expect("a fast-seek -ss");
        assert_eq!(args[ss + 1], "30");
        assert_eq!(args[ss + 2], "-i", "the -ss must immediately precede its input");
        // The graph trim/atrim are relative to the seek: a 3s window from 0.
        let filter = flag_val(&args, "-filter_complex").unwrap();
        assert!(filter.contains("trim=start=0:end=3"), "video trim is seek-relative: {filter}");
        assert!(
            filter.contains("atrim=start=0:end=3"),
            "audio trim is seek-relative: {filter}"
        );
    }

    #[test]
    fn head_clips_emit_no_fast_seek() {
        // A clip that starts at the source head must not gain an -ss (decoding
        // from 0 is free) — args stay byte-identical to the pre-fast-seek build.
        let asset = av_asset(Uuid::new_v4(), 10.0);
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let args = build_export_args(&timeline, &[asset], "/out/x.mp4", &ExportOptions::default()).unwrap();
        assert!(!args.contains(&"-ss".to_string()), "no seek for a head clip: {args:?}");
        let filter = flag_val(&args, "-filter_complex").unwrap();
        assert!(filter.contains("trim=start=0:end=10"), "{filter}");
    }

    #[test]
    fn a_still_image_is_looped_not_seeked() {
        let asset = img_asset(Uuid::new_v4());
        // A full-length still placed deep in the timeline (t=12) — a non-image
        // clip there would gain an `-ss`; a still must not.
        let timeline = single(vec![make_clip(asset.id, 0.0, crate::model::DEFAULT_IMAGE_DURATION, 12.0)]);
        let args = build_export_args(&timeline, &[asset], "/out/x.mp4", &ExportOptions::default()).unwrap();

        assert!(
            !args.contains(&"-ss".to_string()),
            "a still is looped, never seeked: {args:?}"
        );
        let loop_pos = args.iter().position(|a| a == "-loop").expect("a -loop flag");
        assert_eq!(args[loop_pos + 1], "1");
        assert!(
            args.contains(&"-framerate".to_string()),
            "the looped still gets an input framerate"
        );
        // `-t` bounds how long the looped still is read — its source window end.
        assert_eq!(flag_val(&args, "-t"), Some("5"));
        let i_pos = args.iter().position(|a| a == "-i").unwrap();
        assert_eq!(args[i_pos + 1], "/media/title.png");
        // No seek to subtract, so the trim window stays absolute, positioned at t=12.
        let filter = flag_val(&args, "-filter_complex").unwrap();
        assert!(filter.contains("trim=start=0:end=5"), "{filter}");
        assert!(filter.contains("+12/TB"), "still composited at its timeline start: {filter}");
    }

    #[test]
    fn a_trimmed_still_keeps_an_absolute_trim() {
        let asset = img_asset(Uuid::new_v4());
        // The user trimmed the still to its 1s..4s window — for a real video that
        // window would be fast-seeked; for a still the trim stays absolute.
        let timeline = single(vec![make_clip(asset.id, 1.0, 4.0, 0.0)]);
        let args = build_export_args(&timeline, &[asset], "/out/x.mp4", &ExportOptions::default()).unwrap();
        assert!(!args.contains(&"-ss".to_string()));
        assert_eq!(flag_val(&args, "-t"), Some("4"), "read the looped still up to source_out");
        let filter = flag_val(&args, "-filter_complex").unwrap();
        assert!(filter.contains("trim=start=1:end=4"), "absolute, not seek-relative: {filter}");
    }

    #[test]
    fn timeline_frame_does_not_seek_a_still() {
        let asset = img_asset(Uuid::new_v4());
        let timeline = single(vec![make_clip(asset.id, 0.0, crate::model::DEFAULT_IMAGE_DURATION, 0.0)]);
        // Composite at t=2: a still has one frame, so it must be read without `-ss`.
        let args = build_timeline_frame_args(&timeline, &[asset], &ExportOptions::default(), 2.0, 640, 4).unwrap();
        assert!(
            !args.contains(&"-ss".to_string()),
            "a still has one frame; don't seek it: {args:?}"
        );
        let i_pos = args.iter().position(|a| a == "-i").unwrap();
        assert_eq!(args[i_pos + 1], "/media/title.png");
    }

    #[test]
    fn probe_flags_a_lone_still_image() {
        let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"png","width":1920,"height":1080,"r_frame_rate":"25/1"}],"format":{}}"#;
        let r = probe_from_json(serde_json::from_str(json).unwrap(), None);
        assert_eq!(r.duration, 0.0, "a still probes with no duration");
        assert!(r.streams[0].image, "a lone, audio-less png is a still");
    }

    #[test]
    fn probe_does_not_flag_ordinary_video() {
        let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"r_frame_rate":"30/1","duration":"12.0"}],"format":{"duration":"12.0"}}"#;
        let r = probe_from_json(serde_json::from_str(json).unwrap(), None);
        assert!(!r.streams[0].image);
    }

    #[test]
    fn probe_does_not_flag_an_animated_gif() {
        // A multi-frame gif probes with a real duration, so it is treated as video.
        let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"gif","width":480,"height":270,"r_frame_rate":"10/1"}],"format":{"duration":"3.0"}}"#;
        let r = probe_from_json(serde_json::from_str(json).unwrap(), None);
        assert!(!r.streams[0].image);
    }

    #[test]
    fn dip_to_black_fades_both_sides_of_the_cut() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let a = make_clip(asset.id, 0.0, 10.0, 0.0);
        let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
        b.transition_in = Some(crate::model::Transition {
            kind: TransitionKind::DipToBlack,
            duration: 1.0,
        });
        let g = graph_of(&single(vec![a, b]), &[asset]);
        // Outgoing A fades out to black at its end, incoming B fades up from black.
        assert!(g.contains("fade=t=out:st=9.5:d=0.5"), "{g}");
        assert!(g.contains("fade=t=in:st=0:d=0.5"), "{g}");
    }

    // ---- export option mapping -------------------------------------------

    /// The token following `flag` in `args`, if present.
    fn flag_val<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    }

    /// Build the argv for `opts` against a single 1080p video+audio clip.
    fn args_of(opts: &ExportOptions) -> Vec<String> {
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        build_export_args(&timeline, &[asset], "/out/x", opts).unwrap()
    }

    #[test]
    fn build_export_args_default_unchanged() {
        // The bare default must reproduce the legacy argv: maps, but no codec /
        // crf / pix_fmt / faststart flags.
        let args = args_of(&ExportOptions::default());
        assert!(args.contains(&"[outv]".to_string()) && args.contains(&"[outa]".to_string()));
        assert!(!args.contains(&"-c:v".to_string()));
        assert!(!args.contains(&"-c:a".to_string()));
        assert!(!args.contains(&"-crf".to_string()));
        assert!(!args.contains(&"-pix_fmt".to_string()));
        assert!(!args.contains(&"-movflags".to_string()));
        assert_eq!(args.last().unwrap(), "/out/x");
    }

    #[test]
    fn build_export_args_h264_crf_in_order() {
        let opts = ExportOptions {
            video_codec: Some("libx264".into()),
            audio_codec: Some("aac".into()),
            crf: Some(20),
            preset: Some("medium".into()),
            pix_fmt: Some("yuv420p".into()),
            audio_bitrate: Some("192k".into()),
            ..Default::default()
        };
        let args = args_of(&opts);
        assert_eq!(flag_val(&args, "-c:v"), Some("libx264"));
        assert_eq!(flag_val(&args, "-crf"), Some("20"));
        assert_eq!(flag_val(&args, "-preset"), Some("medium"));
        assert_eq!(flag_val(&args, "-pix_fmt"), Some("yuv420p"));
        assert_eq!(flag_val(&args, "-c:a"), Some("aac"));
        assert_eq!(flag_val(&args, "-b:a"), Some("192k"));
        // The maps precede -c:v, which precedes its private -crf.
        let map = args.iter().position(|a| a == "[outv]").unwrap();
        let cv = args.iter().position(|a| a == "-c:v").unwrap();
        let crf = args.iter().position(|a| a == "-crf").unwrap();
        assert!(map < cv && cv < crf);
    }

    #[test]
    fn build_export_args_vp9_crf_pairs_bv0_and_cpu_used() {
        let opts = ExportOptions {
            container: Container::Webm,
            video_codec: Some("libvpx-vp9".into()),
            audio_codec: Some("libopus".into()),
            crf: Some(31),
            ..Default::default()
        };
        let args = args_of(&opts);
        assert_eq!(flag_val(&args, "-crf"), Some("31"));
        assert_eq!(flag_val(&args, "-b:v"), Some("0"));
        assert_eq!(flag_val(&args, "-cpu-used"), Some("4"));
        assert!(!args.contains(&"-preset".to_string()));
    }

    #[test]
    fn build_export_args_nvenc_crf_uses_cq_not_crf() {
        let opts = ExportOptions {
            video_codec: Some("h264_nvenc".into()),
            crf: Some(20),
            preset: Some("p5".into()),
            ..Default::default()
        };
        let args = args_of(&opts);
        assert_eq!(flag_val(&args, "-c:v"), Some("h264_nvenc"));
        // NVENC: VBR steered by -cq with -b:v 0; never the software -crf.
        assert!(!args.contains(&"-crf".to_string()));
        assert_eq!(flag_val(&args, "-rc"), Some("vbr"));
        assert_eq!(flag_val(&args, "-cq"), Some("20"));
        assert_eq!(flag_val(&args, "-b:v"), Some("0"));
        assert_eq!(flag_val(&args, "-preset"), Some("p5"));
    }

    #[test]
    fn build_export_args_qsv_crf_uses_global_quality() {
        let opts = ExportOptions {
            video_codec: Some("h264_qsv".into()),
            crf: Some(23),
            ..Default::default()
        };
        let args = args_of(&opts);
        assert!(!args.contains(&"-crf".to_string()));
        assert_eq!(flag_val(&args, "-global_quality"), Some("23"));
    }

    #[test]
    fn build_export_args_videotoolbox_crf_maps_to_quality() {
        let opts = ExportOptions {
            video_codec: Some("h264_videotoolbox".into()),
            crf: Some(23),
            ..Default::default()
        };
        let args = args_of(&opts);
        assert!(!args.contains(&"-crf".to_string()));
        // crf 23 → round((1 - 23/51) * 100) = 55 on the 1..100 scale.
        assert_eq!(flag_val(&args, "-q:v"), Some("55"));
        assert!(!args.contains(&"-preset".to_string()), "videotoolbox has no -preset");
    }

    #[test]
    fn build_export_args_hevc_hw_gets_hvc1_tag() {
        let opts = ExportOptions {
            video_codec: Some("hevc_nvenc".into()),
            crf: Some(24),
            ..Default::default()
        };
        let args = args_of(&opts);
        // The hvc1 tag must follow HEVC into mp4 for every encoder, not just libx265.
        assert_eq!(flag_val(&args, "-tag:v"), Some("hvc1"));
    }

    #[test]
    fn build_export_args_hwaccel_decode_is_per_input_and_opt_in() {
        // Default: no -hwaccel at all (byte-for-byte legacy decode).
        let plain = args_of(&ExportOptions::default());
        assert!(!plain.contains(&"-hwaccel".to_string()));

        let opts = ExportOptions {
            hwaccel: Some("cuda".into()),
            ..Default::default()
        };
        let args = args_of(&opts);
        assert_eq!(flag_val(&args, "-hwaccel"), Some("cuda"));
        // It's an input option: must precede the `-i` it accelerates.
        let hw = args.iter().position(|a| a == "-hwaccel").unwrap();
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert!(hw < input);

        // "none" is treated as software (no flag emitted).
        let none = args_of(&ExportOptions {
            hwaccel: Some("none".into()),
            ..Default::default()
        });
        assert!(!none.contains(&"-hwaccel".to_string()));
    }

    #[test]
    fn build_export_args_dedupes_identical_inputs_with_split() {
        let asset = av_asset(Uuid::new_v4(), 30.0);
        // The same source window on two video tracks (e.g. a composite) must decode
        // once: one `-i`, fanned out to both consumers with split / asplit.
        let timeline = timeline_of(vec![
            video_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]),
            video_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]),
        ]);
        let args = build_export_args(&timeline, &[asset], "/out/x.mp4", &ExportOptions::default()).unwrap();
        assert_eq!(
            args.iter().filter(|a| a.as_str() == "-i").count(),
            1,
            "the shared source must be one input"
        );
        let f = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(f.contains("[0:v]split=2[vsp0_0][vsp0_1]"), "{f}");
        assert!(f.contains("[0:a]asplit=2[asp0_0][asp0_1]"), "{f}");
        // Each clip's chain reads its own fan-out pad — never the input pad twice.
        assert!(f.contains("[vsp0_0]trim") && f.contains("[vsp0_1]trim"), "{f}");
    }

    #[test]
    fn build_export_args_keeps_distinct_seeks_separate() {
        let asset = av_asset(Uuid::new_v4(), 30.0);
        // Same asset but different source_in → different fast-seek: two inputs, no
        // fan-out, so each still decodes only its own kept region.
        let timeline = timeline_of(vec![
            video_track(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]),
            video_track(vec![make_clip(asset.id, 10.0, 15.0, 0.0)]),
        ]);
        let args = build_export_args(&timeline, &[asset], "/out/x.mp4", &ExportOptions::default()).unwrap();
        assert_eq!(args.iter().filter(|a| a.as_str() == "-i").count(), 2);
        let f = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(!f.contains("split="), "distinct seeks must not be fanned out: {f}");
    }

    #[test]
    fn validate_export_rejects_two_pass_for_hardware_encoders() {
        let opts = ExportOptions {
            video_codec: Some("h264_nvenc".into()),
            rate_control: RateControl::TwoPass,
            video_bitrate: Some("8M".into()),
            ..Default::default()
        };
        let issues = validate_export(&opts, true, true);
        assert!(issues.iter().any(|i| i.contains("Two-pass")), "{issues:?}");
    }

    #[test]
    fn build_export_args_x265_mp4_tags_hvc1_but_mkv_does_not() {
        let mp4 = ExportOptions {
            video_codec: Some("libx265".into()),
            container: Container::Mp4,
            ..Default::default()
        };
        assert_eq!(flag_val(&args_of(&mp4), "-tag:v"), Some("hvc1"));
        let mkv = ExportOptions {
            video_codec: Some("libx265".into()),
            container: Container::Mkv,
            ..Default::default()
        };
        assert!(!args_of(&mkv).contains(&"-tag:v".to_string()));
    }

    #[test]
    fn build_export_args_prores_uses_profile_not_crf() {
        let opts = ExportOptions {
            container: Container::Mov,
            video_codec: Some("prores_ks".into()),
            prores_profile: Some(3),
            crf: Some(18), // ignored for prores
            pix_fmt: Some("yuv422p10le".into()),
            ..Default::default()
        };
        let args = args_of(&opts);
        assert_eq!(flag_val(&args, "-profile:v"), Some("3"));
        assert!(!args.contains(&"-crf".to_string()));
        assert!(!args.contains(&"-preset".to_string()));
        assert_eq!(flag_val(&args, "-pix_fmt"), Some("yuv422p10le"));
    }

    #[test]
    fn build_export_args_faststart_only_for_mp4_mov() {
        let mp4 = ExportOptions {
            video_codec: Some("libx264".into()),
            faststart: true,
            ..Default::default()
        };
        assert_eq!(flag_val(&args_of(&mp4), "-movflags"), Some("+faststart"));
        let mkv = ExportOptions {
            container: Container::Mkv,
            video_codec: Some("libx264".into()),
            faststart: true,
            ..Default::default()
        };
        assert!(!args_of(&mkv).contains(&"-movflags".to_string()));
    }

    #[test]
    fn build_export_args_audio_only_drops_video() {
        let opts = ExportOptions {
            container: Container::Mp3,
            audio_codec: Some("libmp3lame".into()),
            audio_bitrate: Some("320k".into()),
            ..Default::default()
        };
        let args = args_of(&opts);
        assert!(
            !args.contains(&"[outv]".to_string()),
            "no video map for an audio-only container"
        );
        assert!(!args.contains(&"-c:v".to_string()));
        assert!(!args.contains(&"-pix_fmt".to_string()));
        assert!(args.contains(&"[outa]".to_string()));
        assert_eq!(flag_val(&args, "-c:a"), Some("libmp3lame"));
        assert_eq!(flag_val(&args, "-b:a"), Some("320k"));
    }

    #[test]
    fn build_export_args_include_audio_false_emits_an() {
        let opts = ExportOptions {
            video_codec: Some("libx264".into()),
            include_audio: false,
            ..Default::default()
        };
        let args = args_of(&opts);
        assert!(!args.contains(&"[outa]".to_string()));
        assert!(args.contains(&"-an".to_string()));
        assert!(!args.contains(&"-c:a".to_string()));
    }

    #[test]
    fn build_export_args_lossless_per_codec() {
        let x264 = ExportOptions {
            video_codec: Some("libx264".into()),
            rate_control: RateControl::Lossless,
            ..Default::default()
        };
        assert_eq!(flag_val(&args_of(&x264), "-crf"), Some("0"));
        let vp9 = ExportOptions {
            container: Container::Webm,
            video_codec: Some("libvpx-vp9".into()),
            audio_codec: Some("libopus".into()),
            rate_control: RateControl::Lossless,
            ..Default::default()
        };
        assert_eq!(flag_val(&args_of(&vp9), "-lossless"), Some("1"));
    }

    #[test]
    fn build_export_args_two_pass_first_and_second() {
        let opts = ExportOptions {
            video_codec: Some("libx264".into()),
            audio_codec: Some("aac".into()),
            rate_control: RateControl::TwoPass,
            video_bitrate: Some("8M".into()),
            faststart: true,
            metadata_title: Some("Cut".into()),
            ..Default::default()
        };
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let assets = [asset];
        let p1 = build_export_args_phase(
            &timeline,
            &assets,
            "/out/x.mp4",
            &opts,
            PassPhase::First,
            "/dev/null",
            "/tmp/pl",
        )
        .unwrap();
        assert_eq!(flag_val(&p1, "-b:v"), Some("8M"));
        assert_eq!(flag_val(&p1, "-pass"), Some("1"));
        assert_eq!(flag_val(&p1, "-passlogfile"), Some("/tmp/pl"));
        assert!(!p1.contains(&"[outa]".to_string()), "the analysis pass is video-only");
        assert!(p1.contains(&"-f".to_string()) && p1.contains(&"null".to_string()));
        assert_eq!(p1.last().unwrap(), "/dev/null");
        // The null muxer rejects mov/metadata options — they belong to pass 2 only.
        assert!(!p1.contains(&"-movflags".to_string()) && !p1.contains(&"-metadata".to_string()));
        let p2 = build_export_args_phase(
            &timeline,
            &assets,
            "/out/x.mp4",
            &opts,
            PassPhase::Second,
            "/dev/null",
            "/tmp/pl",
        )
        .unwrap();
        assert_eq!(flag_val(&p2, "-pass"), Some("2"));
        assert!(p2.contains(&"[outa]".to_string()));
        assert_eq!(flag_val(&p2, "-movflags"), Some("+faststart"));
        assert_eq!(p2.last().unwrap(), "/out/x.mp4");
    }

    #[test]
    fn filter_pix_fmt_is_threaded_through_the_graph() {
        let opts = ExportOptions {
            video_codec: Some("libx265".into()),
            pix_fmt: Some("yuv420p10le".into()),
            ..Default::default()
        };
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let args = build_export_args(&timeline, &[asset], "/out/x", &opts).unwrap();
        let filter = flag_val(&args, "-filter_complex").unwrap();
        // Both the black base and the per-clip terminal track pix_fmt — no 8-bit
        // bottleneck before the 10-bit encode.
        assert!(filter.matches("yuv420p10le").count() >= 2, "{filter}");
        assert_eq!(flag_val(&args, "-pix_fmt"), Some("yuv420p10le"));
    }

    #[test]
    fn export_format_even_clamps_and_forces_opus_48k() {
        let asset = test_asset(vec![video_stream(1921, 1081, 30.0), audio_stream(44_100, 2)]);
        let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
        let opts = ExportOptions {
            resolution: Some((1921, 1081)),
            audio_codec: Some("libopus".into()),
            ..Default::default()
        };
        let fmt = export_format(&timeline, &[asset], &opts);
        assert_eq!((fmt.width, fmt.height), (1920, 1080));
        assert_eq!(fmt.sample_rate, 48_000);
    }

    #[test]
    fn gif_uses_a_palette_and_drops_audio() {
        let opts = ExportOptions {
            container: Container::Gif,
            video_codec: Some("gif".into()),
            include_audio: false,
            ..Default::default()
        };
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
        let args = build_export_args(&timeline, &[asset], "/out/x.gif", &opts).unwrap();
        let filter = flag_val(&args, "-filter_complex").unwrap();
        assert!(filter.contains("palettegen=stats_mode=diff"), "{filter}");
        assert!(filter.contains("paletteuse=dither=bayer"), "{filter}");
        assert!(!args.contains(&"[outa]".to_string()), "gif carries no audio");
        assert!(!args.contains(&"-pix_fmt".to_string()), "gif is pal8");
        assert_eq!(flag_val(&args, "-loop"), Some("0"));
    }

    #[test]
    fn audio_bitrate_omitted_for_lossless_codecs() {
        let flac = ExportOptions {
            container: Container::Flac,
            audio_codec: Some("flac".into()),
            flac_compression: Some(8),
            ..Default::default()
        };
        let a = args_of(&flac);
        assert!(!a.contains(&"-b:a".to_string()));
        assert_eq!(flag_val(&a, "-compression_level"), Some("8"));
        let wav = ExportOptions {
            container: Container::Wav,
            audio_codec: Some("pcm_s16le".into()),
            audio_bitrate: Some("192k".into()),
            ..Default::default()
        };
        assert!(!args_of(&wav).contains(&"-b:a".to_string()), "pcm ignores a bitrate");
    }

    #[test]
    fn metadata_title_is_a_single_token() {
        let opts = ExportOptions {
            video_codec: Some("libx264".into()),
            metadata_title: Some("My Cut = v2".into()),
            ..Default::default()
        };
        let args = args_of(&opts);
        let i = args.iter().position(|a| a == "-metadata").unwrap();
        assert_eq!(args[i + 1], "title=My Cut = v2");
    }

    #[test]
    fn fps_never_emits_dash_r() {
        let opts = ExportOptions {
            video_codec: Some("libx264".into()),
            fps: Some(24.0),
            ..Default::default()
        };
        let args = args_of(&opts);
        assert!(!args.contains(&"-r".to_string()), "fps lives only in the filtergraph");
        assert!(flag_val(&args, "-filter_complex").unwrap().contains("fps=24"));
    }

    #[test]
    fn validate_export_flags_bad_combinations() {
        let webm_x264 = ExportOptions {
            container: Container::Webm,
            video_codec: Some("libx264".into()),
            ..Default::default()
        };
        assert!(!validate_export(&webm_x264, true, true).is_empty());
        let mp4_opus = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            audio_codec: Some("libopus".into()),
            ..Default::default()
        };
        assert!(!validate_export(&mp4_opus, true, true).is_empty());
        let two_pass_no_bitrate = ExportOptions {
            video_codec: Some("libx264".into()),
            rate_control: RateControl::TwoPass,
            ..Default::default()
        };
        assert!(!validate_export(&two_pass_no_bitrate, true, true).is_empty());
        let mp3_no_audio = ExportOptions {
            container: Container::Mp3,
            audio_codec: Some("libmp3lame".into()),
            ..Default::default()
        };
        assert!(!validate_export(&mp3_no_audio, true, false).is_empty());
        let ok = ExportOptions {
            video_codec: Some("libx264".into()),
            audio_codec: Some("aac".into()),
            crf: Some(20),
            ..Default::default()
        };
        assert!(validate_export(&ok, true, true).is_empty());
    }

    #[test]
    fn prores_without_pix_fmt_defaults_to_10bit_422() {
        let opts = ExportOptions {
            container: Container::Mov,
            video_codec: Some("prores_ks".into()),
            prores_profile: Some(3),
            ..Default::default()
        };
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let args = build_export_args(&timeline, &[asset], "/out/x.mov", &opts).unwrap();
        // Both the argv and the graph terminal use 4:2:2 10-bit — no 4:2:0 bottleneck.
        assert_eq!(flag_val(&args, "-pix_fmt"), Some("yuv422p10le"));
        assert!(flag_val(&args, "-filter_complex").unwrap().contains("yuv422p10le"));
        assert!(!args.contains(&"yuv420p".to_string()));
        // The 4444 profiles upgrade to 4:4:4 with alpha.
        let xq = ExportOptions {
            container: Container::Mov,
            video_codec: Some("prores_ks".into()),
            prores_profile: Some(5),
            ..Default::default()
        };
        assert_eq!(flag_val(&args_of_for(&timeline_mov(), &xq), "-pix_fmt"), Some("yuva444p10le"));
    }

    fn timeline_mov() -> (Timeline, Vec<Asset>) {
        let asset = av_asset(Uuid::new_v4(), 30.0);
        (single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]), vec![asset])
    }
    fn args_of_for(tl: &(Timeline, Vec<Asset>), opts: &ExportOptions) -> Vec<String> {
        build_export_args(&tl.0, &tl.1, "/out/x.mov", opts).unwrap()
    }

    #[test]
    fn x265_invalid_tune_is_dropped_and_flagged() {
        // `film` is an x264-only tune; x265 would fail to open the encoder.
        let opts = ExportOptions {
            video_codec: Some("libx265".into()),
            tune: Some("film".into()),
            ..Default::default()
        };
        assert!(
            !args_of(&opts).contains(&"-tune".to_string()),
            "an invalid tune must not reach ffmpeg"
        );
        assert!(!validate_export(&opts, true, true).is_empty(), "validation must flag it");
        // A valid x265 tune is kept.
        let ok = ExportOptions {
            video_codec: Some("libx265".into()),
            tune: Some("grain".into()),
            ..Default::default()
        };
        assert_eq!(flag_val(&args_of(&ok), "-tune"), Some("grain"));
        assert!(validate_export(&ok, true, true).is_empty());
    }

    #[test]
    fn cover_fills_the_frame_where_contain_letterboxes_it() {
        let clip = make_clip(Uuid::new_v4(), 0.0, 5.0, 0.0);
        // A vertical delivery of landscape footage: the whole point of `cover`.
        let vertical = ExportFormat {
            width: 1080,
            height: 1920,
            ..ExportFormat::default()
        };

        let contained = video_clip_chain(&clip, &vertical, &ClipFx::default(), false, "c0");
        assert!(contained.contains("force_original_aspect_ratio=decrease"));
        assert!(contained.contains("pad=1080:1920"), "contain must letterbox: {contained}");
        assert!(!contained.contains("crop=1080:1920"));

        let covered = video_clip_chain(
            &clip,
            &ExportFormat {
                fit: Fit::Cover,
                ..vertical
            },
            &ClipFx::default(),
            false,
            "c0",
        );
        assert!(covered.contains("force_original_aspect_ratio=increase"));
        assert!(covered.contains("crop=1080:1920"), "cover must crop: {covered}");
        assert!(!covered.contains("pad="), "cover must not letterbox: {covered}");
    }

    #[test]
    fn the_delivery_format_sets_the_frame_every_render_path_uses() {
        let mut timeline = Timeline::default();
        // No delivery frame: the shape still follows the footage / the 1080p default.
        let fmt = export_format(&timeline, &[], &ExportOptions::default());
        assert_eq!((fmt.width, fmt.height), (1920, 1080));
        assert_eq!(fmt.fit, Fit::Contain);

        timeline.format = Some(Delivery::new(1080, 1920, Fit::Cover));
        let fmt = export_format(&timeline, &[], &ExportOptions::default());
        assert_eq!((fmt.width, fmt.height), (1080, 1920), "the project frame wins over the footage");
        assert_eq!(fmt.fit, Fit::Cover, "and brings its fit, so the preview crops like the export");
    }

    #[test]
    fn an_explicit_export_resolution_still_overrides_the_delivery_format() {
        let timeline = Timeline {
            format: Some(Delivery::new(1080, 1920, Fit::Cover)),
            ..Timeline::default()
        };
        let opts = ExportOptions {
            resolution: Some((3840, 2160)),
            ..Default::default()
        };
        let fmt = export_format(&timeline, &[], &opts);
        assert_eq!((fmt.width, fmt.height), (3840, 2160));
        // The fit is not overridden by a *default* Contain — only an explicit
        // Cover would differ, and Contain is what a one-off resize wants anyway.
        assert_eq!(fmt.fit, Fit::Cover);
    }

    #[test]
    fn a_delivery_frame_is_even_clamped_and_never_zero() {
        let d = Delivery::new(1081, 0, Fit::Cover);
        assert_eq!((d.width, d.height), (1080, 2));
    }

    #[test]
    fn the_preview_canvas_follows_the_delivery_frame() {
        let timeline = Timeline {
            format: Some(Delivery::new(1080, 1920, Fit::Cover)),
            ..Timeline::default()
        };
        // Capped to max_width, but at the delivery aspect — not the footage's.
        assert_eq!(preview_resolution(&timeline, &[], 540), (540, 960));
        // Already inside the cap: kept as-is.
        assert_eq!(preview_resolution(&timeline, &[], 2000), (1080, 1920));
    }

    #[test]
    fn the_scrubbed_still_crops_like_the_export_when_the_delivery_covers() {
        let canvas = |fit| StillCanvas {
            w: 1080,
            h: 1920,
            fit,
            sf: String::new(),
        };
        let contained = still_clip_chain(
            &Transform::default(),
            &Color::default(),
            &[],
            None,
            &canvas(Fit::Contain),
        );
        assert!(contained.contains("force_original_aspect_ratio=decrease"));
        assert!(contained.contains("pad=1080:1920"), "contain letterboxes: {contained}");

        let covered = still_clip_chain(&Transform::default(), &Color::default(), &[], None, &canvas(Fit::Cover));
        assert!(covered.contains("force_original_aspect_ratio=increase"));
        assert!(covered.contains("crop=1080:1920"), "cover crops: {covered}");
        assert!(!covered.contains("pad="), "and never letterboxes: {covered}");
    }

    #[test]
    fn fit_defaults_to_the_historical_letterbox() {
        assert_eq!(ExportOptions::default().fit, Fit::Contain);
        let fmt = export_format(&Timeline::default(), &[], &ExportOptions::default());
        assert_eq!(fmt.fit, Fit::Contain);
        // And an explicit choice reaches the format the graph is built from.
        let fmt = export_format(
            &Timeline::default(),
            &[],
            &ExportOptions {
                fit: Fit::Cover,
                ..Default::default()
            },
        );
        assert_eq!(fmt.fit, Fit::Cover);
    }

    #[test]
    fn the_monitors_effect_chain_is_the_export_chain() {
        assert_eq!(audio_effects_filter(&[]), None);
        let effects = vec![AudioEffect::Highpass { hz: 80.0 }, AudioEffect::Gate { threshold_db: -40.0 }];
        let chain = audio_effects_filter(&effects).expect("a chain");
        assert_eq!(chain, "highpass=f=80,agate=threshold=0.01");
        // The preview decodes through exactly what the export renders, so a clip
        // whose chain is auralized cannot drift from the mix it will become.
        let mut clip = make_clip(Uuid::new_v4(), 0.0, 5.0, 0.0);
        clip.audio = effects;
        let exported = audio_clip_chain(&clip, &ExportFormat::default(), &ClipFx::default(), "stereo");
        assert!(exported.contains(&chain), "export chain {exported} must contain {chain}");
    }

    // ---- live preview streaming --------------------------------------------

    #[test]
    fn preview_args_stream_mjpeg_from_the_playhead() {
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let args = build_preview_args(&timeline, &[asset], 4.0, 24.0, 960, 6).unwrap();

        assert_eq!(args.last().unwrap(), "pipe:1");
        assert_eq!(flag_val(&args, "-f"), Some("image2pipe"));
        assert_eq!(flag_val(&args, "-c:v"), Some("mjpeg"));
        assert_eq!(flag_val(&args, "-q:v"), Some("6"));
        // Video only: audio would just compete with the Web Audio engine that
        // already owns playback sound.
        assert!(args.contains(&"-an".to_string()));
        assert!(args.contains(&"[outv]".to_string()));
        assert!(!args.contains(&"[outa]".to_string()));
        // The graph is the export's, so the composite is identical.
        assert!(args.contains(&"-filter_complex".to_string()));
        // Starting at 4s seeks into the source rather than decoding from zero.
        assert_eq!(flag_val(&args, "-ss"), Some("4"));
    }

    #[test]
    fn preview_args_scale_down_but_keep_aspect() {
        let asset = av_asset(Uuid::new_v4(), 30.0); // 1920x1080
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let assets = [asset];
        assert_eq!(preview_resolution(&timeline, &assets, 960), (960, 540));
        // Already small enough: left alone rather than upscaled.
        assert_eq!(preview_resolution(&timeline, &assets, 4096), (1920, 1080));
    }

    #[test]
    fn preview_honors_mute_and_solo() {
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let timeline = timeline_of(vec![Track {
            muted: true,
            ..video_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)])
        }]);
        // The only picture is muted, so there is nothing left to composite —
        // the same gate the export applies, so playback matches the render.
        assert!(build_preview_args(&timeline, &[asset], 0.0, 24.0, 960, 6).is_err());
    }

    #[test]
    fn preview_refuses_when_there_is_nothing_to_play() {
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let assets = std::slice::from_ref(&asset);
        // Past the end of the timeline.
        assert!(build_preview_args(&timeline, assets, 10.0, 24.0, 960, 6).is_err());
        // No video at all.
        assert!(build_preview_args(&Timeline::default(), assets, 0.0, 24.0, 960, 6).is_err());
    }

    #[test]
    fn jpeg_frames_split_on_their_markers() {
        let frame = |body: &[u8]| {
            let mut v = vec![0xFF, 0xD8];
            v.extend_from_slice(body);
            v.extend_from_slice(&[0xFF, 0xD9]);
            v
        };
        let a = frame(&[1, 2, 3]);
        let b = frame(&[4, 5]);

        // One complete frame, consumed exactly.
        assert_eq!(next_jpeg(&a), Some((0, a.len())));

        // Two back to back: the first is returned, the rest left for later.
        let mut both = a.clone();
        both.extend_from_slice(&b);
        let (s, e) = next_jpeg(&both).unwrap();
        assert_eq!(&both[s..e], &a[..]);
        assert_eq!(next_jpeg(&both[e..]), Some((0, b.len())));

        // A partial tail is not a frame yet.
        assert_eq!(next_jpeg(&a[..a.len() - 1]), None);
        assert_eq!(next_jpeg(&[]), None);

        // Leading junk before the start marker is skipped, not mistaken for data.
        let mut noisy = vec![0x00, 0xAB, 0xFF];
        noisy.extend_from_slice(&a);
        let (s, e) = next_jpeg(&noisy).unwrap();
        assert_eq!(&noisy[s..e], &a[..]);
    }

    /// End to end against the real `ffmpeg` binary: synthesize a clip, play two
    /// seconds of a two-track timeline out of it, and check real JPEGs arrive at
    /// roughly the requested rate. Not part of the normal (binary-free) run:
    /// `cargo test -p kerf-core --no-default-features -- --ignored streams_real_frames`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn streams_real_frames_from_a_composited_timeline() {
        let dir = std::env::temp_dir().join(format!("kerf-preview-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let media = dir.join("src.mp4");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc=size=640x360:rate=30:duration=6"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&media)
            .status()
            .expect("run ffmpeg");
        assert!(ok.success(), "could not synthesize test media");

        let mut asset = av_asset(Uuid::new_v4(), 6.0);
        asset.path = media.to_string_lossy().into_owned();
        asset.streams = vec![video_stream(640, 360, 30.0), audio_stream(48_000, 2)];
        // Two video tracks, so the run exercises real compositing (overlay of a
        // second layer over the first) rather than a single passthrough.
        let base = make_clip(asset.id, 0.0, 6.0, 0.0);
        let mut over = make_clip(asset.id, 2.0, 4.0, 1.0);
        over.transform.scale = 0.5;
        let timeline = timeline_of(vec![video_track(vec![base]), video_track(vec![over])]);

        let mut frames: Vec<PreviewFrame> = Vec::new();
        let started = std::time::Instant::now();
        let result = stream_preview(&timeline, &[asset], 1.0, 24.0, &mut |f| {
            frames.push(f);
            frames.len() < 24 // stop after a second's worth
        });
        let elapsed = started.elapsed();
        let _ = std::fs::remove_dir_all(&dir);
        result.expect("stream");

        assert_eq!(frames.len(), 24);
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(&f.jpeg[..2], &[0xFF, 0xD8], "frame {i} is not a JPEG");
            assert_eq!(&f.jpeg[f.jpeg.len() - 2..], &[0xFF, 0xD9], "frame {i} is truncated");
            assert!(f.jpeg.len() > 1000, "frame {i} is suspiciously small");
            // Timeline times advance from the requested start at the frame rate.
            assert!((f.time - (1.0 + i as f64 / 24.0)).abs() < 1e-9);
        }
        // Paced to real time rather than dumped as fast as it renders: a second
        // of frames takes about a second (with slack for the graph starting up).
        assert!(elapsed.as_secs_f64() > 0.8, "playback ran ahead of real time: {elapsed:?}");
    }

    /// The graph features most likely to break only at runtime — a looped still
    /// input, a crossfade, a colour grade, a video effect and a `drawtext`
    /// overlay — all in one playable timeline. Unit tests can only check the
    /// argv; this checks ffmpeg actually accepts it.
    /// `cargo test -p kerf-core --no-default-features -- --ignored plays_a_timeline_with_everything`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn plays_a_timeline_with_everything_on_it() {
        let dir = std::env::temp_dir().join(format!("kerf-preview-rich-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let media = dir.join("src.mp4");
        let still = dir.join("card.png");
        let run = |args: Vec<String>| {
            let ok = command(&ffmpeg_bin()).args(&args).status().expect("run ffmpeg");
            assert!(ok.success(), "ffmpeg failed for {args:?}");
        };
        let s = |v: &str| v.to_string();
        run(vec![
            s("-hide_banner"),
            s("-loglevel"),
            s("error"),
            s("-y"),
            s("-f"),
            s("lavfi"),
            s("-i"),
            s("testsrc=size=640x360:rate=30:duration=6"),
            s("-f"),
            s("lavfi"),
            s("-i"),
            s("sine=frequency=440:duration=6"),
            s("-c:v"),
            s("libx264"),
            s("-pix_fmt"),
            s("yuv420p"),
            s("-c:a"),
            s("aac"),
            s("-shortest"),
            media.to_string_lossy().into_owned(),
        ]);
        run(vec![
            s("-hide_banner"),
            s("-loglevel"),
            s("error"),
            s("-y"),
            s("-f"),
            s("lavfi"),
            s("-i"),
            s("color=c=red:size=640x360"),
            s("-frames:v"),
            s("1"),
            still.to_string_lossy().into_owned(),
        ]);

        let mut video = av_asset(Uuid::new_v4(), 6.0);
        video.path = media.to_string_lossy().into_owned();
        video.streams = vec![video_stream(640, 360, 30.0), audio_stream(48_000, 2)];
        let mut card = av_asset(Uuid::new_v4(), 5.0);
        card.path = still.to_string_lossy().into_owned();
        card.streams = vec![image_stream(640, 360)];

        // A graded, blurred clip; a still crossfading in over it; a title on top.
        let mut base = make_clip(video.id, 0.0, 6.0, 0.0);
        base.color = Color {
            brightness: 0.05,
            contrast: 1.2,
            saturation: 0.8,
            gamma: 1.0,
            temperature: 0.3,
        };
        base.effects = vec![VideoEffect::Blur { sigma: 2.0 }];
        let mut over = make_clip(card.id, 0.0, 3.0, 1.5);
        over.transition_in = Some(crate::model::Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        over.transform.scale = 0.6;
        let mut timeline = timeline_of(vec![video_track(vec![base]), video_track(vec![over])]);
        timeline.overlays = vec![TextOverlay::new("Rough cut", 0.0, 6.0)];

        let mut frames = 0usize;
        let result = stream_preview(&timeline, &[video, card], 1.0, 24.0, &mut |f| {
            assert_eq!(&f.jpeg[..2], &[0xFF, 0xD8]);
            frames += 1;
            frames < 12
        });
        let _ = std::fs::remove_dir_all(&dir);
        result.expect("stream");
        assert_eq!(frames, 12);
    }

    /// A real vertical export of landscape footage, both ways. `Cover` is only
    /// worth anything if the delivered frame actually has picture at the top and
    /// bottom instead of black, which no argv assertion can tell you.
    /// `cargo test -p kerf-core --no-default-features -- --ignored cover_really_fills`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn cover_really_fills_a_vertical_frame() {
        let dir = std::env::temp_dir().join(format!("kerf-fit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let media = dir.join("src.mp4");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc=size=1920x1080:rate=30:duration=2"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&media)
            .status()
            .expect("run ffmpeg");
        assert!(ok.success());

        let mut asset = av_asset(Uuid::new_v4(), 2.0);
        asset.path = media.to_string_lossy().into_owned();
        asset.streams = vec![video_stream(1920, 1080, 30.0)];
        let timeline = single(vec![make_clip(asset.id, 0.0, 2.0, 0.0)]);

        // Mean luma of the top 100 rows of the first frame: black bars sit near
        // 16 (limited-range black), real picture well above it.
        let top_luma = |file: &Path| -> f64 {
            let raw = dir.join("top.raw");
            let ok = command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .arg("-i")
                .arg(file)
                .args([
                    "-vf",
                    "crop=1080:100:0:0",
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "gray",
                ])
                .arg(&raw)
                .status()
                .expect("run ffmpeg");
            assert!(ok.success());
            let bytes = std::fs::read(&raw).expect("raw");
            bytes.iter().map(|b| *b as f64).sum::<f64>() / bytes.len() as f64
        };

        let opts = |fit| ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            resolution: Some((1080, 1920)),
            fit,
            include_audio: false,
            ..Default::default()
        };
        let contained = dir.join("contain.mp4");
        let covered = dir.join("cover.mp4");
        render_with(&timeline, &[asset.clone()], &contained, &opts(Fit::Contain)).expect("contain export");
        render_with(&timeline, &[asset], &covered, &opts(Fit::Cover)).expect("cover export");

        let (dark, bright) = (top_luma(&contained), top_luma(&covered));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(dark < 30.0, "contain should letterbox the top, got luma {dark}");
        assert!(bright > 60.0, "cover should fill the top with picture, got luma {bright}");
    }

    /// The delivery frame is the point of the whole feature: with it set and
    /// **no** export resolution at all, a 16:9 source must still render a filled
    /// 1080x1920 file — the same thing the preview showed while cutting.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored delivery_format_renders`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn delivery_format_renders_the_project_frame_without_an_export_resolution() {
        let dir = std::env::temp_dir().join(format!("kerf-delivery-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let media = dir.join("src.mp4");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc=size=1920x1080:rate=30:duration=2"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&media)
            .status()
            .expect("run ffmpeg");
        assert!(ok.success());

        let mut asset = av_asset(Uuid::new_v4(), 2.0);
        asset.path = media.to_string_lossy().into_owned();
        asset.streams = vec![video_stream(1920, 1080, 30.0)];
        let timeline = Timeline {
            format: Some(Delivery::new(1080, 1920, Fit::Cover)),
            ..single(vec![make_clip(asset.id, 0.0, 2.0, 0.0)])
        };

        let out = dir.join("vertical.mp4");
        let opts = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            include_audio: false,
            ..Default::default()
        };
        render_with(&timeline, &[asset], &out, &opts).expect("delivery export");

        let probed = command(&ffprobe_bin())
            .args(["-v", "error", "-select_streams", "v:0"])
            .args(["-show_entries", "stream=width,height", "-of", "csv=p=0"])
            .arg(&out)
            .output()
            .expect("run ffprobe");
        let dims = String::from_utf8_lossy(&probed.stdout).trim().to_string();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(dims, "1080,1920", "the project frame decides the file's shape");
    }
}
