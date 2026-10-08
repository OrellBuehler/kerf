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
use std::time::Instant;

use super::cpu;
use super::source_probe::{ProbeCache, SOURCE_PROBE_TIMEOUT};
use super::ProbeResult;
use crate::clip_timing::{
    clip_seek, clip_source_window, transition_fx, ClipFx, ClipTiming, FadeEdge, FadeStep, FadeTint, HEAD_PADDED_SUFFIX,
};
use crate::error::{Error, Result};
use crate::model::{
    Asset, AudioEffect, Clip, Color, Delivery, Hdr, Mask, MaskShape, Projection, Property, Reframe, ReframeKeyframe,
    ResolvedReframe, SalienceMap, StreamInfo, StreamKind, TextOverlay, TimeRange, Timeline, Transform, VideoEffect,
};
use crate::render_plan::{active_video_clips, still_size, CompositeColorPolicy};

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

pub(super) fn ffprobe_bin() -> String {
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

/// Record that an accelerated decode failed where the software retry of the very
/// same input worked, so every later preview-class decode skips the accelerated
/// attempt (see [`decode_hwaccel`]). For background decodes living outside this
/// file that run their own retry.
pub(super) fn disable_hwaccel() {
    HWACCEL_OK.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// [`disable_hwaccel`] for a decoder that lives outside the engine (`kerf-gpu`'s
/// long-lived frame decodes): call it when a run with `-hwaccel` died before its
/// first frame and the same run in software worked. One process-wide learned
/// fallback, shared with the preview path, so a broken accelerator costs one failed
/// attempt in the whole process, not one per decoder.
pub fn disable_decode_hwaccel() {
    if HWACCEL_OK.swap(false, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!("hardware decode failed where software worked; decoding in software from now on");
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
    /// How long one candidate gets to encode a single frame.
    const HW_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

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
                let mut probe = command(&bin);
                probe
                    .args([
                        "-hide_banner",
                        "-v",
                        "error",
                        "-f",
                        "lavfi",
                        "-i",
                        "color=black:s=256x256:r=30:d=0.2",
                    ])
                    .args(["-frames:v", "1", "-pix_fmt", "nv12", "-c:v", enc, "-f", "null", "-"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                // A one-frame encode finishes in well under a second where the
                // encoder works; a driver that hangs instead of failing must not
                // take every later caller of this `OnceLock` with it.
                status_within(&mut probe, HW_PROBE_TIMEOUT)
                    .ok()
                    .flatten()
                    .is_some_and(|s| s.success())
            })
            .map(|s| s.to_string())
            .collect();
        tracing::info!(encoders = ?found, "hardware video encoders detected");
        found
    })
}

/// Run `cmd` to completion, but give up on it after `limit`: the child is killed
/// and `None` returned. stdin is closed so nothing can wait on a keypress.
///
/// For probes whose *only* job is to answer "does this work here", where a
/// driver that wedges instead of failing would otherwise take the caller with it
/// (the encoder probe runs inside a `OnceLock` every other thread waits on).
pub(super) fn status_within(cmd: &mut Command, limit: std::time::Duration) -> std::io::Result<Option<std::process::ExitStatus>> {
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

/// A `Command` for a **background** ffmpeg run: everything [`command`] does,
/// plus below-normal scheduling priority so a render or an analysis pass never
/// out-prioritizes the window the user is actually looking at. Interactive
/// decodes (a scrubbed frame, the preview stream) keep normal priority — they
/// are short, and the whole point of them is to land now.
pub(super) fn bg_command(bin: &str) -> Command {
    let mut cmd = command(bin);
    cpu::background(&mut cmd);
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
    avg_frame_rate: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u16>,
    duration: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    color_space: Option<String>,
    pix_fmt: Option<String>,
    /// Container-level stream tags. Only `rotate` matters: FFmpeg before 6.0
    /// reported a phone's turn as this tag (clockwise) as well as in the
    /// `Display Matrix` side data (counter-clockwise).
    #[serde(default)]
    tags: Option<std::collections::HashMap<String, String>>,
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
    rotation: Option<f64>,
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
        let is_video = kind == StreamKind::Video;
        let rotation = if is_video { display_rotation(s) } else { 0 };
        let (width, height) = displayed_size(s.width, s.height, rotation);
        streams.push(StreamInfo {
            index: s.index,
            kind,
            codec: s.codec_name.clone().unwrap_or_default(),
            width,
            height,
            fps: nominal_fps(
                s.r_frame_rate.as_deref().and_then(parse_rational),
                s.avg_frame_rate.as_deref().and_then(parse_rational),
            ),
            sample_rate: s.sample_rate.as_deref().and_then(|r| r.parse().ok()),
            channels: s.channels,
            image: still && kind == StreamKind::Video,
            projection: if is_video { detect_projection(path, s) } else { None },
            rotation,
            color_transfer: if is_video {
                known_color(s.color_transfer.as_deref())
            } else {
                None
            },
            color_primaries: if is_video {
                known_color(s.color_primaries.as_deref())
            } else {
                None
            },
            pix_fmt: if is_video {
                s.pix_fmt.clone().filter(|p| !p.is_empty() && p != "none")
            } else {
                None
            },
            color_space: if is_video {
                known_color(s.color_space.as_deref())
            } else {
                None
            },
        });
    }
    let duration = format_dur.unwrap_or(max_stream_dur).max(0.0);
    ProbeResult { duration, streams }
}

/// How far a video stream is turned for display, in whole degrees
/// counter-clockwise in `(-180, 180]`, from its `Display Matrix` side data (or,
/// on an FFmpeg old enough to lack that, the clockwise `rotate` tag). 0 for an
/// unturned stream.
fn display_rotation(s: &ProbeStream) -> i16 {
    let raw = s.side_data_list.iter().flatten().find_map(|sd| sd.rotation).or_else(|| {
        let tag: f64 = s.tags.as_ref()?.get("rotate")?.trim().parse().ok()?;
        Some(-tag)
    });
    let Some(raw) = raw.filter(|r| r.is_finite()) else {
        return 0;
    };
    let mut deg = raw.round().rem_euclid(360.0) as i16;
    if deg > 180 {
        deg -= 360;
    }
    deg
}

/// The rotation a 3x3 display matrix (nine 16.16 fixed-point values in file
/// order, as libav hands them over) describes, in whole degrees counter-clockwise
/// in `(-180, 180]` — `av_display_rotation_get`, for the libav probe, which gets
/// the raw matrix where ffprobe gets the angle.
#[cfg_attr(not(feature = "ffmpeg"), allow(dead_code))]
pub(crate) fn matrix_rotation(m: &[i32; 9]) -> i16 {
    let f = |i: usize| m[i] as f64 / 65536.0;
    let (s0, s1) = (f(0).hypot(f(3)), f(1).hypot(f(4)));
    if s0 == 0.0 || s1 == 0.0 {
        return 0;
    }
    let deg = -(f(1) / s1).atan2(f(0) / s0).to_degrees();
    let mut deg = deg.round().rem_euclid(360.0) as i16;
    if deg > 180 {
        deg -= 360;
    }
    deg
}

/// The size a stream is *shown* at: the coded size, swapped when the stream is
/// turned a quarter. Every FFmpeg decode autorotates, so this is the size of the
/// pixels the engine actually receives — the one every piece of frame geometry
/// (the project frame, fit, crop, smart crop) has to be done against.
pub(crate) fn displayed_size(width: Option<u32>, height: Option<u32>, rotation: i16) -> (Option<u32>, Option<u32>) {
    if rotation.rem_euclid(180) == 90 {
        (height, width)
    } else {
        (width, height)
    }
}

/// A colour tag as the engine stores it: `None` when the file does not say,
/// rather than ffprobe's literal `"unknown"`.
pub(crate) fn known_color(tag: Option<&str>) -> Option<String> {
    tag.filter(|t| !t.is_empty() && *t != "unknown" && *t != "unspecified")
        .map(str::to_string)
}

/// The frame rate to build a project around. `r_frame_rate` is "the lowest rate
/// all timestamps are a multiple of", which for a variable-frame-rate phone clip
/// can be a multiple of the real one (a ~30 fps clip with jittery timestamps
/// probes as 120) and would turn the whole export into a 4x-duplicated render.
/// When it runs far past the stream's average the average, snapped to the
/// nearest standard rate, is the nominal rate; otherwise `r_frame_rate` stands,
/// since a clip that merely dropped a few frames still belongs to its nominal rate.
pub(crate) fn nominal_fps(r_frame_rate: Option<f64>, avg_frame_rate: Option<f64>) -> Option<f64> {
    const STANDARD: [f64; 12] = [23.976, 24.0, 25.0, 29.97, 30.0, 48.0, 50.0, 59.94, 60.0, 100.0, 119.88, 120.0];
    let r = r_frame_rate.filter(|r| *r > 0.0);
    let avg = avg_frame_rate.filter(|a| *a > 0.0);
    match (r, avg) {
        (Some(r), Some(avg)) if r > avg * 1.5 => Some(
            STANDARD
                .iter()
                .copied()
                .filter(|s| (s - avg).abs() / s < 0.03)
                .min_by(|a, b| (a - avg).abs().total_cmp(&(b - avg).abs()))
                .unwrap_or(avg),
        ),
        (Some(r), _) => Some(r),
        (None, avg) => avg,
    }
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

// ---- HDR → SDR -------------------------------------------------------------

#[cfg(test)]
thread_local! {
    /// What [`zscale_available`] answers on this thread while a test pins it.
    static ZSCALE_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Run `f` as if this ffmpeg did (`true`) or did not (`false`) have `zscale`.
#[cfg(test)]
fn with_zscale<R>(available: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ZSCALE_OVERRIDE.with(|c| c.set(self.0));
        }
    }
    let _reset = Reset(ZSCALE_OVERRIDE.with(|c| c.replace(Some(available))));
    f()
}

/// Whether this ffmpeg can tone-map with `zscale` + `tonemap`, probed once per
/// process. `zscale` needs libzimg, which a minimal or hand-built ffmpeg often
/// lacks; `tonemap` itself is part of every build.
fn zscale_available() -> bool {
    // Tests pin the answer so an argv oracle does not depend on the ffmpeg
    // installed where it runs (see `golden`).
    #[cfg(test)]
    if let Some(forced) = ZSCALE_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let bin = ffmpeg_bin();
        let ok = command(&bin)
            .args(["-hide_banner", "-loglevel", "quiet", "-filters"])
            .stdin(Stdio::null())
            .output()
            .map(|o| {
                let list = String::from_utf8_lossy(&o.stdout);
                let has = |name: &str| list.lines().any(|l| l.split_whitespace().nth(1) == Some(name));
                has("zscale") && has("tonemap")
            })
            // A binary that will not even run renders nothing either way; only one
            // that runs and lacks the filters gets the fallback. It also keeps the
            // graph builders' output from depending on whether a test machine has
            // ffmpeg installed.
            .unwrap_or(true);
        tracing::debug!(available = ok, "probed ffmpeg for zscale + tonemap");
        ok
    })
}

// ---- the composite's colour policy ---------------------------------------------

/// How this ffmpeg picks the matrix of a composite — see [`CompositeColorPolicy`].
/// Probed by **running the still graph** twice on the same picture, once in a
/// clip tagged BT.709 and once in an untagged one, and seeing whether the
/// finished composites differ: FFmpeg 6 hands the encoder an untagged frame
/// (BT.601 whatever the clip was, so the two agree); FFmpeg 9 negotiates the
/// colourspace across the overlay chain, so the tagged clip's composite is
/// converted as BT.709 and the two differ. Measured, not read off a version
/// string — and the probe checks that the tag **survived** into the clip it
/// measures (an `ffprobe` of it), because a tag that was lost on the way would
/// read as "FFmpeg 6" on any build.
///
/// **A probe that could not tell is [`CompositeColorPolicy::Unknown`], not a
/// guess.** FFmpeg 6 and 9 disagree precisely on BT.709 and BT.2020 footage, so
/// no answer is safe for it: `Unknown` mirrors only the stacks both agree on
/// (all BT.601-class) and refuses the rest. It is not remembered — a measured
/// policy is kept for the process, a failed probe is retried on a later call, at
/// most once per backoff period (5 s, doubling to 5 min) so a broken ffmpeg is
/// not respawned for every frame. The whole probe is bounded
/// ([`POLICY_PROBE_TIMEOUT`]): a hung ffmpeg cannot hold a caller for longer.
///
/// **The first call blocks** — 70 to 200 ms where ffmpeg works (six short runs,
/// two at a time; 70 ms measured on a static build, 200 on a distro one) and up
/// to the timeout where it does not — and concurrent first callers wait behind it,
/// so call it from a blocking thread, not an async task or the UI's. Later calls
/// are a lock and a read.
pub fn composite_color_policy() -> CompositeColorPolicy {
    static PROBE: Mutex<PolicyProbe> = Mutex::new(PolicyProbe::new());
    cached_policy(&PROBE, Instant::now(), || {
        measure_composite_color_policy(&ffmpeg_bin(), &ffprobe_bin(), POLICY_PROBE_TIMEOUT)
    })
}

/// The longest the whole policy probe may take. It is six short runs of a
/// 64x64 clip, anything near this is a wedged ffmpeg, and every other caller of
/// [`composite_color_policy`] waits behind it.
const POLICY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// What the process knows about its ffmpeg's composite colour policy.
struct PolicyProbe {
    /// The measured policy, once there is one: kept for good.
    measured: Option<CompositeColorPolicy>,
    /// Failed probes in a row, and the earliest time the next one may run.
    failures: u32,
    retry_at: Option<Instant>,
}

impl PolicyProbe {
    const fn new() -> Self {
        Self {
            measured: None,
            failures: 0,
            retry_at: None,
        }
    }
}

/// The wait before the probe may run again after `failures` failures in a row:
/// 5 s, doubling, capped at 5 minutes.
fn policy_probe_backoff(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300))
}

/// The policy in `state`, measuring it with `measure` if nothing is known yet and
/// the backoff after the last failure has passed. Holds the lock while it
/// measures, so concurrent first callers wait for one probe rather than each
/// running their own; the probe is bounded, so the wait is.
fn cached_policy(
    state: &Mutex<PolicyProbe>,
    now: Instant,
    measure: impl FnOnce() -> Option<CompositeColorPolicy>,
) -> CompositeColorPolicy {
    let mut s = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(policy) = s.measured {
        return policy;
    }
    if s.retry_at.is_some_and(|t| now < t) {
        return CompositeColorPolicy::Unknown;
    }
    match measure() {
        Some(policy) => {
            tracing::debug!(?policy, "probed ffmpeg's composite colour policy");
            s.measured = Some(policy);
            policy
        }
        None => {
            s.failures += 1;
            let wait = policy_probe_backoff(s.failures);
            s.retry_at = Some(Instant::now() + wait);
            tracing::warn!(
                failures = s.failures,
                retry_in_secs = wait.as_secs(),
                "could not measure ffmpeg's composite colour policy; only BT.601-class stacks are drawn on the GPU until it can"
            );
            CompositeColorPolicy::Unknown
        }
    }
}

/// The probe picture: a flat YCbCr whose red the two matrices convert to values
/// well apart (about 20 levels on FFmpeg's converter).
const POLICY_PROBE_YUV: (u8, u8, u8) = (60, 128, 230);

/// Which policy two measured reds say: the same red from the tagged and the
/// untagged clip means the tag did not reach the conversion. A difference between
/// the two readings is not a result.
fn policy_from_reds(tagged: u8, untagged: u8) -> Option<CompositeColorPolicy> {
    match tagged.abs_diff(untagged) {
        0..=3 => Some(CompositeColorPolicy::FixedBt601),
        10.. => Some(CompositeColorPolicy::BottomLayerTag),
        _ => None,
    }
}

/// Measure the policy of `ffmpeg` (with `ffprobe` to check the probe clips), or
/// `None` when it cannot be read: a binary that will not run, a run that fails or
/// outlasts `timeout`, a tag that did not survive, readings that do not decide.
fn measure_composite_color_policy(ffmpeg: &str, ffprobe: &str, timeout: std::time::Duration) -> Option<CompositeColorPolicy> {
    let deadline = Instant::now() + timeout;
    // The two clips are independent, so they are measured side by side.
    let (tagged, untagged) = std::thread::scope(|scope| {
        let tagged = scope.spawn(|| probe_composite_red(ffmpeg, ffprobe, true, deadline));
        let untagged = probe_composite_red(ffmpeg, ffprobe, false, deadline);
        (tagged.join().ok().flatten(), untagged)
    });
    let (tagged, untagged) = (tagged?, untagged?);
    tracing::debug!(tagged, untagged, "composite colour probe");
    policy_from_reds(tagged, untagged)
}

/// Run `cmd` with `input` on its stdin and return its stdout if it exits
/// successfully before `deadline` — killed, and `None`, if it does not. The pipes
/// are served from side threads, so a child that stops reading or never exits
/// cannot wedge the caller; on a timeout those threads are left to finish on
/// their own (a wrapper script that spawned the real ffmpeg keeps the pipe open
/// past the kill, and the caller must not wait for it).
pub(super) fn run_piped_until(cmd: &mut Command, input: Vec<u8>, deadline: Instant) -> Option<Vec<u8>> {
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
    // it all, which is not the feeder's problem.
    let feeder = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
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
    let _ = feeder.join();
    let out = reader.join().ok()?;
    status.success().then_some(out)
}

/// The red of the middle pixel of the still graph's composite over a one-frame
/// clip of [`POLICY_PROBE_YUV`], with the clip tagged BT.709 or not.
fn probe_composite_red(ffmpeg: &str, ffprobe: &str, tag_bt709: bool, deadline: Instant) -> Option<u8> {
    let (y, u, v) = POLICY_PROBE_YUV;
    // 1. A one-frame clip of exactly that picture (raw planes, so no generator gets
    // to round it), tagged the way a camera's file is, or not.
    let mut raw = vec![y; 64 * 64];
    raw.extend(std::iter::repeat_n(u, 32 * 32));
    raw.extend(std::iter::repeat_n(v, 32 * 32));
    // The tag goes on the *input* (a decoder option): put on the output of a
    // raw-video encode it would make FFmpeg 9 convert the picture into the new
    // matrix on the way, and the two clips would no longer hold the same planes.
    let mut maker = command(ffmpeg);
    maker.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-s",
        "64x64",
    ]);
    if tag_bt709 {
        maker.args(["-colorspace", "bt709"]);
    }
    maker.args([
        "-i",
        "pipe:0",
        "-frames:v",
        "1",
        "-c:v",
        "ffv1",
        "-pix_fmt",
        "yuv420p",
        "-f",
        "matroska",
        "pipe:1",
    ]);
    let clip = run_piped_until(&mut maker, raw, deadline).filter(|c| !c.is_empty())?;

    // 2. The tag must have survived into the clip, as the probe the app imports
    // with reads it: a clip that lost it (a wrapper, a build that drops the
    // option) would make the two measurements identical on *any* FFmpeg, and the
    // probe would read "FFmpeg 6" off an FFmpeg 9.
    let mut checker = command(ffprobe);
    checker.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=color_space",
        "-of",
        "csv=p=0",
        "-i",
        "pipe:0",
    ]);
    let found = run_piped_until(&mut checker, clip.clone(), deadline)?;
    let found = String::from_utf8_lossy(&found);
    let found = found.trim();
    let survived = if tag_bt709 {
        found == "bt709"
    } else {
        !matches!(found, "bt709" | "bt2020nc" | "bt2020c")
    };
    if !survived {
        tracing::debug!(found, tag_bt709, "the probe clip's colour tag is not what was written");
        return None;
    }

    // 3. The still graph of a one-clip timeline over that clip, built by the very
    // builder the stills use, ending in raw RGB.
    let asset = Asset {
        id: uuid::Uuid::new_v4(),
        path: "pipe:0".to_string(),
        name: "probe".to_string(),
        duration: 1.0,
        streams: vec![StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "ffv1".to_string(),
            width: Some(64),
            height: Some(64),
            fps: Some(1.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: Some("yuv420p".to_string()),
            color_space: tag_bt709.then(|| "bt709".to_string()),
        }],
        imported_at: chrono::Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
    };
    let timeline = Timeline {
        tracks: vec![crate::model::Track {
            clips: vec![Clip::new(asset.id, 0.0, 1.0, 0.0)],
            ..crate::model::Track::new(StreamKind::Video, "V1")
        }],
        overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    };
    let args = build_still_args(
        &timeline,
        std::slice::from_ref(&asset),
        &ExportOptions::default(),
        0.0,
        64,
        None,
        &StillOutput::RgbPipe,
    )
    .ok()?;
    let mut render = command(ffmpeg);
    render.args(&args);
    let out = run_piped_until(&mut render, clip, deadline)?;
    if out.len() != 64 * 64 * 3 {
        return None;
    }
    Some(out[(32 * 64 + 32) * 3])
}

/// The filter chain that turns decoded HDR frames into 8-bit SDR BT.709 —
/// pure, so it is unit-tested, with the filter probe's answer passed in.
///
/// The `zscale` path is the standard one: light-linear float RGB in the BT.709
/// gamut, a **mobius** roll-off (linear up to 70% so the midtones — most of a
/// picture — are left alone, then a smooth shoulder rather than `clip`'s hard
/// edge or `hable`'s global contrast change), and back to BT.709 with error
/// diffusion so a sky does not band. The input's transfer, primaries and matrix
/// are stated rather than read from the frame, because a phone file with its
/// colour tags stripped would otherwise abort the whole graph with "no path
/// between colorspaces". `npl=100` makes light-linear 1.0 an SDR white. The
/// output frames are tagged BT.709, so an encoder downstream writes an honest
/// SDR file.
///
/// Without `zscale` the fallback is `colorspace`, which moves the BT.2020
/// primaries and matrix into BT.709 but cannot know either HDR curve. That is
/// near-right for HLG — designed to read acceptably on an SDR display — and
/// only approximate for PQ, which is why it is the fallback.
pub(crate) fn tonemap_chain(hdr: Hdr, zscale: bool) -> String {
    if zscale {
        format!(
            "zscale=tin={}:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,\
             tonemap=tonemap=mobius:param=0.7:desat=0,\
             zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p",
            hdr.zscale_name()
        )
    } else {
        "colorspace=all=bt709:iall=bt2020:fast=1,format=yuv420p".to_string()
    }
}

/// [`tonemap_chain`] for this machine's ffmpeg.
pub(super) fn tonemap_filter(hdr: Hdr) -> String {
    tonemap_chain(hdr, zscale_available())
}

/// What the decodes that are handed a bare path rather than an [`Asset`] need to
/// know about a file's first video stream, from one `ffprobe`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SourceTraits {
    /// The HDR transfer, if the stream is HLG or PQ.
    pub hdr: Option<Hdr>,
    /// Seconds between the container's start and the video's first frame (see
    /// [`head_lead`]); `0.0` for an ordinary file.
    pub lead: f64,
    /// The container is an MP4/MOV or Matroska/WebM file: a per-packet timestamp of its own
    /// and a seek index. Transport streams, AVI, program streams and bare elementary streams
    /// are not (their timestamps are measured or guessed and their seeks land on a later
    /// keyframe), and neither is a file whose format the probe did not name.
    pub indexed: bool,
}

/// [`SourceTraits`] of the file at `path`, probed once per file and cached against the file's
/// size and modified time. One `ffprobe`, killed after [`SOURCE_PROBE_TIMEOUT`]; `None` when the
/// probe itself fails or outlasts it, and the failure is remembered for a minute (a broken or
/// hung `ffprobe` is not respawned by every frame that asks), then retried.
///
/// It spawns a process: not under a lock.
pub(crate) fn source_traits(path: &Path) -> Option<SourceTraits> {
    static CACHE: OnceLock<ProbeCache<SourceTraits>> = OnceLock::new();
    CACHE
        .get_or_init(ProbeCache::new)
        .get(path, || probe_source_traits(&ffprobe_bin(), path, SOURCE_PROBE_TIMEOUT))
}

/// One `ffprobe` of `path`, killed after `timeout`.
fn probe_source_traits(ffprobe: &str, path: &Path, timeout: std::time::Duration) -> Option<SourceTraits> {
    let mut cmd = command(ffprobe);
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=color_transfer,start_time:format=start_time,format_name",
    ])
    // JSON rather than `csv`/`default`: the stream and the format both have a
    // `start_time`, and a file with side data (a phone's display matrix) makes
    // the csv writer append a stray empty field to the value.
    .args(["-of", "json"])
    .arg(path);
    let output = run_piped_until(&mut cmd, Vec::new(), Instant::now() + timeout)?;
    Some(parse_source_traits(&String::from_utf8_lossy(&output)))
}

