//! Media engine: probing and rendering.
//!
//! When the `ffmpeg` feature is enabled these are backed by in-process libav
//! (via `ffmpeg-next`). Otherwise they return [`Error::FfmpegDisabled`], which
//! lets the rest of the core (domain model, persistence, MCP read tools) build
//! and run without the FFmpeg development libraries installed.

use std::path::Path;

use crate::error::Result;
use crate::model::StreamInfo;

/// Result of probing a media file.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub duration: f64,
    pub streams: Vec<StreamInfo>,
}

#[cfg(feature = "ffmpeg")]
mod ffmpeg;

#[cfg(feature = "ffmpeg")]
pub use ffmpeg::{probe, render};

#[cfg(not(feature = "ffmpeg"))]
pub fn probe(_path: &Path) -> Result<ProbeResult> {
    Err(crate::error::Error::FfmpegDisabled)
}

#[cfg(not(feature = "ffmpeg"))]
pub fn render(
    _timeline: &crate::model::Timeline,
    _assets: &[crate::model::Asset],
    _output: &Path,
    _format: &str,
) -> Result<()> {
    Err(crate::error::Error::FfmpegDisabled)
}
