//! CLI-driven media engine: probing, analysis, frame/waveform extraction and
//! export by invoking the system `ffmpeg` / `ffprobe` binaries.
//!
//! Unlike [`super::ffmpeg`] (in-process libav, gated behind the `ffmpeg`
//! feature and the FFmpeg *development* libraries), everything here only needs
//! the FFmpeg *binaries* on `PATH`, so it compiles and runs in the
//! `--no-default-features` build. The binaries can be overridden with the
//! `KERF_FFMPEG` / `KERF_FFPROBE` environment variables.

use std::path::Path;
use std::process::{Command, Stdio};

use super::ProbeResult;
use crate::error::{Error, Result};
use crate::model::{Asset, Clip, StreamInfo, StreamKind, TimeRange, Timeline, TransitionKind, Transform};

fn ffmpeg_bin() -> String {
    std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string())
}

fn ffprobe_bin() -> String {
    std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string())
}

fn launch_err(bin: &str, e: std::io::Error) -> Error {
    Error::Engine(format!("failed to launch `{bin}` ({e}); is FFmpeg installed and on PATH?"))
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
}

/// Probe a media file via `ffprobe -of json`.
// In a full `ffmpeg` build the in-process libav probe is used instead.
#[cfg_attr(feature = "ffmpeg", allow(dead_code))]
pub fn probe(path: &Path) -> Result<ProbeResult> {
    let bin = ffprobe_bin();
    let output = Command::new(&bin)
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
    Ok(probe_from_json(parsed))
}

fn probe_from_json(parsed: ProbeJson) -> ProbeResult {
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
        });
    }
    let duration = parsed
        .format
        .and_then(|f| f.duration)
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(max_stream_dur)
        .max(0.0);
    ProbeResult { duration, streams }
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
    let output = Command::new(&bin)
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

/// Detect scene-change timestamps using `select='gt(scene,threshold)'`.
pub fn detect_scenes(path: &Path, threshold: f64) -> Result<Vec<f64>> {
    let bin = ffmpeg_bin();
    let filter = format!("select='gt(scene,{threshold})',showinfo");
    let output = Command::new(&bin)
        .args(["-hide_banner", "-nostats"])
        .arg("-i")
        .arg(path)
        .args(["-map", "0:v:0?", "-vf", &filter, "-f", "null", "-"])
        .stdout(Stdio::null())
        .output()
        .map_err(|e| launch_err(&bin, e))?;
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
    decode_frame(path, time_secs, &scale, "png", None)
}

/// Decode a single frame at `time_secs` as **JPEG** bytes, scaled to at most
/// `max_width` pixels wide, at `quality` (ffmpeg `-q:v`, 2 = best … 31 = worst).
/// JPEG is dramatically smaller than the PNG of [`frame_at`], which matters when
/// the frame is handed to an LLM as an image content block rather than rendered
/// in the GUI.
pub fn frame_jpeg(path: &Path, time_secs: f64, max_width: u32, quality: u8) -> Result<Vec<u8>> {
    let scale = format!("scale='min({max_width},iw)':-2");
    decode_frame(path, time_secs, &scale, "mjpeg", Some(quality))
}

