//! Media engine: probing, analysis, frame/waveform extraction and rendering.
//!
//! Two backends sit behind one API:
//!
//! * [`cli`] drives the system `ffmpeg` / `ffprobe` **binaries**. It is always
//!   compiled, so probing, analysis, frame/waveform extraction and export work
//!   in the `--no-default-features` build (no FFmpeg *dev* libraries needed).
//! * [`ffmpeg`] (the `ffmpeg` feature) is in-process **libav** via
//!   `ffmpeg-next`, used for probing; with the additional `libav-render`
//!   feature it also drives an experimental in-process export pipeline.

use std::path::Path;

use crate::error::Result;
use crate::model::StreamInfo;

/// Result of probing a media file.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub duration: f64,
    pub streams: Vec<StreamInfo>,
}

mod cli;

// The colour specs a filter value may carry: ffmpeg's own grammar, not "looks like a colour".
mod colors;

// How `geq` (a mask's filter) reads the last column and row of a plane on this ffmpeg: measured, once.
mod geq_edge;
pub use geq_edge::{geq_edge, geq_penultimate_plane, GeqEdge};

// What the decodes ask `ffprobe` per file, bounded and remembered: container, HDR, start, and
// whether a seek lands on a keyframe.
mod source_probe;
pub use source_probe::source_seek_points_are_keyframes;

// Waveform peak pyramids and the windows read out of them: one decode per file,
// cached, drawn from at any zoom. Also CLI-only, so it works in every build.
mod peaks;
pub use peaks::{
    cache_path as waveform_cache_path, waveform_pyramid, waveform_range, waveform_range_of, waveform_range_of_cancellable,
    WaveformLevel, WaveformPyramid, WaveformRange, MAX_RANGE_BUCKETS as MAX_WAVEFORM_BUCKETS,
};

// Filmstrip thumbnails for timeline clips: one decode per asset into a few
// tiled JPEG sheets, cached on disk. CLI-only, so it works in every build.
mod filmstrip;
#[cfg(test)]
pub(crate) use filmstrip::cached_entry_dir as cached_filmstrip_dir;
pub use filmstrip::{filmstrip_for, Filmstrip, FilmstripSheet, FILMSTRIP_HEIGHT, MAX_FILMSTRIP_FRAMES};

/// Test-only: what the engine's tests share — the ffmpeg runner that bounds a
/// hung binary, and the asset / clip / timeline fixtures.
#[cfg(test)]
pub(crate) mod test_support;

// How much of the machine the engine may take: the one-heavy-job-at-a-time gate
// and the thread / priority caps every ffmpeg run is launched under.
pub mod cpu;

// Which hardware decoder each input of a run is asked for, and what a one-frame read of a
// preview proxy is capped to: decided at spawn from what is known of the file, never in the
// argv builders.
mod decode_policy;
pub use decode_policy::limit_frame_args;

// Audio analysis (loudness, energy, onsets, tempo, classification): CLI/PCM
// based, available in every build like the rest of `cli`.
mod audio;
pub use audio::{analyze_rhythm_cancellable, energy_envelope, energy_envelope_cancellable, measure_loudness_cancellable};

// Decoded audio as an interleaved f32 buffer (through the ffmpeg binary) and a WAV
// writer for tests and debug renders.
mod pcm;

// Bar-level music structure (fitted beat grid, downbeat, chroma, repeating phrases):
// pure DSP on decoded PCM, cached per file.
pub mod music;

// Word timing and speaker labels for transcripts: energy VAD, mel front end, diarization.
pub(crate) mod diarize;
pub(crate) mod mel;
pub(crate) mod vad;

// Stem separation (Demucs htdemucs on ONNX Runtime; STFT, chunking and overlap-add here).
pub mod stems;
pub use pcm::{decode_audio, decode_audio_cancellable, decode_audio_window, wav_bytes, write_wav, AudioBuffer};

// Lining two recordings up by their sound (GCC-PHAT-β, coarse then refined): pure DSP.
pub mod audio_sync;
pub use stems::{status as stems_status, StemsStatus, STEM_NAMES};

// Speech model provisioning + transcription through the `ffmpeg` binary's
// `whisper` filter. Like the rest of `cli`, needs no dev libraries — so the
// transcript surface works in the `--no-default-features` build too.
pub mod whisper;

// Resumable, cancellable downloads of the optional models and runtimes.
mod download;

// Voiceover: Kokoro text-to-speech on a downloaded ONNX Runtime.
pub mod tts;

// Composing music: a score rendered through a SoundFont synthesizer (rustysynth), mixed in Rust.
pub mod compose;

// Band balance: where a stretch of sound keeps its energy (six bands per window).
pub mod bands;

#[cfg(feature = "ffmpeg")]
mod ffmpeg;

