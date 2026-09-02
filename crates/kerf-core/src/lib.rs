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
pub mod platform;
pub mod project;

mod engine;

#[cfg(feature = "whisper")]
pub use analysis::WhisperTranscriber;
pub use analysis::{
    analyze, analyze_asset_media, analyze_asset_media_cancellable, analyze_asset_media_with_progress, analyze_cancellable,
    analyze_with_progress, transcription_status, AnalysisProgress, AnalysisProviders, CancelFn, FfmpegRhythmAnalyzer,
    FfmpegSceneDetector, FfmpegSilenceDetector, NullAnalyzer, ProgressFn, RhythmAnalyzer, SceneDetector, SilenceDetector,
    Transcriber, TranscriptionStatus, WhisperFilterTranscriber,
};
pub use engine::cpu::{
    budget_threads as cpu_threads, cores as cpu_cores, cpu_percent, set_cpu_percent, DEFAULT_CPU_PERCENT, MIN_CPU_PERCENT,
};
pub use engine::{
    contact_sheet_times, download_speech_model, export_still, generate_proxy, hw_encoders, insta360_pair, proxy_path,
    proxy_width, render_variants, render_with, render_with_progress, set_speech_model, speech_model_names, stitch_insta360,
    stitched_path, stream_preview, validate_export, Container, DownloadProgress, ExportOptions, ExportProgress, ExportVariant,
    Fit, ImageFormat, PreviewFrame, RateControl, Region, RenderStatus, SpeechModelInfo, VariantProgress, DEFAULT_SPEECH_MODEL,
};
pub use error::{Error, Result};
pub use fonts::list_system_fonts;
pub use model::{
    Asset, AssetAnalysis, AudioEffect, CaptionLayout, CaptionOptions, CaptionStyle, Clip, Color, CropFrame, Delivery, DiffEntry,
    DiffKind, EditSource, Framing, Keyframe, Marker, Mask, MaskShape, Projection, Reframe, ReframeKeyframe, ResolvedReframe,
    Revision, Rhythm, SalienceMap, StagedEdit, StreamInfo, StreamKind, Task, TaskStatus, TextKeyframe, TextOverlay, TimeRange,
    Timeline, TimelineDiff, Track, TranscriptSegment, Transform, Transition, TransitionKind, VideoEffect,
};
pub use platform::{
    check_all as check_platforms, CutSummary, DeliveryCheck, DeliveryIssue, IssueKind, PlatformTarget, Severity,
    TARGETS as PLATFORM_TARGETS,
};
pub use project::{FramingPlan, Project, SmartCropJob, SmartCropPlan};
