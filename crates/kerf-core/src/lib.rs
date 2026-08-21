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
    analyze, analyze_asset_media, analyze_asset_media_with_progress, analyze_with_progress, transcription_status,
    AnalysisProgress, AnalysisProviders, FfmpegRhythmAnalyzer, FfmpegSceneDetector, FfmpegSilenceDetector, NullAnalyzer,
    ProgressFn, RhythmAnalyzer, SceneDetector, SilenceDetector, Transcriber, TranscriptionStatus, WhisperFilterTranscriber,
};
pub use engine::{
    download_speech_model, generate_proxy, hw_encoders, insta360_pair, proxy_path, proxy_width, render_with,
    render_with_progress, set_speech_model, speech_model_names, stitch_insta360, stitched_path, stream_preview,
    validate_export, Container, DownloadProgress, ExportOptions, ExportProgress, Fit, PreviewFrame, RateControl, RenderStatus,
    SpeechModelInfo, DEFAULT_SPEECH_MODEL,
};
pub use error::{Error, Result};
pub use fonts::list_system_fonts;
pub use model::{
    Asset, AssetAnalysis, AudioEffect, Clip, Color, Delivery, DiffEntry, DiffKind, EditSource, Keyframe, Marker,
    Projection, Reframe,
    ReframeKeyframe,
    ResolvedReframe, Revision, Rhythm, StagedEdit, StreamInfo, StreamKind, Task, TaskStatus, TextKeyframe, TextOverlay,
    TimeRange,
    Timeline, TimelineDiff, Track, TranscriptSegment, Transform, Transition, TransitionKind, VideoEffect,
};
pub use project::Project;