// Analysis, frame and waveform extraction always go through the CLI backend —
// they only need the FFmpeg binaries, never the dev libraries.
pub use cli::{
    audio_effects_filter, audio_pcm, audio_pcm_cancellable, check_caption_sidecars, contact_sheet_cancellable,
    contact_sheet_times, decode_hwaccel, delivery_frame, detect_scenes_preferring_cancellable, detect_silence_cancellable,
    disable_decode_hwaccel, discard_partial, export_still, export_still_cancellable, file_mtime, frame_at, frame_jpeg,
    frame_jpeg_cancellable, frame_jpeg_region, frame_jpeg_region_cancellable, generate_proxy, generate_proxy_with, hw_encoders,
    insta360_pair, mix_levels, playback_params, proxy_base_width, proxy_path, proxy_width, proxy_width_for, ready_proxy,
    remove_proxy_files, salience_map_cancellable, seek_arg, set_proxy_base_width, source_identity, stitch_insta360,
    stitch_insta360_with, stitched_path, stream_mix_pcm, stream_preview, stream_preview_until, stream_preview_with,
    timeline_frame_cancellable, timeline_frame_region_cancellable, timeline_frame_yuv_cancellable, validate_export, waveform,
    waveform_cancellable, Container, ExportOptions, ExportProgress, ExportVariant, Fit, ImageFormat, LoudnessPreset,
    PlaybackParams, PlaybackRequest, PreviewFrame, ProxyRun, RateControl, Region, RenderStatus, ScrubConfig, ScrubFrames,
    ScrubStats, VariantProgress, VariantsRun, MAX_STREAM_FPS, MAX_STREAM_WIDTH, MIN_STREAM_FPS, MIN_STREAM_WIDTH,
};

#[cfg(test)]
pub(crate) use cli::golden::sample_timelines;
pub(crate) use cli::insta360_pair_name;
#[cfg(test)]
pub(crate) use cli::proxy_sidecar_path;
pub(crate) use cli::proxy_video_info;
pub(crate) use cli::render_geometry;
pub use cli::{
    composite_color_policy, export_seek_arg, ffmpeg_command, ffmpeg_path, fps_mode_flag, limit_ffmpeg_args,
    source_is_indexed_container, text_raster_graph, text_still_filter, TextPart, TextRasterGraph,
};
pub(crate) use cli::{keyframe_value, proxy_fidelity, ProxyFidelity};
pub(crate) use colors::{safe_color, valid_color};

// Speech models: the download itself, plus what can be downloaded. Re-exported
// under speech-prefixed names so the crate root reads unambiguously next to the
// `whisper` build feature (which is only one of the two backends).
pub use whisper::{
    download_model as download_speech_model, download_model_cancellable as download_speech_model_cancellable,
    set_model as set_speech_model, DownloadProgress, ModelInfo as SpeechModelInfo, DEFAULT_MODEL as DEFAULT_SPEECH_MODEL,
};

pub use tts::{
    prepare as prepare_voiceover, status as voiceover_status, VoiceInfo, VoiceoverStatus, DEFAULT_VOICE,
    MAX_SPEED as MAX_VOICE_SPEED, MIN_SPEED as MIN_VOICE_SPEED,
};

pub use bands::{band_balance, BandLevel, BandWindow, WindowBalance};
pub use compose::{prepare as prepare_soundfont, status as music_status, MusicStatus, StyleInfo};

/// The names of every speech model Kerf can download, smallest first.
pub fn speech_model_names() -> Vec<&'static str> {
    whisper::MODELS.iter().map(|m| m.name).collect()
}

#[cfg(feature = "whisper")]
pub use cli::decode_audio_16k_mono_cancellable;

/// How long the in-process probe may take before the caller stops waiting for it. Five minutes: a
/// probe only reads headers, but it is the call a file on a share that is slow to wake (a NAS
/// spinning its disks up, a VPN) takes its time over, and a probe has no progress to listen for,
/// so the bound is a plain one and a cancel ends the wait sooner.
#[cfg_attr(not(feature = "ffmpeg"), allow(dead_code))]
const LIBAV_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Run `probe` on a thread and wait for it for `limit`, or until `cancel` is true. libav opens a
/// file in-process, where nothing can kill it: a file on a share that stopped answering would hold
/// the caller (an import, with the project lock one step away) for good. The thread is left to
/// finish on its own — it holds nothing but the path — and the caller gets an error it can show
/// (`Error::Cancelled` for a stop).
#[cfg_attr(not(feature = "ffmpeg"), allow(dead_code))]
fn probe_within<T: Send + 'static>(
    limit: std::time::Duration,
    path: &Path,
    cancel: &dyn Fn() -> bool,
    probe: impl FnOnce(&Path) -> Result<T> + Send + 'static,
) -> Result<T> {
    use crate::error::Error;
    use std::sync::mpsc::{channel, RecvTimeoutError};
    use std::time::{Duration, Instant};

    let owned = path.to_path_buf();
    let (done, answer) = channel();
    std::thread::spawn(move || {
        let _ = done.send(probe(&owned));
    });
    let started = Instant::now();
    loop {
        let left = limit.saturating_sub(started.elapsed());
        match answer.recv_timeout(left.min(Duration::from_millis(50))) {
            Ok(result) => return result,
            Err(RecvTimeoutError::Timeout) if cancel() => return Err(Error::Cancelled),
            Err(RecvTimeoutError::Timeout) if started.elapsed() >= limit => {
                return Err(Error::Engine(format!(
                    "probing {} did not finish in {} s and was abandoned (is the file on a share that went away?)",
                    path.display(),
                    limit.as_secs()
                )))
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Error::Engine(format!("probing {} failed unexpectedly", path.display())))
            }
        }
    }
}