/// The pure half of [`source_traits`]: read the probe's JSON. Anything missing
/// or unparsable (`N/A` is simply absent) leaves the neutral value, so an odd
/// file is treated as an ordinary one.
fn parse_source_traits(json: &str) -> SourceTraits {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return SourceTraits::default();
    };
    let secs = |field: Option<&serde_json::Value>| -> Option<f64> {
        field?.as_str()?.trim().parse::<f64>().ok().filter(|t| t.is_finite())
    };
    let stream = v.get("streams").and_then(|s| s.get(0));
    let hdr = match stream
        .and_then(|s| s.get("color_transfer"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim()
    {
        "arib-std-b67" => Some(Hdr::Hlg),
        "smpte2084" => Some(Hdr::Pq),
        _ => None,
    };
    let format = v.get("format");
    // MPEG-TS (a camcorder's `.mts` / `.m2ts` included) cannot be fixed this way:
    // its demuxer measures the container's start over only the streams being read,
    // so a read with the audio discarded — which is what makes the proxy — rebases
    // the video to zero, the pad's clone and the first frame come out one tick
    // apart, and a seek into the proxy lands where a plain proxy's would. The lead
    // is reported as none, so the source keeps the plain proxy and its cache key.
    let transport_stream = format
        .and_then(|f| f.get("format_name"))
        .and_then(|n| n.as_str())
        .is_some_and(|n| n.split(',').any(|n| n == "mpegts"));
    let lead = if transport_stream {
        0.0
    } else {
        head_lead(
            secs(stream.and_then(|s| s.get("start_time"))),
            secs(format.and_then(|f| f.get("start_time"))),
        )
    };
    let indexed = format
        .and_then(|f| f.get("format_name"))
        .and_then(|n| n.as_str())
        .is_some_and(|n| n.split(',').any(|n| matches!(n, "mp4" | "matroska")));
    SourceTraits { hdr, lead, indexed }
}

/// Whether the file at `path` is an MP4/MOV or Matroska/WebM file (see
/// [`SourceTraits::indexed`]): the only containers whose frame timestamps and seeks the
/// GPU frame source's decode runs are checked against. One cached `ffprobe` per file (it
/// spawns a process: not under a lock); `false` when the probe fails.
pub fn source_is_indexed_container(path: &Path) -> bool {
    source_traits(path).is_some_and(|t| t.indexed)
}

/// The HDR transfer of the first video stream of the file at `path`, for the
/// decodes that are handed a bare path rather than an [`Asset`] (a scrubbed
/// frame, a contact sheet, a proxy). Probed once per file and cached against the
/// file's size and modified time. A generated proxy answers `None` — it was
/// tone-mapped when it was encoded, which is the point.
pub(crate) fn source_hdr(path: &Path) -> Option<Hdr> {
    source_traits(path)?.hdr
}

// ---- silence / scene analysis ---------------------------------------------

/// Detect silent spans using the `silencedetect` filter.
///
/// `noise_db` is the threshold in dBFS (e.g. `-30.0`); `min_silence` is the
/// shortest span to report, in seconds.
pub fn detect_silence(path: &Path, noise_db: f64, min_silence: f64) -> Result<Vec<TimeRange>> {
    let bin = ffmpeg_bin();
    let filter = format!("silencedetect=noise={noise_db}dB:d={min_silence}");
    // A whole-file decode: takes the heavy-job slot and the budget's threads.
    let cpu = cpu::lease();
    let mut cmd = bg_command(&bin);
    cpu::limit_cmd(&mut cmd, cpu.threads());
    let output = cmd
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
    // The whole file is decoded, so this queues behind any other heavy job.
    let cpu = cpu::lease();
    let run = |hw: Option<&str>| {
        let mut cmd = bg_command(&bin);
        cpu::limit_cmd(&mut cmd, cpu.threads());
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
pub fn salience_map(path: &Path, start: f64, end: f64) -> Result<SalienceMap> {
    use std::sync::atomic::Ordering;

    let bin = ffmpeg_bin();
    let cpu = cpu::lease();
    let mut args = build_salience_args(path, start, end);
    cpu::limit_args(&mut args, cpu.threads());
    let run = |hw: Option<&str>| {
        let mut cmd = bg_command(&bin);
        if let Some(hw) = hw {
            cmd.args(["-hwaccel", hw]);
        }
        cmd.args(&args)
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| launch_err(&bin, e))
    };
    let hw = decode_hwaccel();
    let output = match hw.as_deref() {
        Some(h) => {
            let out = run(Some(h))?;
            if out.status.success() && !out.stdout.is_empty() {
                out
            } else {
                let sw = run(None)?;
                if sw.status.success() {
                    HWACCEL_OK.store(false, Ordering::Relaxed);
                    tracing::warn!("hardware decode failed while sampling salience; using software decode");
                }
                sw
            }
        }
        None => run(None)?,
    };
    if !output.status.success() {
        return Err(Error::Engine(format!(
            "could not sample the shot: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(score_salience(&output.stdout))
}

/// Pure arg builder for [`salience_map`] (no I/O, unit-tested). Fast-seeks to
/// the window, decodes at most [`SALIENCE_SAMPLES`] frames spread across it, and
/// writes them as raw gray at the analysis grid. `-an` because nothing here
/// looks at sound, and `fps` before `scale` so the scaler runs on the frames
/// that survive rather than on every one.
fn build_salience_args(path: &Path, start: f64, end: f64) -> Vec<String> {
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
fn score_salience(raw: &[u8]) -> SalienceMap {
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

// ---- frame / waveform extraction ------------------------------------------

/// Decode a single frame at `time_secs` and return it as PNG bytes, scaled to
/// at most `max_width` pixels wide.
pub fn frame_at(path: &Path, time_secs: f64, max_width: u32) -> Result<Vec<u8>> {
    let scale = format!("scale='min({max_width},iw)':-2");
    decode_frame(path, time_secs, &scale, "png", None, true)
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
    fn crop_filter(&self) -> String {
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
    decode_frame(path, time_secs, &scale, "mjpeg", Some(quality), accurate)
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
    decode_frame(path, time_secs, &vf, "mjpeg", Some(quality), accurate)
}

/// Pure filter chain for [`frame_jpeg_region`] (unit-tested): crop, then scale
/// to an even width no wider than `max_width` or the crop itself.
fn region_frame_filter(region: Region, max_width: u32) -> String {
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
/// hardware-accelerated per [`hwaccel`].
fn decode_frame(path: &Path, time_secs: f64, vf: &str, vcodec: &str, quality: Option<u8>, accurate: bool) -> Result<Vec<u8>> {
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
    // Interactive: never gated and never de-prioritized — a scrubbed frame is
    // wanted now — but still held to the budget's threads.
    cpu::limit_cmd(&mut cmd, cpu::budget_threads());
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
    let tonemap = source_hdr(Path::new(path)).map(tonemap_filter);
    let (mut args, times) = build_contact_sheet_args(path, start, end, columns, rows, cell_width, quality, tonemap.as_deref());
    let bin = ffmpeg_bin();
    // Deliberately ungated: this is how an agent *looks* at the footage, and
    // making it wait out a ten-minute render would read as a hung server. It
    // still runs thread-capped and at background priority.
    cpu::limit_args(&mut args, cpu::budget_threads());
    let output = bg_command(&bin)
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
fn build_contact_sheet_args(
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
    let vf = format!("fps={rate},scale={cell_width}:-2:flags=bilinear,{tone}tile={columns}x{rows}");
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
    // Ungated like the other reads the timeline draws from — a clip's waveform
    // appearing is not worth queueing behind an export — but capped and niced.
    let mut cmd = bg_command(&bin);
    cpu::limit_cmd(&mut cmd, cpu::budget_threads());
    let mut child = cmd
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
        for s in leftover.as_chunks::<4>().0 {
            down.push(f32::from_le_bytes(*s).abs());
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
    // Decodes the whole stream *into memory* — eight of these at once is the
    // several gigabytes an unsupervised agent used to cost — so it is gated.
    let cpu = cpu::lease();
    let mut cmd = bg_command(&bin);
    cpu::limit_cmd(&mut cmd, cpu.threads());
    let output = cmd
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
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
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
    // Interactive, like the frame decode: the preview's playback is waiting on it.
    cpu::limit_cmd(&mut cmd, cpu::budget_threads());
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

/// The width a flat asset's proxy is rendered at: the default is
/// [`PROXY_MAX_WIDTH`], and Settings can pick a smaller one (or turn proxies off,
/// which is the caller's business — this is only the size). It is part of the cache
/// key, so a proxy made at another size is a different file and the default's
/// cache stays good.
static PROXY_BASE_WIDTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(PROXY_MAX_WIDTH);

/// The width a flat asset's proxy is rendered at right now (see [`proxy_width`]).
pub fn proxy_base_width() -> u32 {
    PROXY_BASE_WIDTH.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set the flat proxy width. Takes effect for every later lookup; an existing
/// proxy at the old size stays on disk, unused, until it is deleted.
pub fn set_proxy_base_width(width: u32) {
    PROXY_BASE_WIDTH.store(width.max(2), std::sync::atomic::Ordering::Relaxed);
}

/// The proxy width for an asset when flat proxies are `base` wide (pure,
/// unit-tested): `base` itself, or for a spherical source the same ratio the
/// default has (3072 for 1280), never past [`PROXY_MAX_WIDTH_SPHERICAL`] — the
/// hardware H.264 encoders stop at 4096 across and one refusal turns them all off.
pub fn proxy_width_for(projection: Option<Projection>, base: u32) -> u32 {
    if projection.is_some_and(|p| p.is_spherical()) {
        (base.saturating_mul(PROXY_MAX_WIDTH_SPHERICAL) / PROXY_MAX_WIDTH).min(PROXY_MAX_WIDTH_SPHERICAL)
    } else {
        base
    }
}

/// The proxy width to render (and look up) an asset at. 360 footage is preserved
/// at a larger size because reframing throws most of the frame away.
pub fn proxy_width(projection: Option<Projection>) -> u32 {
    proxy_width_for(projection, proxy_base_width())
}

/// FNV-1a over `s`. A small, dependency-free, deterministic hash for naming a
/// source's proxy file — stability across sessions is what lets a re-import
/// reuse the cached proxy (a non-deterministic hasher would orphan it).
pub(super) fn fnv1a(s: &str) -> u64 {
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
/// regenerates one. Shared by the proxy cache, the Insta360 stitch cache and
/// the waveform peak cache.
pub(super) fn source_key(src: &Path) -> String {
    let meta = std::fs::metadata(src).ok();
    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}|{len}|{mtime}", src.display())
}

/// Which file a cached decode came from, as one number: [`source_key`] (path, size and
/// modified time) hashed, so it changes when the file is replaced and a cache keyed by it
/// never serves a stale frame. A proxy and its original are different paths, so different
/// identities. A path that cannot be read hashes with size 0 and mtime 0: stable, and it
/// changes the moment the file appears.
pub fn source_identity(src: &Path) -> u64 {
    fnv1a(&source_key(src))
}

/// How long after the container's start the video's first frame lies, in
/// seconds: the video stream's start less the container's (the earliest of all
/// its streams — audio at 0 and video at 0.08 s gives 0.08).
///
/// The number matters because ffmpeg's input `-ss T` is **relative to the
/// container's start**, not to the video's. A source that starts its video late
/// is asked for source time `T` and answers with the frame `lead` seconds into
/// the video's own timeline; a proxy is video-only, so its container *is* the
/// video and starts where the video does — which would make the same `-ss T`
/// land `lead` seconds further into the footage (see [`build_proxy_args`]).
/// Negative or missing values are `0.0`.
fn head_lead(stream_start: Option<f64>, format_start: Option<f64>) -> f64 {
    match (stream_start, format_start) {
        (Some(stream), Some(format)) => (stream - format).max(0.0),
        _ => 0.0,
    }
}

/// The smallest lead worth a proxy of its own. Under a millisecond is a hair
/// inside the error of the container's own timestamps; anything above shifts a
/// seek by a whole frame for the stretch of each frame interval it covers.
const HEAD_PAD_MIN: f64 = 0.001;

/// Whether a source with this [`head_lead`] needs its proxy's head filled in.
fn needs_head_pad(lead: f64) -> bool {
    lead > HEAD_PAD_MIN
}

/// The cache-key text of a proxy (pure, unit-tested): the source's identity and
/// the width, plus a suffix for each way the proxy was made differently from the
/// plain one. A suffix is only ever *added* to a source that needs it, so the
/// key of every ordinary SDR file with a normal start is what it always was and
/// its cached proxy stays good.
fn proxy_key(source_key: &str, width: u32, tonemapped: bool, head_padded: bool) -> String {
    let tone = if tonemapped { "|sdr" } else { "" };
    let pad = if head_padded { "|lead" } else { "" };
    format!("{source_key}|{width}{tone}{pad}")
}

/// The on-disk path of `src`'s preview proxy at `width` (whether or not it
/// exists yet): `<cache>/kerf/proxies/<hash>.mp4`. `None` when no OS cache
/// directory is resolvable (a proxy simply can't be cached — preview falls back
/// to the original).
///
/// The width is part of the key, so an asset that is later marked as 360 (or
/// stops being one) looks up a different file and regenerates instead of
/// silently reusing a proxy rendered at the wrong size.
///
/// An HDR source also keys on the fact that its proxy is **tone-mapped**: a
/// proxy cached before the engine did that is an untouched HDR picture squeezed
/// into BT.709, and must be rebuilt rather than trusted. SDR keys are unchanged.
///
/// So does a source whose video **starts after its container** (`|lead`): a proxy
/// cached before the engine filled that head in has the video starting at its own
/// time zero, and a seek into it lands on the wrong frame (see [`head_lead`]).
/// FFmpeg 9 wrote exactly such proxies for every build that had the default
/// frame-sync mode of its day. Only these sources are rebuilt, and their proxy is
/// named `<hash>.lead.mp4` (`is_head_padded_proxy`, in `clip_timing`).
pub fn proxy_path(src: &Path, width: u32) -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("kerf").join("proxies");
    let traits = source_traits(src).unwrap_or_default();
    let padded = needs_head_pad(traits.lead);
    let key = proxy_key(&source_key(src), width, traits.hdr.is_some(), padded);
    Some(dir.join(proxy_file_name(fnv1a(&key), padded)))
}

/// A proxy's file name (pure, unit-tested): `<hash>.mp4`, or `<hash>.lead.mp4` for
/// one made with a padded head.
fn proxy_file_name(hash: u64, head_padded: bool) -> String {
    if head_padded {
        format!("{hash:016x}{HEAD_PADDED_SUFFIX}")
    } else {
        format!("{hash:016x}.mp4")
    }
}

/// The proxy for `src` at `width` **if it has already been generated** (the file
/// exists), for a preview path to decode from instead of the original; `None`
/// otherwise. Preview must never block on generation, so this is a pure
/// existence check.
pub fn ready_proxy(src: &Path, width: u32) -> Option<PathBuf> {
    proxy_path(src, width).filter(|p| p.is_file())
}

/// Delete `src`'s proxy at `width` and the sidecar beside it, returning the bytes
/// freed (0 when there was none). A proxy that is being written is a `.part` file
/// and is left to the encode that owns it.
pub fn remove_proxy_files(src: &Path, width: u32) -> u64 {
    let Some(proxy) = proxy_path(src, width) else {
        return 0;
    };
    [proxy.clone(), proxy_sidecar_path(&proxy)]
        .iter()
        .filter_map(|file| {
            let bytes = std::fs::metadata(file).ok()?.len();
            std::fs::remove_file(file).ok().map(|()| bytes)
        })
        .sum()
}

/// How many CPU threads a single preview-proxy encode may use. Follows the
/// engine's CPU budget (see [`cpu`]), except that it never takes the whole
/// machine even at 100% — an uncapped `libx264` grabs every core, and a proxy
/// is background work the user did not ask to wait for. `KERF_PROXY_THREADS`
/// still overrides it outright (clamped to >= 1).
fn proxy_threads(budget: usize) -> usize {
    if let Some(n) = std::env::var("KERF_PROXY_THREADS").ok().and_then(|v| v.parse::<usize>().ok()) {
        return n.max(1);
    }
    budget.min(cpu::cores().saturating_sub(1).max(1)).max(1)
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
/// always reads the original) would. `tonemap`, for an HDR source, is the chain
/// that brings it down to SDR BT.709 after the downscale: the **proxy is where
/// that conversion happens**, so everything that decodes it afterwards — the
/// preview stream, scrubbed stills, the composited frame — sees ordinary SDR and
/// converts nothing a second time. (Upright rotation is the decoder's: ffmpeg
/// autorotates and writes the proxy without a matrix.)
///
/// `head_pad` is for a source whose video starts after its container (see
/// [`head_lead`]), and carries the spelling of the frame-sync flag
/// ([`fps_mode_flag`]). The proxy has no audio to start the container early, so
/// without help its video would start at its own zero and an input `-ss T` would
/// land `lead` seconds past where the original's does — which is what an
/// FFmpeg-9 proxy of a late-starting video did. The fix keeps every timestamp
/// exactly and fills the gap the way the original answers a seek into it, by
/// holding the first frame: one clone of it at time zero, merged in front of the
/// stream by `interleave` (which orders by timestamp, so nothing is regridded).
/// That is not `-fps_mode cfr`, the other way to fill a head: cfr snaps every
/// frame to a grid anchored at zero, which is half a frame off for most leads
/// (a seek then picks the neighbouring frame at every frame boundary) and turns
/// a variable-frame-rate source into a constant one. The mode is spelled out as
/// `vfr` because FFmpeg 6 and older default an mp4 to cfr — which would undo it —
/// and FFmpeg 7 and newer to vfr. An ordinary source passes `None` and gets the
/// plain `-vf` chain it always did. The clone is a frame the original has no
/// counterpart for, so a reader that starts the proxy from zero without a seek —
/// which the original answers with its real first frame — drops it again (see
/// [`video_clip_chain`]); the proxy's file name says it has one
/// (`is_head_padded_proxy`, in `clip_timing`).
#[allow(clippy::too_many_arguments)]
fn build_proxy_args(
    src: &str,
    dst: &str,
    threads: usize,
    width: u32,
    encoder: &str,
    hw_decode: Option<&str>,
    tonemap: Option<&str>,
    head_pad: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
    ];
    if let Some(hw) = hw_decode {
        args.push("-hwaccel".to_string());
        args.push(hw.to_string());
    }
    let chain = match tonemap {
        Some(t) => format!("scale='min({width},iw)':-2:flags=bilinear,{t}"),
        None => format!("scale='min({width},iw)':-2:flags=bilinear"),
    };
    args.extend(["-i".to_string(), src.to_string(), "-an".to_string()]);
    if head_pad.is_some() {
        args.extend([
            "-filter_complex".to_string(),
            format!("[0:v:0]{chain},split[a][b];[a]trim=end_frame=1,setpts=PTS-STARTPTS[h];[h][b]interleave[v]"),
            "-map".to_string(),
            "[v]".to_string(),
        ]);
    } else {
        args.extend(["-vf".to_string(), chain]);
    }
    args.extend(["-c:v".to_string(), encoder.to_string()]);
    args.extend(quality_args(encoder, 24));
    args.extend([
        "-g".to_string(),
        "1".to_string(),
        "-threads".to_string(),
        threads.max(1).to_string(),
        "-pix_fmt".to_string(),
        encode_pix_fmt(encoder).to_string(),
    ]);
    if let Some(flag) = head_pad {
        args.extend([flag.to_string(), "vfr".to_string()]);
    }
    args.extend([
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
    generate_proxy_with(
        src,
        width,
        ProxyRun {
            reservation: None,
            duration: None,
            progress: &mut |_| {},
            cancel: &|| false,
        },
    )
}

/// What a caller hooks into a proxy encode (see [`generate_proxy_with`]).
pub struct ProxyRun<'a> {
    /// The place in front of the heavy-job queue the caller took when it queued the
    /// proxy ([`cpu::reserve`]); without one the encode asks for the slot in the
    /// same lane when it starts.
    pub reservation: Option<cpu::Reservation>,
    /// The source's length in seconds — what the encoder's position is measured
    /// against to give `progress` a fraction. Without it nothing is reported.
    pub duration: Option<f64>,
    /// Called with how far along the encode is, `0.0..1.0`, about twice a second.
    pub progress: &'a mut dyn FnMut(f64),
    /// Polled while queued and while encoding; once true the encode is killed, its
    /// partial file removed and [`Error::Cancelled`] returned.
    pub cancel: &'a dyn Fn() -> bool,
}

/// [`generate_proxy`] that reports progress and can be abandoned.
///
/// The encode is a whole-file job in the **high** lane of the heavy-job queue: a proxy
/// is what the preview is waiting for (it decodes the original until the proxy lands),
/// where analysis is only wanted eventually, so it goes before every analysis that is
/// waiting. A job already running is not interrupted.
///
/// It runs with `-progress` on a pipe (written here at spawn time, never in
/// [`build_proxy_args`]) and is killed after [`EXPORT_STALL`] without a word, so a
/// wedged decoder cannot hold the machine's one heavy-job slot for good.
pub fn generate_proxy_with(src: &Path, width: u32, run: ProxyRun<'_>) -> Result<PathBuf> {
    let ProxyRun {
        reservation,
        duration,
        progress,
        cancel,
    } = run;
    let dst =
        proxy_path(src, width).ok_or_else(|| Error::Engine("no cache directory available for preview proxies".to_string()))?;
    if dst.is_file() {
        return Ok(dst);
    }
    if cancel() {
        return Err(Error::Cancelled);
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Engine(format!("could not create proxy cache dir: {e}")))?;
    }
    let src_str = src
        .to_str()
        .ok_or_else(|| Error::Engine("asset path is not valid UTF-8".to_string()))?;
    // One name per encode: a rebuild that cancels the encode before it must not share a
    // temp file with the one that replaces it.
    static ENCODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = ENCODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dst.with_extension(format!("{}.{n}.part", std::process::id()));
    let tmp_str = tmp
        .to_str()
        .ok_or_else(|| Error::Engine("proxy temp path is not valid UTF-8".to_string()))?;
    let bin = ffmpeg_bin();
    // A full-file re-encode. Importing a folder queues them one behind the next
    // rather than starting one per file at once; they wait in the high lane.
    let cpu = match reservation {
        Some(reserved) => reserved.lease(),
        None => cpu::lease_priority(cpu::Priority::High),
    };
    // Queued behind a long job, the caller may have changed its mind by now.
    if cancel() {
        return Err(Error::Cancelled);
    }
    let threads = proxy_threads(cpu.threads());
    // One cached probe answers both: whether to tone-map and whether the video
    // starts late (the same one `proxy_path` keyed this proxy on).
    let traits = source_traits(src).unwrap_or_default();
    let tonemap = traits.hdr.map(tonemap_filter);
    let head_pad = needs_head_pad(traits.lead).then(fps_mode_flag);
    let mut run = |encoder: &str, hw_decode: Option<&str>| -> Result<Streamed> {
        let mut args = build_proxy_args(
            src_str,
            tmp_str,
            threads,
            width,
            encoder,
            hw_decode,
            tonemap.as_deref(),
            head_pad,
        );
        cpu::limit_args(&mut args, threads);
        run_ffmpeg_streamed(&bin, &args, duration, &mut *progress, cancel)
    };
    // GPU encode (and decode) when available — a background proxy transcode
    // then costs the CPU almost nothing. A failure falls back to the software
    // pipeline once, and a hardware-*encoder* failure whose software retry
    // succeeds disables hardware encodes for the rest of the process.
    // A padded proxy is encoded from a graph whose output has no frame rate (its
    // `interleave` hands the encoder microsecond timestamps), which a hardware
    // encoder may refuse — and one refusal turns hardware encoding off for the
    // whole process. These sources are rare and the encode is the same all-intra
    // x264 pass, so they never ask.
    let hw_enc = if head_pad.is_some() { None } else { proxy_hw_encoder() };
    let hw_dec = decode_hwaccel();
    let mut output = run(hw_enc.unwrap_or("libx264"), hw_dec.as_deref())?;
    if output.outcome == StreamOutcome::Cancelled {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Cancelled);
    }
    if !output.status.success() && output.outcome == StreamOutcome::Ended && (hw_enc.is_some() || hw_dec.is_some()) {
        let _ = std::fs::remove_file(&tmp);
        let err = output.stderr.trim().to_string();
        output = run("libx264", None)?;
        if output.outcome == StreamOutcome::Cancelled {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::Cancelled);
        }
        if output.status.success() {
            if hw_enc.is_some() {
                HW_ENCODE_OK.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            tracing::warn!(error = %err, "hardware-accelerated proxy encode failed; using software");
        }
    }
    if output.outcome == StreamOutcome::Stalled {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Engine(format!(
            "could not generate preview proxy: ffmpeg stopped reporting progress for {}s and was stopped",
            EXPORT_STALL.as_secs()
        )));
    }
    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Engine(format!(
            "could not generate preview proxy: {}",
            output.stderr.trim()
        )));
    }
    // Another generator may have finished the same proxy while we encoded: theirs is in place
    // with its own sidecar, and ours is redundant.
    finalize_proxy(&tmp, &dst, || {
        probe(&tmp)
            .ok()
            .and_then(|p| p.streams.into_iter().find(|s| s.kind == StreamKind::Video))
    })
}

/// How a streamed ffmpeg run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamOutcome {
    /// ffmpeg exited on its own (successfully or not — see the status).
    Ended,
    /// The cancel callback tripped and ffmpeg was killed.
    Cancelled,
    /// ffmpeg said nothing for [`EXPORT_STALL`] and was killed.
    Stalled,
}

/// The result of [`run_ffmpeg_streamed`].
struct Streamed {
    status: std::process::ExitStatus,
    stderr: String,
    outcome: StreamOutcome,
}

/// Run `ffmpeg` with `args`, reading its `-progress` stream: `progress` gets the
/// encoder's position as a fraction of `duration` (when it is known), `cancel` is
/// polled between reports and while ffmpeg is silent, and a run that says nothing
/// for [`EXPORT_STALL`] is killed. A bounded cousin of `Command::output()` for the
/// whole-file jobs that outlive a glance — stderr is drained on a side thread so a
/// warning flood cannot fill its pipe and wedge the child.
fn run_ffmpeg_streamed(
    bin: &str,
    args: &[String],
    duration: Option<f64>,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> bool,
) -> Result<Streamed> {
    use std::io::{BufRead, BufReader, Read};

    let mut child = bg_command(bin)
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
                if last_output.elapsed() > EXPORT_STALL {
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
    let status = child.wait().map_err(|e| Error::Engine(format!("ffmpeg wait failed: {e}")))?;
    let stderr = stderr_handle.join().unwrap_or_default();
    Ok(Streamed { status, stderr, outcome })
}

/// Put the finished encode `tmp` in place as `dst`, with its sidecar. If `dst` is already there
/// (a concurrent generator got there first) it is left as it is — **its sidecar stays too, no
/// new one is written** — and `tmp` is dropped. Otherwise `probe` describes `tmp` and the
/// sidecar is written *before* the rename, so a reader that finds the proxy finds its
/// description; a failed probe only costs the sidecar (the reader probes once and writes it).
fn finalize_proxy(tmp: &Path, dst: &Path, probe: impl FnOnce() -> Option<StreamInfo>) -> Result<PathBuf> {
    if dst.is_file() {
        let _ = std::fs::remove_file(tmp);
        return Ok(dst.to_path_buf());
    }
    if let Some(video) = probe() {
        write_proxy_sidecar(dst, tmp, video);
    }
    std::fs::rename(tmp, dst).map_err(|e| Error::Engine(format!("could not finalize preview proxy: {e}")))?;
    Ok(dst.to_path_buf())
}

/// What `generate_proxy` writes beside a proxy (`<hash>.json` next to `<hash>.mp4`, the
/// same name a `.lead.mp4` proxy's sidecar takes with its own ending): the video stream
/// **of the proxy file** — its size, pixel format and colour tags, not the original's.
///
/// A proxy is a smaller, always-`yuv420p`, upright picture, so what a renderer that decodes
/// it has to know about its layer (geometry, a format it refuses, a matrix) is a fact about
/// the proxy. Asking `ffprobe` for it would put a process spawn on the interactive path; the
/// sidecar makes it a small file read. It carries the proxy's byte size so a proxy that was
/// replaced under the same name does not keep describing the one that is gone.
#[derive(serde::Serialize, serde::Deserialize)]
struct ProxySidecar {
    version: u32,
    size: u64,
    video: StreamInfo,
}

const PROXY_SIDECAR_VERSION: u32 = 1;

/// Where the sidecar of `proxy` lives.
pub(crate) fn proxy_sidecar_path(proxy: &Path) -> PathBuf {
    proxy.with_extension("json")
}

/// A temp name for writing the sidecar at `dst`, **unique per writer** (the process and a
/// counter: two threads writing the same sidecar must not share one) and not the proxy's own
/// (`<hash>.<pid>.part`), which the write runs beside.
fn sidecar_temp(dst: &Path) -> PathBuf {
    static WRITERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    dst.with_extension(format!("json.{}.{n}.part", std::process::id()))
}

/// Write `proxy`'s sidecar, describing `file` (the proxy itself, or the temp it is being
/// written as). Best effort and atomic (temp file, rename): the worst a failure does is make
/// the reader probe.
fn write_proxy_sidecar(proxy: &Path, file: &Path, video: StreamInfo) {
    let Ok(size) = std::fs::metadata(file).map(|m| m.len()) else {
        return;
    };
    let sidecar = ProxySidecar {
        version: PROXY_SIDECAR_VERSION,
        size,
        video,
    };
    let dst = proxy_sidecar_path(proxy);
    let tmp = sidecar_temp(&dst);
    let written = serde_json::to_vec(&sidecar)
        .ok()
        .is_some_and(|bytes| std::fs::write(&tmp, bytes).is_ok())
        && std::fs::rename(&tmp, &dst).is_ok();
    if !written {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The sidecar of `proxy` if it is there, current and describes this very file.
fn read_proxy_sidecar(proxy: &Path) -> Option<StreamInfo> {
    let sidecar: ProxySidecar = serde_json::from_slice(&std::fs::read(proxy_sidecar_path(proxy)).ok()?).ok()?;
    let size = std::fs::metadata(proxy).ok()?.len();
    (sidecar.version == PROXY_SIDECAR_VERSION && sidecar.size == size).then_some(sidecar.video)
}

/// How long a failed probe of a proxy is remembered before it is tried again.
const PROXY_PROBE_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// The video stream of the proxy at `proxy`, as decoding it yields it: from its sidecar, and
/// for a proxy that has none (one made before sidecars existed) from **one** `ffprobe`,
/// remembered per file (size and modified time) and written back as the sidecar so no later
/// run asks again. `None` when the proxy cannot be read; the caller decodes the original.
///
/// The fallback spawns a process: call it off the project lock, never from a render loop.
/// A plan resolves it once per `Planner`.
pub(crate) fn proxy_video_info(proxy: &Path) -> Option<StreamInfo> {
    /// What one probe of a proxy found, and when.
    type Probed = (Instant, Option<StreamInfo>);
    static CACHE: OnceLock<Mutex<HashMap<String, Probed>>> = OnceLock::new();
    if let Some(video) = read_proxy_sidecar(proxy) {
        return Some(video);
    }
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = source_key(proxy);
    let remembered = cache.lock().ok().and_then(|c| c.get(&key).cloned());
    if let Some((at, found)) = remembered {
        if found.is_some() || at.elapsed() < PROXY_PROBE_RETRY {
            return found;
        }
    }
    let found = probe(proxy)
        .ok()
        .and_then(|p| p.streams.into_iter().find(|s| s.kind == StreamKind::Video));
    if let Some(video) = &found {
        write_proxy_sidecar(proxy, proxy, video.clone());
    }
    if let Ok(mut c) = cache.lock() {
        c.insert(key, (Instant::now(), found.clone()));
    }
    found
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
    let mut args: Vec<String> = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
    ];
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
    let (Some(front_str), Some(rear_str)) = (front.to_str(), rear.to_str()) else {
        return Err(Error::Engine("lens file path is not valid UTF-8".to_string()));
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

/// `-pix_fmt` / filtergraph `format=` values the graph builders will emit. A
/// closed set, unlike a colour: `pix_fmt`/`scaler`/`gif_dither` reach the graph
/// unquoted (see `ExportFormat::scale_flags`, `export_format`, the gif
/// `paletteuse` chain), so an out-of-list value is dropped rather than passed
/// through — a `.kerf` file or an MCP call is untrusted the same as anything
/// else, and this must hold even when `validate_export` was never run.
const PIX_FMTS: &[&str] = &[
    "yuv420p",
    "yuv422p",
    "yuv444p",
    "yuva420p",
    "yuva422p",
    "yuva444p",
    "yuv420p10le",
    "yuv422p10le",
    "yuv444p10le",
    "yuva420p10le",
    "yuva422p10le",
    "yuva444p10le",
    "yuv420p12le",
    "yuv422p12le",
    "yuv444p12le",
    "yuvj420p",
    "yuvj422p",
    "yuvj444p",
    "nv12",
    "nv21",
    "p010le",
    "rgb24",
    "bgr24",
    "rgba",
    "bgra",
    "gray",
];

/// `scale=…:flags=` scalers `swscale` accepts, matching the export dialog's choices.
const SCALERS: &[&str] = &["bicubic", "bilinear", "lanczos", "neighbor", "spline"];

/// `paletteuse=dither=` modes for the gif pipeline.
const GIF_DITHERS: &[&str] = &[
    "bayer",
    "heckbert",
    "floyd_steinberg",
    "sierra2",
    "sierra2_4a",
    "sierra3",
    "burkes",
    "atkinson",
    "none",
];

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
    if let Some(pf) = opts.pix_fmt.as_deref() {
        if !PIX_FMTS.contains(&pf) {
            issues.push(format!("Unsupported pixel format \"{pf}\"."));
        }
    }
    if let Some(s) = opts.scaler.as_deref() {
        if !SCALERS.contains(&s) {
            issues.push(format!("Unsupported scaler \"{s}\"."));
        }
    }
    if let Some(d) = opts.gif_dither.as_deref() {
        if !GIF_DITHERS.contains(&d) {
            issues.push(format!("Unsupported gif dither \"{d}\"."));
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
    /// The `:flags=…` suffix to append to a `scale` filter, or empty. An
    /// out-of-list value (a stale `.kerf` file, a hand-crafted MCP call) is
    /// dropped rather than spliced in unquoted — see [`SCALERS`].
    fn scale_flags(&self) -> String {
        match self.scaler.as_deref() {
            Some(s) if SCALERS.contains(&s) => format!(":flags={s}"),
            _ => String::new(),
        }
    }
}

/// Derive the output shape from the first clip (across all tracks) that carries
/// a video stream and the first that carries audio, falling back to 1080p30
/// stereo defaults. When `opts` carries resolution or fps overrides those win.
/// The frame this timeline actually renders at: the project's delivery format
/// when one is set, otherwise the shape its footage gives it. What a readiness
/// check has to compare a platform's expectations against.
pub fn delivery_frame(timeline: &Timeline, assets: &[Asset]) -> (u32, u32) {
    let f = export_format(timeline, assets, &ExportOptions::default());
    (f.width, f.height)
}

/// The slice of [`ExportFormat`] a renderer of one *frame* needs — the canvas
/// and how footage meets it — taken from the same `export_format` the still and
/// the export use, so a second renderer (the GPU compositor, through
/// [`crate::render_plan::RenderPlan`]) cannot drift from them on what the frame
/// is.
#[derive(Debug, Clone)]
pub(crate) struct RenderGeometry {
    pub width: u32,
    pub height: u32,
    pub fit: Fit,
    /// The export's `scaler` as chosen (`None` is swscale's bicubic default).
    pub scaler: Option<String>,
    /// The output rate, exactly as the graph's `fps=` / `color=r=` print it (`{}` of
    /// this `f64`, which FFmpeg parses back to a rational).
    pub fps: f64,
    /// The delivery's terminal pixel format (`format=` in the graph).
    pub pix_fmt: String,
}

pub(crate) fn render_geometry(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> RenderGeometry {
    let f = export_format(timeline, assets, opts);
    RenderGeometry {
        width: f.width,
        height: f.height,
        fit: f.fit,
        scaler: f.scaler,
        fps: f.fps,
        pix_fmt: f.pix_fmt,
    }
}

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
    // An out-of-list `pix_fmt` (see [`PIX_FMTS`]) is treated as if none were
    // given, falling through to the same ProRes default a bare `None` gets —
    // never spliced into the graph terminal `format=` unchecked.
    if let Some(pf) = opts.pix_fmt.as_deref().filter(|pf| PIX_FMTS.contains(pf)) {
        fmt.pix_fmt = pf.to_string();
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
                args.push(export_seek_arg(seek));
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
    let timeline_has_audio = timeline
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .any(|c| clip_sounds(c, assets));
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
    build_preview_args_with(timeline, assets, start, fps, max_width, quality, decode_hwaccel())
}

/// [`build_preview_args`] with the decode acceleration given rather than read from
/// the machine (`KERF_HWACCEL`, and whether an earlier hardware failure turned it
/// off). The one seam the golden argv oracle needs to be independent of the machine
/// it runs on; the argv it builds is exactly what `build_preview_args` always did.
fn build_preview_args_with(
    timeline: &Timeline,
    assets: &[Asset],
    start: f64,
    fps: f64,
    max_width: u32,
    quality: u8,
    hwaccel: Option<String>,
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
        hwaccel,
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
    let hw = decode_hwaccel().is_some();
    // `None` until ffmpeg is running: a timeline with nothing to play fails
    // before that, and says nothing about the hardware.
    let mut sent = None;
    match stream_preview_once(timeline, assets, start, fps, on_frame, &mut sent) {
        // A hardware decode that dies before the first frame is `-hwaccel`
        // being unusable here, not the graph: a static ffmpeg aborts outright
        // when `auto` probes a VAAPI whose libva isn't installed. Once frames
        // have been shown, restarting would replay them, so only then is the
        // failure reported as it is.
        Err(e) if hw && sent == Some(0) => {
            HWACCEL_OK.store(false, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!("hardware decode failed starting playback; using software decode: {e}");
            stream_preview_once(timeline, assets, start, fps, on_frame, &mut sent)
        }
        r => r,
    }
}

/// How long a playback stream may go without producing a frame before it is
/// declared dead. A stalled ffmpeg (a hardware decoder that opens and then never
/// delivers, a driver stuck in teardown) is otherwise indistinguishable from a
/// slow one, and the read below would wait for it forever — the webview then
/// shows a frozen frame while the stream is "playing". `first` covers startup
/// (graph setup, hardware probing, the first decode of a long source); `stall`
/// covers the gap between frames once running, which a healthy stream paced to
/// real time keeps well under a second.
#[derive(Clone, Copy)]
struct PreviewTimeouts {
    first: std::time::Duration,
    stall: std::time::Duration,
}

const PREVIEW_TIMEOUTS: PreviewTimeouts = PreviewTimeouts {
    first: std::time::Duration::from_secs(30),
    stall: std::time::Duration::from_secs(15),
};

fn stream_preview_once(
    timeline: &Timeline,
    assets: &[Asset],
    start: f64,
    fps: f64,
    on_frame: &mut dyn FnMut(PreviewFrame) -> bool,
    sent: &mut Option<u64>,
) -> Result<()> {
    let fps = fps.clamp(1.0, 60.0);
    let mut args = build_preview_args(timeline, assets, start, fps, PREVIEW_STREAM_WIDTH, PREVIEW_STREAM_QUALITY)?;
    // The composited graph outgrows argv just as the export's does.
    let _script = externalize_filter_complex(&mut args, "preview")?;
    // Playback is paced to the wall clock, so it never races ahead; the cap is
    // there so the budget means the same thing while the cut is playing.
    cpu::limit_args(&mut args, cpu::budget_threads());

    let bin = ffmpeg_bin();
    tracing::debug!(start, fps, "starting preview stream");
    let child = command(&bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    *sent = Some(0);
    pump_preview(child, start, fps, PREVIEW_TIMEOUTS, on_frame, sent)
}

/// Read JPEG frames off a running ffmpeg's stdout, pace them to `fps` against
/// the wall clock and hand them to `on_frame`, until the stream ends, the
/// callback declines a frame, or ffmpeg stops producing for longer than
/// `timeouts` allows (it is then killed and the call fails). Always reaps the
/// child.
fn pump_preview(
    mut child: std::process::Child,
    start: f64,
    fps: f64,
    timeouts: PreviewTimeouts,
    on_frame: &mut dyn FnMut(PreviewFrame) -> bool,
    sent: &mut Option<u64>,
) -> Result<()> {
    use std::io::Read;
    use std::sync::mpsc::{sync_channel, RecvTimeoutError};

    // Drain stderr on a side thread so a warning flood can't deadlock the frame
    // read, keeping only the tail for a failure message.
    let stderr = child.stderr.take().expect("stderr piped");
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::BufReader::new(stderr).read_to_string(&mut buf);
        buf
    });

    // stdout is read on its own thread so the loop below can give up on a
    // silent ffmpeg (`recv_timeout`) instead of blocking in `read`. The bounded
    // channel keeps the old backpressure: once a few chunks are queued the reader
    // stops reading, the pipe fills and ffmpeg is throttled to the pacing.
    let mut stdout = child.stdout.take().expect("stdout piped");
    let (tx, rx) = sync_channel::<std::io::Result<Vec<u8>>>(4);
    // Detached: after the kill it ends on EOF (or on the dropped receiver).
    std::thread::spawn(move || {
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let msg = match stdout.read(&mut chunk) {
                Ok(0) => return,
                Ok(n) => Ok(chunk[..n].to_vec()),
                Err(e) => Err(e),
            };
            let failed = msg.is_err();
            if tx.send(msg).is_err() || failed {
                return;
            }
        }
    });

    let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
    let mut index: u64 = 0;
    let mut origin: Option<std::time::Instant> = None;
    let mut stopped = false;
    let mut failure: Option<String> = None;

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
            *sent = Some(index);
            if !on_frame(frame) {
                stopped = true;
                break 'read;
            }
        }
        let wait = if index == 0 { timeouts.first } else { timeouts.stall };
        match rx.recv_timeout(wait) {
            Ok(Ok(bytes)) => buf.extend_from_slice(&bytes),
            Ok(Err(e)) => {
                failure = Some(format!("preview stream read failed: {e}"));
                break;
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                failure = Some(format!(
                    "preview stream stalled: ffmpeg produced no {} for {}s",
                    if index == 0 { "first frame" } else { "frame" },
                    wait.as_secs_f32()
                ));
                break;
            }
        }
    }

    if stopped || failure.is_some() {
        let _ = child.kill();
    }
    drop(rx);
    let status = child.wait().map_err(|e| Error::Engine(format!("ffmpeg wait failed: {e}")))?;
    let stderr_text = stderr_handle.join().unwrap_or_default();
    let tail = || {
        let mut tail: Vec<&str> = stderr_text.lines().rev().take(12).collect();
        tail.reverse();
        tail.join("\n").trim().to_string()
    };
    if let Some(reason) = failure {
        return Err(Error::Engine(format!("{reason}\n{}", tail()).trim().to_string()));
    }
    if !stopped && !status.success() {
        return Err(Error::Engine(format!("preview stream failed: {}", tail())));
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

/// One delivery of a multi-format export: the frame and where its file goes.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportVariant {
    pub delivery: Delivery,
    pub output: PathBuf,
}

impl ExportVariant {
    /// The variant's file beside `base`, its shape spliced into the name:
    /// `cut.mp4` at 9:16 becomes `cut-9x16.mp4`. The `x` rather than a `:`
    /// because a colon is not a filename character on Windows.
    pub fn beside(base: &Path, delivery: Delivery) -> Self {
        let (w, h) = delivery.ratio();
        let stem = base.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let name = match base.extension() {
            Some(ext) => format!("{stem}-{w}x{h}.{}", ext.to_string_lossy()),
            None => format!("{stem}-{w}x{h}"),
        };
        Self {
            delivery,
            output: base.with_file_name(name),
        }
    }
}

/// Progress across a multi-format export: the overall [`ExportProgress`] plus
/// which variant is rendering, so a bar can say "2 of 3 · 9:16".
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct VariantProgress {
    /// Zero-based index of the variant being rendered.
    pub variant: usize,
    pub total: usize,
    /// Progress through all of them: each variant owns an equal share.
    pub fraction: f64,
    pub elapsed_secs: f64,
    /// Time left across the remaining variants, estimated from how long the
    /// completed share has taken.
    pub eta_secs: Option<f64>,
}

/// Render the same cut once per variant — one file per delivery frame, each
/// shot wearing the crop it carries for that shape ([`Timeline::for_delivery`]).
///
/// Variants render one after another rather than at once: an export already
/// takes every core it is given (`cpu::lease` would serialize them anyway),
/// and a cancelled batch is then clean — the variant in flight is deleted like
/// a cancelled single export, the ones already finished are kept, and the ones
/// not started never existed. `opts` is the encode shared by all of them; its
/// `resolution` and `fit` are replaced per variant by the delivery. Returns the
/// status and how many files were completed.
pub fn render_variants(
    timeline: &Timeline,
    assets: &[Asset],
    variants: &[ExportVariant],
    opts: &ExportOptions,
    progress: &mut dyn FnMut(VariantProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<(RenderStatus, usize)> {
    if variants.is_empty() {
        return Err(Error::InvalidArgument("no delivery formats to export".to_string()));
    }
    let total = variants.len();
    let started = Instant::now();
    for (i, variant) in variants.iter().enumerate() {
        let framed = timeline.for_delivery(variant.delivery);
        let per_variant = ExportOptions {
            resolution: Some((variant.delivery.width, variant.delivery.height)),
            fit: variant.delivery.fit,
            ..opts.clone()
        };
        let mut on_progress = |p: ExportProgress| {
            let fraction = (i as f64 + p.fraction.clamp(0.0, 1.0)) / total as f64;
            let elapsed_secs = started.elapsed().as_secs_f64();
            let eta_secs = (fraction > 0.0).then(|| elapsed_secs / fraction - elapsed_secs);
            progress(VariantProgress {
                variant: i,
                total,
                fraction,
                elapsed_secs,
                eta_secs,
            });
        };
        let status = render_with_progress(&framed, assets, &variant.output, &per_variant, &mut on_progress, cancel)?;
        if status == RenderStatus::Cancelled {
            let _ = std::fs::remove_file(&variant.output);
            return Ok((RenderStatus::Cancelled, i));
        }
    }
    Ok((RenderStatus::Completed, total))
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
    let has_audio = timeline
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .any(|c| clip_sounds(c, assets));
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

/// The options this ffmpeg build knows that the engine's argv depends on, read
/// from `ffmpeg -h full` once per process. Only these booleans are kept — the help
/// text itself is over a megabyte. A binary that cannot run at all reads as
/// "knows none of them", which every probe below takes as the modern spelling: any
/// render on that binary is about to fail the same way regardless.
struct HelpFlags {
    filter_complex_script: bool,
    fps_mode: bool,
    vsync: bool,
}

fn help_flags() -> &'static HelpFlags {
    static FLAGS: OnceLock<HelpFlags> = OnceLock::new();
    FLAGS.get_or_init(|| {
        let help = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "quiet", "-h", "full"])
            .stdin(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        HelpFlags {
            filter_complex_script: help.contains("-filter_complex_script"),
            fps_mode: help.contains("-fps_mode"),
            vsync: help.contains("-vsync"),
        }
    })
}

/// The argv spelling that points *this* ffmpeg at a filtergraph file, probed
/// once per process: FFmpeg 8 removed `-filter_complex_script` (deprecated in
/// 7.0 as equivalent to the generic `-/filter_complex <file>` form), so on a
/// bundled FFmpeg 8 the old spelling aborts every spilled render with
/// `Unrecognized option` — while a pre-7.0 binary knows only the old one.
/// `-h full` still lists the option wherever it exists.
fn graph_script_flag() -> &'static str {
    static FLAG: OnceLock<&'static str> = OnceLock::new();
    FLAG.get_or_init(|| {
        let legacy = help_flags().filter_complex_script;
        tracing::debug!(legacy, "probed ffmpeg for -filter_complex_script");
        if legacy {
            "-filter_complex_script"
        } else {
            "-/filter_complex"
        }
    })
}

/// The flag that sets how this ffmpeg paces output frames, given which of the two
/// spellings its `-h full` lists (pure, unit-tested): `-fps_mode` since 5.1, which
/// FFmpeg 9 is left with — it removed `-vsync`, `Unrecognized option` — and
/// `-vsync` before that.
fn fps_mode_flag_for(knows_fps_mode: bool, knows_vsync: bool) -> &'static str {
    if knows_fps_mode || !knows_vsync {
        "-fps_mode"
    } else {
        "-vsync"
    }
}

/// [`fps_mode_flag_for`] for this machine's ffmpeg, probed once per process: the spelling
/// every spawn outside the engine that passes `passthrough` uses.
pub fn fps_mode_flag() -> &'static str {
    static FLAG: OnceLock<&'static str> = OnceLock::new();
    FLAG.get_or_init(|| {
        let help = help_flags();
        fps_mode_flag_for(help.fps_mode, help.vsync)
    })
}

/// Move an oversized filtergraph out of argv into a script file, pointing ffmpeg
/// at it with [`graph_script_flag`]'s spelling. Leaves ordinary exports
/// untouched, so their argv stays byte-identical (and every pure arg-builder
/// test with it).
fn externalize_filter_complex(args: &mut [String], tag: &str) -> Result<GraphScript> {
    spill_graph(args, tag, graph_script_flag())
}

/// The pure half of [`externalize_filter_complex`], with the flag decided.
///
/// Either flag takes the path as its own argv token, which is why this beats
/// the obvious alternative of `sendcmd=f=`: that would bury the path *inside* a
/// filtergraph value, where `\` escapes and `:` separates options, so a Windows
/// path would have to be mangled first.
fn spill_graph(args: &mut [String], tag: &str, flag: &str) -> Result<GraphScript> {
    let Some(i) = oversized_graph_index(args) else {
        return Ok(GraphScript(None));
    };
    // Unique per call: two renders in one process (the GUI previewing while an
    // agent exports, two playback streams) share a pid and a tag, and the
    // second to finish used to delete the script the other was still reading.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("kerf-graph-{}-{tag}-{seq}.txt", std::process::id()));
    std::fs::write(&path, &args[i]).map_err(|e| Error::Engine(format!("could not write the filtergraph script: {e}")))?;
    args[i] = path.to_string_lossy().into_owned();
    args[i - 1] = flag.to_string();
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

/// How often a running export checks whether it has been cancelled when ffmpeg
/// has said nothing in the meantime.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// How long an export may go without a single `-progress` line before it is
/// treated as hung. ffmpeg reports every half second while it is working, even
/// through a slow filter, so this is a deadlock, not a heavy render — the bound
/// is generous because killing a render that was merely slow is the worse error.
const EXPORT_STALL: std::time::Duration = std::time::Duration::from_secs(300);

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
    // The heaviest thing the engine does. The lease is reentrant, so an export's
    // second pass and a stitch inside an import do not queue behind themselves.
    let cpu = cpu::lease();
    let mut args = args.to_vec();
    cpu::limit_args(&mut args, cpu.threads());
    let args = &args[..];
    tracing::info!(output = %output.display(), "exporting timeline");
    tracing::debug!(command = %format!("{bin} {}", args.join(" ")), "ffmpeg export command");

    // `-progress pipe:1` writes machine-readable key=value blocks to stdout;
    // `-stats_period` bounds how often, and thus the cancel-poll latency.
    let mut child = bg_command(&bin)
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
    let mut last_output = Instant::now();
    loop {
        let line = match rx.recv_timeout(CANCEL_POLL) {
            Ok(line) => line,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if cancel() {
                    let _ = child.kill();
                    cancelled = true;
                    break;
                }
                if last_output.elapsed() > EXPORT_STALL {
                    let _ = child.kill();
                    stalled = true;
                    break;
                }
                continue;
            }
        };
        last_output = Instant::now();
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
    if stalled {
        let mut tail: Vec<&str> = stderr_text.lines().rev().take(20).collect();
        tail.reverse();
        return Err(Error::Engine(format!(
            "ffmpeg stopped reporting progress for {}s and was stopped: {}",
            EXPORT_STALL.as_secs(),
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

/// Whether `clip` puts sound in the mix: its asset carries an audio stream **and**
/// the clip still plays it. A picture clip whose sound was detached
/// (`Clip::source_audio` false, see `Project::detach_audio`) is silent — its sound
/// is the linked audio clip's — and the export otherwise mixes the audio of *every*
/// clip with an audio stream, video tracks included, which is what made an asset
/// extracted onto an audio track while still on V1 sound twice. `source_audio`
/// defaults to true, so a clip that never heard of the flag is gated exactly as it
/// was.
fn clip_sounds(clip: &Clip, assets: &[Asset]) -> bool {
    clip.source_audio
        && assets
            .iter()
            .find(|a| a.id == clip.asset_id)
            .is_some_and(|a| a.streams.iter().any(|s| s.kind == StreamKind::Audio))
}

/// The result of [`build_filter_complex`]: the `-filter_complex` string plus
/// which output pads it produced, so the caller knows which `-map`s to add.
struct FilterGraph {
    filter: String,
    has_video: bool,
    has_audio: bool,
    /// The `ebur128` taps a [metered](build_filter_complex_metered) build added,
    /// each ending on a pad the caller `-map`s. Empty for every other build.
    meters: Vec<Meter>,
}

/// One level meter in a metered audio graph: an `ebur128` instance on a track's
/// finished strip or on the master, ending on a pad to `-map` (the filter passes
/// its audio through, so an output has to be consumed somewhere).
#[derive(Debug, Clone, PartialEq)]
struct Meter {
    /// The pad the meter ends on (`lv3`, `lvmaster`), without brackets.
    pad: String,
    /// The `ebur128@<name>` instance name its log lines carry (`t3`, `master`).
    name: String,
    /// The index of the timeline track it listens to, `None` for the master.
    track: Option<usize>,
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
    build_filter_complex_metered(timeline, assets, fmt, total, opts, want_video, want_audio, plan, false)
}

/// [`build_filter_complex`], optionally with **level meters** in the sound.
///
/// With `meter` off this *is* the export graph, byte for byte. With it on, the
/// sound is built the same way — same per-clip chains, same ducking, same master
/// bus and `loudnorm` — except that each track's clips are first summed into
/// that track's own submix, which is tapped (`asplit` → `ebur128`) before it
/// joins the bus, and the finished mix is tapped last. Summing a submix and then
/// the submixes is the same arithmetic as summing every clip at once
/// (`amix=normalize=0`), so what the master tap hears is what the export would
/// write, and the per-track taps cost one pass over the media rather than one
/// per track. A track's tap is its strip *output* — fader and pan applied, ahead
/// of the duck bus and the master — which is why a ducked track reads as loud as
/// it would play on its own.
#[allow(clippy::too_many_arguments)]
fn build_filter_complex_metered(
    timeline: &Timeline,
    assets: &[Asset],
    fmt: &ExportFormat,
    total: f64,
    opts: &ExportOptions,
    want_video: bool,
    want_audio: bool,
    plan: &InputPlan,
    meter: bool,
) -> FilterGraph {
    let has_audio = |clip: &crate::model::Clip| clip_sounds(clip, assets);
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
    // Audio entries also carry the owning track's mix — the duck flag for the bus
    // split, the fader and the pan for the clip's own chain.
    let mut audio: Vec<(usize, usize, &crate::model::Clip, TrackMix)> = Vec::new();
    // ...and, parallel to it, the index of the track each audio clip is on (what a
    // metered build groups the submixes by).
    let mut audio_track: Vec<usize> = Vec::new();
    let mut base = 0;
    for (track_index, track) in timeline.tracks.iter().enumerate() {
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
                audio.push((
                    flat,
                    input,
                    clip,
                    TrackMix {
                        duck: track.duck,
                        volume: track.volume,
                        pan: track.pan_gains(),
                    },
                ));
                audio_track.push(track_index);
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
            let (start, end) = ClipTiming::new(clip, &fx[*flat]).window();
            // A slide / push adds its travel to whatever position the clip already
            // has, so a transition composes with a static offset or an animated one
            // instead of overriding it.
            let motion = motion_expr(clip, &fx[*flat]);
            // A position that varies over time is a piecewise expression, and a
            // piecewise expression contains commas — which the graph parser reads
            // as the end of the filter unless the value is quoted. A plain static
            // offset has none, and stays unquoted.
            let quote = |v: String, dynamic: bool| if dynamic { format!("'{v}'") } else { v };
            let overlay = if clip.is_animated() {
                // Animated picture position: per-frame overlay x / y expressions.
                // The eased polyline `transform_at` reads too (one curve for every renderer);
                // an axis that is not keyed is its static offset.
                let xs = curve_or_static(clip, Property::PosX);
                let ys = curve_or_static(clip, Property::PosY);
                let px = keyframe_expr(&xs, "t", clip.timeline_start);
                let py = keyframe_expr(&ys, "t", clip.timeline_start);
                let (px, py) = match &motion {
                    Some((mx, my)) => (format!("({px})+({mx})"), format!("({py})+({my})")),
                    None => (px, py),
                };
                format!(
                    "overlay=x={px}:y={py}:eof_action=pass:enable='between(t,{start},{end})'",
                    px = quote(format!("(W-w)/2+({px})*W"), true),
                    py = quote(format!("(H-h)/2+({py})*H"), true),
                )
            } else if clip.transform.is_identity() && motion.is_none() {
                format!("overlay=eof_action=pass:enable='between(t,{start},{end})'")
            } else {
                let t = &clip.transform;
                let (px, py) = match &motion {
                    Some((mx, my)) => (format!("({})+({mx})", t.pos_x), format!("({})+({my})", t.pos_y)),
                    None => (t.pos_x.to_string(), t.pos_y.to_string()),
                };
                format!(
                    "overlay=x={px}:y={py}:eof_action=pass:enable='between(t,{start},{end})'",
                    px = quote(format!("(W-w)/2+({px})*W"), motion.is_some()),
                    py = quote(format!("(H-h)/2+({py})*H"), motion.is_some()),
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
            // An out-of-list dither (see [`GIF_DITHERS`]) falls back to the
            // default rather than reaching `paletteuse=` unchecked.
            let dither = opts
                .gif_dither
                .as_deref()
                .filter(|d| GIF_DITHERS.contains(d))
                .unwrap_or("bayer");
            chains.push("[vcomp]split[gpsrc][gpuse]".to_string());
            chains.push("[gpsrc]palettegen=stats_mode=diff[gpal]".to_string());
            chains.push(format!("[gpuse][gpal]paletteuse=dither={dither}[outv]"));
        }
    }

    // ---- sound: positioned per-clip audio summed with amix ------------------
    let mut meters: Vec<Meter> = Vec::new();
    if has_audio_out {
        for (flat, input, clip, mix) in &audio {
            let src = if acount[*input] > 1 {
                let k = anext[*input];
                anext[*input] += 1;
                format!("asp{input}_{k}")
            } else {
                format!("{input}:a")
            };
            chains.push(format!(
                "[{src}]{chain}[a{flat}]",
                chain = audio_clip_chain(clip, fmt, &fx[*flat], layout, *mix)
            ));
        }
        // What the final sum adds up, as `(pad, ducked)`: every clip's own pad —
        // or, when metering, each track's submix (tapped on its way in).
        let mut sources: Vec<(String, bool)> = audio.iter().map(|(f, _, _, m)| (format!("a{f}"), m.duck)).collect();
        if meter {
            sources.clear();
            let mut tracks: Vec<usize> = audio_track.clone();
            tracks.dedup();
            for ti in tracks {
                let flats: Vec<usize> = audio
                    .iter()
                    .zip(&audio_track)
                    .filter(|(_, t)| **t == ti)
                    .map(|((f, _, _, _), _)| *f)
                    .collect();
                let duck = timeline.tracks[ti].duck;
                let strip = if let [only] = flats[..] {
                    format!("a{only}")
                } else {
                    chains.push(format!(
                        "{ins}amix=inputs={n}:normalize=0:dropout_transition=0[tk{ti}]",
                        ins = flats.iter().map(|f| format!("[a{f}]")).collect::<String>(),
                        n = flats.len(),
                    ));
                    format!("tk{ti}")
                };
                chains.push(format!("[{strip}]asplit=2[tm{ti}][tl{ti}]"));
                // Sample and true peak both: which track is hot is the question a
                // reading answers, and the oversampling that costs (about 5 ms of
                // work per second of audio per tap) is small beside the decode.
                chains.push(format!("[tl{ti}]ebur128@t{ti}=peak=sample+true[lv{ti}]"));
                meters.push(Meter {
                    pad: format!("lv{ti}"),
                    name: format!("t{ti}"),
                    track: Some(ti),
                });
                sources.push((format!("tm{ti}"), duck));
            }
        }
        // The master bus (its fader, then the limiter) and the optional
        // single-pass loudness normalization close the final mix, in that
        // order; loudnorm upsamples to 192 kHz internally, so resample back to
        // the output rate. A neutral master adds nothing, so a graph from before
        // the master existed is unchanged.
        let mix_tail = if opts.loudnorm {
            format!(
                "{master},loudnorm=I=-14:TP=-1.5:LRA=11,aresample={sr}",
                master = master_filters(&timeline.master),
                sr = fmt.sample_rate
            )
        } else {
            master_filters(&timeline.master)
        };
        let pads = |names: &[&str]| names.iter().map(|n| format!("[{n}]")).collect::<String>();
        let ducked: Vec<&str> = sources.iter().filter(|(_, d)| *d).map(|(n, _)| n.as_str()).collect();
        let keyed: Vec<&str> = sources.iter().filter(|(_, d)| !*d).map(|(n, _)| n.as_str()).collect();
        if ducked.is_empty() || keyed.is_empty() {
            // No ducking in play (nothing flagged, or nothing to key from): one
            // flat sum of every clip, exactly as before.
            let all: Vec<&str> = sources.iter().map(|(n, _)| n.as_str()).collect();
            chains.push(format!(
                "{ins}amix=inputs={n}:normalize=0:dropout_transition=0{mix_tail}[outa]",
                ins = pads(&all),
                n = all.len(),
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
        if meter {
            // The finished mix, as the export would write it (the true peak of
            // this one is the number a delivery is judged on).
            chains.push("[outa]ebur128@master=peak=sample+true[lvmaster]".to_string());
            meters.push(Meter {
                pad: "lvmaster".to_string(),
                name: "master".to_string(),
                track: None,
            });
        }
    }

    FilterGraph {
        filter: chains.join(";"),
        has_video,
        has_audio: has_audio_out,
        meters,
    }
}

#[cfg(test)]
thread_local! {
    /// What [`alimiter_latency_available`] answers on this thread while a test pins it.
    static ALIMITER_LATENCY_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Run `f` as if this ffmpeg's `alimiter` did (`true`) or did not (`false`) have `latency`.
#[cfg(test)]
fn with_alimiter_latency<R>(available: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ALIMITER_LATENCY_OVERRIDE.with(|c| c.set(self.0));
        }
    }
    let _reset = Reset(ALIMITER_LATENCY_OVERRIDE.with(|c| c.replace(Some(available))));
    f()
}

/// Whether `ffmpeg -h filter=alimiter` lists a `latency` option (pure, unit-tested).
fn help_lists_latency(help: &str) -> bool {
    help.lines().any(|l| l.split_whitespace().next() == Some("latency"))
}

/// Whether this ffmpeg's `alimiter` takes `latency`, probed once per process. The
/// option arrived after FFmpeg 4.4 (Ubuntu 22.04's ffmpeg), which refuses the whole
/// graph with `Option 'latency' not found` — every limiter-on export and level
/// measurement. A binary that will not run reads as having it (the modern spelling,
/// like [`zscale_available`]: any render on it is about to fail anyway, and it keeps
/// the builders' output from depending on whether a test machine has ffmpeg).
fn alimiter_latency_available() -> bool {
    // Tests pin the answer so an argv oracle does not depend on the ffmpeg
    // installed where it runs (see `golden`).
    #[cfg(test)]
    if let Some(forced) = ALIMITER_LATENCY_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "quiet", "-h", "filter=alimiter"])
            .stdin(Stdio::null())
            .output()
            .map(|o| help_lists_latency(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or(true);
        tracing::debug!(available = ok, "probed ffmpeg for alimiter latency");
        ok
    })
}

/// The filters the master bus adds after the final sum — its fader, then its
/// limiter — each led by the comma that joins it to the `amix` before it, or
/// the empty string while the master is neutral (the graph exactly as it was
/// before there was a master).
///
/// The limiter is `alimiter` with the two options that matter spelled out:
/// `level=0`, because its default **auto-levels** — scales the output back up so
/// the peak lands at full scale, which would turn a ceiling into a makeup gain —
/// and `latency=1`, because without it the lookahead delays the whole mix by the
/// attack time and drops the last few milliseconds at the end, putting the sound
/// out of step with the picture. An ffmpeg whose `alimiter` has no `latency`
/// ([`alimiter_latency_available`]) is given the limiter without it: the mix then
/// trails the picture by the 5 ms attack and loses its last 5 ms, under a frame at
/// any rate. Attack 5 ms is the filter's own lookahead; the
/// 100 ms release is slow enough not to pump on low notes. The ceiling is
/// rounded to six decimals, which is far below anything audible and keeps the
/// text from depending on the last digit of a libm `pow`.
fn master_filters(master: &crate::model::MasterBus) -> String {
    let mut out = String::new();
    let volume = master.safe_volume();
    if (volume - 1.0).abs() > f64::EPSILON {
        out += &format!(",volume={}", fnum(volume));
    }
    if master.limiter {
        let limit = (master.limit_linear() * 1e6).round() / 1e6;
        let latency = if alimiter_latency_available() { ":latency=1" } else { "" };
        out += &format!(",alimiter=limit={}:attack=5:release=100:level=0{latency}", fnum(limit));
    }
    out
}

/// The owning track's mix settings, carried alongside each of its audio clips.
/// The fader rides the *finished* clip — after its own gain and effect chain,
/// the way a channel strip does — so pulling a music bed down does not change
/// what its own compressor was reacting to.
#[derive(Clone, Copy)]
struct TrackMix {
    duck: bool,
    volume: f32,
    pan: (f64, f64),
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

/// The overlay offset a motion transition puts on a clip, as `(x, y)`
/// expressions in frame widths and heights over **timeline** time — or `None`
/// when the clip does not move, which is what keeps every non-motion graph
/// byte-identical.
///
/// Both halves are ordinary keyframes ([`ClipTiming::motion_keys`] decides them),
/// so this is [`keyframe_expr`] twice over rather than a second expression
/// language.
fn motion_expr(clip: &Clip, fx: &ClipFx) -> Option<(String, String)> {
    let keys = ClipTiming::new(clip, fx).motion_keys()?;
    let start = clip.timeline_start;
    Some((keyframe_expr(&keys.x, "t", start), keyframe_expr(&keys.y, "t", start)))
}

/// The `geq` that cuts a clip to its [`Mask`]: the picture is passed through
/// untouched and only the alpha plane is rewritten, so what is outside the shape
/// (or inside it, `inverted`) becomes transparent and a lower track shows
/// through.
///
/// One expression covers both shapes. Each axis is scaled so the shape's edge
/// sits at distance 1, and the shapes differ only in how the two axes combine —
/// `max` gives a rectangle, `hypot` an ellipse. Feathering is then a ramp over
/// the last `feather` of that distance, measured *inside* the edge so a softened
/// mask never grows beyond the shape that was drawn.
///
/// `geq` is per-pixel and therefore slow, the same cost keyframed opacity
/// already pays; a mask is worth it and a full-frame one is simply not written.
fn mask_filter(mask: &Mask) -> String {
    format!(
        "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='({keep})*alpha(X,Y)'",
        keep = mask_keep_expr(mask)
    )
}

/// The 0..1 "keep" expression at the heart of [`mask_filter`], separated out so
/// a clip with both a mask and keyframed opacity can fold the two into one
/// `geq` pass instead of paying the per-pixel cost twice.
fn mask_keep_expr(mask: &Mask) -> String {
    let m = mask.normalized();
    let dx = format!("(X-{cx}*W)/({rw}*W)", cx = fnum(m.x), rw = fnum(m.width / 2.0));
    let dy = format!("(Y-{cy}*H)/({rh}*H)", cy = fnum(m.y), rh = fnum(m.height / 2.0));
    let d = match m.shape {
        MaskShape::Rect => format!("max(abs({dx})\\,abs({dy}))"),
        MaskShape::Ellipse => format!("hypot({dx}\\,{dy})"),
    };
    let inside = if m.feather <= 1e-6 {
        format!("lte({d}\\,1)")
    } else {
        format!("clip((1-{d})/{f}\\,0\\,1)", f = fnum(m.feather))
    };
    if m.inverted {
        format!("(1-{inside})")
    } else {
        inside
    }
}

/// A scale below this is one a picture can be truncated to nothing by: `scale` reads a
/// width or height that evaluates to 0 as "unset" and keeps the *input's* size, so a clip
/// zoomed to 0.0004 of a frame would snap to full size instead of vanishing. The scale
/// expressions only say `max(1, ...)` below it, so every ordinary graph is as it was.
const TINY_SCALE: f64 = 0.01;

/// The `scale` that zooms a clip's picture by `factor`: a plain number for a constant zoom,
/// else (`keyed`) an expression in `t` that is re-evaluated every frame.
///
/// `tiny` (a factor that can reach [`TINY_SCALE`]) keeps both sides at 1 px or more, and
/// `even` (a tone-map follows: `zscale` refuses an odd size in 4:2:0) rounds them down to
/// an even number of pixels. Neither is written when it is not needed.
fn zoom_scale(factor: &str, keyed: bool, tiny: bool, even: bool, sf: &str) -> String {
    let side = |axis: &str| {
        let e = if keyed {
            format!("{axis}*({factor})")
        } else {
            format!("{axis}*{factor}")
        };
        match (even, tiny) {
            (true, _) => format!("max(2,2*trunc(({e})/2))"),
            (false, true) => format!("max(1,{e})"),
            (false, false) => e,
        }
    };
    let (w, h) = (side("iw"), side("ih"));
    if keyed {
        format!("scale=w='{w}':h='{h}':eval=frame{sf}")
    } else if even || tiny {
        format!("scale=w='{w}':h='{h}'{sf}")
    } else {
        format!("scale={w}:{h}{sf}")
    }
}

/// Polyline points above which [`keyframe_expr`] writes a balanced tree instead of a chain.
/// libavutil refuses an expression nested deeper than 100 levels (`Invalid argument`, from
/// the filter that holds it), a chain is a level per point, and an eased key is 12 points: ten
/// eased keys failed the export and the playback stream alike. Up to this many points the
/// chain's depth is harmless and its text is the one every earlier graph carried.
const KEYFRAME_TREE_POINTS: usize = 24;

/// Build a piecewise-linear ffmpeg expression over **clip-local time** for a
/// channel of keyframes. `points` are `(seconds_from_clip_start, value)` and are
/// sorted here. `tvar` is the time variable the target filter exposes (`t` for
/// overlay / scale / rotate, `T` for geq); `start` is the clip's `timeline_start`
/// so the expression reads time relative to the clip. Values hold flat before the
/// first and after the last keyframe.
///
/// Few points are nested `if(lt(..))`s walked from the first segment; many are a balanced
/// binary tree over the segments (`if(lt(t,t_mid),left,right)`, [`KEYFRAME_TREE_POINTS`]),
/// which is `log2(n)` levels deep and `log2(n)` comparisons per evaluation (a `geq` runs it per
/// pixel). Both pick the one segment whose span holds the time, so they evaluate alike.
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
    let last = pts[pts.len() - 1];
    // The part between the first and the last key; the ends hold their values.
    let inside = if pts.len() > KEYFRAME_TREE_POINTS {
        let tree = keyframe_tree(&pts, 0, pts.len() - 1, &lt);
        format!("if(lt({lt},{tl}),{tree},{vl})", tl = fnum(last.0), vl = fnum(last.1))
    } else {
        // Fold segments in from the end; `expr` starts as the value held after the
        // last keyframe.
        let mut expr = fnum(last.1);
        for w in (0..pts.len() - 1).rev() {
            expr = format!(
                "if(lt({lt},{t1}),{seg},{expr})",
                lt = lt,
                t1 = fnum(pts[w + 1].0),
                seg = keyframe_segment(&pts, w, &lt),
                expr = expr
            );
        }
        expr
    };
    // Hold the first value before the first keyframe.
    format!(
        "if(lt({lt},{t0}),{v0},{inside})",
        lt = lt,
        t0 = fnum(pts[0].0),
        v0 = fnum(pts[0].1),
        inside = inside
    )
}

/// The value inside segment `w` (`pts[w]` to `pts[w + 1]`): a straight line, or the first
/// value for a step (equal times).
fn keyframe_segment(pts: &[(f64, f64)], w: usize, lt: &str) -> String {
    let (t0, v0) = pts[w];
    let (t1, v1) = pts[w + 1];
    if (t1 - t0).abs() < 1e-9 {
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
    }
}

/// Segments `lo..hi` of `pts` as a balanced tree, for a time known to lie in
/// `[pts[lo].0, pts[hi].0)`. A step's empty span is never reached.
fn keyframe_tree(pts: &[(f64, f64)], lo: usize, hi: usize, lt: &str) -> String {
    if hi - lo == 1 {
        return keyframe_segment(pts, lo, lt);
    }
    let mid = (lo + hi) / 2;
    format!(
        "if(lt({lt},{t}),{left},{right})",
        t = fnum(pts[mid].0),
        left = keyframe_tree(pts, lo, mid, lt),
        right = keyframe_tree(pts, mid, hi, lt),
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
    if let Some((gamma_r, gamma_b)) = c.temperature_gammas() {
        f.push_str(&format!(":gamma_r={}:gamma_b={}", fnum(gamma_r), fnum(gamma_b)));
    }
    f
}

/// A keyed number's curve, or — not keyed — its static value as a one-point curve (which
/// [`keyframe_expr`] writes as that number).
fn curve_or_static(clip: &Clip, prop: Property) -> Vec<(f64, f64)> {
    if clip.is_keyed(prop) {
        clip.property_curve(prop)
    } else {
        vec![(0.0, clip.static_value(prop))]
    }
}

/// The `eq` filter of a clip with a keyed colour number: `eval=frame`, and each keyed number
/// an expression of the frame's time `t` (the frame's timestamp, which `setpts` has put on the
/// timeline, so clip-local time is `t` minus the clip's start, as every other keyed expression
/// here). Numbers that are not keyed stay the static ones. The temperature is the
/// opposing gammas [`Color::temperature_gammas`] makes of it, `1 + 0.3 t` and `1 - 0.3 t`
/// (the curve's values are held to -1..=1, so the clamp that function applies is already
/// in them). Every expression is quoted — it has commas — and `eq` evaluates them per frame
/// on FFmpeg 4.4 through 9.0 alike.
fn eq_filter_keyed(clip: &Clip) -> String {
    let c = &clip.color;
    let expr = |prop: Property| keyframe_expr(&clip.property_curve(prop), "t", clip.timeline_start);
    let number = |prop: Property, fixed: f64| {
        if clip.is_keyed(prop) {
            format!("'{}'", expr(prop))
        } else {
            fixed.to_string()
        }
    };
    let mut f = format!(
        "eq=brightness={}:contrast={}:saturation={}:gamma={}",
        number(Property::Brightness, c.brightness),
        number(Property::Contrast, c.contrast),
        number(Property::Saturation, c.saturation),
        number(Property::Gamma, c.gamma)
    );
    if clip.is_keyed(Property::Temperature) {
        let e = expr(Property::Temperature);
        f.push_str(&format!(":gamma_r='1+0.3*({e})':gamma_b='1-0.3*({e})'"));
    } else if let Some((gamma_r, gamma_b)) = c.temperature_gammas() {
        f.push_str(&format!(":gamma_r={}:gamma_b={}", fnum(gamma_r), fnum(gamma_b)));
    }
    f.push_str(":eval=frame");
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

/// Whether `s` is a safe ffmpeg colour spec to splice unquoted into a filter
/// value: a bare name (letters only), `#RRGGBB[AA]` hex or `0xRRGGBB[AA]` hex,
/// each with an optional `@alpha` suffix (a decimal in 0..1). `color`/`bg` reach
/// `fontcolor=`/`bordercolor=`/`boxcolor=`/`chromakey=` unquoted — like every
/// other free-form string spliced into this graph, a comma or colon here would
/// start a new filter node or option instead of naming a colour.
pub(crate) fn valid_color(s: &str) -> bool {
    let (base, alpha) = s.split_once('@').map_or((s, None), |(b, a)| (b, Some(a)));
    if let Some(a) = alpha {
        if a.is_empty() || a.matches('.').count() > 1 || !a.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return false;
        }
    }
    if let Some(hex) = base.strip_prefix('#') {
        return matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    if let Some(hex) = base.strip_prefix("0x").or_else(|| base.strip_prefix("0X")) {
        return matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    !base.is_empty() && base.chars().all(|c| c.is_ascii_alphabetic())
}

/// `s` if [`valid_color`], else `fallback` — the safe way to splice a
/// caller-controlled colour into a filter value; rendering must never fail or
/// inject over a bad one.
pub(crate) fn safe_color<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if valid_color(s) {
        s
    } else {
        fallback
    }
}

/// The `chromakey` filter for a chroma-key effect, or `None` for any other.
fn chroma_filter(e: &VideoEffect) -> Option<String> {
    match e {
        VideoEffect::ChromaKey {
            color,
            similarity,
            blend,
        } => Some(format!(
            "chromakey={}:{}:{}",
            safe_color(color, "green"),
            fnum(*similarity),
            fnum(*blend)
        )),
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

/// Escape a value for a single-quoted `drawtext` option inside a filtergraph
/// passed as one argv argument: backslashes, then apostrophes (the filtergraph's
/// own value parser un-escapes a backslash-doubled pair down to one backslash
/// even inside single quotes, so a literal backslash needs doubling to survive
/// it). Newlines collapse to spaces — drawtext here is single-line. Control
/// characters other than tab and carriage return are dropped: a NUL cannot be
/// passed in an argv at all ("nul byte found in provided data" — every preview
/// and export would fail to spawn until the title was found and deleted) and the
/// rest (ESC, the C1 block) draw as nothing useful. Everything else — which is to
/// say every real title — comes out exactly as before. Shared by the `text=` and
/// `fontfile=` escapers below, which differ only in whether `%` also needs
/// escaping.
fn escape_drawtext(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push(' '),
            '\'' => out.push_str("'\\''"),
            c if c.is_control() && !matches!(c, '\t' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// [`escape_drawtext`] for `text=`. `drawtext` expands `%{...}` at
/// configuration time, and a bare `%` is a configuration error that silently
/// blanks the whole overlay — ffmpeg still exits 0, so nothing reports it.
/// drawtext's own escape for a literal `%` is `\%` — but that backslash goes
/// through the same filtergraph value parser [`escape_drawtext`] doubles
/// backslashes against, so it needs the same doubling to arrive as one.
fn escape_drawtext_text(s: &str) -> String {
    escape_drawtext(s).replace('%', "\\\\%")
}

/// [`escape_drawtext`] for `fontfile=`, a raw filesystem path rather than
/// drawtext-expanded text — escaping `%` there would corrupt a path that
/// legitimately contains one.
fn escape_drawtext_path(s: &str) -> String {
    escape_drawtext(s)
}

/// `drawtext` options shared by the export and still paths (text, size, color,
/// font, bold approximation, box) — everything except position / alpha /
/// enable. `frame_h` is the target canvas height (export height, or the
/// still's height).
fn drawtext_common(o: &TextOverlay, frame_h: f64) -> Vec<String> {
    let fontsize = (frame_h * o.size).round().max(1.0);
    let color = safe_color(&o.color, "white");
    let mut parts = vec![
        format!("text='{}'", escape_drawtext_text(&o.text)),
        format!("fontsize={}", fnum(fontsize)),
        format!("fontcolor={color}"),
    ];
    // Resolve a chosen system font to its file on disk; falls through to
    // FFmpeg's drawtext default if unset or no longer installed.
    let resolved_bold = o
        .font
        .as_deref()
        .and_then(|family| crate::fonts::resolve_font_file(family, o.bold))
        .map(|(path, matched_bold)| {
            parts.push(format!("fontfile='{}'", escape_drawtext_path(&path.to_string_lossy())));
            matched_bold
        });
    if o.bold && resolved_bold != Some(true) {
        // No real bold face available: a same-color border thickens the glyphs.
        parts.push("borderw=2".to_string());
        parts.push(format!("bordercolor={color}"));
    }
    // An invalid box colour drops the box entirely rather than falling back to
    // a visible default — a caller that asked for no readable box colour gets
    // no box, not a random one.
    if let Some(bg) = o.bg.as_deref().filter(|bg| valid_color(bg)) {
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
        // Quoted: a piecewise expression contains commas, and an unquoted comma
        // ends the filter as far as the graph parser is concerned.
        parts.push(format!("x='(w*({})-text_w/2)'", keyframe_expr(&xs, "t", o.start)));
        parts.push(format!("y='(h*({})-text_h/2)'", keyframe_expr(&ys, "t", o.start)));
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

/// One `fade` of a clip's picture as ffmpeg spells it.
fn fade_filter(step: &FadeStep) -> String {
    let edge = match step.edge {
        FadeEdge::In => "in",
        FadeEdge::Out => "out",
    };
    let tint = match step.tint {
        FadeTint::Black => "",
        FadeTint::White => ":c=white",
        FadeTint::Alpha => ":alpha=1",
    };
    format!("fade=t={edge}:st={}:d={}{tint}", step.st, step.d)
}

/// The video filter chain for one clip (everything between its `[i:v]` input
/// and its `[v{i}]` output): trim, optional reverse / crop / retime, 360
/// reprojection, fit or transform geometry, color correction, per-clip video
/// effects, keyframe animation, fades and transition alpha. With all properties
/// at their defaults this reduces to the original fit-and-letterbox chain.
///
/// **A keyframed zoom is the one stage that changes the picture's size from frame
/// to frame**, and almost nothing downstream of it can follow: `format`
/// negotiation inserts a fixed-size converter, `eq` / `geq` / `rotate` /
/// `gblur` read the frame size once when the graph is configured, and a filter
/// that does is pinned to the *first* frame's size for the whole clip (a zoom
/// that never showed, an opacity ramp or rotation that stayed at the first
/// frame's geometry). So when [`Clip::zoom_animated`] the zoom `scale` is the
/// **last** stage of the chain — everything else runs at the constant fit size —
/// and it sits after `fps`, so it is evaluated at the *output* frame's time
/// rather than at the source frame's (a 10 fps clip in a 30 fps export zoomed in
/// three-frame steps). Only `overlay` follows, and it reads each picture's own
/// size every frame (`(W-w)/2` centres it as it grows). The chain then ends in
/// `yuva420p`, the format `overlay` takes natively, so no converter is inserted
/// between them. Every other clip keeps the chain it always had.
///
/// `instance` is the clip's unique flat index, used to name its `v360` so
/// `sendcmd` can address it; it is unused for clips that do not reframe.
fn video_clip_chain(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx, is_image: bool, instance: &str) -> String {
    let s = clip.speed_mag();
    let t = &clip.transform;
    let anim = clip.is_animated();
    // Which animated channels actually move (so alpha / per-frame geometry is only
    // forced when needed). A clip animated through the legacy bundle has every number of its
    // transform keyed (a fresh keyframe captures the static transform), so `anim` alone drove
    // the geometry; a clip with per-property channels keys only some, and the rest are built
    // from its static transform as an unkeyed clip's are. Rotation / opacity additionally
    // need an alpha plane.
    let scale_keyed = clip.is_keyed(Property::Scale);
    let rotation_keyed = clip.is_keyed(Property::Rotation);
    let opacity_keyed = clip.is_keyed(Property::Opacity);
    let anim_rotation = clip.property_keys(Property::Rotation).iter().any(|k| k.value != 0.0);
    let anim_opacity = clip.property_keys(Property::Opacity).iter().any(|k| k.value < 1.0);
    // A zoom that really moves: the picture changes size per frame, so its `scale`
    // goes last (see above). A keyed clip whose scale holds still keeps the chain
    // it always had — its picture never changes size, so nothing can be pinned.
    let zoom_keyed = clip.zoom_animated();
    let chroma = clip.effects.iter().any(|e| e.produces_alpha());
    // Alpha is needed for static opacity/rotation, animated opacity/rotation, a
    // chroma key, or a crossfade dissolve.
    let transform_alpha =
        (!rotation_keyed && t.rotation != 0.0) || (!opacity_keyed && t.opacity < 1.0) || anim_rotation || anim_opacity || chroma;
    let needs_alpha = transform_alpha || fx.xfade_in > 0.0 || clip.mask.is_some();
    let timing = ClipTiming::new(clip, fx);
    let dur = timing.duration();
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
    // A head-padded proxy opens with one clone of its first frame at time zero (see
    // [`build_proxy_args`]), which a seek skips — and a read from the start does
    // not. The original has no such frame: its first one is the real one, and the
    // `setpts` below puts *that* at the clip's start. Dropping the clone first makes
    // the proxy do the same, so a clip cut from the very head of a late-starting
    // source plays the frames the export renders instead of `lead` seconds of the
    // first frame held. Only a padded proxy gets the filter; every other input's
    // chain is exactly what it was.
    if fx.head_pad && !is_image && seek == 0.0 {
        p.push("trim=start_frame=1".to_string());
    }
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
    // `zscale` (the tone-map, which follows the geometry) refuses a picture whose size is
    // not a multiple of its chroma subsampling, and a Contain fit of 4:3 footage into a
    // 9:16 frame is 405 rows high. Every size ahead of it is made even for HDR footage.
    let tone_mapped = fx.hdr.is_some();
    // A keyframed clip is treated as non-identity so its picture is centered by
    // the overlay (not padded full-frame), and its zoom is re-evaluated per frame.
    let geom_identity = t.is_identity() && !anim;
    match fmt.fit {
        // Fit inside the frame; the identity case then pads out to full size so
        // the overlay lands on a complete canvas.
        Fit::Contain => {
            p.push(format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease{even}{sf}",
                w = fmt.width,
                h = fmt.height,
                even = if tone_mapped { ":force_divisible_by=2" } else { "" }
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
    let mut late_zoom: Option<String> = None;
    if !geom_identity {
        if scale_keyed {
            // Per-frame zoom: re-evaluate the scale expression every frame.
            let expr = keyframe_expr(&clip.property_curve(Property::Scale), "t", clip.timeline_start);
            let tiny = clip.property_keys(Property::Scale).iter().any(|k| k.value < TINY_SCALE);
            // A moving zoom runs after the tone-map, at the end of the chain: even sizes
            // only matter ahead of `zscale`.
            let zoom = zoom_scale(&expr, true, tiny, tone_mapped && !zoom_keyed, &sf);
            if zoom_keyed {
                late_zoom = Some(zoom);
            } else {
                p.push(zoom);
            }
        } else if (t.scale - 1.0).abs() > 1e-9 {
            p.push(zoom_scale(
                &t.scale.to_string(),
                false,
                t.scale < TINY_SCALE,
                tone_mapped,
                &sf,
            ));
        }
    }
    p.push("setsar=1".to_string());
    if reframe.is_none() {
        p.push(format!("fps={}", fmt.fps));
    }
    // HDR → SDR, once per clip, as late as the geometry allows: after the fit
    // scale has cut a 4K frame to the delivery size and `fps` has dropped the
    // frames that will never be shown, because the float RGB stage is by far the
    // most expensive step in the chain — and before any colour work, which is
    // meant to act on the SDR picture.
    if let Some(hdr) = fx.hdr {
        p.push(tonemap_filter(hdr));
    }
    // Color correction must run BEFORE any alpha plane is established: ffmpeg's `eq`
    // has no alpha-capable input format, so the graph would otherwise auto-insert a
    // conversion that drops the alpha (silently disabling opacity / rotation).
    if clip.color_animated() {
        p.push(eq_filter_keyed(clip));
    } else if !clip.color.is_identity() {
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
    // The shape mask (once alpha exists — it only rewrites the alpha plane, so
    // it composes with a chroma key above it) and opacity: animated opacity is a
    // per-frame geq alpha (geq's time var is `T`), else a constant alpha mix.
    // The mask is the same alpha-plane geq, so a clip that
    // has both shares one pass — geq is per-pixel and by far the most expensive
    // filter in the chain, and two back-to-back passes would double it.
    let opacity_expr = anim_opacity.then(|| keyframe_expr(&clip.property_curve(Property::Opacity), "T", clip.timeline_start));
    match (&clip.mask, opacity_expr) {
        (Some(mask), Some(expr)) => p.push(format!(
            "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='({keep})*({expr})*alpha(X,Y)'",
            keep = mask_keep_expr(mask)
        )),
        (Some(mask), None) => p.push(mask_filter(mask)),
        (None, Some(expr)) => p.push(format!(
            "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='({expr})*alpha(X,Y)'"
        )),
        (None, None) => {}
    }
    if !opacity_keyed && t.opacity < 1.0 {
        p.push(format!("colorchannelmixer=aa={}", t.opacity));
    }
    // Rotation: animated angle expression (degrees → radians), else a constant
    // rotate. Animated rotation uses a fixed bounding box (the frame diagonal).
    //
    // The animated fill is `black@0`, not `none`: `none` means "do not fill", and
    // `rotate` then leaves whatever its output buffer last held outside the
    // picture. A constant angle rewrites the same footprint every frame, so the
    // corners stay as the zeroed buffer was allocated (transparent) and nobody
    // sees it; an angle that moves leaves each earlier frame's footprint behind
    // in every buffer it reuses, and the picture grows into the union of every
    // pose it has had (the "erratic" rotation, on 6.1 and 9.0 alike).
    if anim_rotation {
        let expr = keyframe_expr(&clip.property_curve(Property::Rotation), "t", clip.timeline_start);
        p.push(format!(
            "rotate=a='({expr})*PI/180':fillcolor=black@0:ow='hypot(iw,ih)':oh='hypot(iw,ih)'"
        ));
    } else if !rotation_keyed && t.rotation != 0.0 {
        let rad = t.rotation.to_radians();
        p.push(format!("rotate={rad}:fillcolor=none:ow=rotw({rad}):oh=roth({rad})"));
    }
    // `fade` reads the frame's pts, which `setpts` above already moved onto
    // the timeline, so a fade starts at the clip's timeline position and not
    // at 0 — clip-local times blacked out a later clip's fade-out entirely and
    // made its fade-in / dip / dissolve land before the clip existed. A dip
    // through white lands on the frame itself rather than on the alpha plane, so
    // it needs no `format=yuva420p`; a dissolve's alpha ramp does, and
    // `needs_alpha` above has already established it.
    for step in timing.fades() {
        p.push(fade_filter(&step));
    }
    if let Some(zoom) = late_zoom {
        // The one size-changing stage, after `fps` (output-frame time) and after
        // every filter that has to see a constant size. `overlay` takes only
        // `yuva420p` for its picture, so say so here: a different terminal format
        // would get a fixed-size converter inserted between this and the overlay,
        // pinning the size right back.
        p.push(zoom);
        p.push("format=yuva420p".to_string());
    } else if fx.alpha && !needs_alpha {
        // A source with an alpha channel keeps it to the overlay: the terminal pixel
        // format below has none, and `overlay` takes a `yuva420p` picture natively, so
        // flattening here put a transparent sticker's cut-out on black while the still,
        // which has no terminal format, kept it.
        p.push("format=yuva420p".to_string());
    } else if !needs_alpha {
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
    run_still(timeline, assets, opts, t, max_width, None, &StillOutput::JpegPipe { quality })
}

/// [`timeline_frame`] of one `region` of the composited canvas. The canvas is
/// rendered large enough that the region alone comes out `max_width` wide
/// (capped at the delivery frame, so nothing is invented), then cropped — a
/// zoom into the cut rather than a screenshot of it.
pub fn timeline_frame_region(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    region: Region,
    max_width: u32,
    quality: u8,
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
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let out = StillOutput::File {
        path: path.to_string_lossy().into_owned(),
        format,
        quality,
    };
    // `u32::MAX` asks for no cap: `build_still_args` clamps to the delivery
    // width, which is exactly the cover size.
    run_still(timeline, assets, opts, t, u32::MAX, None, &out)?;
    Ok(path.to_path_buf())
}

/// Run a composited still through ffmpeg, retrying in software if hardware
/// decode was asked for and failed. Returns stdout (empty for a file sink).
fn run_still(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    region: Option<Region>,
    out: &StillOutput,
) -> Result<Vec<u8>> {
    let piping = matches!(out, StillOutput::JpegPipe { .. });
    let run = |o: &ExportOptions| -> Result<Vec<u8>> {
        let mut args = build_still_args(timeline, assets, o, t, max_width, region, out)?;
        cpu::limit_args(&mut args, cpu::budget_threads());
        let bin = ffmpeg_bin();
        let output = command(&bin)
            .args(&args)
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| launch_err(&bin, e))?;
        if !output.status.success() || (piping && output.stdout.is_empty()) {
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

/// The JPEG-pipe still args — [`timeline_frame`]'s shape, and what every still
/// test asserts against.
#[cfg(test)]
fn build_timeline_frame_args(
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
    fn args(&self) -> Vec<String> {
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

/// Pure arg builder for a composited still, parameterized by its sink (no I/O,
/// unit-tested).
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
fn build_still_args(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    region: Option<Region>,
    out: &StillOutput,
) -> Result<Vec<String>> {
    // Same gate as the export, so the still shows the cut that would render.
    let rendered = timeline.for_render();
    let timeline = &rendered;

    let fmt = export_format(timeline, assets, opts);
    // A region zoom composites a canvas wide enough that the region alone is
    // `max_width` — the delivery frame caps it, so a zoom never upscales.
    let region = region.map(Region::normalized);
    let canvas_width = match region {
        Some(r) => ((max_width as f64 / r.width).ceil() as u32).min(fmt.width),
        None => max_width,
    };
    // Output canvas: export aspect ratio, capped to `max_width`, even dimensions.
    let (ow, oh) = still_size(fmt.width, fmt.height, canvas_width);
    let t = t.max(0.0);
    let asset_of = |id| assets.iter().find(|a: &&Asset| a.id == id);

    // Active video clips at `t`, in composite order (tracks in list order, clips
    // within a track in timeline order), paired with their source time. The GPU
    // compositor's `RenderPlan` is built from the same list.
    let active = active_video_clips(timeline, assets, t);

    let mut args: Vec<String> = vec!["-hide_banner".to_string(), "-loglevel".to_string(), "error".to_string()];
    for ac in &active {
        let (clip, src) = (ac.clip, ac.source_time);
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
            args.push(seek_arg(src));
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
    for (n, ac) in active.iter().enumerate() {
        let clip = ac.clip;
        let tf = ac.transform();
        let rf = clip.reframe_at(ac.local_time);
        let hdr = asset_of(clip.asset_id).and_then(|a| a.hdr());
        let tone = hdr.map(|h| format!("{},", tonemap_filter(h))).unwrap_or_default();
        chains.push(format!(
            "[{n}:v]{tone}{chain}[v{n}]",
            chain = still_clip_chain(
                &tf,
                &ac.color(),
                &clip.effects,
                rf.as_ref(),
                &canvas,
                clip.mask.as_ref(),
                clip.zoom_animated()
            )
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
    match region {
        Some(r) => chains.push(format!("[{cur}]{crop}[outv]", crop = r.crop_filter())),
        None => chains.push(format!("[{cur}]null[outv]")),
    }
    let filter = chains.join(";");

    args.extend([
        "-filter_complex".to_string(),
        filter,
        "-map".to_string(),
        "[outv]".to_string(),
    ]);
    args.extend(out.args());
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
///
/// `zoom_last` is [`Clip::zoom_animated`]: the export runs a moving zoom as the **last**
/// stage of the chain (see [`video_clip_chain`]), so its blur, mask, rotation and grade
/// act on the picture at its fit size and are magnified with it. The still follows, or
/// the frame you scrub to would be sharper than the file at a zoom-in and softer at a
/// zoom-out. Every other clip zooms first, as it always did.
fn still_clip_chain(
    tf: &Transform,
    color: &Color,
    effects: &[VideoEffect],
    reframe: Option<&ResolvedReframe>,
    canvas: &StillCanvas,
    mask: Option<&Mask>,
    zoom_last: bool,
) -> String {
    let StillCanvas { w: ow, h: oh, fit, sf } = canvas;
    let (ow, oh, fit) = (*ow, *oh, *fit);
    let chroma = effects.iter().any(|e| e.produces_alpha());
    let needs_alpha = (!tf.is_identity() && tf.needs_alpha()) || chroma || mask.is_some();
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
    let mut late_zoom: Option<String> = None;
    if tf.is_identity() {
        // Cover already fills the frame; padding it would be a no-op that still
        // costs a filter, so only the letterboxed path needs it.
        if fit == Fit::Contain {
            p.push(format!("pad={ow}:{oh}:(ow-iw)/2:(oh-ih)/2"));
        }
    } else if (tf.scale - 1.0).abs() > 1e-9 {
        let zoom = zoom_scale(&tf.scale.to_string(), false, tf.scale < TINY_SCALE, false, sf);
        if zoom_last {
            late_zoom = Some(zoom);
        } else {
            p.push(zoom);
        }
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
    if let Some(mask) = mask {
        p.push(mask_filter(mask));
    }
    if tf.opacity < 1.0 {
        p.push(format!("colorchannelmixer=aa={}", tf.opacity));
    }
    if tf.rotation != 0.0 {
        let rad = tf.rotation.to_radians();
        p.push(format!("rotate={rad}:fillcolor=none:ow=rotw({rad}):oh=roth({rad})"));
    }
    p.extend(late_zoom);
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
fn audio_clip_chain(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx, layout: &str, mix: TrackMix) -> String {
    let s = clip.speed_mag();
    let dur = ClipTiming::new(clip, fx).duration();
    // Mirror the video crossfade tail (extends below source_in when reversed) and
    // the same input-side `-ss` fast-seek, so the atrim is relative to the seek.
    let (trim_start, trim_end) = clip_source_window(clip, fx);
    let seek = clip_seek(trim_start);
    let delay_ms = (clip.timeline_start * 1000.0).round().max(0.0) as i64;
    let fi = clip.fade_in + fx.black_in + fx.white_in + fx.afade_in;
    let fo = clip.fade_out + fx.black_out + fx.white_out + fx.tail;

    let mut p: Vec<String> = Vec::new();
    p.push(format!("atrim=start={}:end={}", trim_start - seek, trim_end - seek));
    p.push("asetpts=PTS-STARTPTS".to_string());
    if clip.is_reversed() {
        p.push("areverse".to_string());
    }
    if (s - 1.0).abs() > 1e-9 {
        p.push(atempo_chain(s));
    }
    if clip.volume_animated() {
        // A keyed gain: `volume` evaluated per frame, after `atempo` so its `t` is the
        // clip's own playing time. It holds one gain for a whole frame, so the frames are
        // cut to a few milliseconds first (`asetnsamples`, without padding the last one) or
        // a fade would step at the decoder's frame size — 21 ms for AAC, audible as zipper noise.
        p.push(format!("asetnsamples=n={VOLUME_FRAME_SAMPLES}:p=0"));
        p.push(format!(
            "volume='{}':eval=frame",
            keyframe_expr(&clip.property_curve(Property::Volume), "t", 0.0)
        ));
    } else {
        p.push(format!("volume={}", clip.volume));
    }
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
    // The track fader, after the clip's own chain. Omitted at unity, so a
    // project that never touched a fader renders the graph it always did.
    if (mix.volume - 1.0).abs() > f32::EPSILON {
        p.push(format!("volume={}", mix.volume));
    }
    p.push(format!(
        "aformat=sample_rates={sr}:channel_layouts={layout}",
        sr = fmt.sample_rate
    ));
    // The track pan, after `aformat` has normalized the stream to the delivery
    // layout: `pan` indexes channels by number, and run before the upmix a mono
    // source has no c1 — the attenuated leg would be synthesized from silence,
    // so panning a mono voice right would mute it instead of leaning it.
    // Omitted at centre, so an untouched mix stays byte-identical.
    let (gl, gr) = mix.pan;
    if layout == "stereo" && ((gl - 1.0).abs() > 1e-9 || (gr - 1.0).abs() > 1e-9) {
        // A balance, not a mono re-pan: each side keeps its own channel and is
        // attenuated, so a stereo music bed leans without collapsing.
        p.push(format!("pan=stereo|c0={}*c0|c1={}*c1", fnum(gl), fnum(gr)));
    }
    p.push(format!("adelay={delay_ms}:all=1"));
    p.join(",")
}

/// Samples in a frame the keyed volume is evaluated on (2.7 ms at 48 kHz): the gain steps by
/// at most the curve's slope times that between two frames.
const VOLUME_FRAME_SAMPLES: usize = 128;

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

/// Measuring the mix (`ebur128` meters on the export's own audio graph): a child
/// module so it reaches the private graph builders.
mod levels;
pub use levels::mix_levels;

/// The golden argv oracle (see its docs): a child module so it reaches the
/// private builders.
#[cfg(test)]
mod golden;

/// What the export graph draws, pinned against rendered pixels (`#[ignore]`d).
#[cfg(test)]
mod rendered;

/// The sound of detached / linked clips: that `extract_audio` used to double, that a
/// detached clip's *own* chain is the chain it had, and that its **level** survives
/// the move between a picture track's fader and an audio track's (graph level, and
/// measured on a render under `#[ignore]`). Pan, duck and mute are the destination
/// track's afterwards — see `Timeline::detach_audio`.
#[cfg(test)]
mod linked_audio;

/// The export plan against the *evaluated* export graph, on a grid of frames.
#[cfg(test)]
mod sweep;

/// The `fps` pick against the frames the export renders (`#[ignore]`d).
#[cfg(test)]
mod picked;

/// A keyframed zoom with every other feature of a clip, measured per output frame
/// against `transform_at` and the scrubbed still (`#[ignore]`d).
#[cfg(test)]
mod keyed_zoom;

/// Per-property channels — a keyed colour and a keyed volume — against the pictures and samples
/// the export renders (`#[ignore]`d).
#[cfg(test)]
mod keyed_channels;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip_timing::is_head_padded_proxy;
    use crate::engine::test_support::{
        audio_stream, audio_track, av_asset, image_stream, img_asset, make_clip, remove_proxy, single, test_asset, timeline_of,
        video_stream, video_track, ProxyGuard, StatusBounded,
    };
    use crate::model::{Asset, Clip, Delivery, StreamInfo, StreamKind, Timeline, Track, TransitionKind};

    /// A track mix that changes nothing — what every test that is not about the
    /// mixer wants.
    fn unity_mix() -> TrackMix {
        TrackMix {
            duck: false,
            volume: 1.0,
            pan: (1.0, 1.0),
        }
    }
    use chrono::Utc;
    use uuid::Uuid;

    // ---- salience sampling --------------------------------------------------

    #[test]
    fn salience_args_sample_the_window_at_the_analysis_grid() {
        let args = build_salience_args(Path::new("/m/a.mp4"), 12.0, 22.0);
        let joined = args.join(" ");
        assert!(joined.contains("-ss 12.000 -t 10.000 -i /m/a.mp4"), "{joined}");
        // 48 samples over 10s.
        assert!(
            joined.contains("fps=4.8000,scale=64:36:flags=bilinear,format=gray"),
            "{joined}"
        );
        assert!(joined.contains("-frames:v 48"), "{joined}");
        assert!(joined.contains("-f rawvideo -pix_fmt gray pipe:1"), "{joined}");
        assert!(joined.contains("-an"), "{joined}");
    }

    #[test]
    fn salience_args_clamp_the_sample_rate_for_absurd_windows() {
        // A quarter-second clip must not ask for 192 fps...
        let fast = build_salience_args(Path::new("/m/a.mp4"), 0.0, 0.25).join(" ");
        assert!(fast.contains("fps=30.0000"), "{fast}");
        // ...nor an hour-long one for a frame every 75 seconds.
        let slow = build_salience_args(Path::new("/m/a.mp4"), 0.0, 3600.0).join(" ");
        assert!(slow.contains("fps=0.2000"), "{slow}");
        // A zero-length window still produces a runnable command.
        let empty = build_salience_args(Path::new("/m/a.mp4"), 5.0, 5.0).join(" ");
        assert!(empty.contains("-t 0.040"), "{empty}");
    }

    #[test]
    fn scoring_no_frames_yields_an_empty_map() {
        assert_eq!(score_salience(&[]), SalienceMap::default());
        // A partial frame is not a frame.
        assert_eq!(score_salience(&[7u8; 128]), SalienceMap::default());
    }

    #[test]
    fn scoring_finds_the_detailed_half_of_a_flat_frame() {
        let (w, h) = (SALIENCE_COLS, SALIENCE_ROWS);
        // Left half flat gray, right half a hard checkerboard.
        let mut frame = vec![40u8; w * h];
        for y in 0..h {
            for x in w / 2..w {
                frame[y * w + x] = if (x + y) % 2 == 0 { 0 } else { 255 };
            }
        }
        let map = score_salience(&frame);
        assert_eq!((map.cols, map.rows), (w, h));
        let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
        assert!(crop.offset > 0.0, "the textured half pulls the window right: {crop:?}");
    }

    #[test]
    fn scoring_weighs_a_moving_subject_over_a_static_one() {
        let (w, h) = (SALIENCE_COLS, SALIENCE_ROWS);
        let block = |frame: &mut Vec<u8>, from: usize, to: usize, v: u8| {
            for y in h / 3..2 * h / 3 {
                for x in from..to {
                    frame[y * w + x] = v;
                }
            }
        };
        // Two frames: a static block on the left, a block on the right that moves.
        let mut a = vec![0u8; w * h];
        block(&mut a, 4, 12, 200);
        block(&mut a, w - 16, w - 8, 200);
        let mut b = a.clone();
        block(&mut b, w - 16, w - 8, 0);
        block(&mut b, w - 20, w - 12, 200);
        let mut raw = a;
        raw.extend_from_slice(&b);
        let map = score_salience(&raw);
        let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
        assert!(crop.offset > 0.0, "motion wins over equal detail: {crop:?}");
    }

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
        let max = out.iter().copied().fold(0.0_f32, f32::max);
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
        let max = out.iter().copied().fold(0.0_f32, f32::max);
        assert!((max - 0.9).abs() < 1e-6, "got {max}");
    }

    #[test]
    fn proxy_args_are_all_intra_audioless_and_keep_timing() {
        let args = build_proxy_args("/in.mov", "/out.mp4", 3, PROXY_MAX_WIDTH, "libx264", None, None, None);
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
        let args = build_proxy_args(
            "/in.mov",
            "/out.mp4",
            3,
            PROXY_MAX_WIDTH,
            "h264_nvenc",
            Some("auto"),
            None,
            None,
        );
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
    fn an_ordinary_proxy_is_the_same_argv_it_always_was() {
        // Nothing here may change for a source that starts its video with its
        // container: that is every file whose proxy is already cached and right.
        assert_eq!(
            build_proxy_args("/in.mov", "/out.mp4", 3, 1280, "libx264", None, None, None).join(" "),
            "-hide_banner -loglevel error -y -i /in.mov -an \
             -vf scale='min(1280,iw)':-2:flags=bilinear -c:v libx264 -preset veryfast -crf 24 \
             -g 1 -threads 3 -pix_fmt yuv420p -f mp4 /out.mp4"
        );
    }

    #[test]
    fn a_late_video_start_is_filled_by_one_held_frame_and_keeps_every_timestamp() {
        let args = build_proxy_args(
            "/in.mov",
            "/out.mp4",
            3,
            1280,
            "libx264",
            Some("auto"),
            Some("TONEMAP"),
            Some("-fps_mode"),
        );
        let flag = |name: &str| args.iter().position(|a| a == name).map(|i| args[i + 1].as_str());
        // One clone of the first frame at time zero, merged in by timestamp.
        let graph = flag("-filter_complex").expect("a filter_complex");
        assert!(
            graph.contains("split[a][b]") && graph.contains("trim=end_frame=1,setpts=PTS-STARTPTS[h]"),
            "{graph}"
        );
        assert!(graph.ends_with("[h][b]interleave[v]"), "{graph}");
        // The downscale and the tone-map run once, before the split, so the clone is
        // a proxy-sized SDR frame like the rest.
        assert!(
            graph.starts_with("[0:v:0]scale='min(1280,iw)':-2:flags=bilinear,TONEMAP,split"),
            "{graph}"
        );
        assert_eq!(flag("-map"), Some("[v]"));
        assert!(!args.contains(&"-vf".to_string()), "one chain, not two");
        // Variable frame rate, spelled the way this ffmpeg takes it: cfr (every
        // FFmpeg-6 mp4's default) would snap the frames to a grid again.
        assert_eq!(flag("-fps_mode"), Some("vfr"));
        assert!(!args.contains(&"cfr".to_string()));
        // Everything else about the proxy is as ever: all-intra, no retime, no seek.
        assert_eq!(flag("-g"), Some("1"));
        assert!(args.contains(&"-an".to_string()));
        assert!(!args.contains(&"-ss".to_string()) && !args.contains(&"-r".to_string()));
        assert_eq!(args.last().unwrap(), "/out.mp4");
        // The flag's spelling is the caller's, for a pre-5.1 ffmpeg.
        let legacy = build_proxy_args("/in.mov", "/out.mp4", 3, 1280, "libx264", None, None, Some("-vsync"));
        assert!(legacy.windows(2).any(|w| w[0] == "-vsync" && w[1] == "vfr"));
        // A hardware encoder only changes the encoder's own flags.
        let nvenc = build_proxy_args("/in.mov", "/out.mp4", 3, 1280, "h264_nvenc", None, None, Some("-fps_mode"));
        assert!(nvenc.windows(2).any(|w| w[0] == "-pix_fmt" && w[1] == "nv12"));
        assert!(nvenc.windows(2).any(|w| w[0] == "-fps_mode" && w[1] == "vfr"));
    }

    #[test]
    fn head_lead_is_the_video_start_past_the_container_start() {
        // Audio at 0 and video at 0.08 s.
        assert!((head_lead(Some(0.08), Some(0.0)) - 0.08).abs() < 1e-12);
        // Relative to the container, whatever clock it started on (an MPEG-TS
        // capture begins at some arbitrary tens of seconds).
        assert!((head_lead(Some(603.5), Some(600.0)) - 3.5).abs() < 1e-9);
        // The video *is* the start, the video is early, or something is missing:
        // no lead.
        assert_eq!(head_lead(Some(0.0), Some(0.0)), 0.0);
        assert_eq!(head_lead(Some(0.0), Some(0.5)), 0.0);
        assert_eq!(head_lead(None, Some(0.0)), 0.0);
        assert_eq!(head_lead(Some(1.0), None), 0.0);
        assert!(needs_head_pad(0.08) && needs_head_pad(5.0));
        assert!(!needs_head_pad(0.0) && !needs_head_pad(HEAD_PAD_MIN) && !needs_head_pad(0.0004));
    }

    #[test]
    fn a_files_identity_follows_its_path_size_and_modified_time() {
        let dir = std::env::temp_dir().join(format!("kerf-identity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.bin"), dir.join("b.bin"));
        std::fs::write(&a, b"one").unwrap();
        std::fs::write(&b, b"one").unwrap();
        let first = source_identity(&a);
        assert_eq!(first, source_identity(&a), "stable while the file is untouched");
        assert_eq!(first, fnv1a(&source_key(&a)));
        assert_ne!(first, source_identity(&b), "the path is part of it");
        // A replaced file (another size) is another identity, even when written at once.
        std::fs::write(&a, b"three").unwrap();
        assert_ne!(first, source_identity(&a));
        // A path that is not there is still a stable key, and it changes when the file appears.
        let missing = dir.join("missing.bin");
        let before = source_identity(&missing);
        assert_eq!(before, source_identity(&missing));
        std::fs::write(&missing, b"x").unwrap();
        assert_ne!(before, source_identity(&missing));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The learned fallback is one-way and is what `decode_hwaccel()` reads, so a decoder
    /// outside the engine that calls it silences the preview path's attempts too. (Leaves the
    /// flag cleared: nothing else in the suite may depend on it being set — a run under
    /// `KERF_HWACCEL=none` already decodes in software with the flag never consulted.)
    #[test]
    fn disabling_decode_hwaccel_is_shared_with_the_preview_path() {
        disable_decode_hwaccel();
        assert_eq!(decode_hwaccel(), None);
        disable_decode_hwaccel();
        assert_eq!(decode_hwaccel(), None);
    }

    #[test]
    fn source_traits_reads_hdr_and_lead_from_one_probe() {
        let json = r#"{"programs":[],"streams":[{"color_transfer":"arib-std-b67","start_time":"0.080013"}],
                       "format":{"start_time":"0.000000"}}"#;
        let t = parse_source_traits(json);
        assert_eq!(t.hdr, Some(Hdr::Hlg));
        assert!((t.lead - 0.080013).abs() < 1e-9);
        let pq = parse_source_traits(r#"{"streams":[{"color_transfer":"smpte2084"}],"format":{"start_time":"0.0"}}"#);
        assert_eq!((pq.hdr, pq.lead), (Some(Hdr::Pq), 0.0));
        // `N/A` is left out by ffprobe's JSON writer; an unknown transfer is SDR.
        let plain = parse_source_traits(r#"{"streams":[{"color_transfer":"bt709"}],"format":{"start_time":"0.000000"}}"#);
        assert_eq!(plain, SourceTraits::default());
        assert_eq!(parse_source_traits(r#"{"streams":[],"format":{}}"#), SourceTraits::default());
        assert_eq!(parse_source_traits("not json"), SourceTraits::default());
        // A transport stream (a camcorder's .mts / .m2ts) is reported as having no
        // lead whatever its streams' starts say: a padded proxy cannot fix it, so it
        // keeps the plain proxy and its key.
        let ts = r#"{"streams":[{"start_time":"1.521333"}],"format":{"format_name":"mpegts","start_time":"1.400000"}}"#;
        assert_eq!(parse_source_traits(ts).lead, 0.0);
        let mp4 =
            r#"{"streams":[{"start_time":"1.521333"}],"format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","start_time":"1.4"}}"#;
        assert!((parse_source_traits(mp4).lead - 0.121333).abs() < 1e-9);
    }

    /// A hung `ffprobe` is killed at the timeout and a failing one is no answer, where
    /// `Command::output()` waited for as long as the process cared to run.
    #[cfg(unix)]
    #[test]
    fn probing_a_source_is_bounded() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kerf-source-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = |name: &str, body: &str| {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.to_string_lossy().into_owned()
        };
        let file = Path::new("/media/a.mp4");
        let limit = std::time::Duration::from_millis(400);
        let started = Instant::now();
        assert_eq!(probe_source_traits(&script("hang", "exec sleep 30"), file, limit), None);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(probe_source_traits(&script("fail", "exit 1"), file, limit), None);
        assert_eq!(probe_source_traits("/no/such/ffprobe", file, limit), None);
        let json = r#"{"streams":[{"color_transfer":"smpte2084"}],"format":{"format_name":"matroska,webm"}}"#;
        let ok = probe_source_traits(&script("ok", &format!("echo '{json}'")), file, limit).expect("an answer");
        assert_eq!((ok.hdr, ok.indexed), (Some(Hdr::Pq), true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_mp4_and_matroska_are_indexed_containers() {
        let indexed =
            |name: &str| parse_source_traits(&format!(r#"{{"streams":[{{}}],"format":{{"format_name":"{name}"}}}}"#)).indexed;
        assert!(indexed("mov,mp4,m4a,3gp,3g2,mj2"));
        assert!(indexed("matroska,webm"));
        for other in ["mpegts", "avi", "mpeg", "mpegvideo", "h264", "flv", "asf", "mxf", "image2"] {
            assert!(!indexed(other), "{other}");
        }
        // No format named (a failed or odd probe) is not indexed.
        assert!(!parse_source_traits(r#"{"streams":[{}],"format":{}}"#).indexed);
        assert!(!parse_source_traits("not json").indexed);
    }

    #[test]
    fn a_proxy_is_rebuilt_only_for_the_sources_that_were_made_wrong() {
        let src = "/media/a.mov|1234|99";
        // The key of an ordinary SDR file with a normal start is what it has
        // always been, so its cached proxy is still found.
        assert_eq!(proxy_key(src, 1280, false, false), "/media/a.mov|1234|99|1280");
        // The `|sdr` precedent, and the new suffix stacked after it.
        assert_eq!(proxy_key(src, 1280, true, false), "/media/a.mov|1234|99|1280|sdr");
        assert_eq!(proxy_key(src, 1280, false, true), "/media/a.mov|1234|99|1280|lead");
        assert_eq!(proxy_key(src, 3072, true, true), "/media/a.mov|1234|99|3072|sdr|lead");
        let keys = [
            proxy_key(src, 1280, false, false),
            proxy_key(src, 1280, true, false),
            proxy_key(src, 1280, false, true),
            proxy_key(src, 1280, true, true),
        ];
        let hashes: std::collections::HashSet<u64> = keys.iter().map(|k| fnv1a(k)).collect();
        assert_eq!(hashes.len(), keys.len(), "every variant gets its own file");
    }

    #[test]
    fn a_padded_proxy_is_told_by_its_name_and_nothing_else_is() {
        assert_eq!(proxy_file_name(0xabc, false), "0000000000000abc.mp4");
        assert_eq!(proxy_file_name(0xabc, true), "0000000000000abc.lead.mp4");
        let padded = is_head_padded_proxy;
        assert!(padded("/home/u/.cache/kerf/proxies/0000000000000abc.lead.mp4"));
        #[cfg(windows)]
        assert!(padded(r"C:\Users\u\AppData\Local\kerf\proxies\0123456789ABCDEF.lead.mp4"));
        // The plain proxy of the same source, an original, a lookalike elsewhere and a
        // name that is not a proxy's hash are none of them padded.
        assert!(!padded("/home/u/.cache/kerf/proxies/0000000000000abc.mp4"));
        assert!(!padded("/media/clip.mp4"));
        assert!(!padded("/media/0000000000000abc.lead.mp4"));
        assert!(!padded("/home/u/.cache/kerf/proxies/clip.lead.mp4"));
        assert!(!padded("/home/u/.cache/kerf/proxies/0000000000000abcd.lead.mp4"));
        assert!(!padded("/home/u/.cache/other/proxies/0000000000000abc.lead.mp4"));
        // And the names `generate_proxy` writes, in the directory it writes them to,
        // are the ones that read back.
        if let Some(dir) = dirs::cache_dir().map(|d| d.join("kerf").join("proxies")) {
            assert!(!is_head_padded_proxy(&dir.join(proxy_file_name(7, false)).to_string_lossy()));
            assert!(is_head_padded_proxy(&dir.join(proxy_file_name(7, true)).to_string_lossy()));
        }
    }

    /// A proxy's sidecar is its video stream, named after the proxy, and describes *that*
    /// file: another size, a replaced file, another version or garbage reads as none.
    #[test]
    fn a_proxy_sidecar_describes_the_file_beside_it_and_nothing_else() {
        let dir = Scratch::new("sidecar");
        let video = StreamInfo {
            width: Some(1280),
            height: Some(720),
            pix_fmt: Some("yuv420p".into()),
            ..crate::engine::test_support::video_stream(1280, 720, 30.0)
        };
        // Named after the proxy, whatever its ending, and apart from its own temp file.
        let plain = dir.join("0000000000000abc.mp4");
        let padded = dir.join("0000000000000abc.lead.mp4");
        assert_eq!(proxy_sidecar_path(&plain), dir.join("0000000000000abc.json"));
        assert_eq!(proxy_sidecar_path(&padded), dir.join("0000000000000abc.lead.json"));
        for proxy in [&plain, &padded] {
            std::fs::write(proxy, b"not really a video").unwrap();
            assert_eq!(read_proxy_sidecar(proxy), None, "no sidecar yet");
            write_proxy_sidecar(proxy, proxy, video.clone());
            assert_eq!(read_proxy_sidecar(proxy), Some(video.clone()));
            // The file it describes was replaced by another one: it no longer applies.
            std::fs::write(proxy, b"a different, longer encode").unwrap();
            assert_eq!(read_proxy_sidecar(proxy), None);
            // A sidecar from another version of the format, and a half-written one.
            write_proxy_sidecar(proxy, proxy, video.clone());
            let path = proxy_sidecar_path(proxy);
            let current = std::fs::read_to_string(&path).unwrap();
            std::fs::write(&path, current.replace("\"version\":1", "\"version\":99")).unwrap();
            assert_eq!(read_proxy_sidecar(proxy), None);
            std::fs::write(&path, &current[..current.len() / 2]).unwrap();
            assert_eq!(read_proxy_sidecar(proxy), None);
        }
        // Writing leaves nothing behind, and never touches the proxy's own temp name.
        let temps: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".part"))
            .collect();
        assert!(temps.is_empty(), "{temps:?}");
    }

    /// A finished encode lands with its sidecar — written before the proxy is visible — unless a
    /// concurrent generator got there first, in which case theirs stays, untouched, and no
    /// sidecar is written over it. Two writers never share a temp name.
    #[test]
    fn a_finished_proxy_lands_with_its_sidecar_unless_another_generator_got_there_first() {
        use std::cell::Cell;
        let dir = Scratch::new("finalize");
        let video = |w| StreamInfo {
            width: Some(w),
            height: Some(w / 2),
            pix_fmt: Some("yuv420p".into()),
            ..crate::engine::test_support::video_stream(w, w / 2, 30.0)
        };
        let (tmp, dst) = (dir.join("0000000000000abc.1.part"), dir.join("0000000000000abc.mp4"));
        std::fs::write(&tmp, b"ours").unwrap();
        assert_eq!(finalize_proxy(&tmp, &dst, || Some(video(1280))).unwrap(), dst);
        assert!(!tmp.exists());
        assert_eq!(std::fs::read(&dst).unwrap(), b"ours");
        assert_eq!(read_proxy_sidecar(&dst), Some(video(1280)));

        // Another generator's proxy is in place with its sidecar: a second encode is dropped, the
        // proxy and its sidecar stay as they are, and nobody probes.
        let before = std::fs::read(proxy_sidecar_path(&dst)).unwrap();
        std::fs::write(&tmp, b"a second, different encode").unwrap();
        let probed = Cell::new(false);
        finalize_proxy(&tmp, &dst, || {
            probed.set(true);
            Some(video(640))
        })
        .unwrap();
        assert!(!probed.get() && !tmp.exists());
        assert_eq!(std::fs::read(&dst).unwrap(), b"ours");
        assert_eq!(std::fs::read(proxy_sidecar_path(&dst)).unwrap(), before);

        // A probe that fails still lands the proxy; it only has no sidecar yet.
        let other = dir.join("0000000000000def.mp4");
        std::fs::write(&tmp, b"third").unwrap();
        finalize_proxy(&tmp, &other, || None).unwrap();
        assert!(other.is_file() && !proxy_sidecar_path(&other).exists());

        // The temp names of two writers of one sidecar differ, and neither is the proxy's own.
        let sidecar = proxy_sidecar_path(&dst);
        let (a, b) = (sidecar_temp(&sidecar), sidecar_temp(&sidecar));
        assert_ne!(a, b);
        let proxy_own = dst.with_extension(format!("{}.part", std::process::id()));
        assert!(a != proxy_own && b != proxy_own && a.to_string_lossy().ends_with(".part"));
    }

    /// A clip read from the very start of a padded proxy (no `-ss`) opens with the
    /// pad's clone of the first frame, which the original it stands for has no
    /// frame for: the chain drops it before the trim. A read that is seeked skips
    /// the clone already, and anything that is not a padded proxy is untouched.
    #[test]
    fn a_head_clip_of_a_padded_proxy_drops_the_pads_clone_and_nothing_else_does() {
        let proxy = "/home/u/.cache/kerf/proxies/0123456789abcdef.lead.mp4";
        let original = Asset {
            path: "/media/clip.mp4".into(),
            ..av_asset(Uuid::new_v4(), 30.0)
        };
        let padded = Asset {
            path: proxy.into(),
            ..original.clone()
        };
        let plain_proxy = Asset {
            path: "/home/u/.cache/kerf/proxies/0123456789abcdef.mp4".into(),
            ..original.clone()
        };
        let graph = |asset: &Asset, source_in: f64| {
            let timeline = single(vec![make_clip(asset.id, source_in, source_in + 4.0, 0.0)]);
            let args = build_preview_args(&timeline, std::slice::from_ref(asset), 0.0, 30.0, 960, 6).unwrap();
            flag_val(&args, "-filter_complex").unwrap().to_string()
        };
        // From the head: the clone goes, ahead of the trim that rebases the clip.
        let head = graph(&padded, 0.0);
        assert!(head.contains("trim=start_frame=1,trim=start=0:end=4,"), "{head}");
        // `clip_seek` treats a millisecond as the head too; just past it is seeked.
        assert!(graph(&padded, 0.0005).contains("trim=start_frame=1,"));
        assert!(!graph(&padded, 0.002).contains("start_frame"), "a seek skips the clone");
        assert!(!graph(&padded, 1.5).contains("start_frame"));
        // The same clip on the original, or on a proxy that was not padded: no filter,
        // and the graph is the one it always was.
        for asset in [&original, &plain_proxy] {
            assert!(!graph(asset, 0.0).contains("start_frame"), "{asset:?}");
        }
        assert_eq!(graph(&original, 0.0), graph(&plain_proxy, 0.0));
        // An export reads the original, so it never has one either way.
        let timeline = single(vec![make_clip(original.id, 0.0, 4.0, 0.0)]);
        let export = build_export_args(&timeline, &[original], "out.mp4", &ExportOptions::default()).unwrap();
        assert!(!export.join(" ").contains("start_frame"));
    }

    #[test]
    fn the_frame_sync_flag_is_spelled_the_way_this_ffmpeg_takes_it() {
        // (knows -fps_mode, knows -vsync): FFmpeg 6 knows both; 9 removed -vsync;
        // 4 predates -fps_mode.
        assert_eq!(fps_mode_flag_for(true, true), "-fps_mode");
        assert_eq!(fps_mode_flag_for(true, false), "-fps_mode");
        assert_eq!(fps_mode_flag_for(false, true), "-vsync");
        // A binary that would not run: the modern spelling, like every probe here.
        assert_eq!(fps_mode_flag_for(false, false), "-fps_mode");
    }

    #[test]
    fn stitch_args_with_hw_encoder_accelerate_both_lens_decodes() {
        let args = build_stitch_args(
            "/dcim/f_00_.mp4",
            "/dcim/r_10_.mp4",
            "/cache/out.mp4",
            "hevc_nvenc",
            Some("auto"),
        );
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
    fn cover_frame_renders_to_a_file_at_full_delivery_size() {
        let asset = test_asset(vec![video_stream(3840, 2160, 30.0)]);
        let assets = vec![asset.clone()];
        let mut timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
        timeline.format = Some(crate::model::Delivery {
            width: 1080,
            height: 1920,
            fit: Fit::Cover,
        });
        let out = StillOutput::File {
            path: "/covers/cover.jpg".to_string(),
            format: ImageFormat::Jpeg,
            quality: 2,
        };
        let args = build_still_args(&timeline, &assets, &ExportOptions::default(), 1.0, u32::MAX, None, &out).unwrap();
        // The uncapped width resolves to the project's delivery frame, not to a
        // preview size — a cover is a delivered image.
        let graph = args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone();
        assert!(graph.contains("s=1080x1920"), "canvas is the delivery frame: {graph}");
        // Written as a real file, overwriting, with the muxer stated.
        assert!(args.windows(2).any(|w| w[0] == "-f" && w[1] == "image2"));
        assert!(args.windows(2).any(|w| w[0] == "-vcodec" && w[1] == "mjpeg"));
        assert!(args.windows(2).any(|w| w[0] == "-q:v" && w[1] == "2"));
        assert!(args.contains(&"-y".to_string()));
        assert!(!args.contains(&"pipe:1".to_string()));
        assert_eq!(args.last().unwrap(), "/covers/cover.jpg");
        // Exactly one frame, whichever sink it goes to.
        assert!(args.windows(2).any(|w| w[0] == "-frames:v" && w[1] == "1"));
    }

    #[test]
    fn a_png_cover_is_lossless_and_carries_no_jpeg_quality() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let assets = vec![asset.clone()];
        let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
        let out = StillOutput::File {
            path: "/covers/cover.png".to_string(),
            format: ImageFormat::Png,
            quality: 2,
        };
        let args = build_still_args(&timeline, &assets, &ExportOptions::default(), 1.0, u32::MAX, None, &out).unwrap();
        assert!(args.windows(2).any(|w| w[0] == "-vcodec" && w[1] == "png"));
        assert!(!args.contains(&"-q:v".to_string()), "-q:v is meaningless for png: {args:?}");
    }

    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_cover_frame_is_really_written_at_the_delivery_size() {
        // The arg builder has unit tests, but nothing above ever ran the binary
        // — and a still sink that never produced a file would look identical.
        let dir = std::env::temp_dir().join(format!("kerf-cover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let media = dir.join("src.mp4");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc=size=1920x1080:rate=30:duration=2"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&media)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());

        let mut asset = av_asset(Uuid::new_v4(), 2.0);
        asset.path = media.to_string_lossy().into_owned();
        asset.streams = vec![video_stream(1920, 1080, 30.0)];
        let mut timeline = single(vec![make_clip(asset.id, 0.0, 2.0, 0.0)]);
        timeline.format = Some(crate::model::Delivery {
            width: 1080,
            height: 1350,
            fit: Fit::Cover,
        });

        for (name, format) in [("cover.jpg", ImageFormat::Jpeg), ("cover.png", ImageFormat::Png)] {
            let out = dir.join(name);
            export_still(&timeline, &[asset.clone()], &ExportOptions::default(), 1.0, &out, format, 2).expect("cover");
            let size = std::fs::metadata(&out).expect("cover written").len();
            assert!(size > 1024, "{name} should be a real image, got {size} bytes");
            // And it is the delivery frame, not the source shape.
            let probe = probe(&out).expect("probe the cover");
            let stream = probe.streams.first().expect("a video stream");
            assert_eq!((stream.width, stream.height), (Some(1080), Some(1350)), "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cover_format_follows_the_file_extension() {
        assert_eq!(ImageFormat::from_path(Path::new("/a/cover.png")), ImageFormat::Png);
        assert_eq!(ImageFormat::from_path(Path::new("/a/cover.PNG")), ImageFormat::Png);
        assert_eq!(ImageFormat::from_path(Path::new("/a/cover.jpg")), ImageFormat::Jpeg);
        // Anything else falls back to the format every platform accepts.
        assert_eq!(ImageFormat::from_path(Path::new("/a/cover")), ImageFormat::Jpeg);
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
        assert!(build_proxy_args(
            "/in.mp4",
            "/out.mp4",
            1,
            PROXY_MAX_WIDTH_SPHERICAL,
            "libx264",
            None,
            None,
            None
        )
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

    #[test]
    fn a_smaller_proxy_size_scales_a_360_proxy_by_the_same_ratio_and_the_default_is_unchanged() {
        // The default is what Kerf always made, so its cache stays valid.
        assert_eq!(proxy_width_for(None, 1280), 1280);
        assert_eq!(proxy_width_for(Some(Projection::Equirect), 1280), PROXY_MAX_WIDTH_SPHERICAL);
        assert_eq!(proxy_width_for(Some(Projection::Flat), 720), 720);
        // 3072 / 1280 = 2.4: a 720 flat proxy is a 1728 equirect one, 1080 is 2592.
        assert_eq!(proxy_width_for(Some(Projection::Equirect), 720), 1728);
        assert_eq!(proxy_width_for(Some(Projection::DualFisheye), 1080), 2592);
        // …and never past what a hardware H.264 encoder takes.
        assert_eq!(proxy_width_for(Some(Projection::Equirect), 4000), PROXY_MAX_WIDTH_SPHERICAL);
        // Each size is its own file: the width is in the key.
        if let (Some(a), Some(b)) = (
            proxy_path(Path::new("/media/a.mov"), 720),
            proxy_path(Path::new("/media/a.mov"), 1280),
        ) {
            assert_ne!(a, b);
        }
    }

    /// The streamed encode reports a rising fraction under 1 and can be abandoned: a cancel
    /// kills the encode, leaves no `.part` file and returns `Cancelled`.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored proxy_encode_reports`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn proxy_encode_reports_progress_and_can_be_cancelled() {
        let dir = Scratch::new("proxy-progress");
        let media = dir.join("clip.mp4");
        run_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=1920x1080:r=30:d=8",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            media.to_str().unwrap(),
        ]);
        let mut seen: Vec<f64> = Vec::new();
        let proxy = generate_proxy_with(
            &media,
            PROXY_MAX_WIDTH,
            ProxyRun {
                reservation: Some(cpu::reserve()),
                duration: Some(8.0),
                progress: &mut |f| seen.push(f),
                cancel: &|| false,
            },
        )
        .expect("proxy");
        let _cleanup = ProxyGuard(proxy.clone());
        assert!(proxy.is_file());
        assert!(!seen.is_empty(), "no progress was reported");
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "progress went backwards: {seen:?}");
        assert!(
            seen.iter().all(|f| (0.0..1.0).contains(f)),
            "a build is not done until it is renamed: {seen:?}"
        );

        // A second source, abandoned the moment it starts.
        let media2 = dir.join("clip2.mp4");
        run_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=1920x1080:r=30:d=8",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            media2.to_str().unwrap(),
        ]);
        let cancelled = generate_proxy_with(
            &media2,
            PROXY_MAX_WIDTH,
            ProxyRun {
                reservation: None,
                duration: Some(8.0),
                progress: &mut |_| {},
                cancel: &|| true,
            },
        );
        assert!(matches!(cancelled, Err(Error::Cancelled)), "{cancelled:?}");
        let dst = proxy_path(&media2, PROXY_MAX_WIDTH).expect("path");
        assert!(!dst.exists(), "a cancelled build left a proxy behind");
        let strays: Vec<_> = std::fs::read_dir(dst.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name().to_string_lossy().ends_with(".part")
                    && e.file_name().to_string_lossy().starts_with(&format!(
                        "{:016x}",
                        fnv1a(&proxy_key(&source_key(&media2), PROXY_MAX_WIDTH, false, false))
                    ))
            })
            .collect();
        assert!(strays.is_empty(), "a cancelled build left a temp file: {strays:?}");
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
            master: Default::default(),
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
        let chain = audio_clip_chain(&clip, &fmt, &ClipFx::default(), "stereo", unity_mix());
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

    /// The deepest `if(` nesting of an expression, and its deepest nesting of any kind of
    /// bracket: what libavutil counts (a level per `(`, a function call's included).
    fn nesting(expr: &str) -> (usize, usize) {
        let (mut brackets, mut ifs, mut open_ifs) = (0usize, 0usize, Vec::new());
        let (mut deepest_brackets, mut deepest_ifs) = (0, 0);
        let bytes = expr.as_bytes();
        for (i, &c) in bytes.iter().enumerate() {
            match c {
                b'(' => {
                    brackets += 1;
                    let is_if = i >= 2 && &bytes[i - 2..i] == b"if" && (i == 2 || !bytes[i - 3].is_ascii_alphanumeric());
                    open_ifs.push(is_if);
                    if is_if {
                        ifs += 1;
                    }
                }
                b')' => {
                    brackets -= 1;
                    if open_ifs.pop() == Some(true) {
                        ifs -= 1;
                    }
                }
                _ => {}
            }
            deepest_brackets = deepest_brackets.max(brackets);
            deepest_ifs = deepest_ifs.max(ifs);
        }
        (deepest_ifs, deepest_brackets)
    }

    /// `n` points on a line with time = value = index.
    fn ramp(n: usize) -> Vec<(f64, f64)> {
        (0..n).map(|i| (i as f64, i as f64)).collect()
    }

    #[test]
    fn a_short_keyframe_expression_is_the_chain_it_always_was() {
        assert_eq!(
            keyframe_expr(&[(0.0, 0.0), (1.0, 10.0), (3.0, -2.0)], "t", 1.5),
            "if(lt((t-1.5),0),0,if(lt((t-1.5),1),(0+(10)*((t-1.5)-0)/(1)),\
             if(lt((t-1.5),3),(10+(-12)*((t-1.5)-1)/(2)),-2)))"
        );
        // Right up to the threshold every point is a level; one past it, the tree.
        assert_eq!(
            nesting(&keyframe_expr(&ramp(KEYFRAME_TREE_POINTS), "t", 0.0)).0,
            KEYFRAME_TREE_POINTS
        );
        assert!(nesting(&keyframe_expr(&ramp(KEYFRAME_TREE_POINTS + 1), "t", 0.0)).0 < 8);
    }

    #[test]
    fn a_long_keyframe_expression_is_a_balanced_tree_over_its_segments() {
        // Five points, four segments: halve twice. A step (equal times) is a leaf like any other.
        let pts = ramp(5);
        assert_eq!(
            keyframe_tree(&pts, 0, 4, "L"),
            "if(lt(L,2),if(lt(L,1),(0+(1)*(L-0)/(1)),(1+(1)*(L-1)/(1))),\
             if(lt(L,3),(2+(1)*(L-2)/(1)),(3+(1)*(L-3)/(1))))"
        );
        let step = [(0.0, 1.0), (2.0, 1.0), (2.0, 5.0), (4.0, 7.0)];
        assert_eq!(
            keyframe_tree(&step, 0, 3, "L"),
            "if(lt(L,2),(1+(0)*(L-0)/(2)),if(lt(L,2),1,(5+(2)*(L-2)/(2))))"
        );
    }

    #[test]
    fn many_eased_keys_stay_far_inside_the_expression_nesting_limit() {
        // libavutil refuses an expression nested past 100 levels. 40 keys eased into each other
        // are 12 pieces a segment: the chain was a level per piece (ten eased keys already failed
        // the export and the playback stream with `Invalid argument`).
        let mut clip = make_clip(Uuid::nil(), 0.0, 40.0, 0.0);
        use crate::model::Easing;
        let easings = [Easing::EaseInOut, Easing::Hold, Easing::EaseOut, Easing::Linear];
        clip.keyframes = (0..40)
            .map(|i| crate::model::Keyframe {
                time: f64::from(i),
                scale: 1.0 + f64::from(i % 3) * 0.1,
                pos_x: f64::from(i % 5) * 0.05,
                pos_y: 0.0,
                rotation: f64::from(i % 7),
                opacity: 1.0 - f64::from(i % 4) * 0.1,
                easing: easings[i as usize % easings.len()],
            })
            .collect();
        for channel in [
            clip.keyframe_channel(|k| k.scale),
            clip.keyframe_channel(|k| k.pos_x),
            clip.keyframe_channel(|k| k.rotation),
            clip.keyframe_channel(|k| k.opacity),
        ] {
            assert!(channel.len() > 200, "{} points", channel.len());
            let (ifs, brackets) = nesting(&keyframe_expr(&channel, "t", 0.0));
            assert!(
                ifs <= 12 && brackets <= 20,
                "{} points nest {ifs} ifs / {brackets} brackets",
                channel.len()
            );
        }
        // ... and inside the filters that wrap it, which add a few levels of their own.
        let graph = video_clip_chain(&clip, &ExportFormat::default(), &ClipFx::default(), false, "c0");
        assert!(nesting(&graph).1 < 40, "{}", nesting(&graph).1);
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
                easing: Default::default(),
            },
            crate::model::Keyframe {
                time: 4.0,
                scale: 1.5,
                pos_x: 0.3,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
                easing: Default::default(),
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
            g.filter.contains("overlay=x='(W-w)/2+(if(lt((t-2)"),
            "animated overlay x: {}",
            g.filter
        );
    }

    /// A key at clip-local `time` carrying the given channels.
    fn key(time: f64, scale: f64, pos_x: f64, rotation: f64, opacity: f64) -> crate::model::Keyframe {
        crate::model::Keyframe {
            time,
            scale,
            pos_x,
            pos_y: 0.0,
            rotation,
            opacity,
            easing: Default::default(),
        }
    }

    /// Everything after the zoom's `scale ... eval=frame` in a chain: the scaler
    /// flags are part of the `scale` itself, so what is left starts at the next filter.
    fn after_zoom(chain: &str) -> &str {
        let at = chain.find("eval=frame").expect("a keyed zoom has an eval=frame scale");
        chain[at..].split_once(',').map_or("", |(_, rest)| rest)
    }

    /// The structural half of the keyed-zoom fix, over every feature a clip's chain can
    /// carry: the zoom is the **last** thing in the chain, after `fps`, and ends in the
    /// format `overlay` takes, because every filter after a size-changing `scale` is
    /// pinned to the first frame's size (the picture stops growing, the opacity ramp or
    /// the rotation runs at the first frame's geometry). The rendered tests
    /// (`cli/keyed_zoom.rs`) are what show ffmpeg agrees; this is what keeps a later
    /// edit of the chain from putting a filter back behind the zoom.
    #[test]
    fn a_keyed_zoom_is_the_last_stage_of_its_chain() {
        use crate::model::{Mask, MaskShape, VideoEffect};
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let base = || {
            let mut clip = make_clip(asset.id, 0.0, 8.0, 2.0);
            clip.keyframes = vec![key(0.0, 0.4, 0.0, 0.0, 1.0), key(3.0, 1.6, 0.0, 0.0, 1.0)];
            clip
        };
        let mask = Mask {
            shape: MaskShape::Ellipse,
            ..Mask::default()
        };
        let cases: Vec<(&str, Clip, ExportFormat, ClipFx, bool)> = {
            let plain = ExportFormat::default();
            let mut out = vec![("scale only", base(), plain.clone(), ClipFx::default(), false)];
            let mut c = base();
            c.keyframes = vec![key(0.0, 0.4, -0.2, 0.0, 1.0), key(3.0, 1.6, 0.2, 0.0, 1.0)];
            out.push(("scale + position", c, plain.clone(), ClipFx::default(), false));
            let mut c = base();
            c.keyframes = vec![key(0.0, 0.4, 0.0, 0.0, 1.0), key(3.0, 1.6, 0.0, 0.0, 0.5)];
            out.push(("scale + opacity", c, plain.clone(), ClipFx::default(), false));
            let mut c = base();
            c.keyframes = vec![key(0.0, 0.4, 0.0, 0.0, 1.0), key(3.0, 1.6, 0.0, 40.0, 1.0)];
            out.push(("scale + rotation", c, plain.clone(), ClipFx::default(), false));
            let mut c = base();
            c.keyframes = vec![key(0.0, 0.4, -0.2, 0.0, 1.0), key(3.0, 1.6, 0.2, 40.0, 0.5)];
            out.push(("everything keyed", c, plain.clone(), ClipFx::default(), false));
            let mut c = base();
            c.transform.crop_left = 0.1;
            c.transform.crop_top = 0.2;
            out.push(("crop", c, plain.clone(), ClipFx::default(), false));
            let mut c = base();
            c.mask = Some(mask);
            out.push(("mask", c, plain.clone(), ClipFx::default(), false));
            let mut c = base();
            c.mask = Some(mask);
            c.keyframes = vec![key(0.0, 0.4, 0.0, 0.0, 1.0), key(3.0, 1.6, 0.0, 0.0, 0.5)];
            out.push(("mask + opacity", c, plain.clone(), ClipFx::default(), false));
            let mut c = base();
            c.effects = vec![
                VideoEffect::Blur { sigma: 3.0 },
                VideoEffect::Vignette,
                VideoEffect::ChromaKey {
                    color: "green".into(),
                    similarity: 0.3,
                    blend: 0.1,
                },
            ];
            out.push(("effects + chroma key", c, plain.clone(), ClipFx::default(), false));
            let mut c = base();
            c.color.brightness = 0.1;
            c.color.temperature = 0.3;
            out.push(("colour", c, plain.clone(), ClipFx::default(), false));
            let mut c = base();
            c.speed = 2.0;
            out.push(("speed", c, plain.clone(), ClipFx::default(), false));
            let mut c = base();
            c.speed = -0.5;
            out.push(("reverse", c, plain.clone(), ClipFx::default(), false));
            let mut c = base();
            c.fade_in = 0.5;
            c.fade_out = 0.5;
            out.push(("fades", c, plain.clone(), ClipFx::default(), false));
            let fx = ClipFx {
                xfade_in: 0.5,
                tail: 0.5,
                black_out: 0.3,
                ..ClipFx::default()
            };
            out.push(("dissolve + dip", base(), plain.clone(), fx, false));
            let hdr = ClipFx {
                hdr: Some(crate::model::Hdr::Hlg),
                ..ClipFx::default()
            };
            out.push(("hdr", base(), plain.clone(), hdr, false));
            let cover = ExportFormat {
                fit: Fit::Cover,
                scaler: Some("lanczos".into()),
                ..ExportFormat::default()
            };
            out.push(("cover + scaler flag", base(), cover, ClipFx::default(), false));
            out.push(("still image", base(), plain.clone(), ClipFx::default(), true));
            let mut c = base();
            c.reframe = Some(crate::model::Reframe::new(crate::model::Projection::Equirect));
            out.push(("360 reframe", c, plain, ClipFx::default(), false));
            out
        };
        for (name, clip, fmt, fx, image) in cases {
            assert!(clip.zoom_animated(), "{name}: the case has to zoom");
            let chain = video_clip_chain(&clip, &fmt, &fx, image, "c0");
            assert_eq!(chain.matches("eval=frame").count(), 1, "{name}: one zoom: {chain}");
            // Nothing follows the zoom but the format `overlay` takes natively.
            assert_eq!(after_zoom(&chain), "format=yuva420p", "{name}: {chain}");
            // Every filter that reads the frame's size or its time is ahead of it, at
            // the constant size, and `fps` is ahead of all of them so they and the zoom
            // run on *output* frames.
            let zoom = chain.find("eval=frame").unwrap();
            let fps = chain.find("fps=").unwrap_or_else(|| panic!("{name}: no fps: {chain}"));
            assert!(fps < zoom, "{name}: the zoom is evaluated after fps: {chain}");
            for needle in [
                "geq=",
                "rotate=",
                "eq=",
                "gblur=",
                "vignette",
                "chromakey=",
                "fade=",
                "zscale",
                "colorspace=",
            ] {
                if let Some(at) = chain.find(needle) {
                    assert!(at < zoom, "{name}: `{needle}` runs behind the zoom: {chain}");
                    // ...and after the output rate is set (the 360 path hoists `fps`
                    // above `v360`, which is earlier still).
                    assert!(fps < at, "{name}: `{needle}` before fps: {chain}");
                }
            }
            // The old terminal pixel format is what pinned the size.
            assert!(!chain.ends_with("format=yuv420p"), "{name}: {chain}");
        }
    }

    /// `rotate`'s `fillcolor=none` means "do not fill": the corners keep whatever the
    /// buffer last held, which a rotation that moves turns into every earlier pose. A
    /// keyed rotation fills with transparent black; a constant one, whose footprint
    /// never changes, keeps the chain it always had.
    #[test]
    fn a_keyed_rotation_fills_transparent_and_a_constant_one_keeps_its_chain() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let fmt = ExportFormat::default();
        let mut keyed = make_clip(asset.id, 0.0, 8.0, 2.0);
        keyed.keyframes = vec![key(0.0, 1.0, 0.0, 0.0, 1.0), key(3.0, 1.0, 0.0, 40.0, 1.0)];
        assert!(!keyed.zoom_animated(), "the rotation alone moves");
        let chain = video_clip_chain(&keyed, &fmt, &ClipFx::default(), false, "c0");
        assert!(chain.contains(":fillcolor=black@0:ow='hypot(iw,ih)'"), "{chain}");
        assert!(!chain.contains("fillcolor=none"), "{chain}");
        let mut zoomed = keyed;
        zoomed.keyframes = vec![key(0.0, 0.5, 0.0, 0.0, 1.0), key(3.0, 1.5, 0.0, 40.0, 1.0)];
        let chain = video_clip_chain(&zoomed, &fmt, &ClipFx::default(), false, "c0");
        assert!(chain.contains(":fillcolor=black@0:ow='hypot(iw,ih)'"), "{chain}");
        // A constant rotation is untouched.
        let mut fixed = make_clip(asset.id, 0.0, 8.0, 2.0);
        fixed.transform.rotation = 30.0;
        let chain = video_clip_chain(&fixed, &fmt, &ClipFx::default(), false, "c0");
        assert!(chain.contains(":fillcolor=none:ow=rotw("), "{chain}");
    }

    /// A keyed clip whose scale holds still never changes the picture's size, so it
    /// keeps the chain it always had — and so does every unkeyed clip.
    #[test]
    fn a_keyed_clip_whose_scale_holds_keeps_the_chain_it_always_had() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let mut clip = make_clip(asset.id, 0.0, 8.0, 2.0);
        clip.keyframes = vec![key(0.0, 0.5, -0.2, 0.0, 1.0), key(3.0, 0.5, 0.2, 0.0, 1.0)];
        assert!(clip.is_animated() && !clip.zoom_animated());
        let chain = video_clip_chain(&clip, &ExportFormat::default(), &ClipFx::default(), false, "c0");
        // The (constant-valued) per-frame scale stays where it was: before `setsar`
        // and `fps`, with the clip's own pixel format at the end.
        let zoom = chain.find("eval=frame").unwrap();
        assert!(
            zoom < chain.find("setsar=1").unwrap() && zoom < chain.find("fps=").unwrap(),
            "{chain}"
        );
        assert!(chain.ends_with(",format=yuv420p"), "{chain}");
        assert!(!chain.contains("format=yuva420p"), "{chain}");
        // An unkeyed zoom is a plain `scale=iw*..` and gains nothing.
        let mut plain = make_clip(asset.id, 0.0, 8.0, 2.0);
        plain.transform.scale = 0.5;
        let chain = video_clip_chain(&plain, &ExportFormat::default(), &ClipFx::default(), false, "c0");
        assert!(!chain.contains("eval=frame") && chain.ends_with(",format=yuv420p"), "{chain}");
    }

    fn still_canvas(fit: Fit) -> StillCanvas {
        StillCanvas {
            w: 640,
            h: 360,
            fit,
            sf: String::new(),
        }
    }

    /// The still follows the export for a moving zoom: its zoom is the last stage, behind the
    /// grade, the effects, the mask, the opacity and the rotation, which then act on the
    /// picture at its fit size. Every other clip's still zooms first, as it always did.
    #[test]
    fn the_still_of_a_moving_zoom_runs_the_zoom_last_and_every_other_still_zooms_first() {
        use crate::model::{Mask, VideoEffect};
        let tf = Transform {
            scale: 1.4,
            pos_x: 0.1,
            rotation: 20.0,
            opacity: 0.7,
            ..Transform::default()
        };
        let color = Color {
            contrast: 1.2,
            ..Color::default()
        };
        let effects = [VideoEffect::Blur { sigma: 3.0 }];
        let mask = Mask::default();
        let canvas = still_canvas(Fit::Contain);
        let last = still_clip_chain(&tf, &color, &effects, None, &canvas, Some(&mask), true);
        let first = still_clip_chain(&tf, &color, &effects, None, &canvas, Some(&mask), false);
        let zoom = "scale=iw*1.4:ih*1.4";
        // Last: after the rotation, with nothing behind it, and only once.
        assert!(last.ends_with(zoom), "{last}");
        assert_eq!(last.matches(zoom).count(), 1, "{last}");
        for before in ["eq=", "gblur=", "geq=", "colorchannelmixer=", "rotate="] {
            assert!(last.find(before).unwrap() < last.find(zoom).unwrap(), "{before}: {last}");
        }
        // First: ahead of `setsar` and all of them, and the chain otherwise the same.
        assert!(first.find(zoom).unwrap() < first.find("setsar=1").unwrap(), "{first}");
        assert!(!first.ends_with(zoom), "{first}");
        let strip = |c: &str| c.replace(&format!(",{zoom}"), "");
        assert_eq!(strip(&last), strip(&first), "only the zoom's place differs");
        // A moving zoom that has no zoom at the sampled instant has none to move.
        let at_rest = Transform { scale: 1.0, ..tf };
        assert_eq!(
            still_clip_chain(&at_rest, &color, &effects, None, &canvas, None, true),
            still_clip_chain(&at_rest, &color, &effects, None, &canvas, None, false)
        );
    }

    /// Through the whole still graph: the moving-zoom clip's chain ends in the zoom, the same
    /// keys at a constant scale zoom first.
    #[test]
    fn a_still_of_a_moving_zoom_clip_ends_its_chain_in_the_zoom() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let assets = vec![asset.clone()];
        let blurred = |first: f64, last: f64| {
            let mut c = make_clip(asset.id, 0.0, 4.0, -0.5); // the still at 1 s is 1.5 s in
            c.keyframes = vec![key(0.0, first, 0.0, 30.0, 1.0), key(2.0, last, 0.0, 30.0, 1.0)];
            c.effects = vec![crate::model::VideoEffect::Blur { sigma: 4.0 }];
            c
        };
        let chain_of = |clip: Clip| {
            let tl = single(vec![clip]);
            let args = build_timeline_frame_args(&tl, &assets, &ExportOptions::default(), 1.0, 640, 4).unwrap();
            let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
            graph.split(';').find(|c| c.ends_with("[v0]")).unwrap().to_string()
        };
        // 0.5 to 1.5 over two seconds is 1.25 at 1.5 s.
        let moving = chain_of(blurred(0.5, 1.5));
        assert!(moving.ends_with(",scale=iw*1.25:ih*1.25[v0]"), "{moving}");
        assert!(
            moving.find("gblur=").unwrap() < moving.find("scale=iw*1.25").unwrap(),
            "{moving}"
        );
        // Held at 1.25 the whole way: zoom first, as ever.
        let held = chain_of(blurred(1.25, 1.25));
        assert!(
            !held.ends_with("[v0]") || !held.contains(",scale=iw*1.25:ih*1.25[v0]"),
            "{held}"
        );
        assert!(held.find("scale=iw*1.25").unwrap() < held.find("gblur=").unwrap(), "{held}");
    }

    /// `scale` reads a size that evaluates to 0 as "keep the input's", so a picture zoomed
    /// to a fraction of a pixel snapped to full size. Below `TINY_SCALE` the sizes are held
    /// at 1 px, in the export, the playback and the still, for a constant zoom and a keyed
    /// one; above it, no graph has changed by a character.
    #[test]
    fn a_tiny_scale_keeps_a_pixel_instead_of_snapping_to_full_size() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let fmt = ExportFormat::default();
        let chain = |clip: &Clip| video_clip_chain(clip, &fmt, &ClipFx::default(), false, "c0");
        // A constant scale.
        let mut tiny = make_clip(asset.id, 0.0, 4.0, 0.0);
        tiny.transform.scale = 0.0004;
        let c = chain(&tiny);
        assert!(c.contains("scale=w='max(1,iw*0.0004)':h='max(1,ih*0.0004)'"), "{c}");
        let mut small = tiny;
        small.transform.scale = 0.5;
        assert!(chain(&small).contains(",scale=iw*0.5:ih*0.5,"), "{}", chain(&small));
        // A keyed one, moving or not, clamps when any key is tiny.
        for last in [1.0, 0.0004] {
            let mut keyed = make_clip(asset.id, 0.0, 4.0, 0.0);
            keyed.keyframes = vec![key(0.0, 0.0004, 0.0, 0.0, 1.0), key(2.0, last, 0.0, 0.0, 1.0)];
            let c = chain(&keyed);
            assert!(c.contains("scale=w='max(1,iw*(") && c.contains("eval=frame"), "{c}");
            assert!(c.contains("':h='max(1,ih*("), "{c}");
        }
        let mut keyed = make_clip(asset.id, 0.0, 4.0, 0.0);
        keyed.keyframes = vec![key(0.0, 0.3, 0.0, 0.0, 1.0), key(2.0, 1.6, 0.0, 0.0, 1.0)];
        assert!(!chain(&keyed).contains("max(1,"), "{}", chain(&keyed));
        // The still.
        let canvas = still_canvas(Fit::Contain);
        let tf = Transform {
            scale: 0.0004,
            ..Transform::default()
        };
        let c = still_clip_chain(&tf, &Color::default(), &[], None, &canvas, None, false);
        assert!(c.contains("scale=w='max(1,iw*0.0004)':h='max(1,ih*0.0004)'"), "{c}");
        let c = still_clip_chain(
            &Transform { scale: 0.5, ..tf },
            &Color::default(),
            &[],
            None,
            &canvas,
            None,
            false,
        );
        assert!(c.contains("scale=iw*0.5:ih*0.5"), "{c}");
    }

    /// `zscale` refuses an odd size in 4:2:0, so for HDR footage (tone-mapped after the
    /// geometry) the fit and a constant zoom come out even; a moving zoom runs after the
    /// tone-map and needs nothing; and SDR footage is byte-for-byte what it was.
    #[test]
    fn the_sizes_ahead_of_a_tone_map_are_even() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let hlg = ClipFx {
            hdr: Some(crate::model::Hdr::Hlg),
            ..ClipFx::default()
        };
        let fmt = ExportFormat::default();
        let mut clip = make_clip(asset.id, 0.0, 4.0, 0.0);
        clip.transform.scale = 0.45;
        let sdr = video_clip_chain(&clip, &fmt, &ClipFx::default(), false, "c0");
        let hdr = video_clip_chain(&clip, &fmt, &hlg, false, "c0");
        assert!(!sdr.contains("force_divisible_by") && !sdr.contains("trunc("), "{sdr}");
        assert!(
            hdr.contains(":force_original_aspect_ratio=decrease:force_divisible_by=2"),
            "the fit: {hdr}"
        );
        assert!(
            hdr.contains("scale=w='max(2,2*trunc((iw*0.45)/2))':h='max(2,2*trunc((ih*0.45)/2))'"),
            "the zoom: {hdr}"
        );
        // Both sit ahead of the tone-map.
        let tone = hdr.find("zscale").or_else(|| hdr.find("colorspace")).unwrap();
        assert!(
            hdr.find("force_divisible_by").unwrap() < tone && hdr.find("trunc(").unwrap() < tone,
            "{hdr}"
        );
        // Cover crops to the (even) frame; its intermediate size never reaches `zscale`.
        let cover = ExportFormat {
            fit: Fit::Cover,
            ..ExportFormat::default()
        };
        assert!(!video_clip_chain(&clip, &cover, &hlg, false, "c0").contains("force_divisible_by"));
        // A moving zoom is the last stage, after the tone-map: its sizes are not made even.
        clip.keyframes = vec![key(0.0, 0.3, 0.0, 0.0, 1.0), key(2.0, 1.6, 0.0, 0.0, 1.0)];
        let moving = video_clip_chain(&clip, &fmt, &hlg, false, "c0");
        assert!(!moving.contains("trunc("), "{moving}");
        assert!(moving.contains("force_divisible_by=2"), "the fit is still even: {moving}");
        // The still tone-maps the source before any geometry: nothing to do there.
        let still = still_clip_chain(
            &clip.transform,
            &Color::default(),
            &[],
            None,
            &still_canvas(Fit::Contain),
            None,
            false,
        );
        assert!(!still.contains("force_divisible_by") && !still.contains("trunc("), "{still}");
    }

    /// A source with an alpha channel keeps it to the overlay (the terminal `format=` of a
    /// non-alpha chain flattened a transparent sticker onto black), and an unrecorded or
    /// opaque pixel format keeps the chain it always had.
    #[test]
    fn a_source_with_alpha_ends_its_chain_in_an_alpha_format() {
        let mut sticker = img_asset(Uuid::new_v4());
        sticker.streams[0].pix_fmt = Some("rgba".into());
        let mut clip = make_clip(sticker.id, 0.0, 3.0, 0.0);
        clip.transform.scale = 0.5;
        let timeline = single(vec![clip.clone()]);
        let fx = transition_fx(&timeline, std::slice::from_ref(&sticker));
        assert!(fx[0].alpha, "the probed format says rgba");
        let fmt = ExportFormat::default();
        let chain = video_clip_chain(&clip, &fmt, &fx[0], true, "c0");
        assert!(chain.ends_with(",format=yuva420p"), "{chain}");
        assert!(!chain.contains("format=yuv420p"), "{chain}");
        // Opaque, and never recorded: today's chain.
        for pix_fmt in [Some("rgb24"), Some("yuv420p"), None] {
            sticker.streams[0].pix_fmt = pix_fmt.map(String::from);
            let fx = transition_fx(&timeline, std::slice::from_ref(&sticker));
            assert!(!fx[0].alpha, "{pix_fmt:?}");
            let chain = video_clip_chain(&clip, &fmt, &fx[0], true, "c0");
            assert!(chain.ends_with(",format=yuv420p"), "{pix_fmt:?}: {chain}");
        }
        // The names that carry alpha, by `pix_fmt_has_alpha`.
        for pix_fmt in ["rgba", "bgra", "yuva420p", "yuva444p10le", "gbrap", "pal8", "ya8"] {
            sticker.streams[0].pix_fmt = Some(pix_fmt.into());
            assert!(transition_fx(&timeline, std::slice::from_ref(&sticker))[0].alpha, "{pix_fmt}");
        }
        // A chain that is already alpha, or a moving zoom, ends in the one `format=yuva420p`.
        let alpha = ClipFx {
            alpha: true,
            ..ClipFx::default()
        };
        let mut faded = clip.clone();
        faded.transform.opacity = 0.5;
        let chain = video_clip_chain(&faded, &fmt, &alpha, true, "c0");
        assert!(chain.ends_with("colorchannelmixer=aa=0.5"), "{chain}");
        let mut moving = clip;
        moving.keyframes = vec![key(0.0, 0.3, 0.0, 0.0, 1.0), key(2.0, 1.6, 0.0, 0.0, 1.0)];
        let chain = video_clip_chain(&moving, &fmt, &alpha, true, "c0");
        assert!(
            chain.ends_with(",format=yuva420p") && chain.matches("format=yuva420p").count() == 1,
            "{chain}"
        );
    }

    /// The clip's picture reaches `overlay` straight from the zoom's `format`, with no
    /// label in between that a converter could be spliced into, and the overlay
    /// centres it by the picture's own `w` / `h` — which change every frame.
    #[test]
    fn a_keyed_zoom_feeds_overlay_directly_and_centres_by_the_pictures_own_size() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let assets = vec![asset.clone()];
        let mut clip = make_clip(asset.id, 0.0, 4.0, 1.0);
        clip.keyframes = vec![key(0.0, 0.4, -0.2, 0.0, 1.0), key(2.0, 1.6, 0.2, 0.0, 1.0)];
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
        let zoomed = g
            .filter
            .split(';')
            .find(|c| c.starts_with("[0:v]"))
            .expect("the clip's chain");
        assert!(zoomed.ends_with(",format=yuva420p[v0]"), "{zoomed}");
        assert!(
            g.filter.contains("[vbase][v0]overlay=x='(W-w)/2+(if(lt((t-1)"),
            "{}",
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
    fn drawtext_drops_control_characters_and_changes_nothing_else() {
        // A NUL makes `Command::spawn` fail for the whole filtergraph; ESC and the
        // C1 block are noise. All of them go.
        assert_eq!(escape_drawtext_text("a\0b\u{1b}[0mc\u{7f}d\u{85}e"), "ab[0mcde");
        assert_eq!(escape_drawtext_path("/fonts/\0x.ttf"), "/fonts/x.ttf");
        // Real text is byte-identical to what it always was: quotes, backslashes,
        // percent, newline-to-space, tab, CR, accents, emoji, CJK.
        for plain in [
            "Hello, world",
            "50% OFF — it's \"live\"",
            "C:\\path\\to",
            "tab\there",
            "crlf\r\nline",
            "Café ☕ 日本語 🎬",
        ] {
            let expected = plain.replace('\\', "\\\\").replace('\n', " ").replace('\'', "'\\''");
            assert_eq!(escape_drawtext(plain), expected, "{plain:?}");
        }
    }

    #[test]
    fn drawtext_text_escapes_percent_but_path_does_not() {
        // A bare `%` is a drawtext configuration error that silently blanks the
        // whole overlay. drawtext's own literal-`%` escape is `\%`, doubled to
        // `\\%` to survive the filtergraph value parser the same way a literal
        // backslash does. `fontfile=` is a path, not drawtext-expanded text, so
        // it must not be touched.
        assert_eq!(escape_drawtext_text("50% OFF"), "50\\\\% OFF");
        assert_eq!(
            escape_drawtext_path("C:\\fonts\\100% Arial.ttf"),
            "C:\\\\fonts\\\\100% Arial.ttf"
        );
    }

    #[test]
    fn drawtext_export_escapes_percent_in_text() {
        let o = TextOverlay::new("Battery: 82%", 0.0, 1.0);
        let f = drawtext_export(&o, &ExportFormat::default());
        assert!(f.contains("text='Battery: 82\\\\%'"), "{f}");
    }

    #[test]
    fn valid_color_accepts_names_hex_and_alpha() {
        for ok in [
            "white",
            "AliceBlue",
            "#fff000",
            "#FFAA00CC",
            "0x000000",
            "0XAABBCCDD",
            "white@0.5",
            "black@1",
            "black@.5",
        ] {
            assert!(valid_color(ok), "{ok}");
        }
    }

    #[test]
    fn valid_color_rejects_injection_and_malformed_input() {
        for bad in [
            "white,drawtext=textfile='/etc/passwd'",
            "white:x=1",
            "",
            "#ff",
            "0xZZZZZZ",
            "white@",
            "white@1.2.3",
            "white@nan",
        ] {
            assert!(!valid_color(bad), "{bad}");
        }
    }

    #[test]
    fn malicious_colour_never_reaches_the_graph() {
        let mut overlay = TextOverlay::new("Hello", 0.0, 1.0);
        let injection = "white,drawtext=textfile='/etc/passwd'";
        overlay.color = injection.to_string();
        overlay.bg = Some(injection.to_string());
        let drawn = drawtext_export(&overlay, &ExportFormat::default());
        assert!(!drawn.contains("/etc/passwd") && !drawn.contains("textfile"), "{drawn}");
        assert!(drawn.contains("fontcolor=white"), "invalid colour falls back: {drawn}");
        assert!(!drawn.contains("box=1"), "invalid box colour drops the box: {drawn}");

        let chroma = chroma_filter(&VideoEffect::ChromaKey {
            color: injection.to_string(),
            similarity: 0.1,
            blend: 0.0,
        })
        .unwrap();
        assert!(!chroma.contains("/etc/passwd"), "{chroma}");
        assert_eq!(chroma, "chromakey=green:0.1:0", "invalid colour falls back: {chroma}");
    }

    #[test]
    fn malicious_pix_fmt_scaler_and_gif_dither_never_reach_the_graph() {
        let opts = ExportOptions {
            video_codec: Some("libx264".into()),
            pix_fmt: Some("yuv420p,drawtext=textfile='/etc/passwd'".into()),
            scaler: Some("bilinear:x=1".into()),
            ..Default::default()
        };
        let args = args_of(&opts);
        let filter = flag_val(&args, "-filter_complex").unwrap();
        assert!(!filter.contains("/etc/passwd") && !filter.contains(":x=1"), "{filter}");
        // Invalid pix_fmt is dropped, never passed through raw.
        assert_eq!(flag_val(&args, "-pix_fmt"), Some("yuv420p"));

        let gif_opts = ExportOptions {
            container: Container::Gif,
            video_codec: Some("gif".into()),
            include_audio: false,
            gif_dither: Some("bayer,drawtext=textfile='/etc/passwd'".into()),
            ..Default::default()
        };
        let gif_filter = flag_val(&args_of(&gif_opts), "-filter_complex").unwrap().to_string();
        assert!(gif_filter.contains("paletteuse=dither=bayer["), "{gif_filter}");
        assert!(!gif_filter.contains("/etc/passwd"), "{gif_filter}");
    }

    #[test]
    fn validate_export_flags_unsupported_pix_fmt_scaler_and_dither() {
        let opts = ExportOptions {
            pix_fmt: Some("nope".into()),
            scaler: Some("nope".into()),
            gif_dither: Some("nope".into()),
            ..Default::default()
        };
        let issues = validate_export(&opts, true, true);
        assert_eq!(issues.len(), 3, "{issues:?}");
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
        let (args, times) = build_contact_sheet_args("/media/clip.mp4", 0.0, 40.0, 4, 4, 240, 5, None);
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

    /// A region zoom crops before it scales, so the width budget lands on the
    /// region; the whole frame is left byte-identical to the plain decode.
    #[test]
    fn region_frame_crops_then_scales_and_a_full_region_is_no_crop() {
        let r = Region {
            left: 0.25,
            top: 0.1,
            width: 0.5,
            height: 0.3,
        };
        let vf = region_frame_filter(r, 640);
        assert_eq!(
            vf,
            "crop=2*trunc(iw*0.5000/2):2*trunc(ih*0.3000/2):iw*0.2500:ih*0.1000,scale='2*trunc(min(640,iw)/2)':-2"
        );
        assert!(Region::FULL.is_full());
        assert!(!r.is_full());
    }

    /// Whatever the model asks for is pulled into the frame: a corner past the
    /// far edge comes back in, and a region hanging over the edge is shrunk
    /// rather than moved — the corner is the thing that was pointed at.
    #[test]
    fn region_normalizes_into_the_frame() {
        let r = Region {
            left: 0.8,
            top: -0.5,
            width: 0.6,
            height: f64::NAN,
        }
        .normalized();
        assert!((r.left - 0.8).abs() < 1e-9);
        assert_eq!(r.top, 0.0);
        assert!((r.width - 0.2).abs() < 1e-9, "shrunk to the edge: {r:?}");
        assert_eq!(r.height, Region::MIN_SIDE);
        let far = Region {
            left: 2.0,
            top: 0.0,
            width: 1.0,
            height: 1.0,
        }
        .normalized();
        assert!((far.left - (1.0 - Region::MIN_SIDE)).abs() < 1e-9);
        assert!((far.width - Region::MIN_SIDE).abs() < 1e-9);
    }

    /// Zooming the composite renders a canvas large enough for the region to
    /// fill the requested width, then crops it — never past the delivery frame.
    #[test]
    fn timeline_region_widens_the_canvas_and_crops_the_composite() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let assets = vec![asset.clone()];
        let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
        let r = Region {
            left: 0.5,
            top: 0.5,
            width: 0.25,
            height: 0.25,
        };
        let out = StillOutput::JpegPipe { quality: 2 };
        let args = build_still_args(&timeline, &assets, &ExportOptions::default(), 1.0, 320, Some(r), &out).unwrap();
        let graph = args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone();
        // 320 / 0.25 = 1280 canvas, so the crop comes out 320 wide.
        assert!(graph.contains("s=1280x720"), "{graph}");
        assert!(
            graph.ends_with("crop=2*trunc(iw*0.2500/2):2*trunc(ih*0.2500/2):iw*0.5000:ih*0.5000[outv]"),
            "{graph}"
        );
        // Capped at the delivery frame: a zoom invents no pixels.
        let args = build_still_args(&timeline, &assets, &ExportOptions::default(), 1.0, 1920, Some(r), &out).unwrap();
        let graph = args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone();
        assert!(graph.contains("s=1920x1080"), "{graph}");
        // Without a region the graph is what it always was.
        let plain = build_still_args(&timeline, &assets, &ExportOptions::default(), 1.0, 320, None, &out).unwrap();
        let graph = plain[plain.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone();
        assert!(graph.contains("s=320x180") && graph.ends_with("null[outv]"), "{graph}");
    }

    #[test]
    fn contact_sheet_times_match_the_sheet() {
        let (_, times) = build_contact_sheet_args("/x.mp4", 10.0, 20.0, 2, 2, 160, 3, None);
        assert_eq!(times, contact_sheet_times(10.0, 20.0, 2, 2));
    }

    #[test]
    fn contact_sheet_respects_a_subrange() {
        let (args, times) = build_contact_sheet_args("/x.mp4", 10.0, 20.0, 2, 2, 160, 3, None);
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
        assert!(joined.contains("-ss 7.000000 -i /x.mp4"), "{joined}");
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

    /// The FFmpeg still and the GPU's `RenderPlan` are built from one active-clip
    /// list, but the argv is still the thing that decides what FFmpeg decodes —
    /// so pin that the two describe the same frame: same inputs in the same
    /// order at the same source times, same canvas size.
    #[test]
    fn the_render_plan_and_the_still_args_agree_on_layers_timing_and_canvas() {
        use crate::render_plan::RenderPlan;
        let mk = |path: &str| {
            let mut a = test_asset(vec![video_stream(1920, 1080, 30.0)]);
            a.path = path.into();
            a
        };
        let (a, b, c) = (mk("/m/a.mp4"), mk("/m/b.mp4"), mk("/m/c.mp4"));
        let mut fast = make_clip(a.id, 10.0, 30.0, 0.0);
        fast.speed = 2.0;
        let mut reversed = make_clip(b.id, 0.0, 20.0, 1.0);
        reversed.speed = -1.0;
        let mut late = make_clip(c.id, 0.0, 5.0, 40.0);
        late.transform.scale = 0.5;
        let timeline = timeline_of(vec![
            video_track(vec![fast, late]),
            video_track(vec![reversed]),
            // A muted track appears in neither.
            Track {
                muted: true,
                ..video_track(vec![make_clip(c.id, 0.0, 9.0, 0.0)])
            },
        ]);
        let assets = vec![a, b, c];
        let opts = ExportOptions::default();
        for (t, max_width) in [(2.5, 640), (0.5, 640), (6.0, 1920), (45.0, 320)] {
            let args = build_timeline_frame_args(&timeline, &assets, &opts, t, max_width, 4).unwrap();
            let plan = RenderPlan::at(
                &timeline,
                &assets,
                &opts,
                t,
                crate::render_plan::CompositeColorPolicy::FixedBt601,
            )
            .unwrap();

            // `-ss <t> -i <path>` pairs, in input order.
            let mut from_args = Vec::new();
            let mut ss: Option<String> = None;
            for w in args.windows(2) {
                match w[0].as_str() {
                    "-ss" => ss = Some(w[1].clone()),
                    "-i" => from_args.push((w[1].clone(), ss.take().unwrap_or_default())),
                    _ => {}
                }
            }
            let from_plan: Vec<(String, String)> = plan
                .layers
                .iter()
                .map(|l| (l.path.clone(), format!("{:.6}", l.source_time)))
                .collect();
            assert_eq!(from_args, from_plan, "t={t}");

            let (w, h) = plan.size(max_width);
            let canvas = format!("color=c=black:s={w}x{h}:d=0.1[base]");
            assert!(args.iter().any(|a| a.starts_with(&canvas)), "t={t}: {canvas} not in {args:?}");
        }
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
            voiceover: None,
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
            voiceover: None,
        };
        let a2 = Asset {
            id: Uuid::new_v4(),
            path: "/media/b.mp4".into(),
            name: "b.mp4".into(),
            duration: 10.0,
            streams: vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)],
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
        };
        let timeline = single(vec![make_clip(a1.id, 0.0, 20.0, 0.0), make_clip(a2.id, 0.0, 10.0, 20.0)]);
        let assets = vec![a1, a2];
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
            voiceover: None,
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
            voiceover: None,
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
            voiceover: None,
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
            voiceover: None,
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

        // spill_graph rather than externalize_filter_complex: the wrapper
        // probes the real binary for which flag spelling it takes, and the
        // default test run must stay binary-free.
        let mut spilled = args.clone();
        let guard = spill_graph(&mut spilled, "test", "-/filter_complex").unwrap();
        assert_eq!(spilled[i - 1], "-/filter_complex");
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

    /// The video chain of the first clip of `tl`.
    fn first_video_chain(tl: &Timeline, assets: &[Asset]) -> String {
        let g = graph_of(tl, assets);
        g.split(';')
            .find(|c| c.starts_with("[0:v]"))
            .expect("a video chain")
            .to_string()
    }

    fn pkey(time: f64, value: f64) -> crate::model::PropertyKey {
        crate::model::PropertyKey::new(time, value)
    }

    #[test]
    fn a_keyed_colour_writes_the_eq_per_frame_and_leaves_the_rest_of_the_chain_as_it_was() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut still_graded = make_clip(asset.id, 2.0, 8.0, 1.5);
        still_graded.color.contrast = 1.2;
        let mut keyed = still_graded.clone();
        keyed.set_property_keys(Property::Brightness, vec![pkey(0.0, -0.3), pkey(2.0, 0.3)]);
        keyed.set_property_keys(Property::Temperature, vec![pkey(0.0, -1.0), pkey(2.0, 0.5)]);
        let chain = first_video_chain(&single(vec![keyed.clone()]), std::slice::from_ref(&asset));
        // The keyed numbers are quoted expressions of `t` minus the clip's start; the others stay
        // the numbers they were; the temperature is the gammas of its curve; one `eval=frame`.
        assert!(
            chain.contains("eq=brightness='if(lt((t-1.5),0),-0.3,if(lt((t-1.5),2),(-0.3+(0.6)*((t-1.5)-0)/(2)),0.3))':contrast=1.2:saturation=1:gamma=1:gamma_r='1+0.3*(if(lt((t-1.5),0),-1,"),
            "{chain}"
        );
        assert!(
            chain.contains(":gamma_b='1-0.3*(") && chain.ends_with(":eval=frame,format=yuv420p[v0]"),
            "{chain}"
        );
        // Nothing but the `eq` differs from the clip with the same grade held still.
        let still = first_video_chain(&single(vec![still_graded]), std::slice::from_ref(&asset));
        let strip = |c: &str| c.split(",eq=").next().unwrap().to_string();
        assert_eq!(strip(&chain), strip(&still), "the picture is placed as a static clip's is");
        let after = |c: &str| c.rsplit_once(",format=").unwrap().1.to_string();
        assert_eq!(after(&chain), after(&still));
        // A keyed clip is graded even when its static grade is the identity.
        keyed.color = crate::model::Color::default();
        let chain = first_video_chain(&single(vec![keyed]), std::slice::from_ref(&asset));
        assert!(chain.contains("contrast=1:saturation=1:gamma=1:gamma_r="), "{chain}");
    }

    #[test]
    fn a_transform_with_some_numbers_keyed_builds_the_rest_from_the_static_transform() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let assets = std::slice::from_ref(&asset);
        let mut fading = make_clip(asset.id, 0.0, 10.0, 0.0);
        fading.transform.scale = 1.5;
        fading.transform.rotation = 20.0;
        fading.set_property_keys(Property::Opacity, vec![pkey(0.0, 0.2), pkey(2.0, 1.0)]);
        let chain = first_video_chain(&single(vec![fading.clone()]), assets);
        // The opacity is a `geq` alpha; the zoom and the turn are the constant ones.
        assert!(chain.contains("geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='("), "{chain}");
        assert!(chain.contains("scale=iw*1.5:ih*1.5"), "{chain}");
        assert!(!chain.contains("eval=frame"), "no zoom is keyed: {chain}");
        assert!(chain.contains("rotate=0.3490658503988659:fillcolor=none"), "{chain}");
        assert!(!chain.contains("colorchannelmixer"), "{chain}");
        // A moving position beside a static opacity: that opacity is the constant mix.
        let mut moving = make_clip(asset.id, 0.0, 10.0, 0.0);
        moving.transform.opacity = 0.6;
        moving.set_property_keys(Property::PosX, vec![pkey(0.0, -0.2), pkey(3.0, 0.2)]);
        let g = graph_of(&single(vec![moving.clone()]), assets);
        assert!(g.contains("colorchannelmixer=aa=0.6"), "{g}");
        assert!(
            g.contains("overlay=x='(W-w)/2+(") && g.contains(")*W':y='(H-h)/2+(0)*H'"),
            "{g}"
        );
        // Taking a number off the bundle: the bundle's other numbers still drive the rest.
        let mut bundle = make_clip(asset.id, 0.0, 10.0, 0.0);
        bundle.keyframes = vec![
            crate::model::Keyframe {
                time: 0.0,
                scale: 1.0,
                pos_x: 0.0,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
                easing: Default::default(),
            },
            crate::model::Keyframe {
                time: 2.0,
                scale: 2.0,
                pos_x: 0.1,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 0.5,
                easing: Default::default(),
            },
        ];
        let keyed_chain = first_video_chain(&single(vec![bundle.clone()]), assets);
        assert!(
            keyed_chain.contains("geq=") && keyed_chain.contains("scale=w='iw*"),
            "{keyed_chain}"
        );
        bundle.set_property_keys(Property::Opacity, vec![]);
        bundle.transform.opacity = 0.7;
        let off = first_video_chain(&single(vec![bundle]), assets);
        assert!(
            !off.contains("geq=") && off.contains("colorchannelmixer=aa=0.7") && off.contains("scale=w='iw*"),
            "{off}"
        );
    }

    #[test]
    fn a_keyed_volume_is_a_per_frame_gain_in_small_frames_and_an_unkeyed_one_is_not() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut clip = make_clip(asset.id, 0.0, 10.0, 3.0);
        clip.volume = 0.5;
        let plain = graph_of(&single(vec![clip.clone()]), std::slice::from_ref(&asset));
        assert!(plain.contains("volume=0.5,") && !plain.contains("asetnsamples"), "{plain}");
        clip.set_property_keys(Property::Volume, vec![pkey(0.0, 0.0), pkey(2.0, 1.0)]);
        let keyed = graph_of(&single(vec![clip]), std::slice::from_ref(&asset));
        assert!(
            keyed.contains(
                ",asetnsamples=n=128:p=0,volume='if(lt((t-0),0),0,if(lt((t-0),2),(0+(1)*((t-0)-0)/(2)),1))':eval=frame,"
            ),
            "{keyed}"
        );
        assert!(!keyed.contains("volume=0.5"), "the static gain is replaced: {keyed}");
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
    fn a_mask_cuts_the_clip_to_its_shape() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        clip.mask = Some(crate::model::Mask {
            shape: crate::model::MaskShape::Ellipse,
            x: 0.25,
            y: 0.5,
            width: 0.4,
            height: 0.6,
            feather: 0.2,
            inverted: false,
        });
        let g = graph_of(&single(vec![clip.clone()]), std::slice::from_ref(&asset));
        // A mask needs an alpha plane, and it must be established before the geq.
        let alpha = g.find("format=yuva420p").expect("alpha");
        let geq = g.find("geq=lum=").expect("mask");
        assert!(alpha < geq, "alpha must precede the mask: {g}");
        // An ellipse combines the axes with hypot; the edge sits at distance 1.
        assert!(g.contains("hypot((X-0.25*W)/(0.2*W)"), "{g}");
        assert!(g.contains("clip((1-hypot"), "feathered edge: {g}");

        // A rectangle is the same expression with max instead of hypot…
        let mut rect = clip;
        rect.mask = Some(crate::model::Mask {
            shape: crate::model::MaskShape::Rect,
            feather: 0.0,
            ..Default::default()
        });
        let g = graph_of(&single(vec![rect.clone()]), std::slice::from_ref(&asset));
        assert!(g.contains("max(abs("), "{g}");
        // …and no feather is a hard test rather than a ramp.
        assert!(g.contains("lte(max(abs("), "{g}");
        assert!(!g.contains("clip((1-"), "{g}");

        // Inverted keeps what is outside the shape.
        let mut inv = rect;
        inv.mask = Some(crate::model::Mask {
            inverted: true,
            feather: 0.0,
            ..Default::default()
        });
        let g = graph_of(&single(vec![inv]), &[asset]);
        assert!(g.contains("a='((1-lte("), "{g}");
    }

    #[test]
    fn a_mask_and_keyframed_opacity_share_one_geq_pass() {
        // Both rewrite only the alpha plane, and geq is the most expensive
        // filter in the chain — a clip with both must fold them into one pass.
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        clip.mask = Some(crate::model::Mask::default());
        let key = |time: f64, opacity: f64| crate::model::Keyframe {
            easing: Default::default(),
            time,
            scale: 1.0,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity,
        };
        clip.keyframes = vec![key(0.0, 0.0), key(5.0, 1.0)];
        let g = graph_of(&single(vec![clip]), &[asset]);
        assert_eq!(g.matches("geq=").count(), 1, "one pass for both: {g}");
        // The mask's keep expression and the opacity ramp share the alpha term.
        assert!(g.contains("clip((1-max(abs("), "the mask survives: {g}");
        assert!(g.contains(")*(if(lt((T"), "the opacity ramp survives: {g}");
    }

    #[test]
    fn an_unmasked_clip_writes_no_mask() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let g = graph_of(&single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]), &[asset]);
        assert!(!g.contains("geq="), "{g}");
        assert!(!g.contains("format=yuva420p"), "no mask, no alpha: {g}");
    }

    #[test]
    fn a_degenerate_mask_is_clamped_rather_than_blanking_the_clip() {
        // A zero-width shape would make the whole clip transparent, which is
        // never what was meant by dragging a handle too far.
        let m = crate::model::Mask {
            width: 0.0,
            height: -3.0,
            x: 9.0,
            feather: f64::NAN,
            ..Default::default()
        }
        .normalized();
        assert!(m.width >= 0.01 && m.height >= 0.01);
        assert_eq!(m.x, 1.0);
        assert!((0.0..=1.0).contains(&m.feather));
    }

    #[test]
    fn the_track_fader_and_pan_ride_the_finished_clip() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
        clip.volume = 0.8;
        clip.audio = vec![crate::model::AudioEffect::Highpass { hz: 80.0 }];
        let mut timeline = single(vec![clip]);
        timeline.tracks[0].volume = 0.5;
        timeline.tracks[0].pan = -1.0;
        let g = graph_of(&timeline, &[asset]);
        // The clip's own gain, then its effects, then the fader: a channel strip,
        // so the compressor upstream never sees the fader move.
        let clip_gain = g.find("volume=0.8").expect("clip gain");
        let effect = g.find("highpass").expect("clip effect");
        let fader = g.find("volume=0.5").expect("track fader");
        assert!(clip_gain < effect && effect < fader, "fader must come last: {g}");
        // Hard left is the right channel gone and the left untouched.
        assert!(g.contains("pan=stereo|c0=1*c0|c1=0*c1"), "{g}");
        // And the pan runs after the aformat upmix: run before it, a mono
        // source has no c1 and the attenuated leg would be pure silence.
        let af = g.find("aformat=").expect("aformat");
        let pan = g.find("pan=stereo").expect("pan");
        assert!(af < pan, "pan must follow the layout normalize: {g}");
    }

    #[test]
    fn an_untouched_track_mix_emits_nothing() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let g = graph_of(&timeline, &[asset]);
        // Unity and centre are the historical graph exactly — no fader, no pan.
        assert!(!g.contains("pan=stereo"), "{g}");
        assert_eq!(g.matches("volume=").count(), 1, "only the clip's own gain: {g}");
    }

    #[test]
    fn a_pan_is_dropped_when_there_is_nowhere_to_pan_to() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        timeline.tracks[0].pan = 1.0;
        let opts = ExportOptions {
            audio_channels: Some(1),
            ..Default::default()
        };
        let args = build_export_args(&timeline, &[asset], "out.mp4", &opts).unwrap();
        let g = args.join(" ");
        assert!(!g.contains("pan=stereo"), "a mono delivery has no stereo field: {g}");
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

    // ---- the master bus ----------------------------------------------------

    use crate::model::MasterBus;

    /// A video track with one clip of sound, delivered through a master `bus`.
    fn master_cut(bus: MasterBus) -> (Timeline, Vec<Asset>) {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut tl = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        tl.master = bus;
        (tl, vec![asset])
    }

    fn mix_of(tl: &Timeline, assets: &[Asset], opts: &ExportOptions) -> String {
        build_export_args(tl, assets, "out.mp4", opts).unwrap().join(" ")
    }

    #[test]
    fn a_neutral_master_adds_nothing_to_the_mix() {
        let (tl, assets) = master_cut(MasterBus::default());
        let opts = ExportOptions::default();
        let g = mix_of(&tl, &assets, &opts);
        assert!(g.contains("amix=inputs=1:normalize=0:dropout_transition=0[outa]"), "{g}");
        assert!(!g.contains("alimiter"), "{g}");
        // A ceiling kept while the limiter is off is neutral too: the same argv.
        let mut parked = tl;
        parked.master.ceiling_db = -6.0;
        assert_eq!(mix_of(&parked, &assets, &opts), g);
    }

    #[test]
    fn the_master_fader_follows_the_final_sum() {
        let (tl, assets) = master_cut(MasterBus {
            volume: 0.5,
            ..MasterBus::default()
        });
        let g = mix_of(&tl, &assets, &ExportOptions::default());
        assert!(
            g.contains("amix=inputs=1:normalize=0:dropout_transition=0,volume=0.5[outa]"),
            "{g}"
        );
        assert!(!g.contains("alimiter"), "{g}");
    }

    #[test]
    fn the_limiter_holds_the_ceiling_without_levelling_or_delaying_the_mix() {
        let (tl, assets) = master_cut(MasterBus {
            limiter: true,
            ..MasterBus::default()
        });
        let g = with_alimiter_latency(true, || mix_of(&tl, &assets, &ExportOptions::default()));
        // -1.5 dB is 0.841395 linear. `level=0` because the default auto-levels the
        // output back up to full scale; `latency=1` because without it the lookahead
        // delays the sound against the picture and drops its tail.
        assert!(
            g.contains("dropout_transition=0,alimiter=limit=0.841395:attack=5:release=100:level=0:latency=1[outa]"),
            "{g}"
        );
        assert!(!g.contains("dropout_transition=0,volume"), "no fader at unity: {g}");
    }

    #[test]
    fn an_alimiter_without_latency_is_given_the_limiter_without_it() {
        // FFmpeg 4.4 refuses the option (`Option 'latency' not found`) and with it the
        // whole graph; the rest of the limiter is spelled exactly as before.
        let (tl, assets) = master_cut(MasterBus {
            limiter: true,
            ..MasterBus::default()
        });
        let g = with_alimiter_latency(false, || mix_of(&tl, &assets, &ExportOptions::default()));
        assert!(
            g.contains("dropout_transition=0,alimiter=limit=0.841395:attack=5:release=100:level=0[outa]"),
            "{g}"
        );
        assert!(!g.contains("latency"), "{g}");
        // A fader-only master has no limiter to spell.
        let (tl, assets) = master_cut(MasterBus {
            volume: 0.5,
            ..MasterBus::default()
        });
        let g = with_alimiter_latency(false, || mix_of(&tl, &assets, &ExportOptions::default()));
        assert!(g.contains("dropout_transition=0,volume=0.5[outa]"), "{g}");
    }

    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn the_latency_probe_agrees_with_whether_this_ffmpeg_accepts_the_option() {
        // The probe reads help text; what matters is that a graph spelled by the answer
        // runs. FFmpeg 4.4 exits 1 on `latency=1` ("Option 'latency' not found").
        let accepts = |opts: &str| {
            command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "quiet", "-f", "lavfi", "-i", "sine=d=0.1"])
                .args(["-af", &format!("alimiter={opts}"), "-f", "null", "-"])
                .status_bounded()
                .expect("run ffmpeg")
                .success()
        };
        assert!(accepts("limit=0.8:attack=5:release=100:level=0"), "the limiter itself");
        assert_eq!(
            accepts("limit=0.8:attack=5:release=100:level=0:latency=1"),
            alimiter_latency_available()
        );
        let tl = master_cut(MasterBus {
            limiter: true,
            ..MasterBus::default()
        });
        let g = mix_of(&tl.0, &tl.1, &ExportOptions::default());
        assert_eq!(g.contains(":latency=1"), alimiter_latency_available(), "{g}");
    }

    #[test]
    fn the_latency_option_is_read_off_the_filters_own_help() {
        // `ffmpeg -h filter=alimiter` as FFmpeg 4.4.2 (Ubuntu 22.04) prints it, and 9.0.2.
        let old = "Filter alimiter\n  Audio lookahead limiter.\nalimiter AVOptions:\n  \
                   level_in          <double>     ..F.A...... set input level (from 0.015625 to 64) (default 1)\n  \
                   release           <double>     ..F.A...... set release (from 1 to 8000) (default 50)\n  \
                   level             <boolean>    ..F.A...... auto level (default true)\n";
        let new = format!(
            "{old}   latency           <boolean>    ..F.A....T. compensate delay (default false)\n\n\
             This filter has support for timeline through the 'enable' option.\n"
        );
        assert!(!help_lists_latency(old));
        assert!(help_lists_latency(&new));
        // Only an option of that name: a description that mentions it is not one.
        assert!(!help_lists_latency("  asc <boolean> set the latency of the asc\n"));
        assert!(!help_lists_latency(""));
    }

    #[test]
    fn the_master_runs_after_the_sum_and_before_loudnorm() {
        let (tl, assets) = master_cut(MasterBus {
            volume: 0.8,
            limiter: true,
            ceiling_db: -3.0,
        });
        let opts = ExportOptions {
            loudnorm: true,
            ..ExportOptions::default()
        };
        let g = mix_of(&tl, &assets, &opts);
        let amix = g.find("amix=inputs=1").expect(&g);
        let fader = g.find(",volume=0.8,").expect(&g);
        let limiter = g.find(",alimiter=limit=0.707946:").expect(&g);
        let norm = g.find(",loudnorm=I=-14").expect(&g);
        assert!(amix < fader && fader < limiter && limiter < norm, "{g}");
        assert!(g.contains("loudnorm=I=-14:TP=-1.5:LRA=11,aresample=48000[outa]"), "{g}");
    }

    #[test]
    fn the_master_closes_the_duck_bus_sum() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let mut music = audio_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        music.duck = true;
        let mut tl = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]), music]);
        tl.master.volume = 0.6;
        let g = mix_of(&tl, &[asset], &ExportOptions::default());
        assert!(
            g.contains("[akmix][aducked]amix=inputs=2:normalize=0:dropout_transition=0,volume=0.6[outa]"),
            "{g}"
        );
    }

    #[test]
    fn a_range_export_keeps_the_master() {
        let (tl, assets) = master_cut(MasterBus {
            limiter: true,
            ..MasterBus::default()
        });
        let opts = ExportOptions {
            range: Some(crate::model::TimeRange { start: 2.0, end: 6.0 }),
            ..ExportOptions::default()
        };
        assert!(mix_of(&tl, &assets, &opts).contains("alimiter=limit=0.841395"));
    }

    #[test]
    fn a_delivery_variant_keeps_the_master() {
        // `render_variants` builds every file from `for_delivery`: the same mix at
        // every frame, only the picture changes.
        let (tl, assets) = master_cut(MasterBus {
            volume: 0.5,
            limiter: true,
            ceiling_db: -2.0,
        });
        let variant = tl.for_delivery(Delivery::new(1080, 1920, Fit::Cover));
        let g = mix_of(&variant, &assets, &ExportOptions::default());
        assert!(g.contains(",volume=0.5,alimiter=limit=0.794328:"), "{g}");
    }

    #[test]
    fn the_playback_stream_has_no_sound_for_the_master_to_touch() {
        let (tl, assets) = master_cut(MasterBus {
            volume: 0.5,
            limiter: true,
            ceiling_db: -1.0,
        });
        let args = build_preview_args_with(&tl, &assets, 0.0, 30.0, 640, 4, None)
            .unwrap()
            .join(" ");
        assert!(!args.contains("alimiter") && !args.contains("volume=0.5"), "{args}");
    }

    #[test]
    fn master_filters_are_safe_for_a_file_that_never_went_through_the_op() {
        let bus = |volume, ceiling_db| MasterBus {
            volume,
            limiter: true,
            ceiling_db,
        };
        let filters = |bus| with_alimiter_latency(true, || master_filters(&bus));
        // Not a number: no fader, the default ceiling.
        assert_eq!(
            filters(bus(f64::NAN, f64::NAN)),
            ",alimiter=limit=0.841395:attack=5:release=100:level=0:latency=1"
        );
        // Beyond the ends: clamped to what `alimiter` accepts (its `limit` is 0.0625..=1).
        assert_eq!(
            filters(bus(9.0, 12.0)),
            ",volume=4,alimiter=limit=1:attack=5:release=100:level=0:latency=1"
        );
        assert_eq!(
            filters(bus(-1.0, -90.0)),
            ",volume=0,alimiter=limit=0.063096:attack=5:release=100:level=0:latency=1"
        );
        assert_eq!(filters(MasterBus::default()), "");
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
        // Incoming clip B fades up via alpha, from where it sits on the timeline.
        assert!(g.contains("fade=t=in:st=10:d=1:alpha=1"), "{g}");
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
        assert!(
            g.contains("fade=t=in:st=15:d=1:alpha=1"),
            "incoming dissolves from black: {g}"
        );
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
    fn dip_to_white_fades_both_sides_through_white() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let a = make_clip(asset.id, 0.0, 10.0, 0.0);
        let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
        b.transition_in = Some(crate::model::Transition {
            kind: TransitionKind::DipToWhite,
            duration: 1.0,
        });
        let g = graph_of(&single(vec![a, b]), &[asset]);
        assert!(g.contains("fade=t=out:st=9.5:d=0.5:c=white"), "{g}");
        assert!(g.contains("fade=t=in:st=10:d=0.5:c=white"), "{g}");
        // A dip never borrows a handle: neither clip is extended.
        assert!(g.contains("trim=start=0:end=10"), "{g}");
        assert!(!g.contains(":alpha=1"), "a dip is not a dissolve: {g}");
    }

    #[test]
    fn slide_travels_the_incoming_clip_in_over_the_held_outgoing_one() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let a = make_clip(asset.id, 0.0, 10.0, 0.0);
        let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
        b.transition_in = Some(crate::model::Transition {
            kind: TransitionKind::SlideLeft,
            duration: 1.0,
        });
        let g = graph_of(&single(vec![a, b]), &[asset]);
        // The outgoing clip keeps playing under the incoming one, as for a dissolve.
        assert!(g.contains("trim=start=0:end=11"), "outgoing holds under the slide: {g}");
        // The incoming clip starts a whole frame to the right and travels to 0 over
        // the transition; local time is measured from its timeline start.
        assert!(g.contains("(t-10)"), "motion is expressed in clip-local time: {g}");
        assert!(g.contains("1+(-1)*((t-10)-0)/(1)"), "one frame of travel over 1s: {g}");
        // A slide is not a dissolve — the picture stays hard-edged.
        assert!(!g.contains(":alpha=1"), "{g}");
        // …but the sound still crossfades.
        assert!(g.contains("afade=t=in:st=0:d=1"), "{g}");
    }

    #[test]
    fn push_carries_the_outgoing_clip_out_of_frame_too() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let a = make_clip(asset.id, 0.0, 10.0, 0.0);
        let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
        b.transition_in = Some(crate::model::Transition {
            kind: TransitionKind::PushUp,
            duration: 1.0,
        });
        let g = graph_of(&single(vec![a, b]), &[asset]);
        // The outgoing clip sits still until its own end, then leaves upwards over
        // its tail: y travels 0 → -1 between local 10s and 11s.
        assert!(g.contains("0+(-1)*((t-0)-10)/(1)"), "outgoing is pushed out: {g}");
        // The incoming one arrives from below over the same second.
        assert!(g.contains("1+(-1)*((t-10)-0)/(1)"), "incoming arrives: {g}");
    }

    #[test]
    fn a_slide_composes_with_the_clip_position_it_already_has() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let a = make_clip(asset.id, 0.0, 10.0, 0.0);
        let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
        b.transform = crate::model::Transform {
            pos_x: 0.25,
            ..Default::default()
        };
        b.transition_in = Some(crate::model::Transition {
            kind: TransitionKind::SlideLeft,
            duration: 1.0,
        });
        let g = graph_of(&single(vec![a, b]), &[asset]);
        // The travel is added to the offset the clip already has, not substituted
        // for it — a picture-in-picture slides in to where it lives.
        assert!(g.contains("x='(W-w)/2+((0.25)+(if(lt((t-10)"), "{g}");
    }

    #[test]
    fn slide_without_source_handle_is_a_hard_cut() {
        let asset = av_asset(Uuid::new_v4(), 20.0);
        let a = make_clip(asset.id, 0.0, 20.0, 0.0); // no handle left to borrow
        let mut b = make_clip(asset.id, 0.0, 10.0, 20.0);
        b.transition_in = Some(crate::model::Transition {
            kind: TransitionKind::PushLeft,
            duration: 1.0,
        });
        let g = graph_of(&single(vec![a, b]), &[asset]);
        assert!(g.contains("trim=start=0:end=20"), "outgoing tail must not be extended: {g}");
        assert!(
            !g.contains("overlay=x="),
            "nothing moves when there is nothing to move over: {g}"
        );
    }

    #[test]
    fn a_transition_out_of_a_still_keeps_its_transition() {
        // A still loops, so it never runs out of source: a dissolve (or slide /
        // push) out of a title card must not degrade to a hard cut just because
        // the clip already spans the asset's nominal duration.
        let still = img_asset(Uuid::new_v4());
        let footage = av_asset(Uuid::new_v4(), 20.0);
        let d = crate::model::DEFAULT_IMAGE_DURATION;
        let a = make_clip(still.id, 0.0, d, 0.0);
        let mut b = make_clip(footage.id, 0.0, 10.0, d);
        b.transition_in = Some(crate::model::Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let g = graph_of(&single(vec![a, b]), &[still, footage]);
        // The still is held for the tail and the incoming clip dissolves in.
        assert!(g.contains(&format!("trim=start=0:end={}", d + 1.0)), "{g}");
        assert!(g.contains(":alpha=1"), "the dissolve must survive: {g}");
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
        assert_eq!(
            (fmt.width, fmt.height),
            (1080, 1920),
            "the project frame wins over the footage"
        );
        assert_eq!(
            fmt.fit,
            Fit::Cover,
            "and brings its fit, so the preview crops like the export"
        );
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
            None,
            false,
        );
        assert!(contained.contains("force_original_aspect_ratio=decrease"));
        assert!(contained.contains("pad=1080:1920"), "contain letterboxes: {contained}");

        let covered = still_clip_chain(
            &Transform::default(),
            &Color::default(),
            &[],
            None,
            &canvas(Fit::Cover),
            None,
            false,
        );
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
        let exported = audio_clip_chain(&clip, &ExportFormat::default(), &ClipFx::default(), "stereo", unity_mix());
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

    /// A stand-in for ffmpeg: `script` run by `sh` with piped stdout / stderr.
    /// `exec` in the script so a kill reaches the process holding the pipes.
    #[cfg(unix)]
    fn fake_stream(script: &str) -> std::process::Child {
        Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sh")
    }

    #[cfg(unix)]
    fn short_timeouts() -> PreviewTimeouts {
        PreviewTimeouts {
            first: std::time::Duration::from_millis(400),
            stall: std::time::Duration::from_millis(400),
        }
    }

    /// An ffmpeg that opens and then never produces a frame must fail the
    /// stream, not freeze it: the read used to wait on it forever.
    #[cfg(unix)]
    #[test]
    fn a_preview_stream_that_never_produces_a_frame_times_out() {
        let started = std::time::Instant::now();
        let mut sent = Some(0);
        let result = pump_preview(
            fake_stream("echo no frames today >&2; exec sleep 60"),
            0.0,
            24.0,
            short_timeouts(),
            &mut |_| true,
            &mut sent,
        );
        let err = result.expect_err("a silent stream must fail").to_string();
        assert!(err.contains("stalled") && err.contains("first frame"), "{err}");
        assert!(err.contains("no frames today"), "stderr tail is surfaced: {err}");
        assert_eq!(sent, Some(0), "nothing was shown, so a software retry is safe");
        assert!(started.elapsed().as_secs() < 10, "killed promptly: {:?}", started.elapsed());
    }

    /// Frames that did arrive are delivered, then a stall fails the stream with
    /// the count intact (so the caller knows replaying would repeat them).
    #[cfg(unix)]
    #[test]
    fn a_preview_stream_that_stalls_midway_delivers_what_it_had_then_fails() {
        let mut frames = 0;
        let mut sent = Some(0);
        let result = pump_preview(
            fake_stream(r"printf '\377\330one\377\331\377\330two\377\331'; exec sleep 60"),
            1.0,
            1000.0,
            short_timeouts(),
            &mut |f| {
                assert_eq!(&f.jpeg[..2], &[0xFF, 0xD8]);
                frames += 1;
                true
            },
            &mut sent,
        );
        let err = result.expect_err("stall").to_string();
        assert!(err.contains("stalled") && !err.contains("first frame"), "{err}");
        assert_eq!((frames, sent), (2, Some(2)));
    }

    /// A callback that declines a frame still stops playback cleanly, and a
    /// stream that simply ends is not an error.
    #[cfg(unix)]
    #[test]
    fn a_preview_stream_stops_on_request_and_ends_cleanly() {
        let mut sent = Some(0);
        let mut frames = 0;
        pump_preview(
            fake_stream(r"printf '\377\330a\377\331\377\330b\377\331'; exec sleep 60"),
            0.0,
            1000.0,
            short_timeouts(),
            &mut |_| {
                frames += 1;
                false
            },
            &mut sent,
        )
        .expect("a requested stop is not a failure");
        assert_eq!(frames, 1);

        let mut frames = 0;
        pump_preview(
            fake_stream(r"printf '\377\330a\377\331'"),
            0.0,
            1000.0,
            short_timeouts(),
            &mut |_| {
                frames += 1;
                true
            },
            &mut sent,
        )
        .expect("a stream that finishes is not a failure");
        assert_eq!(frames, 1);
    }

    #[test]
    fn a_smart_cropped_clip_crops_before_the_fit_so_cover_has_nothing_left_to_take() {
        let asset = av_asset(Uuid::new_v4(), 30.0); // 1920x1080
        let mut clip = make_clip(asset.id, 0.0, 5.0, 0.0);
        // What `smart_crop` writes for a 9:16 delivery, pulled left of centre.
        let map = SalienceMap::new(4, 2, vec![1.0, 1.0, 0.01, 0.01, 1.0, 1.0, 0.01, 0.01]);
        let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
        clip.transform.crop_left = crop.left;
        clip.transform.crop_right = crop.right;

        let fmt = ExportFormat {
            width: 1080,
            height: 1920,
            fit: Fit::Cover,
            ..ExportFormat::default()
        };
        let chain = video_clip_chain(&clip, &fmt, &ClipFx::default(), false, "c0");
        let cropped = chain.find("crop=w=iw*").expect("the smart crop is in the graph");
        let scaled = chain.find("scale=1080:1920").expect("the fit is in the graph");
        assert!(
            cropped < scaled,
            "the crop must pick the window before the fit scales it: {chain}"
        );
        // The window is off-centre — a plain Cover would have taken the middle.
        assert!(chain.contains(&format!("x=iw*{}", crop.left)), "{chain}");
        assert!(crop.left < 0.3, "the subject is left of centre: {crop:?}");
    }

    #[test]
    fn variant_files_land_beside_the_base_named_by_shape() {
        let v = ExportVariant::beside(Path::new("/renders/cut.mp4"), Delivery::new(1080, 1920, Fit::Cover));
        assert_eq!(v.output, PathBuf::from("/renders/cut-9x16.mp4"));
        let v = ExportVariant::beside(Path::new("cut"), Delivery::new(1080, 1080, Fit::Cover));
        assert_eq!(v.output, PathBuf::from("cut-1x1"));
        let v = ExportVariant::beside(Path::new("/r/my.cut.mov"), Delivery::new(1920, 1080, Fit::Contain));
        assert_eq!(v.output, PathBuf::from("/r/my.cut-16x9.mov"));
    }

    #[test]
    fn each_variant_renders_its_own_frame_and_its_own_crop() {
        let asset = av_asset(Uuid::new_v4(), 30.0); // 1920x1080
        let mut clip = make_clip(asset.id, 0.0, 5.0, 0.0);
        // Cut 9:16 with a left-leaning smart crop in the transform, and a
        // different framing kept for 1:1.
        clip.transform.crop_left = 0.05;
        clip.transform.crop_right = 0.6336;
        clip.framings.push(crate::model::Framing {
            aspect_w: 1,
            aspect_h: 1,
            crop_left: 0.3,
            crop_right: 0.2625,
            crop_top: 0.0,
            crop_bottom: 0.0,
        });
        let mut timeline = timeline_of(vec![video_track(vec![clip])]);
        timeline.format = Some(Delivery::new(1080, 1920, Fit::Cover));

        let graph_for = |d: Delivery| {
            let framed = timeline.for_delivery(d);
            let opts = ExportOptions {
                resolution: Some((d.width, d.height)),
                fit: d.fit,
                ..ExportOptions::default()
            };
            build_export_args(&framed, std::slice::from_ref(&asset), "out.mp4", &opts)
                .unwrap()
                .join(" ")
        };
        let vertical = graph_for(Delivery::new(1080, 1920, Fit::Cover));
        assert!(vertical.contains("scale=1080:1920"), "{vertical}");
        assert!(
            vertical.contains("x=iw*0.05"),
            "the project frame keeps its own crop: {vertical}"
        );

        let square = graph_for(Delivery::new(1080, 1080, Fit::Cover));
        assert!(square.contains("scale=1080:1080"), "{square}");
        assert!(
            square.contains("x=iw*0.3"),
            "the 1:1 delivery wears the 1:1 framing: {square}"
        );
        assert!(!square.contains("x=iw*0.05"), "{square}");

        // A shape nothing was framed for keeps the crop it has, and the
        // delivery's own fit.
        let wide = graph_for(Delivery::new(1920, 1080, Fit::Contain));
        assert!(wide.contains("scale=1920:1080"), "{wide}");
        assert!(wide.contains("x=iw*0.05"), "{wide}");
    }

    /// End to end: two files from one cut, each at its delivery frame. Run with
    /// `cargo test -p kerf-core --no-default-features -- --ignored renders_every`.
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn renders_every_variant_to_its_own_file() {
        let dir = std::env::temp_dir().join(format!("kerf-variants-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let media = dir.join("src.mp4");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc=size=640x360:rate=30:duration=2"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&media)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success(), "could not synthesize test media");

        let mut asset = av_asset(Uuid::new_v4(), 2.0);
        asset.path = media.to_string_lossy().into_owned();
        asset.streams = vec![video_stream(640, 360, 30.0)];
        let timeline = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, 2.0, 0.0)])]);

        let base = dir.join("cut.mp4");
        let variants = vec![
            ExportVariant::beside(&base, Delivery::new(180, 320, Fit::Cover)),
            ExportVariant::beside(&base, Delivery::new(200, 200, Fit::Cover)),
        ];
        let mut ticks = Vec::new();
        let (status, done) = render_variants(
            &timeline,
            &[asset],
            &variants,
            &ExportOptions::default(),
            &mut |p| ticks.push(p),
            &|| false,
        )
        .expect("render");
        assert_eq!((status, done), (RenderStatus::Completed, 2));
        for (v, (w, h)) in variants.iter().zip([(180, 320), (200, 200)]) {
            let probed = probe(&v.output).expect("probe the variant");
            let video = probed.streams.iter().find(|s| s.kind == StreamKind::Video).expect("video");
            assert_eq!((video.width, video.height), (Some(w), Some(h)), "{}", v.output.display());
        }
        assert!(ticks.iter().any(|p| p.variant == 1), "progress names the second variant");
        assert!(ticks.iter().all(|p| p.total == 2 && (0.0..=1.0).contains(&p.fraction)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End to end against the real `ffmpeg` binary: synthesize a 16:9 shot whose
    /// only content sits in the left third, and check the sampler finds it and
    /// the 9:16 crop keeps it — the case a centre crop gets wrong, and the whole
    /// reason smart crop exists. Not part of the normal (binary-free) run:
    /// `cargo test -p kerf-core --no-default-features -- --ignored samples_a_real`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn samples_a_real_shot_and_frames_its_subject() {
        let dir = std::env::temp_dir().join(format!("kerf-salience-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let media = dir.join("left.mp4");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc=size=360x240:rate=30:duration=3"])
            .args(["-f", "lavfi", "-i", "color=c=black:s=1280x720:rate=30:duration=3"])
            .args(["-filter_complex", "[1][0]overlay=x=80:y=240"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&media)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success(), "could not synthesize test media");

        let map = salience_map(&media, 0.0, 3.0).expect("sample");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!((map.cols, map.rows), (SALIENCE_COLS, SALIENCE_ROWS));
        assert!(map.cells.iter().any(|c| *c > 0.0), "the map is empty");

        let crop = map.crop_for(1280, 720, 1080.0 / 1920.0).expect("crops");
        assert!(crop.offset < 0.0, "the subject is on the left: {crop:?}");
        // The subject spans x = 80..440 of 1280, i.e. 0.0625..0.344 — a centre
        // crop (0.342..0.658) would miss it entirely.
        assert!(crop.left < 0.0625 && 1.0 - crop.right > 0.344, "{crop:?}");
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
            .status_bounded()
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
            let ok = command(&ffmpeg_bin()).args(&args).status_bounded().expect("run ffmpeg");
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
            .status_bounded()
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
                .status_bounded()
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

    /// A piecewise position expression contains commas, and the filtergraph
    /// parser treats an unquoted comma as the end of the filter — so a clip with
    /// position keyframes, or an *animated text overlay*, made ffmpeg fail the
    /// whole render with `No such filter`. Nothing caught it: every unit test
    /// above asserts on the graph string, and the graph string looked right.
    ///
    /// Both are on the default path — the Text overlays style chips animate an
    /// overlay's opacity, which puts it on the keyframed branch — so this renders
    /// one of each and only asks that ffmpeg accept the graph.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored animated_positions`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn animated_positions_survive_the_filtergraph_parser() {
        let dir = std::env::temp_dir().join(format!("kerf-anim-parse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let media = dir.join("src.mp4");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc=size=320x180:rate=30:duration=2"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&media)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());

        let mut asset = av_asset(Uuid::new_v4(), 2.0);
        asset.path = media.to_string_lossy().into_owned();
        asset.streams = vec![video_stream(320, 180, 30.0)];

        let mut clip = make_clip(asset.id, 0.0, 2.0, 0.0);
        clip.keyframes = vec![
            crate::model::Keyframe {
                time: 0.0,
                scale: 1.0,
                pos_x: -0.2,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
                easing: Default::default(),
            },
            crate::model::Keyframe {
                time: 2.0,
                scale: 1.0,
                pos_x: 0.2,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
                easing: Default::default(),
            },
        ];
        let mut timeline = single(vec![clip]);
        // An overlay animated the way the Text overlays style chips animate one.
        let mut overlay = TextOverlay::new("Kerf".to_string(), 0.0, 2.0);
        overlay.keyframes = vec![
            crate::model::TextKeyframe {
                time: 0.0,
                pos_x: 0.5,
                pos_y: 0.5,
                opacity: 0.0,
            },
            crate::model::TextKeyframe {
                time: 1.0,
                pos_x: 0.5,
                pos_y: 0.8,
                opacity: 1.0,
            },
        ];
        timeline.overlays = vec![overlay];

        let out = dir.join("anim.mp4");
        let opts = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            include_audio: false,
            ..Default::default()
        };
        let rendered = render_with(&timeline, &[asset], &out, &opts);
        let ok = rendered.is_ok() && out.exists();
        let err = rendered.err().map(|e| e.to_string()).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ok, "an animated clip and overlay must render: {err}");
    }

    /// A mask is only worth anything if a lower track really shows through it,
    /// which no assertion on the graph string can establish. Black over white,
    /// masked to a hard-edged ellipse: the middle must come out black and the
    /// corners white.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored a_mask_really`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_mask_really_lets_the_track_below_show_through() {
        let dir = std::env::temp_dir().join(format!("kerf-mask-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mk = |name: &str, color: &str| -> PathBuf {
            let out = dir.join(name);
            let ok = command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(["-f", "lavfi", "-i", &format!("color=c={color}:s=320x180:r=30:d=2")])
                .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
                .arg(&out)
                .status_bounded()
                .expect("run ffmpeg");
            assert!(ok.success());
            out
        };
        let (white, black) = (mk("white.mp4", "white"), mk("black.mp4", "black"));
        let mut lower = av_asset(Uuid::new_v4(), 2.0);
        lower.path = white.to_string_lossy().into_owned();
        lower.streams = vec![video_stream(320, 180, 30.0)];
        let mut upper = av_asset(Uuid::new_v4(), 2.0);
        upper.path = black.to_string_lossy().into_owned();
        upper.streams = vec![video_stream(320, 180, 30.0)];

        let mut top = make_clip(upper.id, 0.0, 2.0, 0.0);
        top.mask = Some(crate::model::Mask {
            shape: crate::model::MaskShape::Ellipse,
            x: 0.5,
            y: 0.5,
            width: 0.5,
            height: 0.5,
            feather: 0.0,
            inverted: false,
        });
        let timeline = timeline_of(vec![
            video_track(vec![make_clip(lower.id, 0.0, 2.0, 0.0)]),
            video_track(vec![top]),
        ]);

        let out = dir.join("masked.mp4");
        let opts = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            include_audio: false,
            ..Default::default()
        };
        render_with(&timeline, &[lower, upper], &out, &opts).expect("export");

        // Mean luma of a 20x20 patch at (x, y) of the first frame.
        let patch = |x: u32, y: u32| -> f64 {
            let raw = dir.join("patch.raw");
            let ok = command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .arg("-i")
                .arg(&out)
                .args(["-vf", &format!("crop=20:20:{x}:{y}"), "-frames:v", "1"])
                .args(["-f", "rawvideo", "-pix_fmt", "gray"])
                .arg(&raw)
                .status_bounded()
                .expect("run ffmpeg");
            assert!(ok.success());
            let bytes = std::fs::read(&raw).expect("raw");
            bytes.iter().map(|b| *b as f64).sum::<f64>() / bytes.len() as f64
        };
        let (middle, corner) = (patch(150, 80), patch(0, 0));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(middle < 60.0, "inside the mask the upper clip is kept, got {middle}");
        assert!(corner > 180.0, "outside it the lower track shows through, got {corner}");
    }

    #[test]
    fn a_seek_is_spelled_to_the_microsecond() {
        assert_eq!(seek_arg(1.0006), "1.000600");
        assert_eq!(seek_arg(7.0), "7.000000");
        assert_eq!(seek_arg(0.0), "0.000000");
        assert_eq!(seek_arg(12.3456789), "12.345679");
    }

    /// A frame at 1.0006 s is returned by `-ss 1.0006` and skipped by the
    /// spelling `1.001`: on a fine time base the still has to say the time to the
    /// microsecond. The clip is a 10 fps ramp (frame `n` is luma `16 + 8n`) in
    /// which every frame from the 10th on sits 0.6 ms late, written to an mp4
    /// with a 1/10000 time base (the encoder's too, which would otherwise round
    /// every timestamp to a tenth of a second) so the offset survives the container.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored fine_time_base`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_still_picks_the_frame_at_a_fine_time_base_second() {
        let dir = std::env::temp_dir().join(format!("kerf-fine-ss-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let media = dir.join("ramp.mp4");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "color=c=gray:s=64x64:r=10:d=2,format=yuv420p,geq=lum='16+8*N':cb=128:cr=128,\
                 settb=1/10000,setpts='PTS+if(gte(N,10),6,0)'",
            ])
            .args(["-c:v", "libx264", "-crf", "8", "-g", "1", "-bf", "0", "-pix_fmt", "yuv420p"])
            .args([
                "-fps_mode",
                "passthrough",
                "-enc_time_base",
                "1/10000",
                "-video_track_timescale",
                "10000",
            ])
            .arg(&media)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());

        // Which ramp frame a decode at `-ss <ss>` returns, read off its first luma sample.
        let frame_at = |ss: &str| -> u32 {
            let out = command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-ss", ss, "-i"])
                .arg(&media)
                .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"])
                .stdin(Stdio::null())
                .output()
                .expect("run ffmpeg");
            assert!(out.status.success() && !out.stdout.is_empty());
            ((f64::from(out.stdout[0]) - 16.0) / 8.0).round() as u32
        };
        // The premise, on this ffmpeg: the exact time lands on frame 10, the
        // millisecond rounding of it skips to frame 11.
        assert_eq!(frame_at("1.000600"), 10, "the fixture has no frame at 1.0006");
        assert_eq!(frame_at("1.001"), 11, "this ffmpeg does not skip a frame on `-ss 1.001`");

        let mut asset = av_asset(Uuid::new_v4(), 2.0);
        asset.path = media.to_string_lossy().into_owned();
        asset.streams = vec![video_stream(64, 64, 10.0)];
        // Source 1.0006 at timeline 0: the still at t=0 is that source second.
        let timeline = single(vec![make_clip(asset.id, 1.0006, 1.9, 0.0)]);
        let args = build_still_args(
            &timeline,
            std::slice::from_ref(&asset),
            &ExportOptions::default(),
            0.0,
            64,
            None,
            &StillOutput::RgbPipe,
        )
        .expect("still args");
        let rgb = run_still(
            &timeline,
            std::slice::from_ref(&asset),
            &ExportOptions::default(),
            0.0,
            64,
            None,
            &StillOutput::RgbPipe,
        )
        .expect("render the still");
        let _ = std::fs::remove_dir_all(&dir);
        // Full-range grey of luma 16 + 8n is 255 * 8n / 219: 93 for frame 10, 102 for 11.
        let grey = f64::from(rgb[0]);
        let frame = (grey * 219.0 / 255.0 / 8.0).round() as u32;
        assert_eq!(
            frame, 10,
            "the still showed ramp frame {frame} (grey {grey}), not the one at 1.0006 s"
        );
        assert!(args.join(" ").contains("-ss 1.000600 -i "), "{args:?}");
    }

    /// A bare `%` used to be a drawtext configuration error that silently
    /// blanked the whole overlay — ffmpeg still exits 0, so nothing above can
    /// tell a rendered "50% OFF" from a plain black frame. Render both and
    /// compare mean luma.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored percent_overlay`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn percent_overlay_is_not_silently_blanked() {
        let dir = std::env::temp_dir().join(format!("kerf-percent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let media = dir.join("src.mp4");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=c=black:s=640x360:r=30:d=1"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&media)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());

        let mut asset = av_asset(Uuid::new_v4(), 1.0);
        asset.path = media.to_string_lossy().into_owned();
        asset.streams = vec![video_stream(640, 360, 30.0)];

        let mut timeline = single(vec![make_clip(asset.id, 0.0, 1.0, 0.0)]);
        let mut overlay = TextOverlay::new("50% OFF", 0.0, 1.0);
        overlay.size = 0.3;
        timeline.overlays = vec![overlay];

        let out = dir.join("percent.mp4");
        let opts = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            include_audio: false,
            ..Default::default()
        };
        render_with(&timeline, &[asset], &out, &opts).expect("export");

        // Mean luma of the first frame.
        let mean_luma = |path: &Path| -> f64 {
            let raw = dir.join("frame.raw");
            let ok = command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .arg("-i")
                .arg(path)
                .args(["-frames:v", "1"])
                .args(["-f", "rawvideo", "-pix_fmt", "gray"])
                .arg(&raw)
                .status_bounded()
                .expect("run ffmpeg");
            assert!(ok.success());
            let bytes = std::fs::read(&raw).expect("raw");
            bytes.iter().map(|b| *b as f64).sum::<f64>() / bytes.len() as f64
        };
        let (blank, text) = (mean_luma(&media), mean_luma(&out));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(blank < 2.0, "control should be plain black, got {blank}");
        assert!(
            text > blank + 5.0,
            "\"50% OFF\" should be visible over black: {text} vs blank {blank}"
        );
    }

    /// A fade on a clip that does not start the timeline. The chain's `setpts`
    /// has already put the frames on the timeline, so a fade timed from 0
    /// blacked out the whole of a later clip that had a fade-out and played a
    /// later clip's fade-in before the clip existed.
    #[test]
    fn a_later_clips_fades_are_timed_from_where_it_sits() {
        let asset = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
        let a = make_clip(asset.id, 0.0, 5.0, 0.0);
        let mut b = make_clip(asset.id, 0.0, 4.0, 5.0);
        b.fade_in = 0.5;
        b.fade_out = 1.0;
        let g = graph_of(&single(vec![a, b]), &[asset]);
        assert!(g.contains("fade=t=in:st=5:d=0.5,fade=t=out:st=8:d=1,"), "{g}");
        // Audio is re-based to the clip before `adelay` moves it, so its fades stay clip-local.
        assert!(g.contains("afade=t=in:st=0:d=0.5,afade=t=out:st=3:d=1,"), "{g}");
    }

    /// The same, rendered: the middle of a later clip with a fade-out must be
    /// picture, not black.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored later_clip_fade`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_later_clip_fade_out_does_not_black_out_the_clip() {
        let dir = std::env::temp_dir().join(format!("kerf-fade-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let media = dir.join("gray.mp4");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=c=gray:s=320x180:r=25:d=10"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&media)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());

        let mut asset = av_asset(Uuid::new_v4(), 10.0);
        asset.path = media.to_string_lossy().into_owned();
        asset.streams = vec![video_stream(320, 180, 25.0)];
        let a = make_clip(asset.id, 0.0, 3.0, 0.0);
        let mut b = make_clip(asset.id, 3.0, 7.0, 3.0);
        b.fade_in = 0.5;
        b.fade_out = 1.0;
        let timeline = single(vec![a, b]);

        let out = dir.join("faded.mp4");
        let opts = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            include_audio: false,
            ..Default::default()
        };
        render_with(&timeline, &[asset], &out, &opts).expect("export");

        let luma_at = |t: f64| -> f64 {
            let raw = dir.join(format!("frame-{t}.raw"));
            let ok = command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(["-ss", &t.to_string()])
                .arg("-i")
                .arg(&out)
                .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray"])
                .arg(&raw)
                .status_bounded()
                .expect("run ffmpeg");
            assert!(ok.success());
            let bytes = std::fs::read(&raw).expect("raw");
            bytes.iter().map(|b| *b as f64).sum::<f64>() / bytes.len() as f64
        };
        let (first, fading_in, middle, tail) = (luma_at(1.0), luma_at(3.1), luma_at(4.5), luma_at(6.9));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(first > 100.0, "first clip is plain gray: {first}");
        assert!(middle > 100.0, "a later clip's fade-out must not black it out: {middle}");
        assert!(
            fading_in < middle - 30.0,
            "the fade-in starts dark at the cut: {fading_in} vs {middle}"
        );
        assert!(tail < middle - 30.0, "the fade-out darkens the tail: {tail} vs {middle}");
    }

    /// A slide and a push look identical in the graph builder's assertions —
    /// both put the incoming clip a frame away and walk it home — and differ
    /// only in whether the *outgoing* clip moves. Nothing above can tell them
    /// apart, so this renders both and looks at the pixels.
    ///
    /// The outgoing source carries a white stripe down its left edge. Halfway
    /// through the transition a slide has left it where it was (the stripe is
    /// still on screen); a push has carried it half a frame left (the stripe has
    /// gone with it).
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored slide_and_push`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn slide_and_push_really_move_the_picture() {
        let dir = std::env::temp_dir().join(format!("kerf-motion-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        // Outgoing: black with a white stripe down the left edge. Incoming: gray.
        let striped = dir.join("striped.mp4");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=c=black:s=640x360:r=30:d=4"])
            .args(["-vf", "drawbox=x=0:y=0:w=64:h=360:color=white:t=fill"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&striped)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());
        let plain = dir.join("gray.mp4");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", "color=c=gray:s=640x360:r=30:d=4"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&plain)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());

        let mut out_asset = av_asset(Uuid::new_v4(), 4.0);
        out_asset.path = striped.to_string_lossy().into_owned();
        out_asset.streams = vec![video_stream(640, 360, 30.0)];
        let mut in_asset = av_asset(Uuid::new_v4(), 4.0);
        in_asset.path = plain.to_string_lossy().into_owned();
        in_asset.streams = vec![video_stream(640, 360, 30.0)];

        // Mean luma of the leftmost 32 px, one frame into the middle of the
        // transition (the cut is at 2.0s, the transition runs a second).
        let stripe_luma = |file: &Path| -> f64 {
            let raw = dir.join("stripe.raw");
            let ok = command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(["-ss", "2.5"])
                .arg("-i")
                .arg(file)
                .args(["-vf", "crop=32:360:0:0", "-frames:v", "1"])
                .args(["-f", "rawvideo", "-pix_fmt", "gray"])
                .arg(&raw)
                .status_bounded()
                .expect("run ffmpeg");
            assert!(ok.success());
            let bytes = std::fs::read(&raw).expect("raw");
            bytes.iter().map(|b| *b as f64).sum::<f64>() / bytes.len() as f64
        };

        let render = |kind: TransitionKind, name: &str| -> PathBuf {
            let a = make_clip(out_asset.id, 0.0, 2.0, 0.0);
            let mut b = make_clip(in_asset.id, 0.0, 2.0, 2.0);
            b.transition_in = Some(crate::model::Transition { kind, duration: 1.0 });
            let timeline = single(vec![a, b]);
            let out = dir.join(name);
            let opts = ExportOptions {
                container: Container::Mp4,
                video_codec: Some("libx264".into()),
                include_audio: false,
                ..Default::default()
            };
            render_with(&timeline, &[out_asset.clone(), in_asset.clone()], &out, &opts).expect("export");
            out
        };
        let slid = stripe_luma(&render(TransitionKind::SlideLeft, "slide.mp4"));
        let pushed = stripe_luma(&render(TransitionKind::PushLeft, "push.mp4"));
        let _ = std::fs::remove_dir_all(&dir);

        assert!(slid > 180.0, "a slide leaves the outgoing clip where it was, got luma {slid}");
        assert!(
            pushed < 60.0,
            "a push carries the outgoing clip out of frame, got luma {pushed}"
        );
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
            .status_bounded()
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

    // ---- rotation, colour and frame rate at probe time ----------------------

    fn probe_json(video: &str) -> ProbeResult {
        let json = format!(
            r#"{{"streams":[{{"index":0,"codec_type":"video","codec_name":"hevc",{video}}}],"format":{{"duration":"10.0"}}}}"#
        );
        probe_from_json(serde_json::from_str(&json).expect("json"), None)
    }

    #[test]
    fn the_pixel_format_is_recorded_for_video_and_tells_alpha_apart() {
        let plain = probe_json(r#""width":1280,"height":720,"pix_fmt":"yuv420p""#);
        assert_eq!(plain.streams[0].pix_fmt.as_deref(), Some("yuv420p"));
        assert_eq!(plain.streams[0].has_alpha(), Some(false));
        let alpha = probe_json(r#""width":1280,"height":720,"pix_fmt":"yuva420p""#);
        assert_eq!(alpha.streams[0].has_alpha(), Some(true));
        // Absent from the JSON (an old ffprobe, an audio-only file): not known,
        // which is not the same as "no alpha".
        let unknown = probe_json(r#""width":1280,"height":720"#);
        assert_eq!(unknown.streams[0].pix_fmt, None);
        assert_eq!(unknown.streams[0].has_alpha(), None);
    }

    #[test]
    fn the_probe_reads_a_policy_off_two_reds() {
        // The tag did not reach the conversion: the same red either way.
        assert_eq!(policy_from_reds(193, 193), Some(CompositeColorPolicy::FixedBt601));
        assert_eq!(policy_from_reds(194, 192), Some(CompositeColorPolicy::FixedBt601));
        // It did: BT.709 reads this picture's red about 18 levels higher.
        assert_eq!(policy_from_reds(211, 193), Some(CompositeColorPolicy::BottomLayerTag));
        // Neither: not a difference the probe knows how to read.
        assert_eq!(policy_from_reds(200, 193), None);
    }

    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn the_composite_policy_is_measured_from_the_graph_not_guessed() {
        // Whatever this ffmpeg is, the probe must read *something* off the real
        // graph (a `None` would mean it fell back to `Unknown`).
        let (ffmpeg, ffprobe) = (ffmpeg_bin(), ffprobe_bin());
        let deadline = Instant::now() + POLICY_PROBE_TIMEOUT;
        let started = Instant::now();
        let measured = measure_composite_color_policy(&ffmpeg, &ffprobe, POLICY_PROBE_TIMEOUT);
        eprintln!(
            "composite colour policy of {ffmpeg}: {measured:?} in {:?} (reds tagged {:?} / untagged {:?})",
            started.elapsed(),
            probe_composite_red(&ffmpeg, &ffprobe, true, deadline),
            probe_composite_red(&ffmpeg, &ffprobe, false, deadline)
        );
        assert!(measured.is_some(), "the probe could not read the still graph's matrix");
        assert_eq!(composite_color_policy(), measured.unwrap());
    }

    #[test]
    #[ignore = "needs ffmpeg"]
    #[cfg(unix)]
    fn a_probe_whose_tag_does_not_survive_measures_nothing() {
        // Standing in for an `ffprobe` that reports no matrix for the tagged clip
        // (`echo` prints its arguments, which is not `bt709`): the measurement is
        // refused rather than read as "FFmpeg 6", which is what two identical
        // clips would say on any build.
        let ffmpeg = ffmpeg_bin();
        assert!(measure_composite_color_policy(&ffmpeg, "echo", POLICY_PROBE_TIMEOUT).is_none());
        assert!(measure_composite_color_policy(&ffmpeg, &ffprobe_bin(), POLICY_PROBE_TIMEOUT).is_some());
    }

    /// `crop` rounds its window to the chroma grid of the pixel format it is given,
    /// and [`crate::model::pix_fmt_subsampling`] says which grid that is: measured
    /// here by cropping a 33x35 window out of a picture in each format and reading
    /// the size ffmpeg reports for what came out.
    #[test]
    #[ignore = "needs ffmpeg"]
    fn the_subsampling_table_matches_ffmpegs_crop() {
        let bin = ffmpeg_bin();
        let supported = command(&bin)
            .args(["-hide_banner", "-v", "error", "-pix_fmts"])
            .stdin(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let has = |fmt: &str| supported.lines().any(|l| l.split_whitespace().nth(1) == Some(fmt));
        let mut checked = 0;
        for fmt in [
            "yuv420p",
            "yuvj420p",
            "yuv420p9le",
            "yuv420p10le",
            "yuv420p12le",
            "yuv420p16le",
            "nv12",
            "nv21",
            "p010le",
            "p016le",
            "yuv422p",
            "yuvj422p",
            "yuv422p10le",
            "yuv422p16le",
            "nv16",
            "p210le",
            "yuv440p",
            "yuvj440p",
            "yuv440p10le",
            "yuv411p",
            "yuv410p",
            "yuv444p",
            "yuvj444p",
            "yuv444p9le",
            "yuv444p16le",
            "nv24",
            "nv42",
            "p410le",
            "gray",
            "gray10le",
            "gray16le",
            "gbrp",
            "gbrp10le",
            "gbrp16le",
            "rgb24",
            "bgr24",
            "rgb0",
            "bgr0",
            "0rgb",
            "0bgr",
            "rgb48le",
            "bgr48le",
        ] {
            if !has(fmt) {
                continue;
            }
            let want = crate::model::pix_fmt_subsampling(fmt).unwrap_or_else(|| panic!("{fmt} is not in the table"));
            let output = command(&bin)
                .args(["-hide_banner", "-v", "info", "-f", "lavfi", "-i"])
                .arg(format!("color=c=gray:s=128x128:r=1,format={fmt}"))
                .args(["-vf", "crop=w=33:h=35:x=1:y=1,showinfo", "-frames:v", "1", "-f", "null", "-"])
                .stdin(Stdio::null())
                .output()
                .expect("run ffmpeg");
            let log = String::from_utf8_lossy(&output.stderr);
            let size = log
                .lines()
                .filter(|l| l.contains("Parsed_showinfo"))
                .find_map(|l| {
                    let rest = l.split(" s:").nth(1)?;
                    let dims = rest.split_whitespace().next()?;
                    let (w, h) = dims.split_once('x')?;
                    Some((w.parse::<i64>().ok()?, h.parse::<i64>().ok()?))
                })
                .unwrap_or_else(|| panic!("{fmt}: no showinfo size in {log}"));
            assert_eq!(
                size,
                (want.round_w(33), want.round_h(35)),
                "{fmt}: crop gave {size:?}, the table says {want:?}"
            );
            // ...and the origin is rounded to the same grid: a 32x32 window at (1, 1)
            // is the window at the rounded origin, and where an axis is not rounded
            // it is *not* the window one pixel back.
            let md5_at = |x: i64, y: i64| -> String {
                let output = command(&bin)
                    .args(["-hide_banner", "-v", "error", "-f", "lavfi", "-i"])
                    .arg(format!("testsrc2=s=128x128:r=1,format={fmt}"))
                    .args([
                        "-vf",
                        &format!("crop=w=32:h=32:x={x}:y={y}"),
                        "-frames:v",
                        "1",
                        "-f",
                        "framemd5",
                        "-",
                    ])
                    .stdin(Stdio::null())
                    .output()
                    .expect("run ffmpeg");
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .find(|l| !l.starts_with('#') && !l.trim().is_empty())
                    .and_then(|l| l.split(',').next_back())
                    .map(|m| m.trim().to_string())
                    .unwrap_or_else(|| panic!("{fmt}: no frame md5 for crop at ({x}, {y})"))
            };
            assert_eq!(
                md5_at(1, 1),
                md5_at(want.round_w(1), want.round_h(1)),
                "{fmt}: the origin (1, 1) is not rounded to {want:?}"
            );
            if want.log2_w == 0 {
                assert_ne!(md5_at(1, 1), md5_at(0, 1), "{fmt}: x was rounded, the table says it is not");
            }
            if want.log2_h == 0 {
                assert_ne!(md5_at(1, 1), md5_at(1, 0), "{fmt}: y was rounded, the table says it is not");
            }
            checked += 1;
        }
        assert!(checked >= 20, "only {checked} formats were available to check");
    }

    #[test]
    fn a_probe_that_cannot_run_measures_nothing() {
        let t = std::time::Duration::from_secs(5);
        assert!(measure_composite_color_policy("/nonexistent/ffmpeg", "/nonexistent/ffprobe", t).is_none());
        // A binary that runs and fails.
        #[cfg(unix)]
        assert!(measure_composite_color_policy("false", "false", t).is_none());
    }

    #[test]
    #[cfg(unix)]
    fn a_child_that_never_exits_is_killed_at_the_deadline() {
        let started = Instant::now();
        let out = run_piped_until(
            Command::new("sh").args(["-c", "sleep 30"]),
            b"input".to_vec(),
            Instant::now() + std::time::Duration::from_millis(300),
        );
        assert!(out.is_none());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        // One that finishes in time hands back its output, with the input fed in.
        let out = run_piped_until(
            &mut Command::new("cat"),
            b"input".to_vec(),
            Instant::now() + std::time::Duration::from_secs(10),
        );
        assert_eq!(out.as_deref(), Some(&b"input"[..]));
        // ...and one that fails hands back nothing.
        let out = run_piped_until(
            &mut Command::new("false"),
            Vec::new(),
            Instant::now() + std::time::Duration::from_secs(10),
        );
        assert!(out.is_none());
    }

    #[test]
    fn a_measured_policy_is_kept_and_a_failed_probe_is_not() {
        let state = Mutex::new(PolicyProbe::new());
        let now = Instant::now();
        let runs = std::cell::Cell::new(0);
        let counter = &runs;
        let measure = |answer: Option<CompositeColorPolicy>| {
            move || {
                counter.set(counter.get() + 1);
                answer
            }
        };
        // A probe that fails answers `Unknown` and is not remembered...
        assert_eq!(cached_policy(&state, now, measure(None)), CompositeColorPolicy::Unknown);
        assert_eq!(runs.get(), 1);
        // ...nor run again before its backoff has passed (a broken ffmpeg is not
        // respawned for every frame)...
        assert_eq!(
            cached_policy(&state, now, measure(Some(CompositeColorPolicy::FixedBt601))),
            CompositeColorPolicy::Unknown
        );
        assert_eq!(runs.get(), 1, "the backoff holds the probe off");
        // ...but it is retried once the backoff is over, and a policy it then
        // measures is the answer for good.
        state.lock().unwrap().retry_at = Some(Instant::now());
        assert_eq!(
            cached_policy(
                &state,
                Instant::now() + std::time::Duration::from_secs(1),
                measure(Some(CompositeColorPolicy::BottomLayerTag))
            ),
            CompositeColorPolicy::BottomLayerTag
        );
        assert_eq!(runs.get(), 2);
        assert_eq!(
            cached_policy(&state, now, measure(Some(CompositeColorPolicy::FixedBt601))),
            CompositeColorPolicy::BottomLayerTag
        );
        assert_eq!(runs.get(), 2, "a measured policy is never measured again");
    }

    #[test]
    fn the_backoff_after_a_failed_probe_doubles_up_to_five_minutes() {
        let secs = |n| policy_probe_backoff(n).as_secs();
        assert_eq!((secs(1), secs(2), secs(3), secs(4)), (5, 10, 20, 40));
        assert_eq!((secs(7), secs(8), secs(100)), (300, 300, 300));
    }

    #[test]
    fn a_failed_probe_leaves_bt709_footage_to_ffmpeg() {
        // The reviewer's repro: an ffmpeg wrapper that fails the probe. The answer
        // used to be `BottomLayerTag`, which draws a BT.709 clip as BT.709 and is
        // 30 dB / 31 levels wrong on every FFmpeg 6.
        let state = Mutex::new(PolicyProbe::new());
        let policy = cached_policy(&state, Instant::now(), || {
            measure_composite_color_policy(
                "/nonexistent/ffmpeg",
                "/nonexistent/ffprobe",
                std::time::Duration::from_secs(5),
            )
        });
        assert_eq!(policy, CompositeColorPolicy::Unknown);
        let clip_of = |color_space: Option<&str>| {
            let asset = Asset {
                id: uuid::Uuid::new_v4(),
                path: "/media/a.mp4".to_string(),
                name: "a".to_string(),
                duration: 5.0,
                streams: vec![StreamInfo {
                    index: 0,
                    kind: StreamKind::Video,
                    codec: "h264".to_string(),
                    width: Some(640),
                    height: Some(360),
                    fps: Some(30.0),
                    sample_rate: None,
                    channels: None,
                    image: false,
                    projection: None,
                    rotation: 0,
                    color_transfer: None,
                    color_primaries: None,
                    pix_fmt: Some("yuv420p".to_string()),
                    color_space: color_space.map(str::to_string),
                }],
                imported_at: chrono::Utc::now(),
                source_paths: Vec::new(),
                voiceover: None,
            };
            let timeline = Timeline {
                tracks: vec![crate::model::Track {
                    clips: vec![Clip::new(asset.id, 0.0, 5.0, 0.0)],
                    ..crate::model::Track::new(StreamKind::Video, "V1")
                }],
                overlays: Vec::new(),
                markers: Vec::new(),
                format: None,
                master: Default::default(),
            };
            crate::render_plan::RenderPlan::at(&timeline, &[asset], &ExportOptions::default(), 1.0, policy).unwrap()
        };
        let plan = clip_of(Some("bt709"));
        assert!(!plan.gpu_supported(), "a BT.709 clip must go to FFmpeg");
        assert!(plan.unsupported_reasons().iter().any(|r| r.contains("could not be measured")));
        // What every FFmpeg draws the same way is still drawn.
        let plan = clip_of(None);
        assert!(plan.gpu_supported(), "{:?}", plan.unsupported_reasons());
        assert_eq!(plan.canvas.matrix, crate::render_plan::YuvMatrix::Bt601);
    }

    #[test]
    fn the_declared_ycbcr_matrix_is_recorded_when_there_is_one() {
        let tagged = probe_json(r#""width":1280,"height":720,"color_space":"bt709""#);
        assert_eq!(tagged.streams[0].color_space.as_deref(), Some("bt709"));
        // ffprobe says "unknown" for none; that is no tag, not a tag named unknown.
        let untagged = probe_json(r#""width":1280,"height":720,"color_space":"unknown""#);
        assert_eq!(untagged.streams[0].color_space, None);
        assert_eq!(probe_json(r#""width":1280,"height":720"#).streams[0].color_space, None);
    }

    #[test]
    fn a_turned_phone_clip_probes_at_its_displayed_size() {
        for rotation in [90, -90, 270] {
            let probed = probe_json(&format!(
                r#""width":1920,"height":1080,"side_data_list":[{{"side_data_type":"Display Matrix","rotation":{rotation}}}]"#
            ));
            let v = &probed.streams[0];
            assert_eq!((v.width, v.height), (Some(1080), Some(1920)), "rotation {rotation}");
            assert_eq!(v.rotation, if rotation == 270 { -90 } else { rotation as i16 });
        }
        // Upside-down is still landscape; no rotation is no rotation.
        let flipped =
            probe_json(r#""width":1920,"height":1080,"side_data_list":[{"side_data_type":"Display Matrix","rotation":180}]"#);
        assert_eq!(
            (flipped.streams[0].width, flipped.streams[0].height),
            (Some(1920), Some(1080))
        );
        assert_eq!(flipped.streams[0].rotation, 180);
        let plain = probe_json(r#""width":1920,"height":1080"#);
        assert_eq!((plain.streams[0].width, plain.streams[0].height), (Some(1920), Some(1080)));
        assert_eq!(plain.streams[0].rotation, 0);
    }

    #[test]
    fn an_old_ffprobe_reports_the_turn_as_a_clockwise_tag() {
        let probed = probe_json(r#""width":1920,"height":1080,"tags":{"rotate":"90"}"#);
        let v = &probed.streams[0];
        assert_eq!((v.width, v.height), (Some(1080), Some(1920)));
        assert_eq!(v.rotation, -90, "clockwise 90 is -90 in the counter-clockwise convention");
        // The side data wins when both are present.
        let both = probe_json(
            r#""width":1920,"height":1080,"tags":{"rotate":"90"},"side_data_list":[{"side_data_type":"Display Matrix","rotation":90}]"#,
        );
        assert_eq!(both.streams[0].rotation, 90);
    }

    #[test]
    fn a_display_matrix_reads_back_as_the_angle_ffprobe_reports() {
        const ONE: i32 = 65536;
        let w = 1 << 30;
        // The matrix of a real phone clip, which ffprobe reports as rotation 90.
        assert_eq!(matrix_rotation(&[0, -ONE, 0, ONE, 0, 0, 0, 0, w]), 90);
        assert_eq!(matrix_rotation(&[0, ONE, 0, -ONE, 0, 0, 0, 0, w]), -90);
        assert_eq!(matrix_rotation(&[ONE, 0, 0, 0, ONE, 0, 0, 0, w]), 0);
        assert_eq!(matrix_rotation(&[-ONE, 0, 0, 0, -ONE, 0, 0, 0, w]), 180);
        assert_eq!(matrix_rotation(&[0; 9]), 0, "a degenerate matrix is no rotation");
    }

    #[test]
    fn displayed_size_swaps_only_for_quarter_turns() {
        assert_eq!(displayed_size(Some(4), Some(3), 90), (Some(3), Some(4)));
        assert_eq!(displayed_size(Some(4), Some(3), -90), (Some(3), Some(4)));
        assert_eq!(displayed_size(Some(4), Some(3), 180), (Some(4), Some(3)));
        assert_eq!(displayed_size(Some(4), Some(3), 0), (Some(4), Some(3)));
        assert_eq!(displayed_size(None, Some(3), 90), (Some(3), None));
    }

    #[test]
    fn probe_records_the_hdr_transfer_and_ignores_unknown_tags() {
        let hlg = probe_json(r#""width":3840,"height":2160,"color_transfer":"arib-std-b67","color_primaries":"bt2020""#);
        let v = &hlg.streams[0];
        assert_eq!(v.color_transfer.as_deref(), Some("arib-std-b67"));
        assert_eq!(v.color_primaries.as_deref(), Some("bt2020"));
        assert_eq!(v.hdr(), Some(crate::model::Hdr::Hlg));
        let pq = probe_json(r#""width":3840,"height":2160,"color_transfer":"smpte2084","color_primaries":"bt2020""#);
        assert_eq!(pq.streams[0].hdr(), Some(crate::model::Hdr::Pq));
        let sdr = probe_json(r#""width":1920,"height":1080,"color_transfer":"bt709","color_primaries":"bt709""#);
        assert_eq!(sdr.streams[0].hdr(), None);
        let untagged = probe_json(r#""width":1920,"height":1080,"color_transfer":"unknown""#);
        assert_eq!(untagged.streams[0].color_transfer, None);
        assert_eq!(untagged.streams[0].hdr(), None);
    }

    #[test]
    fn a_jittery_vfr_clip_keeps_its_nominal_frame_rate() {
        // A phone clip that dropped frames keeps its nominal rate...
        assert_eq!(nominal_fps(Some(30.0), Some(25.75)), Some(30.0));
        assert_eq!(nominal_fps(Some(30000.0 / 1001.0), Some(29.9)), Some(30000.0 / 1001.0));
        // ...but a timestamp grid far finer than the footage does not become the
        // project's frame rate.
        assert_eq!(nominal_fps(Some(120.0), Some(30.07)), Some(30.0));
        assert_eq!(nominal_fps(Some(600.0), Some(29.97)), Some(29.97));
        assert_eq!(nominal_fps(Some(600.0), Some(41.3)), Some(41.3));
        assert_eq!(nominal_fps(Some(25.0), None), Some(25.0));
        assert_eq!(nominal_fps(None, Some(24.0)), Some(24.0));
        assert_eq!(nominal_fps(None, None), None);
        assert_eq!(nominal_fps(Some(0.0), Some(0.0)), None);
    }

    // ---- HDR → SDR -----------------------------------------------------------

    fn hlg_asset() -> Asset {
        let mut asset = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
        asset.path = "/media/hdr.mov".into();
        asset.streams[0].color_transfer = Some("arib-std-b67".into());
        asset.streams[0].color_primaries = Some("bt2020".into());
        asset
    }

    #[test]
    fn the_tonemap_chain_names_the_input_transfer_and_ends_in_sdr() {
        let hlg = tonemap_chain(Hdr::Hlg, true);
        assert!(
            hlg.starts_with("zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,"),
            "{hlg}"
        );
        assert!(hlg.contains("tonemap=tonemap=mobius"), "{hlg}");
        assert!(
            hlg.ends_with("zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"),
            "{hlg}"
        );
        assert!(tonemap_chain(Hdr::Pq, true).starts_with("zscale=tin=smpte2084:"));
        // No libzimg: still leaves BT.2020 and lands on 8-bit 4:2:0.
        let fallback = tonemap_chain(Hdr::Hlg, false);
        assert!(
            !fallback.contains("zscale") && fallback.contains("colorspace=all=bt709:iall=bt2020"),
            "{fallback}"
        );
        assert!(fallback.ends_with("format=yuv420p"));
    }

    #[test]
    fn an_hdr_clip_is_tonemapped_once_after_fps_and_before_colour() {
        let asset = hlg_asset();
        let mut clip = make_clip(asset.id, 0.0, 5.0, 0.0);
        clip.color.saturation = 1.3;
        let timeline = single(vec![clip.clone()]);
        let fx = transition_fx(&timeline, std::slice::from_ref(&asset));
        assert_eq!(fx[0].hdr, Some(Hdr::Hlg));
        let chain = video_clip_chain(&clip, &fmt_1080p(), &fx[0], false, "c0");
        assert_eq!(chain.matches("tonemap=tonemap").count(), 1, "{chain}");
        let (fps, tone, eq) = (
            chain.find("fps=").unwrap(),
            chain.find("zscale=tin=").unwrap(),
            chain.find("eq=").unwrap(),
        );
        assert!(
            chain.find("scale=1920").unwrap() < tone,
            "geometry first, on the cheap pixels: {chain}"
        );
        assert!(fps < tone && tone < eq, "{chain}");
        // SDR is untouched.
        let sdr = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c0");
        assert!(
            !sdr.contains("zscale") && !sdr.contains("tonemap") && !sdr.contains("colorspace"),
            "{sdr}"
        );
    }

    #[test]
    fn hdr_reaches_export_preview_stream_and_still_graphs_but_sdr_never_does() {
        let hdr = hlg_asset();
        let sdr = Asset {
            id: Uuid::new_v4(),
            path: "/media/sdr.mp4".into(),
            ..test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)])
        };
        let assets = vec![hdr.clone(), sdr.clone()];
        let timeline = single(vec![make_clip(hdr.id, 0.0, 5.0, 0.0), make_clip(sdr.id, 0.0, 5.0, 5.0)]);
        let count = |args: &[String]| args.join(" ").matches("tonemap=tonemap").count();

        let export = build_export_args(&timeline, &assets, "out.mp4", &ExportOptions::default()).unwrap();
        assert_eq!(count(&export), 1, "only the HDR clip: {export:?}");
        let stream = build_preview_args(&timeline, &assets, 1.0, 24.0, 960, 6).unwrap();
        assert_eq!(count(&stream), 1);
        let still = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 2.0, 640, 4).unwrap();
        assert_eq!(count(&still), 1);
        let sdr_still = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 7.0, 640, 4).unwrap();
        assert_eq!(count(&sdr_still), 0);

        // Through a proxy the asset is SDR, and nothing converts a second time.
        let proxied = vec![hdr.as_sdr_proxy(), sdr];
        let export = build_export_args(&timeline, &proxied, "out.mp4", &ExportOptions::default()).unwrap();
        assert_eq!(count(&export), 0);
    }

    #[test]
    fn single_frame_contact_sheet_and_proxy_decodes_tonemap_after_the_downscale() {
        let (args, _) = build_contact_sheet_args("/m/a.mov", 0.0, 8.0, 2, 2, 240, 5, Some("TONEMAP"));
        assert!(
            args.join(" ").contains("scale=240:-2:flags=bilinear,TONEMAP,tile=2x2"),
            "{args:?}"
        );
        let proxy = build_proxy_args("/in.mov", "/out.mp4", 3, 1280, "libx264", None, Some("TONEMAP"), None).join(" ");
        assert!(
            proxy.contains("scale='min(1280,iw)':-2:flags=bilinear,TONEMAP -c:v libx264"),
            "{proxy}"
        );
        // SDR stays byte-identical.
        let plain = build_proxy_args("/in.mov", "/out.mp4", 3, 1280, "libx264", None, None, None).join(" ");
        assert!(plain.contains("flags=bilinear -c:v libx264"), "{plain}");
    }

    // ---- phone / mirrorless footage, end to end ------------------------------

    /// Say why an end-to-end test has nothing to check on this ffmpeg.
    #[allow(clippy::print_stderr)]
    fn skip(why: &str) {
        eprintln!("{why}");
    }

    /// A scratch directory that is removed when the test ends, pass or fail.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("kerf-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            Scratch(dir)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Run `ffmpeg` with `args` (after the usual quiet flags); true when it exits 0.
    fn try_ffmpeg(args: &[&str]) -> bool {
        command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .stdin(Stdio::null())
            .status_bounded()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn run_ffmpeg(args: &[&str]) {
        assert!(try_ffmpeg(args), "ffmpeg {args:?} failed");
    }

    /// The first frame of `path` (optionally through `vf`) as `(width, height, rgb24)`,
    /// decoded by ffmpeg with autorotation on — the ground truth for "upright".
    fn rgb_frame(path: &Path, vf: Option<&str>) -> (usize, usize, Vec<u8>) {
        let mut cmd = command(&ffmpeg_bin());
        cmd.args(["-hide_banner", "-loglevel", "error", "-i"]).arg(path);
        if let Some(vf) = vf {
            cmd.args(["-vf", vf]);
        }
        let out = cmd
            .args([
                "-frames:v",
                "1",
                "-pix_fmt",
                "rgb24",
                "-f",
                "image2pipe",
                "-vcodec",
                "ppm",
                "pipe:1",
            ])
            .output()
            .expect("run ffmpeg");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let b = &out.stdout;
        let mut fields = Vec::new();
        let mut at = 0;
        while fields.len() < 4 {
            while b[at].is_ascii_whitespace() {
                at += 1;
            }
            let start = at;
            while !b[at].is_ascii_whitespace() {
                at += 1;
            }
            fields.push(String::from_utf8_lossy(&b[start..at]).into_owned());
        }
        let (w, h): (usize, usize) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
        (w, h, b[at + 1..].to_vec())
    }

    /// Which edge of the picture the pure-red bar sits on (`top` / `bottom` /
    /// `left` / `right`), judged by where the red pixels' centroid falls.
    fn red_bar_edge(w: usize, h: usize, rgb: &[u8]) -> &'static str {
        let (mut n, mut sx, mut sy) = (0.0, 0.0, 0.0);
        for y in 0..h {
            for x in 0..w {
                let p = &rgb[(y * w + x) * 3..][..3];
                if p[0] > 180 && p[1] < 90 && p[2] < 90 {
                    n += 1.0;
                    sx += x as f64;
                    sy += y as f64;
                }
            }
        }
        if n == 0.0 {
            return "none";
        }
        let (cx, cy) = (sx / n / w as f64 - 0.5, sy / n / h as f64 - 0.5);
        if cx.abs() > cy.abs() {
            if cx > 0.0 {
                "right"
            } else {
                "left"
            }
        } else if cy > 0.0 {
            "bottom"
        } else {
            "top"
        }
    }

    /// The probed asset for a file, as the import path would build it.
    fn probed_asset(path: &Path) -> Asset {
        let probed = probe(path).expect("probe");
        let mut asset = av_asset(Uuid::new_v4(), probed.duration);
        asset.path = path.to_string_lossy().into_owned();
        asset.streams = probed.streams;
        asset
    }

    fn ffprobe_field(path: &Path, stream: &str, entries: &str) -> String {
        let out = command(&ffprobe_bin())
            .args(["-v", "error", "-select_streams", stream, "-show_entries", entries])
            .args(["-of", "default=nw=1:nk=1"])
            .arg(path)
            .output()
            .expect("run ffprobe");
        String::from_utf8_lossy(&out.stdout).lines().collect::<Vec<_>>().join(",")
    }

    fn mp4_export_opts() -> ExportOptions {
        ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            ..Default::default()
        }
    }

    /// `src` remuxed with a display matrix turning it by `rotation` degrees, the
    /// way a phone marks a portrait recording. `None` when this ffmpeg can not
    /// write a display matrix at all (the test then has nothing to say).
    fn turned(dir: &Scratch, src: &Path, rotation: i32, name: &str) -> Option<PathBuf> {
        let out = dir.join(name);
        let (src, outs) = (src.to_str().unwrap(), out.to_str().unwrap());
        let rot = rotation.to_string();
        // FFmpeg 6+ takes the angle as an input option; older builds write the
        // matrix from a `rotate` tag (clockwise, the opposite sign).
        let neg = (-rotation).to_string();
        let wrote = try_ffmpeg(&["-display_rotation", &rot, "-i", src, "-c", "copy", outs])
            || try_ffmpeg(&["-i", src, "-c", "copy", "-metadata:s:v:0", &format!("rotate={neg}"), outs]);
        let dump = command(&ffprobe_bin())
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(&out)
            .output()
            .expect("run ffprobe");
        let dump = String::from_utf8_lossy(&dump.stdout);
        (wrote && (dump.contains("\"rotation\"") || dump.contains("\"rotate\""))).then_some(out)
    }

    /// A landscape clip with a red bar along the top of its coded frame, turned
    /// by [`turned`].
    fn rotated_clip(dir: &Scratch, rotation: i32) -> Option<PathBuf> {
        let base = dir.join("landscape.mp4");
        run_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "color=c=gray:s=640x360:r=30:d=2,drawbox=x=0:y=0:w=640:h=60:color=red:t=fill",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            base.to_str().unwrap(),
        ]);
        turned(dir, &base, rotation, "portrait.mp4")
    }

    /// A phone clip is a landscape sensor frame plus a rotation. Every ffmpeg
    /// decode turns it upright, so the probe must report the *displayed* size or
    /// the project frame, the fit and the crop maths all describe a picture that
    /// is not the one being rendered.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored rotated_phone_clip`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn rotated_phone_clip_probes_display_dims_and_exports_upright() {
        let dir = Scratch::new("rotated");
        let Some(media) = rotated_clip(&dir, -90) else {
            skip("skipped: this ffmpeg cannot write a display matrix");
            return;
        };
        let (rw, rh, ref_rgb) = rgb_frame(&media, None);
        assert_eq!((rw, rh), (360, 640), "the reference decode is upright");
        let want_edge = red_bar_edge(rw, rh, &ref_rgb);
        assert_ne!(want_edge, "top", "the rotation moved the bar off the top");

        let asset = probed_asset(&media);
        let video = asset.streams.iter().find(|s| s.kind == StreamKind::Video).unwrap();
        assert_eq!(
            (video.width, video.height),
            (Some(360), Some(640)),
            "probe reports display dims"
        );
        assert_ne!(video.rotation, 0);

        let timeline = single(vec![make_clip(asset.id, 0.0, 2.0, 0.0)]);
        assert_eq!(delivery_frame(&timeline, std::slice::from_ref(&asset)), (360, 640));

        let out = dir.join("export.mp4");
        render_with(&timeline, std::slice::from_ref(&asset), &out, &mp4_export_opts()).expect("export");
        let (w, h, rgb) = rgb_frame(&out, None);
        assert_eq!((w, h), (360, 640), "the file is portrait, not a letterboxed landscape");
        assert_eq!(red_bar_edge(w, h, &rgb), want_edge, "and upright");

        let jpeg = timeline_frame(
            &timeline,
            std::slice::from_ref(&asset),
            &ExportOptions::default(),
            0.5,
            360,
            3,
        )
        .expect("composited still");
        let still = dir.join("still.jpg");
        std::fs::write(&still, jpeg).unwrap();
        let (w, h, rgb) = rgb_frame(&still, None);
        assert_eq!((w, h), (360, 640));
        assert_eq!(red_bar_edge(w, h, &rgb), want_edge);

        let jpeg = frame_jpeg(&media, 0.5, 360, 3, true).expect("frame");
        std::fs::write(&still, jpeg).unwrap();
        let (w, h, rgb) = rgb_frame(&still, None);
        assert_eq!((w, h), (360, 640));
        assert_eq!(red_bar_edge(w, h, &rgb), want_edge);
    }

    /// The proxy is what the preview actually decodes; it must come out upright
    /// and carry no matrix of its own that would turn it a second time.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored rotated_proxy`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn rotated_proxy_is_upright_and_carries_no_matrix() {
        let dir = Scratch::new("rotated-proxy");
        let Some(media) = rotated_clip(&dir, -90) else {
            skip("skipped: this ffmpeg cannot write a display matrix");
            return;
        };
        let (rw, rh, ref_rgb) = rgb_frame(&media, None);
        let proxy = generate_proxy(&media, PROXY_MAX_WIDTH).expect("proxy");
        let dump = command(&ffprobe_bin())
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(&proxy)
            .output()
            .expect("run ffprobe");
        let dump = String::from_utf8_lossy(&dump.stdout).into_owned();
        let (w, h, rgb) = rgb_frame(&proxy, None);
        remove_proxy(&proxy);
        assert!(!dump.contains("Display Matrix") && !dump.contains("\"rotate\""), "{dump}");
        assert_eq!((w, h), (rw, rh));
        assert_eq!(red_bar_edge(w, h, &rgb), red_bar_edge(rw, rh, &ref_rgb));
    }

    /// Six seconds of 30 fps video whose frames number themselves — bit `k` of the
    /// frame number is the `k`-th eighth of the picture's width, white for 1 — so a
    /// decoded frame says which source frame it is, however it was scaled or
    /// compressed. The video starts `lead` seconds after the audio (an empty edit
    /// in the mp4, the way a camera that opens its mic first writes it). `None`
    /// when this ffmpeg cannot make the file.
    fn late_barcode_clip(dir: &Scratch, name: &str, lead: f64) -> Option<PathBuf> {
        let (video, audio, out) = (dir.join(&format!("v-{name}")), dir.join(&format!("a-{name}")), dir.join(name));
        let bars = "color=c=black:s=640x360:r=30:d=6,format=yuv420p,\
                    geq=lum='if(bitand(trunc(N/pow(2,trunc(X*8/W))),1),235,16)':cb=128:cr=128";
        let delay = lead.to_string();
        let ok = try_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            bars,
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "30",
            "-pix_fmt",
            "yuv420p",
            video.to_str().unwrap(),
        ]) && try_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=8",
            "-c:a",
            "aac",
            audio.to_str().unwrap(),
        ]) && try_ffmpeg(&[
            "-itsoffset",
            &delay,
            "-i",
            video.to_str().unwrap(),
            "-i",
            audio.to_str().unwrap(),
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c",
            "copy",
            out.to_str().unwrap(),
        ]);
        ok.then_some(out)
    }

    /// Which numbered frame of [`late_barcode_clip`] `path` answers a seek to `t` with,
    /// through the same single-frame decode the preview scrubs with.
    fn barcode_at(dir: &Scratch, path: &Path, t: f64) -> u32 {
        let jpeg = frame_jpeg(path, t, 640, 2, true).expect("frame");
        let file = dir.join("barcode.jpg");
        std::fs::write(&file, jpeg).unwrap();
        let (w, h, rgb) = rgb_frame(&file, None);
        (0..8)
            .filter(|k| rgb[((h / 2) * w + (2 * k + 1) * w / 16) * 3] > 125)
            .map(|k| 1u32 << k)
            .sum()
    }

    /// The presentation times of the first `n` video packets in `path`'s own clock.
    fn first_packet_times(path: &Path, n: usize) -> Vec<f64> {
        let out = command(&ffprobe_bin())
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time"])
            .args(["-of", "csv=p=0"])
            .arg(path)
            .output()
            .expect("run ffprobe");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .take(n)
            .map(|l| l.trim().parse().expect("a packet time"))
            .collect()
    }

    /// A source whose video starts after its audio. FFmpeg 7+ no longer pads such
    /// a head when it writes the audio-less proxy, so the proxy's container began
    /// at the video and `-ss T` on it landed `lead` seconds deeper into the footage
    /// than `-ss T` on the original — preview and export showed different frames
    /// (two at half a second in, a five-second slip for a five-second lead). The
    /// leads are chosen to break each cheaper fix: 0.064 s is 1.92 frames, which
    /// `-fps_mode cfr` rounds *up* and then seeks a frame early at every frame
    /// boundary; 1.5 s is a gap long enough to see; 0 is the control — an ordinary
    /// file whose proxy must come out exactly as before.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored late_starting_video`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn proxy_of_a_late_starting_video_answers_every_seek_like_the_original() {
        let dir = Scratch::new("late-proxy");
        let times = [
            0.0, 0.01, 0.03, 0.05, 0.07, 0.1, 0.2, 0.5, 0.51, 1.0, 1.49, 1.51, 1.7, 3.333, 4.0, 5.9,
        ];
        for (i, lead) in [0.0, 0.064, 0.08, 1.5].into_iter().enumerate() {
            let Some(media) = late_barcode_clip(&dir, &format!("late{i}.mp4"), lead) else {
                skip("skipped: this ffmpeg cannot make the late-starting test clip");
                return;
            };
            let traits = source_traits(&media).expect("probe");
            assert_eq!(
                needs_head_pad(traits.lead),
                lead > 0.0,
                "lead {lead}: the probe saw {}",
                traits.lead
            );
            if lead > 0.0 {
                assert!(
                    (traits.lead - lead).abs() < 0.005,
                    "lead {lead}: the probe saw {}",
                    traits.lead
                );
            }

            let proxy = generate_proxy(&media, PROXY_MAX_WIDTH).expect("proxy");
            let _cleanup = ProxyGuard(proxy.clone());
            assert_eq!(
                is_head_padded_proxy(&proxy.to_string_lossy()),
                lead > 0.0,
                "lead {lead}: the name says whether the head was padded: {proxy:?}"
            );
            let mut seen = Vec::new();
            for t in times {
                let (want, got) = (barcode_at(&dir, &media, t), barcode_at(&dir, &proxy, t));
                assert_eq!(
                    got, want,
                    "lead {lead}: a seek to {t}s lands on frame {got} of the proxy, {want} of the original"
                );
                seen.push(want);
            }
            assert!(seen.first() < seen.last(), "lead {lead}: the clip never moved: {seen:?}");

            // The structure behind it: the proxy starts with its container, holding
            // the first frame until the footage begins where the original does.
            let (orig, prox) = (first_packet_times(&media, 1), first_packet_times(&proxy, 2));
            let after = source_traits(&proxy).map(|t| t.lead).unwrap_or(f64::NAN);
            assert_eq!(after, 0.0, "lead {lead}: the proxy's video starts with its container");
            if lead > 0.0 {
                assert_eq!(prox[0], 0.0);
                assert!((prox[1] - orig[0]).abs() < 0.002, "lead {lead}: {prox:?} against {orig:?}");
            } else {
                assert_eq!(prox[0], orig[0], "an ordinary source is not given a pad");
            }
        }

        // A transport stream cannot be fixed this way (its demuxer rebases a read with
        // the audio discarded), so it is left alone: no lead, the plain proxy and key.
        let ts = dir.join("late.ts");
        let late = dir.join("late1.mp4");
        if try_ffmpeg(&[
            "-i",
            late.to_str().unwrap(),
            "-c",
            "copy",
            "-f",
            "mpegts",
            ts.to_str().unwrap(),
        ]) {
            let traits = source_traits(&ts).expect("probe");
            assert_eq!(traits.lead, 0.0, "a .ts is never padded");
            let proxy = generate_proxy(&ts, PROXY_MAX_WIDTH).expect("proxy");
            let _cleanup = ProxyGuard(proxy.clone());
            assert!(!is_head_padded_proxy(&proxy.to_string_lossy()));
        }
    }

    /// 10-bit HLG BT.2020 footage, made by re-encoding a tagged SDR test card, so
    /// the SDR original is the answer key. (The tags matter: an untagged source
    /// has no colourspace for `zscale` to start from on older FFmpegs.) `None`
    /// without libx265 / zscale.
    fn hlg_clip(dir: &Scratch) -> Option<(PathBuf, PathBuf)> {
        let sdr = dir.join("sdr.mp4");
        run_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30:duration=2,format=yuv420p",
            "-c:v",
            "libx264",
            "-crf",
            "12",
            "-pix_fmt",
            "yuv420p",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
            sdr.to_str().unwrap(),
        ]);
        let hlg = dir.join("hlg.mp4");
        let convert = |input: &[&str]| {
            let mut args: Vec<&str> = input.to_vec();
            args.extend([
                "-vf",
                "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt2020:t=arib-std-b67:m=bt2020nc:r=tv,format=yuv420p10le",
                "-c:v",
                "libx265",
                "-x265-params",
                "log-level=error",
                "-color_primaries",
                "bt2020",
                "-color_trc",
                "arib-std-b67",
                "-colorspace",
                "bt2020nc",
                hlg.to_str().unwrap(),
            ]);
            try_ffmpeg(&args)
        };
        // FFmpeg 9 wants the card's colourspace stated in the graph, FFmpeg 4
        // wants it on a file: one of the two starts `zscale` from a known place.
        let ok = convert(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30:duration=2,format=yuv420p,\
             setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv",
        ]) || convert(&["-i", sdr.to_str().unwrap()]);
        ok.then_some((sdr, hlg))
    }

    /// Mean absolute difference and mean colourfulness (`max-min` over RGB) of two
    /// same-sized frames.
    fn compare_frames(a: &[u8], b: &[u8]) -> (f64, f64, f64) {
        let mad = a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).abs()).sum::<f64>() / a.len() as f64;
        let colourful = |f: &[u8]| {
            f.as_chunks::<3>()
                .0
                .iter()
                .map(|p| (p.iter().max().unwrap() - p.iter().min().unwrap()) as f64)
                .sum::<f64>()
                / (f.len() / 3) as f64
        };
        (mad, colourful(a), colourful(b))
    }

    /// An iPhone's HLG BT.2020 footage squeezed into BT.709 untouched is washed
    /// out and mis-tagged. Exported next to its SDR original it must come back
    /// as the same picture, in an SDR file.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored hlg_footage`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn hlg_footage_exports_as_sdr_with_matching_colour() {
        let dir = Scratch::new("hlg");
        let Some((sdr, hlg)) = hlg_clip(&dir) else {
            skip("skipped: this ffmpeg cannot make the HLG test clip");
            return;
        };
        let hlg_asset = probed_asset(&hlg);
        assert_eq!(hlg_asset.hdr(), Some(Hdr::Hlg), "probe reads the transfer");
        assert_eq!(probed_asset(&sdr).hdr(), None);

        let export = |asset: &Asset, name: &str, opts: &ExportOptions| {
            let timeline = single(vec![make_clip(asset.id, 0.0, 2.0, 0.0)]);
            let out = dir.join(name);
            render_with(&timeline, std::slice::from_ref(asset), &out, opts).expect("export");
            out
        };
        let reference = export(&probed_asset(&sdr), "ref.mp4", &mp4_export_opts());
        let (rw, rh, ref_rgb) = rgb_frame(&reference, None);
        // Both the plain export and the GUI default (hardware decode if any).
        for (name, opts) in [
            ("plain.mp4", mp4_export_opts()),
            (
                "hw.mp4",
                ExportOptions {
                    hwaccel: Some("auto".into()),
                    ..mp4_export_opts()
                },
            ),
        ] {
            let out = export(&hlg_asset, name, &opts);
            let (w, h, rgb) = rgb_frame(&out, None);
            assert_eq!((w, h), (rw, rh));
            let (mad, colour, ref_colour) = compare_frames(&rgb, &ref_rgb);
            let trc = ffprobe_field(&out, "v:0", "stream=color_transfer");
            assert!(
                !matches!(trc.as_str(), "arib-std-b67" | "smpte2084"),
                "{name} is still tagged {trc}"
            );
            assert!(
                (0.8..1.25).contains(&(colour / ref_colour)),
                "{name}: colourfulness {colour:.1} vs the SDR original's {ref_colour:.1}"
            );
            assert!(
                mad < 25.0,
                "{name}: differs from the SDR original by {mad:.1} levels on average"
            );
        }

        // A phone clip is both at once. The side data in the file must not hide
        // the colour tags from the probe, the proxy has to be converted *and*
        // upright, and the export portrait and SDR.
        if let Some(rotated) = turned(&dir, &hlg, -90, "hlg-portrait.mp4") {
            assert_eq!(source_hdr(&rotated), Some(Hdr::Hlg));
            let asset = probed_asset(&rotated);
            assert_eq!(
                (asset.hdr(), asset.streams[0].width, asset.streams[0].height),
                (Some(Hdr::Hlg), Some(360), Some(640))
            );
            let proxy = generate_proxy(&rotated, PROXY_MAX_WIDTH).expect("proxy");
            let tags = ffprobe_field(&proxy, "v:0", "stream=width,height,pix_fmt,color_transfer");
            remove_proxy(&proxy);
            assert_eq!(tags, "360,640,yuv420p,bt709", "the proxy is upright SDR");
            let out = export(&asset, "portrait.mp4", &mp4_export_opts());
            assert_eq!(ffprobe_field(&out, "v:0", "stream=width,height"), "360,640");
            let (_, _, rgb) = rgb_frame(&out, None);
            let (_, colour, ref_colour) = compare_frames(&rgb, &ref_rgb);
            assert!(
                (0.8..1.25).contains(&(colour / ref_colour)),
                "colourfulness {colour:.1} vs the SDR original's {ref_colour:.1}"
            );
        }

        // The single-frame decodes tone-map too: a scrubbed frame and a contact
        // sheet cell look like the SDR original, not like a washed-out copy.
        let jpeg_rgb = |name: &str, bytes: Vec<u8>| {
            let file = dir.join(name);
            std::fs::write(&file, bytes).unwrap();
            rgb_frame(&file, Some("scale=640:360"))
        };
        let (.., sdr_frame) = jpeg_rgb("sdr-frame.jpg", frame_jpeg(&sdr, 0.5, 640, 2, true).unwrap());
        let (.., hlg_frame) = jpeg_rgb("hlg-frame.jpg", frame_jpeg(&hlg, 0.5, 640, 2, true).unwrap());
        let (.., hlg_sheet) = jpeg_rgb("hlg-sheet.jpg", contact_sheet(&hlg, 0.5, 1.5, 1, 1, 640, 2).unwrap().0);
        for (what, rgb) in [("frame", &hlg_frame), ("contact sheet", &hlg_sheet)] {
            let (mad, colour, ref_colour) = compare_frames(rgb, &sdr_frame);
            assert!(
                (0.8..1.25).contains(&(colour / ref_colour)),
                "{what}: colourfulness {colour:.1} vs {ref_colour:.1}"
            );
            // Looser than the export: the sheet cell is a neighbouring frame of a
            // moving test card, bilinear-scaled. A washed-out copy is ~70 off.
            assert!(mad < 35.0, "{what}: differs from the SDR original by {mad:.1}");
        }
    }

    /// A Canon R5 II records 10-bit 4:2:2 HEVC with 24-bit PCM audio in an MP4.
    /// Hardware decoders often cannot do 4:2:2, so the GUI's `auto` hwaccel has
    /// to fall back rather than fail, and the PCM must survive every audio path.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored mirrorless_422`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn mirrorless_422_10bit_hevc_with_24bit_pcm_exports_with_audio() {
        let dir = Scratch::new("r5");
        // PCM in an MP4 is what the camera writes; older FFmpegs refuse it there
        // and take it in a MOV, which is the same audio for every reader below.
        let make = |name: &str| {
            let media = dir.join(name);
            try_ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=25:duration=3,format=yuv422p10le",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=3",
                "-c:v",
                "libx265",
                "-x265-params",
                "log-level=error",
                "-pix_fmt",
                "yuv422p10le",
                "-c:a",
                "pcm_s24le",
                "-ac",
                "2",
                media.to_str().unwrap(),
            ])
            .then_some(media)
        };
        let Some(media) = make("r5.mp4").or_else(|| make("r5.mov")) else {
            skip("skipped: this ffmpeg cannot make the 4:2:2 test clip");
            return;
        };
        let asset = probed_asset(&media);
        assert!(asset
            .streams
            .iter()
            // FFmpeg 6 reads the MP4 `ipcm` box back as 32-bit; any PCM is the point.
            .any(|s| s.kind == StreamKind::Audio && s.codec.starts_with("pcm_s")));

        let timeline = single(vec![make_clip(asset.id, 0.0, 3.0, 0.0)]);
        // `vaapi` is a hardware decode that is requested by name: where it cannot
        // do 4:2:2 (or is not there at all) the export has to retry in software
        // rather than fail.
        for (name, hw) in [
            ("plain.mp4", None),
            ("hw.mp4", Some("auto".to_string())),
            ("named.mp4", Some("vaapi".to_string())),
        ] {
            let out = dir.join(name);
            let opts = ExportOptions {
                hwaccel: hw,
                ..mp4_export_opts()
            };
            render_with(&timeline, std::slice::from_ref(&asset), &out, &opts).expect("export");
            assert_eq!(
                ffprobe_field(&out, "a:0", "stream=codec_type"),
                "audio",
                "{name} lost its audio"
            );
            let probe = command(&ffmpeg_bin())
                .args(["-hide_banner", "-i"])
                .arg(&out)
                .args(["-af", "volumedetect", "-vn", "-f", "null", "-"])
                .output()
                .expect("run ffmpeg");
            let log = String::from_utf8_lossy(&probe.stderr);
            let mean: f64 = log
                .lines()
                .find_map(|l| l.split("mean_volume:").nth(1))
                .and_then(|v| v.trim().trim_end_matches(" dB").parse().ok())
                .unwrap_or(-91.0);
            assert!(mean > -40.0, "{name}: exported audio is silent ({mean} dB)\n{log}");
            let (w, h, _) = rgb_frame(&out, None);
            assert_eq!((w, h), (640, 360));
        }

        // The other audio readers: preview PCM, waveform, loudness analysis.
        let pcm = audio_pcm(&media, 0.5, 0.5, 8_000, None).expect("preview audio");
        // FFmpeg 6 seeks PCM-in-MP4 to the nearest packet, so allow a short read.
        assert!(
            (5_000..=8_000).contains(&pcm.len()),
            "about half a second of mono s16le: {}",
            pcm.len()
        );
        assert!(pcm.as_chunks::<2>().0.iter().any(|b| i16::from_le_bytes(*b).abs() > 1000));
        let wave = waveform(&media, 50, 8_000).expect("waveform");
        assert!(wave.iter().copied().fold(0.0, f32::max) > 0.1);
        let loud = super::super::audio::measure_loudness(&media).expect("loudness");
        assert!(loud.integrated_lufs > -40.0, "{loud:?}");

        // Preview frames decode (with hardware decode attempted first).
        assert!(!frame_jpeg(&media, 1.0, 320, 4, true).expect("frame").is_empty());
        assert!(!frame_at(&media, 1.0, 320).expect("png frame").is_empty());
    }

    /// Cut 3.5s..8.5s out of a variable-frame-rate phone clip (every seventh
    /// frame missing) and butt a constant-rate clip against it. A white flash and
    /// a beep land together at every whole second of the source, so after the cut
    /// they must still land together, on the right half-seconds.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored vfr_cut`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn vfr_cut_keeps_duration_and_av_sync() {
        let dir = Scratch::new("vfr");
        let vfr = dir.join("vfr.mp4");
        let video = "color=c=black:s=640x360:r=30:d=12,\
                     drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='lt(mod(t,1),0.1)',\
                     select='mod(n,7)'";
        let beeps = "aevalsrc='sin(2*PI*1000*t)*lt(mod(t,1),0.1)':s=48000:d=12";
        let made = ["-fps_mode", "-vsync"].iter().any(|flag| {
            try_ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                video,
                "-f",
                "lavfi",
                "-i",
                beeps,
                flag,
                "vfr",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                vfr.to_str().unwrap(),
            ])
        });
        assert!(made, "could not make the VFR clip");
        let tail = dir.join("tail.mp4");
        run_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "color=c=gray:s=640x360:r=25:d=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=220:sample_rate=48000:duration=3",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            tail.to_str().unwrap(),
        ]);

        let (a, b) = (probed_asset(&vfr), probed_asset(&tail));
        let timeline = single(vec![make_clip(a.id, 3.5, 8.5, 0.0), make_clip(b.id, 0.0, 3.0, 5.0)]);
        let out = dir.join("cut.mp4");
        render_with(&timeline, &[a, b], &out, &mp4_export_opts()).expect("export");

        let total: f64 = {
            let o = command(&ffprobe_bin())
                .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
                .arg(&out)
                .output()
                .expect("run ffprobe");
            String::from_utf8_lossy(&o.stdout).trim().parse().unwrap()
        };
        assert!((total - 8.0).abs() < 0.15, "expected an 8s file, got {total}");

        // Flash times: per-frame mean luma of the picture sampled at 100 Hz.
        let luma = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&out)
            .args(["-an", "-vf", "fps=100,scale=1:1,format=gray", "-f", "rawvideo", "pipe:1"])
            .output()
            .expect("run ffmpeg")
            .stdout;
        // Beep times: RMS of the mono audio in 10 ms windows.
        let pcm = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&out)
            .args(["-vn", "-ac", "1", "-ar", "8000", "-f", "s16le", "pipe:1"])
            .output()
            .expect("run ffmpeg")
            .stdout;
        let onsets = |active: Vec<bool>| -> Vec<f64> {
            let mut times = Vec::new();
            let mut prev = false;
            for (i, on) in active.into_iter().enumerate() {
                if on && !prev {
                    times.push(i as f64 / 100.0);
                }
                prev = on;
            }
            times
        };
        let flashes = onsets(luma.iter().map(|l| *l > 128).collect());
        let samples: Vec<f64> = pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as f64).collect();
        let beeps = onsets(
            samples
                .chunks(80)
                .map(|w| (w.iter().map(|s| s * s).sum::<f64>() / w.len() as f64).sqrt() > 12000.0)
                .collect(),
        );
        // Source seconds 4..8 land at output 0.5, 1.5, 2.5, 3.5 — the clip's own
        // 3.5 s head start is gone, the 1-second beat is not.
        let want = [0.5, 1.5, 2.5, 3.5, 4.5];
        let near = |times: &[f64]| want.iter().filter(|w| times.iter().any(|t| (*t - **w).abs() < 0.08)).count();
        assert!(near(&flashes) >= 4, "flashes at {flashes:?}, wanted {want:?}");
        assert!(near(&beeps) >= 4, "beeps at {beeps:?}, wanted {want:?}");
        for f in flashes.iter().filter(|f| **f < 4.9) {
            let nearest = beeps.iter().map(|b| (b - f).abs()).fold(f64::MAX, f64::min);
            assert!(nearest < 0.08, "flash at {f} has no beep within 80ms (beeps {beeps:?})");
        }
    }
}
