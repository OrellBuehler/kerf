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
use crate::model::{Asset, StreamInfo, StreamKind, TimeRange, Timeline};

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
    let bin = ffmpeg_bin();
    let scale = format!("scale='min({max_width},iw)':-2");
    let output = Command::new(&bin)
        .args(["-hide_banner", "-loglevel", "error", "-ss"])
        .arg(format!("{:.3}", time_secs.max(0.0)))
        .arg("-i")
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &scale,
            "-f",
            "image2pipe",
            "-vcodec",
            "png",
            "pipe:1",
        ])
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| launch_err(&bin, e))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(Error::Engine(format!(
            "could not extract frame at {time_secs:.3}s: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
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

/// Configurable options for the CLI export pipeline.
///
/// `Default` reproduces the original hard-coded behaviour exactly: no explicit
/// codec flags (ffmpeg picks libx264 + aac from the container), no CRF
/// override, and no resolution/fps override (both derived from the source
/// clips).
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// Container / muxer hint passed to ffmpeg via the output extension.
    /// `None` means the extension on the output path determines the format
    /// (current default behaviour).
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    /// Constant Rate Factor; `None` lets ffmpeg use its encoder default.
    pub crf: Option<u32>,
    /// Force a specific output resolution, overriding the value derived from
    /// the first video clip.
    pub resolution: Option<(u32, u32)>,
    /// Force a specific output frame rate, overriding the value derived from
    /// the first video clip.
    pub fps: Option<f64>,
}

/// The single output shape every clip is normalized to before `concat`. The
/// `concat` filter requires identical resolution / frame rate / sample format
/// across its inputs, and `concat`'s `a=1` requires every segment to carry
/// audio — so clips from a video-only asset get synthesized silence.
#[derive(Debug, Clone, Copy)]
struct ExportFormat {
    width: u32,
    height: u32,
    fps: f64,
    sample_rate: u32,
    channels: u16,
}

impl Default for ExportFormat {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30.0,
            sample_rate: 48_000,
            channels: 2,
        }
    }
}

impl ExportFormat {
    fn channel_layout(self) -> &'static str {
        if self.channels <= 1 {
            "mono"
        } else {
            "stereo"
        }
    }
}

