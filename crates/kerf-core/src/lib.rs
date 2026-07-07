//! `kerf-core` — the domain model, `.kerf` project persistence (SQLite), and
//! FFmpeg media engine for Kerf.
//!
//! Everything an editor needs that is independent of the UI shell or the MCP
//! server lives here: assets, cached analysis metadata, the non-destructive
//! timeline (EDL), and the operations that mutate it.

pub mod analysis;
pub mod error;
pub mod fonts;
pub mod model;
pub mod project;

mod engine;

#[cfg(feature = "whisper")]
pub use analysis::WhisperTranscriber;
pub use analysis::{
    analyze, analyze_asset_media, AnalysisProviders, FfmpegSceneDetector, FfmpegSilenceDetector, NullAnalyzer, SceneDetector,
    SilenceDetector, Transcriber,
};
pub use engine::{
    generate_proxy, proxy_path, render_with, render_with_progress, validate_export, Container, ExportOptions, ExportProgress,
    RateControl, RenderStatus,
};
pub use error::{Error, Result};
pub use fonts::list_system_fonts;
pub use model::{
    Asset, AssetAnalysis, AudioEffect, Clip, Color, EditSource, Keyframe, Revision, StreamInfo, StreamKind, Task, TaskStatus,
    TextKeyframe, TextOverlay, TimeRange, Timeline, Track, TranscriptSegment, Transform, Transition, TransitionKind, VideoEffect,
};
pub use project::Project;
