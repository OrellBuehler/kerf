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

// Audio analysis (loudness, energy, onsets, tempo, classification): CLI/PCM
// based, available in every build like the rest of `cli`.
mod audio;
pub use audio::{detect_onsets, energy_envelope, measure_loudness};

#[cfg(feature = "ffmpeg")]
mod ffmpeg;

// Analysis, frame and waveform extraction always go through the CLI backend —
// they only need the FFmpeg binaries, never the dev libraries.
pub use cli::{
    contact_sheet, detect_scenes, detect_silence, frame_at, frame_jpeg, generate_proxy, proxy_path, ready_proxy,
    timeline_frame, validate_export, waveform, Container, ExportOptions, ExportProgress, RateControl, RenderStatus,
};

#[cfg(feature = "whisper")]
pub use cli::decode_audio_16k_mono;

/// Probe a media file for duration and per-stream metadata.
#[cfg(feature = "ffmpeg")]
pub fn probe(path: &Path) -> Result<ProbeResult> {
    ffmpeg::probe(path)
}

/// Probe a media file for duration and per-stream metadata.
#[cfg(not(feature = "ffmpeg"))]
pub fn probe(path: &Path) -> Result<ProbeResult> {
    cli::probe(path)
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