/// Seek to `time_secs`, run the `-vf` chain on a single frame and pipe it out in
/// the given image codec (`png` / `mjpeg`); `quality`, when set, becomes `-q:v`.
/// Shared by [`frame_at`] and [`frame_jpeg`]. `-ss` is input-side (fast,
/// keyframe-accurate) as for the preview path.
fn decode_frame(path: &Path, time_secs: f64, vf: &str, vcodec: &str, quality: Option<u8>) -> Result<Vec<u8>> {
    let bin = ffmpeg_bin();
    let mut cmd = Command::new(&bin);
    cmd.args(["-hide_banner", "-loglevel", "error", "-ss"])
        .arg(format!("{:.3}", time_secs.max(0.0)))
        .arg("-i")
        .arg(path)
        .args(["-frames:v", "1", "-vf", vf]);
    if let Some(q) = quality {
        cmd.args(["-q:v", q.to_string().as_str()]);
    }
    cmd.args(["-f", "image2pipe", "-vcodec", vcodec, "pipe:1"]).stderr(Stdio::piped());
    let output = cmd.output().map_err(|e| launch_err(&bin, e))?;
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
    let path = path.to_str().ok_or_else(|| Error::Engine("asset path is not valid UTF-8".to_string()))?;
    let (args, times) = build_contact_sheet_args(path, start, end, columns, rows, cell_width, quality);
    let bin = ffmpeg_bin();
    let output = Command::new(&bin).args(&args).stderr(Stdio::piped()).output().map_err(|e| launch_err(&bin, e))?;
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
    let samples = decode_audio_mono_f32(path, sample_rate)?;
    Ok(peaks(&samples, buckets.max(1)))
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

fn decode_audio_mono_f32(path: &Path, sample_rate: u32) -> Result<Vec<f32>> {
    let bin = ffmpeg_bin();
    let output = Command::new(&bin)
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
        match self {
            Self::Mp4 => &["libx264", "libx265", "libsvtav1"],
            Self::Mov => &["prores_ks", "libx264", "libx265"],
            Self::Mkv => &["libx264", "libx265", "libvpx-vp9", "libsvtav1"],
            Self::Webm => &["libvpx-vp9", "libsvtav1"],
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

    /// Output WxH, baked into the filtergraph. Even-clamped.
    pub resolution: Option<(u32, u32)>,
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
            resolution: None,
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
        }
    }
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
        "libx264" => &["film", "animation", "grain", "stillimage", "zerolatency", "fastdecode", "psnr", "ssim"],
        "libx265" => &["psnr", "ssim", "grain", "zerolatency", "fastdecode", "animation"],
        _ => &[],
    }
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
        issues.push(format!("{} is audio-only, but the timeline has no audio.", c.ext().to_uppercase()));
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
    if let Some(v) = clips().find_map(|c| stream_of(c, StreamKind::Video)) {
        if let (Some(w), Some(h)) = (v.width, v.height) {
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
    let path_of = |id| assets.iter().find(|a| a.id == id).map(|a| a.path.as_str());

    // Stream gating: decide what the graph emits and what we `-map`, in lockstep.
    let timeline_has_video = timeline.tracks.iter().any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    let timeline_has_audio = timeline
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .any(|c| assets.iter().find(|a| a.id == c.asset_id).is_some_and(|a| a.streams.iter().any(|s| s.kind == StreamKind::Audio)));
    let c = opts.container;
    let want_video = timeline_has_video && !c.is_audio_only();
    let want_audio = timeline_has_audio && !c.is_video_only() && opts.include_audio && pass != PassPhase::First;

    // `-hide_banner -nostats` keep the captured stderr to genuine warnings/errors
    // (matching the probe/frame calls); without `-nostats` the per-frame progress
    // lines would accumulate unbounded in memory for a long export.
    let mut args: Vec<String> = vec!["-y".to_string(), "-hide_banner".to_string(), "-nostats".to_string()];
    for clip in timeline.tracks.iter().flat_map(|t| t.clips.iter()) {
        let path = path_of(clip.asset_id).ok_or(Error::AssetNotFound(clip.asset_id))?;
        args.push("-i".to_string());
        args.push(path.to_string());
    }

    let fmt = export_format(timeline, assets, opts);
    let total = timeline.duration();
    let graph = build_filter_complex(timeline, assets, &fmt, total, opts, want_video, want_audio);
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

    match opts.rate_control {
        RateControl::Crf => {
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
        RateControl::Bitrate => {
            if let Some(b) = &opts.video_bitrate {
                args.push("-b:v".to_string());
                args.push(b.clone());
            }
            if let Some(m) = &opts.max_rate {
                args.push("-maxrate".to_string());
                args.push(m.clone());
            }
            if let Some(b) = &opts.buf_size {
                args.push("-bufsize".to_string());
                args.push(b.clone());
            }
        }
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
        RateControl::Lossless => match vc {
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
    }

    // Speed preset: named for x264/x265/svt-av1, -cpu-used for libvpx-vp9.
    match vc {
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
    }

    if matches!(vc, "libx264" | "libx265") {
        // Only emit a tune the encoder actually accepts (x265 lacks film/stillimage)
        // so a stale value never makes ffmpeg fail to open the encoder.
        if let Some(t) = opts.tune.as_deref().filter(|t| video_tunes(vc).contains(t)) {
            args.push("-tune".to_string());
            args.push(t.to_string());
        }
        if let Some(p) = &opts.profile_v {
            args.push("-profile:v".to_string());
            args.push(p.clone());
        }
    }
    // HEVC in mp4/mov needs the hvc1 tag or QuickTime / iOS refuse to play it.
    if vc == "libx265" && matches!(opts.container, Container::Mp4 | Container::Mov) {
        args.push("-tag:v".to_string());
        args.push("hvc1".to_string());
    }
}

/// Render the timeline by driving the `ffmpeg` binary with a generated
/// `filter_complex` (trim + per-clip volume + normalize + concat).
// With the `libav-render` feature the in-process libav executor is used instead.
#[cfg_attr(feature = "libav-render", allow(dead_code))]
pub fn render(timeline: &Timeline, assets: &[Asset], output: &Path, _format: &str) -> Result<()> {
    render_with(timeline, assets, output, &ExportOptions::default())
}

/// Like [`render`] but with explicit export options. Validates the options
/// against the timeline's available streams before launching, and runs ffmpeg
/// twice for [`RateControl::TwoPass`].
#[cfg_attr(feature = "libav-render", allow(dead_code))]
pub fn render_with(timeline: &Timeline, assets: &[Asset], output: &Path, opts: &ExportOptions) -> Result<()> {
    if !timeline.tracks.iter().any(|t| !t.clips.is_empty()) {
        return Err(Error::InvalidArgument("timeline has no clips to export".to_string()));
    }

    let has_video = timeline.tracks.iter().any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    let has_audio = timeline
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .any(|c| assets.iter().find(|a| a.id == c.asset_id).is_some_and(|a| a.streams.iter().any(|s| s.kind == StreamKind::Audio)));
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
        && matches!(opts.video_codec.as_deref(), Some(vc) if vc != "prores_ks" && vc != "gif");

    if two_pass {
        let null_sink = if cfg!(windows) { "NUL" } else { "/dev/null" };
        // ffmpeg appends "-N.log" to the passlog prefix; scope it to this process.
        let passlog = std::env::temp_dir().join(format!("kerf-2pass-{}", std::process::id())).to_string_lossy().into_owned();
        let a1 = build_export_args_phase(timeline, assets, output_str, opts, PassPhase::First, null_sink, &passlog)?;
        run_ffmpeg(&a1, output)?;
        let a2 = build_export_args_phase(timeline, assets, output_str, opts, PassPhase::Second, null_sink, &passlog)?;
        let res = run_ffmpeg(&a2, output);
        for suffix in ["-0.log", "-0.log.mbtree", ".log", ".log.mbtree"] {
            let _ = std::fs::remove_file(format!("{passlog}{suffix}"));
        }
        res
    } else {
        let args = build_export_args(timeline, assets, output_str, opts)?;
        run_ffmpeg(&args, output)
    }
}

/// Spawn the `ffmpeg` binary with `args`, capturing stderr so a failure's actual
/// reason (rather than inherited console noise) ends up in the error and log.
fn run_ffmpeg(args: &[String], output: &Path) -> Result<()> {
    let bin = ffmpeg_bin();
    tracing::info!(output = %output.display(), "exporting timeline");
    tracing::debug!(command = %format!("{bin} {}", args.join(" ")), "ffmpeg export command");

    let out = Command::new(&bin).args(args).output().map_err(|e| launch_err(&bin, e))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut tail: Vec<&str> = stderr.lines().rev().take(20).collect();
        tail.reverse();
        let tail = tail.join("\n");
        tracing::error!(status = %out.status, "ffmpeg export failed:\n{tail}");
        return Err(Error::Engine(format!("ffmpeg exited with {}: {}", out.status, tail.trim())));
    }
    tracing::info!(output = %output.display(), "export complete");
    Ok(())
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
fn build_filter_complex(
    timeline: &Timeline,
    assets: &[Asset],
    fmt: &ExportFormat,
    total: f64,
    opts: &ExportOptions,
    want_video: bool,
    want_audio: bool,
) -> FilterGraph {
    let has_audio = |clip: &crate::model::Clip| {
        assets
            .iter()
            .find(|a| a.id == clip.asset_id)
            .is_some_and(|a| a.streams.iter().any(|s| s.kind == StreamKind::Audio))
    };
    let layout = fmt.channel_layout();

    // Assign each clip its ffmpeg input index (track-then-storage order, matching
    // the `-i` order) and split into composited video clips and mixed audio clips.
    // Within a track the clips are visited in *timeline* order so video overlays
    // composite in timeline order (a later clip on top of an earlier one's tail,
    // e.g. during a crossfade); tracks keep their list order so a later track
    // still composites on top.
    let mut video: Vec<(usize, &crate::model::Clip)> = Vec::new();
    let mut audio: Vec<(usize, &crate::model::Clip)> = Vec::new();
    let mut base = 0;
    for track in &timeline.tracks {
        let mut order: Vec<usize> = (0..track.clips.len()).collect();
        order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
        for &cj in &order {
            let clip = &track.clips[cj];
            let i = base + cj; // storage-order input index
            if track.kind == StreamKind::Video {
                video.push((i, clip));
            }
            if has_audio(clip) {
                audio.push((i, clip));
            }
        }
        base += track.clips.len();
    }

    // Per-clip transition adjustments (crossfade tail / alpha, dip-to-black
    // fades), computed per track from each clip's `transition_in`.
    let fx = transition_fx(timeline, assets);

    let mut chains: Vec<String> = Vec::new();

    // ---- picture: black base + positioned overlays --------------------------
    let gif = opts.container == Container::Gif;
    let has_video = want_video && !video.is_empty();
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
        // For gif the composite lands on `vcomp`, then palettegen / paletteuse
        // produce the real `[outv]`; otherwise the last overlay is `[outv]`.
        let final_pad = if gif { "vcomp" } else { "outv" };
        let last = video.len() - 1;
        for (n, (i, clip)) in video.iter().enumerate() {
            chains.push(format!("[{i}:v]{chain}[v{i}]", i = i, chain = video_clip_chain(clip, fmt, &fx[*i])));
            let out = if n == last { final_pad.to_string() } else { format!("vov{n}") };
            let end = clip.timeline_end() + fx[*i].tail;
            let overlay = if clip.transform.is_identity() {
                format!("overlay=eof_action=pass:enable='between(t,{start},{end})'", start = clip.timeline_start)
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
            chains.push(format!("[{cur}][v{i}]{overlay}[{out}]"));
            cur = out;
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
    let has_audio_out = want_audio && !audio.is_empty();
    if has_audio_out {
        for (i, clip) in &audio {
            chains.push(format!("[{i}:a]{chain}[a{i}]", i = i, chain = audio_clip_chain(clip, fmt, &fx[*i], layout)));
        }
        let inputs: String = audio.iter().map(|(i, _)| format!("[a{i}]")).collect();
        chains.push(format!(
            "{inputs}amix=inputs={n}:normalize=0:dropout_transition=0[outa]",
            inputs = inputs,
            n = audio.len(),
        ));
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

/// The video filter chain for one clip (everything between its `[i:v]` input
/// and its `[v{i}]` output): trim, optional reverse / crop / retime, fit or
/// transform geometry, color correction, fades and transition alpha. With all
/// new properties at their defaults this reduces to the original
/// fit-and-letterbox chain.
fn video_clip_chain(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx) -> String {
    let s = clip.speed_mag();
    let t = &clip.transform;
    // `transform_alpha` is alpha from opacity/rotation (established in the geometry
    // step); `needs_alpha` also covers the crossfade alpha dissolve.
    let transform_alpha = !t.is_identity() && t.needs_alpha();
    let needs_alpha = transform_alpha || fx.xfade_in > 0.0;
    let dur = clip.duration() + fx.tail;
    // A crossfade tail borrows unused source: forward clips extend past source_out,
    // reversed clips extend below source_in (reverse plays high->low, so the visible
    // tail is at the low end).
    let (trim_start, trim_end) = if clip.is_reversed() {
        ((clip.source_in - fx.tail * s).max(0.0), clip.source_out)
    } else {
        (clip.source_in, clip.source_out + fx.tail * s)
    };

    let mut p: Vec<String> = Vec::new();
    p.push(format!("trim=start={trim_start}:end={trim_end}"));
    if clip.is_reversed() {
        p.push("reverse".to_string());
    }
    if t.has_crop() {
        let cw = (1.0 - t.crop_left - t.crop_right).max(0.0);
        let ch = (1.0 - t.crop_top - t.crop_bottom).max(0.0);
        p.push(format!("crop=w=iw*{cw}:h=ih*{ch}:x=iw*{cl}:y=ih*{ct}", cl = t.crop_left, ct = t.crop_top));
    }
    if (s - 1.0).abs() < 1e-9 {
        p.push(format!("setpts=PTS-STARTPTS+{}/TB", clip.timeline_start));
    } else {
        p.push(format!("setpts=(PTS-STARTPTS)/{}+{}/TB", s, clip.timeline_start));
    }
    let sf = fmt.scale_flags();
    if t.is_identity() {
        p.push(format!("scale={w}:{h}:force_original_aspect_ratio=decrease{sf}", w = fmt.width, h = fmt.height));
        p.push(format!("pad={w}:{h}:(ow-iw)/2:(oh-ih)/2", w = fmt.width, h = fmt.height));
    } else {
        p.push(format!("scale={w}:{h}:force_original_aspect_ratio=decrease{sf}", w = fmt.width, h = fmt.height));
        if (t.scale - 1.0).abs() > 1e-9 {
            p.push(format!("scale=iw*{sc}:ih*{sc}{sf}", sc = t.scale));
        }
    }
    p.push("setsar=1".to_string());
    p.push(format!("fps={}", fmt.fps));
    // Color correction must run BEFORE any alpha plane is established: ffmpeg's `eq`
    // has no alpha-capable input format, so the graph would otherwise auto-insert a
    // conversion that drops the alpha (silently disabling opacity / rotation).
    if !clip.color.is_identity() {
        let c = &clip.color;
        p.push(format!(
            "eq=brightness={}:contrast={}:saturation={}:gamma={}",
            c.brightness, c.contrast, c.saturation, c.gamma
        ));
    }
    if !t.is_identity() {
        if transform_alpha {
            p.push("format=yuva420p".to_string());
        }
        if t.opacity < 1.0 {
            p.push(format!("colorchannelmixer=aa={}", t.opacity));
        }
        if t.rotation != 0.0 {
            let rad = t.rotation.to_radians();
            p.push(format!("rotate={rad}:fillcolor=none:ow=rotw({rad}):oh=roth({rad})"));
        }
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
        if !transform_alpha {
            p.push("format=yuva420p".to_string());
        }
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
    let args = build_timeline_frame_args(timeline, assets, opts, t, max_width, quality)?;
    let bin = ffmpeg_bin();
    let output = Command::new(&bin).args(&args).stderr(Stdio::piped()).output().map_err(|e| launch_err(&bin, e))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(Error::Engine(format!(
            "could not render timeline frame at {t:.3}s: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
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
            let raw = if clip.is_reversed() { clip.source_out - off } else { clip.source_in + off };
            let dur = asset_of(clip.asset_id).map(|a| a.duration).unwrap_or(clip.source_out);
            active.push((clip, raw.clamp(0.0, dur.max(0.0))));
        }
    }

    let mut args: Vec<String> = vec!["-hide_banner".to_string(), "-loglevel".to_string(), "error".to_string()];
    for (clip, src) in &active {
        let path = asset_of(clip.asset_id).ok_or(Error::AssetNotFound(clip.asset_id))?.path.clone();
        args.push("-ss".to_string());
        args.push(format!("{src:.3}"));
        args.push("-i".to_string());
        args.push(path);
    }

    // Black base + each active clip's still chain, overlaid in order. With no
    // visible clip the bare canvas is the output (a gap renders black).
    let sf = fmt.scale_flags();
    let filter = if active.is_empty() {
        format!("color=c=black:s={ow}x{oh}:d=0.1[outv]")
    } else {
        let mut chains: Vec<String> = vec![format!("color=c=black:s={ow}x{oh}:d=0.1[base]")];
        let mut cur = "base".to_string();
        let last = active.len() - 1;
        for (n, (clip, _)) in active.iter().enumerate() {
            chains.push(format!("[{n}:v]{chain}[v{n}]", chain = still_clip_chain(clip, ow, oh, &sf)));
            let out = if n == last { "outv".to_string() } else { format!("ov{n}") };
            chains.push(format!("[{cur}][v{n}]{overlay}[{out}]", overlay = still_overlay(&clip.transform)));
            cur = out;
        }
        chains.join(";")
    };

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

/// The still video chain for one clip in a [`timeline_frame`] composite: take a
/// single decoded frame, then apply the same crop / fit-or-transform / color /
/// opacity / rotation geometry as [`video_clip_chain`], minus every time-domain
/// step (trim/setpts/fps/fades) since the `-ss` input seek already positioned it.
fn still_clip_chain(clip: &Clip, ow: u32, oh: u32, sf: &str) -> String {
    let t = &clip.transform;
    let transform_alpha = !t.is_identity() && t.needs_alpha();
    let mut p: Vec<String> = vec!["trim=end_frame=1".to_string(), "setpts=PTS-STARTPTS".to_string()];
    if t.has_crop() {
        let cw = (1.0 - t.crop_left - t.crop_right).max(0.0);
        let ch = (1.0 - t.crop_top - t.crop_bottom).max(0.0);
        p.push(format!("crop=w=iw*{cw}:h=ih*{ch}:x=iw*{cl}:y=ih*{ct}", cl = t.crop_left, ct = t.crop_top));
    }
    p.push(format!("scale={ow}:{oh}:force_original_aspect_ratio=decrease{sf}"));
    if t.is_identity() {
        p.push(format!("pad={ow}:{oh}:(ow-iw)/2:(oh-ih)/2"));
    } else if (t.scale - 1.0).abs() > 1e-9 {
        p.push(format!("scale=iw*{sc}:ih*{sc}{sf}", sc = t.scale));
    }
    p.push("setsar=1".to_string());
    if !clip.color.is_identity() {
        let c = &clip.color;
        p.push(format!(
            "eq=brightness={}:contrast={}:saturation={}:gamma={}",
            c.brightness, c.contrast, c.saturation, c.gamma
        ));
    }
    if !t.is_identity() {
        if transform_alpha {
            p.push("format=yuva420p".to_string());
        }
        if t.opacity < 1.0 {
            p.push(format!("colorchannelmixer=aa={}", t.opacity));
        }
        if t.rotation != 0.0 {
            let rad = t.rotation.to_radians();
            p.push(format!("rotate={rad}:fillcolor=none:ow=rotw({rad}):oh=roth({rad})"));
        }
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
    // Mirror the video crossfade tail: extend below source_in for a reversed clip,
    // past source_out otherwise.
    let (trim_start, trim_end) = if clip.is_reversed() {
        ((clip.source_in - fx.tail * s).max(0.0), clip.source_out)
    } else {
        (clip.source_in, clip.source_out + fx.tail * s)
    };
    let delay_ms = (clip.timeline_start * 1000.0).round().max(0.0) as i64;
    let fi = clip.fade_in + fx.black_in + fx.xfade_in;
    let fo = clip.fade_out + fx.black_out + fx.tail;

    let mut p: Vec<String> = Vec::new();
    p.push(format!("atrim=start={trim_start}:end={trim_end}"));
    p.push("asetpts=PTS-STARTPTS".to_string());
    if clip.is_reversed() {
        p.push("areverse".to_string());
    }
    if (s - 1.0).abs() > 1e-9 {
        p.push(atempo_chain(s));
    }
    p.push(format!("volume={}", clip.volume));
    if fi > 0.0 {
        p.push(format!("afade=t=in:st=0:d={}", fi.clamp(0.0, dur)));
    }
    if fo > 0.0 {
        p.push(format!("afade=t=out:st={}:d={}", (dur - fo).max(0.0), fo.clamp(0.0, dur)));
    }
    p.push(format!("aformat=sample_rates={sr}:channel_layouts={layout}", sr = fmt.sample_rate));
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
    use crate::model::{Asset, Clip, StreamInfo, StreamKind, Timeline, Track};
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

    fn test_asset(streams: Vec<StreamInfo>) -> Asset {
        Asset {
            id: Uuid::new_v4(),
            path: "/x.mp4".into(),
            name: "x.mp4".into(),
            duration: 100.0,
            streams,
            imported_at: Utc::now(),
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

        let g = build_filter_complex(&timeline, &assets, &fmt, timeline.duration(), &ExportOptions::default(), true, true);
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
        let g = build_filter_complex(&timeline, &assets, &fmt, timeline.duration(), &ExportOptions::default(), true, true);
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
        let g = build_filter_complex(&timeline, &assets, &fmt, timeline.duration(), &ExportOptions::default(), true, true);
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
        let f = build_filter_complex(&timeline, &assets, &fmt, timeline.duration(), &ExportOptions::default(), true, true).filter;
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
        let f = build_filter_complex(&timeline, &assets, &fmt, timeline.duration(), &ExportOptions::default(), true, true).filter;
        assert!(!f.contains("fade="), "no fade filter when fades are zero");
        assert!(!f.contains("afade="), "no afade filter when fades are zero");
    }

    #[test]
    fn export_format_falls_back_to_defaults() {
        let timeline = Timeline { tracks: Vec::new() };
        let fmt = export_format(&timeline, &[], &ExportOptions::default());
        assert_eq!((fmt.width, fmt.height), (1920, 1080));
        assert_eq!(fmt.channel_layout(), "stereo");
    }

    fn make_clip(asset_id: uuid::Uuid, source_in: f64, source_out: f64, timeline_start: f64) -> Clip {
        Clip::new(asset_id, source_in, source_out, timeline_start)
    }

    fn video_track(clips: Vec<Clip>) -> Track {
        Track {
            id: Uuid::new_v4(),
            kind: StreamKind::Video,
            name: "V1".into(),
            clips,
        }
    }

    fn audio_track(clips: Vec<Clip>) -> Track {
        Track {
            id: Uuid::new_v4(),
            kind: StreamKind::Audio,
            name: "A1".into(),
            clips,
        }
    }

    fn timeline_of(tracks: Vec<Track>) -> Timeline {
        Timeline { tracks }
    }

    /// A timeline with a single video track holding `clips`.
    fn single(clips: Vec<Clip>) -> Timeline {
        timeline_of(vec![video_track(clips)])
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
        assert!(joined.contains("overlay=(W-w)/2:(H-h)/2[outv]"));
        assert!(joined.contains("-vcodec mjpeg"));
    }

    #[test]
    fn timeline_frame_renders_black_on_a_gap() {
        let asset = test_asset(vec![video_stream(1280, 720, 30.0)]);
        let assets = vec![asset.clone()];
        let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]); // covers 0..5
        let args = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 8.0, 640, 4).unwrap();
        let joined = args.join(" ");
        // Nothing visible at t=8 -> no inputs, bare black canvas straight to [outv].
        assert!(!joined.contains("-i "));
        assert!(joined.contains("color=c=black:s=640x360:d=0.1[outv]"));
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
        assert!(joined.contains("[v1]overlay=x=(W-w)/2+(0.25)*W:y=(H-h)/2+(0)*H[outv]"));
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
        };
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let assets = vec![asset];
        let opts = ExportOptions::default();

        let args = build_export_args(&timeline, &assets, "/out/result.mp4", &opts).unwrap();

        assert_eq!(args[0], "-y");
        assert!(args.contains(&"-nostats".to_string()), "progress stats suppressed so stderr stays bounded");
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
        };
        let a2 = Asset {
            id: Uuid::new_v4(),
            path: "/media/b.mp4".into(),
            name: "b.mp4".into(),
            duration: 10.0,
            streams: vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)],
            imported_at: Utc::now(),
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
        }
    }

    fn graph_of(timeline: &Timeline, assets: &[Asset]) -> String {
        build_filter_complex(timeline, assets, &fmt_1080p(), timeline.duration(), &ExportOptions::default(), true, true).filter
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
        clip.transform = crate::model::Transform { opacity: 0.5, ..Default::default() };
        clip.color = crate::model::Color { brightness: 0.1, ..Default::default() };
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
        assert!(g.contains("trim=start=0:end=10"), "outgoing clip must not bleed across the gap: {g}");
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
        assert!(g.contains("trim=start=4:end=15"), "reversed tail extends below source_in: {g}");
        assert!(g.contains(",reverse,"), "{g}");
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
        args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
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
    fn build_export_args_x265_mp4_tags_hvc1_but_mkv_does_not() {
        let mp4 = ExportOptions { video_codec: Some("libx265".into()), container: Container::Mp4, ..Default::default() };
        assert_eq!(flag_val(&args_of(&mp4), "-tag:v"), Some("hvc1"));
        let mkv = ExportOptions { video_codec: Some("libx265".into()), container: Container::Mkv, ..Default::default() };
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
        let mp4 = ExportOptions { video_codec: Some("libx264".into()), faststart: true, ..Default::default() };
        assert_eq!(flag_val(&args_of(&mp4), "-movflags"), Some("+faststart"));
        let mkv = ExportOptions { container: Container::Mkv, video_codec: Some("libx264".into()), faststart: true, ..Default::default() };
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
        assert!(!args.contains(&"[outv]".to_string()), "no video map for an audio-only container");
        assert!(!args.contains(&"-c:v".to_string()));
        assert!(!args.contains(&"-pix_fmt".to_string()));
        assert!(args.contains(&"[outa]".to_string()));
        assert_eq!(flag_val(&args, "-c:a"), Some("libmp3lame"));
        assert_eq!(flag_val(&args, "-b:a"), Some("320k"));
    }

    #[test]
    fn build_export_args_include_audio_false_emits_an() {
        let opts = ExportOptions { video_codec: Some("libx264".into()), include_audio: false, ..Default::default() };
        let args = args_of(&opts);
        assert!(!args.contains(&"[outa]".to_string()));
        assert!(args.contains(&"-an".to_string()));
        assert!(!args.contains(&"-c:a".to_string()));
    }

    #[test]
    fn build_export_args_lossless_per_codec() {
        let x264 = ExportOptions { video_codec: Some("libx264".into()), rate_control: RateControl::Lossless, ..Default::default() };
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
        let p1 = build_export_args_phase(&timeline, &assets, "/out/x.mp4", &opts, PassPhase::First, "/dev/null", "/tmp/pl").unwrap();
        assert_eq!(flag_val(&p1, "-b:v"), Some("8M"));
        assert_eq!(flag_val(&p1, "-pass"), Some("1"));
        assert_eq!(flag_val(&p1, "-passlogfile"), Some("/tmp/pl"));
        assert!(!p1.contains(&"[outa]".to_string()), "the analysis pass is video-only");
        assert!(p1.contains(&"-f".to_string()) && p1.contains(&"null".to_string()));
        assert_eq!(p1.last().unwrap(), "/dev/null");
        // The null muxer rejects mov/metadata options — they belong to pass 2 only.
        assert!(!p1.contains(&"-movflags".to_string()) && !p1.contains(&"-metadata".to_string()));
        let p2 = build_export_args_phase(&timeline, &assets, "/out/x.mp4", &opts, PassPhase::Second, "/dev/null", "/tmp/pl").unwrap();
        assert_eq!(flag_val(&p2, "-pass"), Some("2"));
        assert!(p2.contains(&"[outa]".to_string()));
        assert_eq!(flag_val(&p2, "-movflags"), Some("+faststart"));
        assert_eq!(p2.last().unwrap(), "/out/x.mp4");
    }

    #[test]
    fn filter_pix_fmt_is_threaded_through_the_graph() {
        let opts = ExportOptions { video_codec: Some("libx265".into()), pix_fmt: Some("yuv420p10le".into()), ..Default::default() };
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
        let opts = ExportOptions { resolution: Some((1921, 1081)), audio_codec: Some("libopus".into()), ..Default::default() };
        let fmt = export_format(&timeline, &[asset], &opts);
        assert_eq!((fmt.width, fmt.height), (1920, 1080));
        assert_eq!(fmt.sample_rate, 48_000);
    }

    #[test]
    fn gif_uses_a_palette_and_drops_audio() {
        let opts = ExportOptions { container: Container::Gif, video_codec: Some("gif".into()), include_audio: false, ..Default::default() };
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
        let opts = ExportOptions { video_codec: Some("libx264".into()), metadata_title: Some("My Cut = v2".into()), ..Default::default() };
        let args = args_of(&opts);
        let i = args.iter().position(|a| a == "-metadata").unwrap();
        assert_eq!(args[i + 1], "title=My Cut = v2");
    }

    #[test]
    fn fps_never_emits_dash_r() {
        let opts = ExportOptions { video_codec: Some("libx264".into()), fps: Some(24.0), ..Default::default() };
        let args = args_of(&opts);
        assert!(!args.contains(&"-r".to_string()), "fps lives only in the filtergraph");
        assert!(flag_val(&args, "-filter_complex").unwrap().contains("fps=24"));
    }

    #[test]
    fn validate_export_flags_bad_combinations() {
        let webm_x264 = ExportOptions { container: Container::Webm, video_codec: Some("libx264".into()), ..Default::default() };
        assert!(!validate_export(&webm_x264, true, true).is_empty());
        let mp4_opus = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            audio_codec: Some("libopus".into()),
            ..Default::default()
        };
        assert!(!validate_export(&mp4_opus, true, true).is_empty());
        let two_pass_no_bitrate = ExportOptions { video_codec: Some("libx264".into()), rate_control: RateControl::TwoPass, ..Default::default() };
        assert!(!validate_export(&two_pass_no_bitrate, true, true).is_empty());
        let mp3_no_audio = ExportOptions { container: Container::Mp3, audio_codec: Some("libmp3lame".into()), ..Default::default() };
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
        let opts = ExportOptions { container: Container::Mov, video_codec: Some("prores_ks".into()), prores_profile: Some(3), ..Default::default() };
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let args = build_export_args(&timeline, &[asset], "/out/x.mov", &opts).unwrap();
        // Both the argv and the graph terminal use 4:2:2 10-bit — no 4:2:0 bottleneck.
        assert_eq!(flag_val(&args, "-pix_fmt"), Some("yuv422p10le"));
        assert!(flag_val(&args, "-filter_complex").unwrap().contains("yuv422p10le"));
        assert!(!args.contains(&"yuv420p".to_string()));
        // The 4444 profiles upgrade to 4:4:4 with alpha.
        let xq = ExportOptions { container: Container::Mov, video_codec: Some("prores_ks".into()), prores_profile: Some(5), ..Default::default() };
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
        let opts = ExportOptions { video_codec: Some("libx265".into()), tune: Some("film".into()), ..Default::default() };
        assert!(!args_of(&opts).contains(&"-tune".to_string()), "an invalid tune must not reach ffmpeg");
        assert!(!validate_export(&opts, true, true).is_empty(), "validation must flag it");
        // A valid x265 tune is kept.
        let ok = ExportOptions { video_codec: Some("libx265".into()), tune: Some("grain".into()), ..Default::default() };
        assert_eq!(flag_val(&args_of(&ok), "-tune"), Some("grain"));
        assert!(validate_export(&ok, true, true).is_empty());
    }
}
