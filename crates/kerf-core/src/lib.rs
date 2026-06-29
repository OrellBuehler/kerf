//! `kerf-core` — the domain model, `.kerf` project persistence (SQLite), and
//! FFmpeg media engine for Kerf.
//!
//! Everything an editor needs that is independent of the UI shell or the MCP
//! server lives here: assets, cached analysis metadata, the non-destructive
//! timeline (EDL), and the operations that mutate it.

pub mod analysis;
pub mod error;
pub mod model;
pub mod project;

mod engine;

#[cfg(feature = "whisper")]
pub use analysis::WhisperTranscriber;
pub use analysis::{
    analyze, analyze_asset_media, AnalysisProviders, FfmpegSceneDetector, FfmpegSilenceDetector, NullAnalyzer,
    SceneDetector, SilenceDetector, Transcriber,
};
pub use engine::{render_with, validate_export, Container, ExportOptions, RateControl};
pub use error::{Error, Result};
pub use model::{
    Asset, AssetAnalysis, Clip, Color, EditSource, Revision, StreamInfo, StreamKind, Task, TaskStatus, TimeRange, Timeline,
    Track, TranscriptSegment, Transform, Transition, TransitionKind,
};
pub use project::Project;
