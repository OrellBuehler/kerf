//! `kerf-core` — the domain model, `.kerf` project persistence (SQLite), and
//! FFmpeg media engine for Kerf.
//!
//! Everything an editor needs that is independent of the UI shell or the MCP
//! server lives here: assets, cached analysis metadata, the non-destructive
//! timeline (EDL), and the operations that mutate it.

pub mod analysis;
pub mod captions_import;
mod clip_timing;
pub mod error;
pub mod fonts;
pub mod layer_geometry;
pub mod model;
pub mod plan_caps;
pub mod planner;
pub mod platform;
pub mod project;
pub mod render_plan;

mod engine;

#[cfg(feature = "whisper")]
pub use analysis::WhisperTranscriber;
pub use analysis::{
    analyze, analyze_asset_media, analyze_asset_media_cancellable, analyze_asset_media_with_progress, analyze_cancellable,
    analyze_with_progress, set_transcription_enabled, transcription_enabled, transcription_status, AnalysisProgress,
    AnalysisProviders, CancelFn, FfmpegRhythmAnalyzer, FfmpegSceneDetector, FfmpegSilenceDetector, NullAnalyzer, ProgressFn,
    RhythmAnalyzer, SceneDetector, SilenceDetector, Transcriber, TranscriptionStatus, WhisperFilterTranscriber,
};
pub use captions_import::{
    decode_caption_bytes, parse_ass, parse_captions, parse_srt, read_caption_file, CaptionFile, CaptionFormat,
    CaptionImportRequest, ImportSummary, ImportedCue, ParsedCaptions, MAX_CAPTION_CUES, MAX_CAPTION_FILE_BYTES,
    MAX_CAPTION_OFFSET, MAX_CAPTION_WORDS, MAX_CUE_CHARS, MAX_IMPORTED_CAPTIONS, MAX_LINE_CHARS,
};
pub use clip_timing::{FadeEdge, FadeStep, FadeTint, MotionKeys, Rational};
pub use engine::cpu::{
    budget_threads as cpu_threads, cores as cpu_cores, cpu_percent, set_cpu_percent, DEFAULT_CPU_PERCENT, MIN_CPU_PERCENT,
};
pub use engine::{
    composite_color_policy, contact_sheet_times, download_speech_model, export_still, ffmpeg_command, ffmpeg_path, filmstrip_for,
    generate_proxy, hw_encoders, insta360_pair, limit_ffmpeg_args, prepare_voiceover, proxy_path, proxy_width, render_variants,
    render_with, render_with_progress, seek_arg, set_speech_model, speech_model_names, stitch_insta360, stitched_path,
    stream_preview, validate_export, voiceover_status, waveform_pyramid, waveform_range, waveform_range_of, Container,
    DownloadProgress, ExportOptions, ExportProgress, ExportVariant, Filmstrip, FilmstripSheet, Fit, ImageFormat, PreviewFrame,
    RateControl, Region, RenderStatus, SpeechModelInfo, VariantProgress, VoiceInfo, VoiceoverStatus, WaveformLevel,
    WaveformPyramid, WaveformRange, DEFAULT_SPEECH_MODEL, DEFAULT_VOICE, FILMSTRIP_HEIGHT, MAX_FILMSTRIP_FRAMES, MAX_VOICE_SPEED,
    MAX_WAVEFORM_BUCKETS, MIN_VOICE_SPEED,
};
pub use error::{Error, Result};
pub use fonts::list_system_fonts;
pub use model::{
    Asset, AssetAnalysis, AudioEffect, CaptionLayout, CaptionOptions, CaptionPlacement, CaptionStyle, CaptionTimeBase, Clip,
    ClipCut, ClipMove, Color, CropFrame, Delivery, DeltaRange, Detached, DetachedMany, DiffEntry, DiffKind, EditOutcome,
    EditSource, Framing, Keyframe, Marker, Mask, MaskShape, Projection, Reframe, ReframeKeyframe, ResolvedReframe, Revision,
    Rhythm, SalienceMap, SkippedDetach, SourceLimits, SplitSide, StagedEdit, StreamInfo, StreamKind, Subsampling, Task,
    TaskStatus, TextKeyframe, TextOverlay, TimeRange, Timeline, TimelineDiff, Track, TranscriptSegment, Transform, Transition,
    TransitionKind, VideoEffect, Voiceover, ADJACENT_EPS, MIN_EDIT_CLIP,
};
pub use plan_caps::{EffectKinds, GpuCaps, LayerRef, Unsupported};
pub use planner::{PlanRequest, Planner};
pub use platform::{
    check_all as check_platforms, CutSummary, DeliveryCheck, DeliveryIssue, IssueKind, PlatformTarget, Severity,
    TARGETS as PLATFORM_TARGETS,
};
pub use project::{FramingPlan, Project, SmartCropJob, SmartCropPlan, VOICEOVER_TRACK};
pub use render_plan::{
    Animated, CompositeColorPolicy, LayerFx, PlanCanvas, PlanLayer, PlanMode, PlanReframe, PlanStream, PlanText, PlanTiming,
    ReframeInterp, RenderPlan, YuvMatrix, MAX_SHRINK,
};
