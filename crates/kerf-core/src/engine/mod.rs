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

#[cfg(feature = "ffmpeg")]
mod ffmpeg;

// Analysis, frame and waveform extraction always go through the CLI backend —
// they only need the FFmpeg binaries, never the dev libraries.
pub use cli::{detect_scenes, detect_silence, frame_at, waveform};

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
pub fn render(
    timeline: &crate::model::Timeline,
    assets: &[crate::model::Asset],
    output: &Path,
    format: &str,
) -> Result<()> {
    ffmpeg::render(timeline, assets, output, format)
}

/// Render the timeline to `output` (in-process libav with the `libav-render`
/// feature, otherwise by driving the `ffmpeg` binary).
#[cfg(not(feature = "libav-render"))]
pub fn render(
    timeline: &crate::model::Timeline,
    assets: &[crate::model::Asset],
    output: &Path,
    format: &str,
) -> Result<()> {
    cli::render(timeline, assets, output, format)
}