/// Derive the output shape from the first clip that carries a video stream and
/// the first that carries audio, falling back to 1080p30 stereo defaults.
/// When `opts` carries resolution or fps overrides those values win.
fn export_format(track: &crate::model::Track, assets: &[Asset], opts: &ExportOptions) -> ExportFormat {
    let stream_of = |clip: &crate::model::Clip, kind: StreamKind| {
        assets
            .iter()
            .find(|a| a.id == clip.asset_id)
            .and_then(|a| a.streams.iter().find(|s| s.kind == kind))
    };

    let mut fmt = ExportFormat::default();
    if let Some(v) = track.clips.iter().find_map(|c| stream_of(c, StreamKind::Video)) {
        if let (Some(w), Some(h)) = (v.width, v.height) {
            fmt.width = w;
            fmt.height = h;
        }
        if let Some(f) = v.fps.filter(|f| *f > 0.0) {
            fmt.fps = f;
        }
    }
    if let Some(a) = track.clips.iter().find_map(|c| stream_of(c, StreamKind::Audio)) {
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
    fmt
}

/// Build the complete argument list for `ffmpeg` (everything after the binary
/// name) that renders `track` to `output_path`.
///
/// The function is pure — it performs no I/O and does not spawn ffmpeg —
/// which makes it unit-testable without the binary being present. The actual
/// render call feeds the returned `Vec<String>` straight to `Command::args`.
pub fn build_export_args(
    track: &crate::model::Track,
    assets: &[Asset],
    output_path: &str,
    opts: &ExportOptions,
) -> Result<Vec<String>> {
    let path_of = |id| assets.iter().find(|a| a.id == id).map(|a| a.path.as_str());

    let mut args: Vec<String> = vec!["-y".to_string()];
    for clip in &track.clips {
        let path = path_of(clip.asset_id).ok_or(Error::AssetNotFound(clip.asset_id))?;
        args.push("-i".to_string());
        args.push(path.to_string());
    }
    let fmt = export_format(track, assets, opts);
    let filter = build_filter_complex(track, assets, &fmt);
    args.push("-filter_complex".to_string());
    args.push(filter);
    args.push("-map".to_string());
    args.push("[outv]".to_string());
    args.push("-map".to_string());
    args.push("[outa]".to_string());
    if let Some(ref vc) = opts.video_codec {
        args.push("-c:v".to_string());
        args.push(vc.clone());
    }
    if let Some(ref ac) = opts.audio_codec {
        args.push("-c:a".to_string());
        args.push(ac.clone());
    }
    if let Some(crf) = opts.crf {
        args.push("-crf".to_string());
        args.push(crf.to_string());
    }
    args.push(output_path.to_string());
    Ok(args)
}

/// Render the timeline by driving the `ffmpeg` binary with a generated
/// `filter_complex` (trim + per-clip volume + normalize + concat).
// With the `libav-render` feature the in-process libav executor is used instead.
#[cfg_attr(feature = "libav-render", allow(dead_code))]
pub fn render(timeline: &Timeline, assets: &[Asset], output: &Path, _format: &str) -> Result<()> {
    render_with(timeline, assets, output, &ExportOptions::default())
}

/// Like [`render`] but with explicit export options.
#[cfg_attr(feature = "libav-render", allow(dead_code))]
pub fn render_with(timeline: &Timeline, assets: &[Asset], output: &Path, opts: &ExportOptions) -> Result<()> {
    let track = timeline
        .tracks
        .iter()
        .find(|t| t.kind == StreamKind::Video && !t.clips.is_empty())
        .or_else(|| timeline.tracks.iter().find(|t| !t.clips.is_empty()))
        .ok_or_else(|| Error::InvalidArgument("timeline has no clips to export".to_string()))?;

    let output_str = output
        .to_str()
        .ok_or_else(|| Error::InvalidArgument(format!("non-UTF-8 output path: {}", output.display())))?;

    let args = build_export_args(track, assets, output_str, opts)?;

    let bin = ffmpeg_bin();
    let status = Command::new(&bin).args(&args).status().map_err(|e| launch_err(&bin, e))?;
    if !status.success() {
        return Err(Error::Engine(format!("ffmpeg exited with {status}")));
    }
    Ok(())
}

/// Build the `filter_complex` string that trims each clip to its source range,
/// normalizes it to `fmt`, applies per-clip volume, and concatenates the clips
/// into `[outv][outa]`.
///
/// Each video segment is scaled (preserving aspect, padded to fit) to a common
/// resolution, frame rate and pixel format so `concat` accepts them. Clips
/// whose asset has no audio stream get synthesized silence (`anullsrc`) of the
/// clip's duration, so `concat`'s `a=1` always has an audio input to consume.
///
/// Kept pure (no I/O) so it is unit-testable.
fn build_filter_complex(track: &crate::model::Track, assets: &[Asset], fmt: &ExportFormat) -> String {
    let has_audio = |clip: &crate::model::Clip| {
        assets
            .iter()
            .find(|a| a.id == clip.asset_id)
            .is_some_and(|a| a.streams.iter().any(|s| s.kind == StreamKind::Audio))
    };
    let layout = fmt.channel_layout();

    let mut filter = String::new();
    let mut concat_inputs = String::new();
    for (i, clip) in track.clips.iter().enumerate() {
        filter.push_str(&format!(
            "[{i}:v]trim=start={start}:end={end},setpts=PTS-STARTPTS,\
             scale={w}:{h}:force_original_aspect_ratio=decrease,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps},format=yuv420p[v{i}];",
            i = i,
            start = clip.source_in,
            end = clip.source_out,
            w = fmt.width,
            h = fmt.height,
            fps = fmt.fps,
        ));
        if has_audio(clip) {
            filter.push_str(&format!(
                "[{i}:a]atrim=start={start}:end={end},asetpts=PTS-STARTPTS,volume={vol},\
                 aformat=sample_rates={sr}:channel_layouts={layout}[a{i}];",
                i = i,
                start = clip.source_in,
                end = clip.source_out,
                vol = clip.volume,
                sr = fmt.sample_rate,
                layout = layout,
            ));
        } else {
            filter.push_str(&format!(
                "anullsrc=r={sr}:cl={layout}:d={dur},asetpts=PTS-STARTPTS[a{i}];",
                sr = fmt.sample_rate,
                layout = layout,
                dur = clip.duration(),
                i = i,
            ));
        }
        concat_inputs.push_str(&format!("[v{i}][a{i}]", i = i));
    }
    filter.push_str(&format!(
        "{inputs}concat=n={n}:v=1:a=1[outv][outa]",
        inputs = concat_inputs,
        n = track.clips.len()
    ));
    filter
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Asset, Clip, StreamInfo, StreamKind, Track};
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
    fn filter_complex_concats_and_normalizes_clips() {
        // One clip with audio, one from a video-only asset.
        let with_audio = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
        let video_only = test_asset(vec![video_stream(3840, 2160, 24.0)]);
        let assets = vec![with_audio.clone(), video_only.clone()];

        let track = Track {
            id: Uuid::new_v4(),
            kind: StreamKind::Video,
            name: "V1".into(),
            clips: vec![
                Clip {
                    id: Uuid::new_v4(),
                    asset_id: with_audio.id,
                    source_in: 0.0,
                    source_out: 5.0,
                    timeline_start: 0.0,
                    volume: 0.5,
                },
                Clip {
                    id: Uuid::new_v4(),
                    asset_id: video_only.id,
                    source_in: 2.0,
                    source_out: 4.0,
                    timeline_start: 5.0,
                    volume: 1.0,
                },
            ],
        };

        let opts = ExportOptions::default();
        let fmt = export_format(&track, &assets, &opts);
        // Output shape comes from the first video/audio-bearing clips.
        assert_eq!((fmt.width, fmt.height), (1920, 1080));
        assert_eq!(fmt.sample_rate, 48_000);

        let f = build_filter_complex(&track, &assets, &fmt);
        assert!(f.contains("concat=n=2:v=1:a=1[outv][outa]"));
        assert!(f.contains("volume=0.5"));
        assert!(f.contains("[0:v]trim=start=0:end=5"));
        // Every video segment is scaled/padded to the common resolution.
        assert_eq!(f.matches("scale=1920:1080").count(), 2);
        assert!(f.contains("format=yuv420p"));
        // The clip with audio is trimmed + reformatted; the video-only clip
        // gets synthesized silence so concat's a=1 has an input.
        assert!(f.contains("[0:a]atrim=start=0:end=5"));
        assert!(f.contains("aformat=sample_rates=48000:channel_layouts=stereo"));
        assert!(f.contains("anullsrc=r=48000:cl=stereo:d=2"));
        assert!(!f.contains("[1:a]"));
    }

    #[test]
    fn export_format_falls_back_to_defaults() {
        let track = Track {
            id: Uuid::new_v4(),
            kind: StreamKind::Video,
            name: "V1".into(),
            clips: Vec::new(),
        };
        let fmt = export_format(&track, &[], &ExportOptions::default());
        assert_eq!((fmt.width, fmt.height), (1920, 1080));
        assert_eq!(fmt.channel_layout(), "stereo");
    }

    fn make_track(clips: Vec<Clip>) -> Track {
        Track {
            id: Uuid::new_v4(),
            kind: StreamKind::Video,
            name: "V1".into(),
            clips,
        }
    }

    fn make_clip(asset_id: uuid::Uuid, source_in: f64, source_out: f64, timeline_start: f64) -> Clip {
        Clip {
            id: Uuid::new_v4(),
            asset_id,
            source_in,
            source_out,
            timeline_start,
            volume: 1.0,
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
        };
        let track = make_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let assets = vec![asset];
        let opts = ExportOptions::default();

        let args = build_export_args(&track, &assets, "/out/result.mp4", &opts).unwrap();

        assert_eq!(args[0], "-y");
        assert_eq!(args[1], "-i");
        assert_eq!(args[2], "/media/clip.mp4");
        assert!(args.contains(&"-filter_complex".to_string()));
        let fc_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
        let filter = &args[fc_pos + 1];
        assert!(filter.contains("trim=start=0:end=10"));
        assert!(filter.contains("concat=n=1:v=1:a=1[outv][outa]"));
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
    fn build_export_args_multi_clip_concat() {
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
        let track = make_track(vec![make_clip(a1.id, 0.0, 20.0, 0.0), make_clip(a2.id, 0.0, 10.0, 20.0)]);
        let assets = vec![a1, a2.clone()];
        let opts = ExportOptions::default();

        let args = build_export_args(&track, &assets, "/out/concat.mp4", &opts).unwrap();

        // Two -i flags for the two clips.
        let input_count = args.windows(2).filter(|w| w[0] == "-i").count();
        assert_eq!(input_count, 2);
        let fc_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
        let filter = &args[fc_pos + 1];
        assert!(filter.contains("concat=n=2:v=1:a=1[outv][outa]"));
        assert_eq!(args.last().unwrap(), "/out/concat.mp4");
    }

    #[test]
    fn build_export_args_video_only_asset_gets_silence() {
        let video_only = Asset {
            id: Uuid::new_v4(),
            path: "/media/vo.mp4".into(),
            name: "vo.mp4".into(),
            duration: 5.0,
            streams: vec![video_stream(1920, 1080, 30.0)],
            imported_at: Utc::now(),
        };
        let track = make_track(vec![make_clip(video_only.id, 0.0, 5.0, 0.0)]);
        let assets = vec![video_only];
        let opts = ExportOptions::default();

        let args = build_export_args(&track, &assets, "/out/vo.mp4", &opts).unwrap();

        let fc_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
        let filter = &args[fc_pos + 1];
        assert!(filter.contains("anullsrc"), "video-only clip must get synthesized silence");
        assert!(!filter.contains("[0:a]"), "no real audio stream should be trimmed");
        assert!(filter.contains("concat=n=1:v=1:a=1[outv][outa]"));
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
        let track = make_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let assets = vec![asset];
        let opts = ExportOptions {
            video_codec: Some("libx264".to_string()),
            audio_codec: Some("aac".to_string()),
            crf: Some(23),
            resolution: None,
            fps: None,
        };

        let args = build_export_args(&track, &assets, "/out/result.mp4", &opts).unwrap();

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
        let track = make_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let assets = vec![asset];
        let opts = ExportOptions {
            video_codec: None,
            audio_codec: None,
            crf: None,
            resolution: Some((1920, 1080)),
            fps: Some(30.0),
        };

        let args = build_export_args(&track, &assets, "/out/downscaled.mp4", &opts).unwrap();

        let fc_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
        let filter = &args[fc_pos + 1];
        // Override forces 1920x1080 even though the source is 4K.
        assert!(filter.contains("scale=1920:1080"), "resolution override must apply");
        assert!(filter.contains("fps=30"), "fps override must apply");
    }

    #[test]
    fn build_export_args_error_on_missing_asset() {
        let track = make_track(vec![make_clip(Uuid::new_v4(), 0.0, 5.0, 0.0)]);
        let result = build_export_args(&track, &[], "/out/result.mp4", &ExportOptions::default());
        assert!(matches!(result, Err(Error::AssetNotFound(_))));
    }
}
