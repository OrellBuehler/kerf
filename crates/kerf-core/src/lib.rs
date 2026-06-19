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

pub use analysis::{
    analyze, AnalysisProviders, FfmpegSceneDetector, FfmpegSilenceDetector, NullAnalyzer,
    SceneDetector, SilenceDetector, Transcriber,
};
#[cfg(feature = "whisper")]
pub use analysis::WhisperTranscriber;
pub use error::{Error, Result};
pub use model::{
    Asset, AssetAnalysis, Clip, EditSource, Revision, StreamInfo, StreamKind, TimeRange, Timeline,
    Track, TranscriptSegment,
};
pub use project::Project;