/// Probe a media file for duration and per-stream metadata. `cancel` stops it:
/// [`crate::error::Error::Cancelled`] once it is true.
#[cfg(feature = "ffmpeg")]
pub fn probe_cancellable(path: &Path, cancel: &dyn Fn() -> bool) -> Result<ProbeResult> {
    probe_within(LIBAV_PROBE_TIMEOUT, path, cancel, ffmpeg::probe)
}

/// Probe a media file for duration and per-stream metadata. `cancel` stops it:
/// [`crate::error::Error::Cancelled`] once it is true.
#[cfg(not(feature = "ffmpeg"))]
pub fn probe_cancellable(path: &Path, cancel: &dyn Fn() -> bool) -> Result<ProbeResult> {
    cli::probe_cancellable(path, cancel)
}

/// Render the timeline to `output` (in-process libav with the `libav-render`
/// feature, otherwise by driving the `ffmpeg` binary).
#[cfg(feature = "libav-render")]
pub fn render(timeline: &crate::model::Timeline, assets: &[crate::model::Asset], output: &Path, format: &str) -> Result<()> {
    ffmpeg::render(timeline, assets, output, format)
}

/// Render the timeline to `output` (in-process libav with the `libav-render`
/// feature, otherwise by driving the `ffmpeg` binary).
#[cfg(not(feature = "libav-render"))]
pub fn render(timeline: &crate::model::Timeline, assets: &[crate::model::Asset], output: &Path, format: &str) -> Result<()> {
    cli::render(timeline, assets, output, format)
}

/// Like [`render`] but with explicit [`ExportOptions`]. Always uses the CLI
/// backend (the libav-render feature does not yet support options).
pub fn render_with(
    timeline: &crate::model::Timeline,
    assets: &[crate::model::Asset],
    output: &Path,
    opts: &ExportOptions,
) -> Result<()> {
    cli::render_with(timeline, assets, output, opts)
}

/// Like [`render_with`] but streams [`ExportProgress`] and polls `cancel`.
/// Always the CLI backend.
pub fn render_with_progress(
    timeline: &crate::model::Timeline,
    assets: &[crate::model::Asset],
    output: &Path,
    opts: &ExportOptions,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    cli::render_with_progress(timeline, assets, output, opts, progress, cancel)
}

/// Check every variant and measure what they share, once — see [`cli::prepare_variants`].
pub fn prepare_variants(
    timeline: &crate::model::Timeline,
    assets: &[crate::model::Asset],
    variants: &[ExportVariant],
    opts: &ExportOptions,
    progress: &mut dyn FnMut(VariantProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<Option<ExportOptions>> {
    cli::prepare_variants(timeline, assets, variants, opts, progress, cancel)
}

/// Render one file per delivery frame — see [`cli::render_variants`].
pub fn render_variants(
    timeline: &crate::model::Timeline,
    assets: &[crate::model::Asset],
    variants: &[ExportVariant],
    opts: &ExportOptions,
    together: bool,
    progress: &mut dyn FnMut(VariantProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<VariantsRun> {
    cli::render_variants(timeline, assets, variants, opts, together, progress, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_probe_that_never_returns_is_abandoned_with_an_error() {
        let started = Instant::now();
        let err = probe_within(Duration::from_millis(300), Path::new("/mnt/gone/a.mp4"), &|| false, |_| {
            std::thread::sleep(Duration::from_secs(5));
            Ok(0u8)
        })
        .expect_err("a hung probe");
        assert!(
            err.to_string().contains("/mnt/gone/a.mp4") && err.to_string().contains("abandoned"),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    }

    #[test]
    fn a_probe_that_answers_gives_its_result_or_its_error() {
        assert_eq!(
            probe_within(Duration::from_secs(5), Path::new("a.mp4"), &|| false, |_| Ok(7u8)).unwrap(),
            7
        );
        let err = probe_within(Duration::from_secs(5), Path::new("a.mp4"), &|| false, |_| {
            Err::<u8, _>(crate::error::Error::Engine("no such stream".into()))
        })
        .unwrap_err();
        assert!(err.to_string().contains("no such stream"), "{err}");
    }

    #[test]
    fn a_cancelled_probe_is_given_up_on_at_once() {
        let started = Instant::now();
        let err = probe_within(Duration::from_secs(60), Path::new("/mnt/gone/a.mp4"), &|| true, |_| {
            std::thread::sleep(Duration::from_secs(5));
            Ok(0u8)
        })
        .unwrap_err();
        assert!(matches!(err, crate::error::Error::Cancelled), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    }

    #[test]
    fn a_probe_that_panics_is_an_error_not_a_hang() {
        let err = probe_within(Duration::from_secs(5), Path::new("a.mp4"), &|| false, |_| -> Result<u8> {
            panic!("libav")
        })
        .unwrap_err();
        assert!(err.to_string().contains("failed unexpectedly"), "{err}");
    }
}
