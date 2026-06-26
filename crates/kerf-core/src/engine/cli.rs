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
use crate::model::{Asset, Clip, StreamInfo, StreamKind, TimeRange, Timeline, TransitionKind};

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
    fmt
}

/// Build the complete argument list for `ffmpeg` (everything after the binary
/// name) that renders the whole `timeline` to `output_path`.
///
/// One input (`-i`) is added per clip, in track-then-clip order; the filtergraph
/// (see [`build_filter_complex`]) references those inputs by the same index. The
/// `[outv]` / `[outa]` maps are added only for the streams the graph actually
/// produces (a timeline with no audio-bearing clips yields no `[outa]`).
///
/// The function is pure — it performs no I/O and does not spawn ffmpeg —
/// which makes it unit-testable without the binary being present. The actual
/// render call feeds the returned `Vec<String>` straight to `Command::args`.
pub fn build_export_args(
    timeline: &Timeline,
    assets: &[Asset],
    output_path: &str,
    opts: &ExportOptions,
) -> Result<Vec<String>> {
    let path_of = |id| assets.iter().find(|a| a.id == id).map(|a| a.path.as_str());

    // `-hide_banner -nostats` keep the captured stderr to genuine warnings/errors
    // (matching the probe/frame calls); without `-nostats` the per-frame progress
    // lines would accumulate unbounded in memory for a long export.
    let mut args: Vec<String> = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-nostats".to_string(),
    ];
    for clip in timeline.tracks.iter().flat_map(|t| t.clips.iter()) {
        let path = path_of(clip.asset_id).ok_or(Error::AssetNotFound(clip.asset_id))?;
        args.push("-i".to_string());
        args.push(path.to_string());
    }
    let fmt = export_format(timeline, assets, opts);
    let total = timeline.duration();
    let graph = build_filter_complex(timeline, assets, &fmt, total);
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
    if !timeline.tracks.iter().any(|t| !t.clips.is_empty()) {
        return Err(Error::InvalidArgument("timeline has no clips to export".to_string()));
    }

    let output_str = output
        .to_str()
        .ok_or_else(|| Error::InvalidArgument(format!("non-UTF-8 output path: {}", output.display())))?;

    let args = build_export_args(timeline, assets, output_str, opts)?;

    let bin = ffmpeg_bin();
    let clips: usize = timeline.tracks.iter().map(|t| t.clips.len()).sum();
    tracing::info!(output = %output.display(), clips, "exporting timeline");
    tracing::debug!(command = %format!("{bin} {}", args.join(" ")), "ffmpeg export command");

    // Capture output (rather than inheriting) so ffmpeg's stderr — the actual
    // failure reason — ends up in the error and the log, not just the console.
    let out = Command::new(&bin).args(&args).output().map_err(|e| launch_err(&bin, e))?;
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
fn build_filter_complex(timeline: &Timeline, assets: &[Asset], fmt: &ExportFormat, total: f64) -> FilterGraph {
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
    let has_video = !video.is_empty();
    if has_video {
        chains.push(format!(
            "color=c=black:s={w}x{h}:r={fps}:d={total},format=yuv420p[vbase]",
            w = fmt.width,
            h = fmt.height,
            fps = fmt.fps,
            total = total.max(0.0),
        ));
        let mut cur = "vbase".to_string();
        let last = video.len() - 1;
        for (n, (i, clip)) in video.iter().enumerate() {
            chains.push(format!("[{i}:v]{chain}[v{i}]", i = i, chain = video_clip_chain(clip, fmt, &fx[*i])));
            let out = if n == last { "outv".to_string() } else { format!("vov{n}") };
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
    }

    // ---- sound: positioned per-clip audio summed with amix ------------------
    let has_audio_out = !audio.is_empty();
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
    if t.is_identity() {
        p.push(format!("scale={w}:{h}:force_original_aspect_ratio=decrease", w = fmt.width, h = fmt.height));
        p.push(format!("pad={w}:{h}:(ow-iw)/2:(oh-ih)/2", w = fmt.width, h = fmt.height));
    } else {
        p.push(format!("scale={w}:{h}:force_original_aspect_ratio=decrease", w = fmt.width, h = fmt.height));
        if (t.scale - 1.0).abs() > 1e-9 {
            p.push(format!("scale=iw*{sc}:ih*{sc}", sc = t.scale));
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
        p.push("format=yuv420p".to_string());
    }
    p.join(",")
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

        let g = build_filter_complex(&timeline, &assets, &fmt, timeline.duration());
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
        let g = build_filter_complex(&timeline, &assets, &fmt, timeline.duration());
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
        let g = build_filter_complex(&timeline, &assets, &fmt, timeline.duration());
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
        let f = build_filter_complex(&timeline, &assets, &fmt, timeline.duration()).filter;
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
        let f = build_filter_complex(&timeline, &assets, &fmt, timeline.duration()).filter;
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
            resolution: None,
            fps: None,
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
            video_codec: None,
            audio_codec: None,
            crf: None,
            resolution: Some((1920, 1080)),
            fps: Some(30.0),
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
        }
    }

    fn graph_of(timeline: &Timeline, assets: &[Asset]) -> String {
        build_filter_complex(timeline, assets, &fmt_1080p(), timeline.duration()).filter
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
}
