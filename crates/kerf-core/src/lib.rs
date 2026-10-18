//! `kerf-core` — the domain model, `.kerf` project persistence (SQLite), and
//! FFmpeg media engine for Kerf.
//!
//! Everything an editor needs that is independent of the UI shell or the MCP
//! server lives here: assets, cached analysis metadata, the non-destructive
//! timeline (EDL), and the operations that mutate it.

pub mod analysis;
pub mod captions_import;
mod clip_timing;
pub mod color_lut;
pub mod error;
pub mod fonts;
pub mod frame_pick;
pub mod layer_geometry;
pub mod mask_plane;
pub mod media;
pub mod model;
pub mod plan_caps;
pub mod plan_epoch;
pub mod planner;
pub mod platform;
pub mod play_plan;
pub mod project;
pub mod proxy;
pub mod render_plan;
pub mod svg;

mod engine;

#[cfg(feature = "whisper")]
pub use analysis::WhisperTranscriber;
pub use analysis::{
    analysis_status, analyze_asset_media, analyze_asset_steps, analyze_steps, auto_analysis, default_kinds, resolve_steps,
    set_auto_analysis, transcription_backend, transcription_status, AnalysisKindStatus, AnalysisProgress, AnalysisProviders,
    AnalysisState, AnalysisStatus, AutoAnalysis, CancelFn, FfmpegRhythmAnalyzer, FfmpegSceneDetector, FfmpegSilenceDetector,
    NullAnalyzer, ProgressFn, RhythmAnalyzer, SceneDetector, SilenceDetector, StepsRun, Transcriber, TranscriptionStatus,
    WhisperFilterTranscriber,
};
pub use captions_import::{
    decode_caption_bytes, parse_ass, parse_captions, parse_srt, read_caption_file, CaptionFile, CaptionFormat,
    CaptionImportRequest, ImportSummary, ImportedCue, ParsedCaptions, MAX_CAPTION_CUES, MAX_CAPTION_FILE_BYTES,
    MAX_CAPTION_OFFSET, MAX_CAPTION_WORDS, MAX_CUE_CHARS, MAX_IMPORTED_CAPTIONS, MAX_LINE_CHARS,
};
pub use clip_timing::{fade_alpha, fade_chroma, fade_luma, FadeEdge, FadeStep, FadeTint, MotionKeys, Rational, FADE_FULL};
pub use engine::cpu::{
    budget_threads as cpu_threads, cores as cpu_cores, cpu_percent, gpu_lane, high_pending as proxy_jobs_pending,
    set_cpu_percent, set_gpu_lane, DEFAULT_CPU_PERCENT, MAX_GPU_JOBS, MIN_CPU_PERCENT,
};
pub use engine::{
    composite_color_policy, contact_sheet_times, decode_audio, decode_hwaccel, disable_decode_hwaccel, download_speech_model,
    export_seek_arg, export_still, ffmpeg_command, ffmpeg_path, filmstrip_for, fps_mode_flag, generate_proxy, geq_edge,
    geq_penultimate_plane, hw_encoders, insta360_pair, limit_ffmpeg_args, limit_frame_args, playback_params, prepare_voiceover,
    proxy_path, proxy_width, render_variants, render_with, render_with_progress, seek_arg, set_speech_model, source_identity,
    source_is_indexed_container, source_seek_points_are_keyframes, speech_model_names, stems_status, stitch_insta360,
    stitched_path, stream_preview, stream_preview_until, stream_preview_with, text_raster_graph, text_still_filter,
    validate_export, voiceover_status, wav_bytes, waveform_pyramid, waveform_range, waveform_range_of, write_wav, AudioBuffer,
    Container, DownloadProgress, ExportOptions, ExportProgress, ExportVariant, Filmstrip, FilmstripSheet, Fit, GeqEdge,
    ImageFormat, LoudnessPreset, PlaybackParams, PlaybackRequest, PreviewFrame, RateControl, Region, RenderStatus, ScrubConfig,
    ScrubFrames, ScrubStats, SpeechModelInfo, StemsStatus, TextPart, TextRasterGraph, VariantProgress, VoiceInfo,
    VoiceoverStatus, WaveformLevel, WaveformPyramid, WaveformRange, DEFAULT_SPEECH_MODEL, DEFAULT_VOICE, FILMSTRIP_HEIGHT,
    MAX_FILMSTRIP_FRAMES, MAX_STREAM_FPS, MAX_STREAM_WIDTH, MAX_VOICE_SPEED, MAX_WAVEFORM_BUCKETS, MIN_STREAM_FPS,
    MIN_STREAM_WIDTH, MIN_VOICE_SPEED, STEM_NAMES,
};
pub use error::{Error, Result};
pub use fonts::list_system_fonts;
pub use frame_pick::{fps_pick, seek_ticks, FpsPick, Pick, PickProgress, SourceFrames};
pub use mask_plane::mask_alpha_plane;
pub use media::{MediaResolver, OriginalMedia, ProxyMedia, SourceMedia};
pub use model::{
    AnalysisKind, Asset, AssetAnalysis, AudioEffect, CaptionLayout, CaptionOptions, CaptionPlacement, CaptionStyle,
    CaptionTimeBase, Clip, ClipCut, ClipLut, ClipMove, Color, CropFrame, CurvePoint, Curves, Delivery, DeltaRange, Detached,
    DetachedMany, DiffEntry, DiffKind, Easing, EditOutcome, EditSource, Framing, Grade, Keyframe, LevelReading, Levels, Marker,
    Mask, MaskShape, MasterBus, Projection, Property, PropertyKey, PropertyTrack, Reframe, ReframeKeyframe, ResolvedReframe,
    Revision, Rhythm, SalienceMap, SkippedDetach, SourceLimits, SplitSide, StagedEdit, StreamInfo, StreamKind, Subsampling,
    SvgOverlay, SvgOverlayPatch, SvgPreset, SvgPresetKind, SvgPresetParams, Task, TaskStatus, TextKeyframe, TextOverlay,
    TimeRange, Timeline, TimelineDiff, Track, TrackLevels, TranscriptSegment, Transform, Transition, TransitionKind, VideoEffect,
    Voiceover, Wheel, Wheels, ADJACENT_EPS, LEVELS_TARGET_LUFS, LEVELS_TRUE_PEAK_CEILING_DBTP, MASTER_DEFAULT_CEILING_DB,
    MASTER_MAX_VOLUME, MASTER_MIN_CEILING_DB, MAX_CHANNEL_VOLUME, MAX_SVG_OVERLAYS, MAX_SVG_SECONDS, MIN_EDIT_CLIP,
};
pub use plan_caps::{DipWhy, EffectKinds, GpuCaps, LayerRef, Unsupported};
pub use plan_epoch::{bump_plan_epoch, plan_epoch};
pub use planner::{Handover, PlanRequest, Planner, SpanPlan, SpanRun};
pub use platform::{
    check_all as check_platforms, CutSummary, DeliveryCheck, DeliveryIssue, IssueKind, PlatformReport, PlatformTarget, Severity,
    TARGETS as PLATFORM_TARGETS,
};
pub use play_plan::{PlayAhead, PlayCut, PlayRefusal};
pub use project::{
    FramingPlan, Project, SmartCropJob, SmartCropPlan, StemsPlaced, SvgChecked, SvgLayout, SvgOverlayResult, SvgSource,
    MAX_CHANNEL_KEYS, VOICEOVER_TRACK,
};
pub use proxy::{PreviewSource, ProxyInput, ProxyPhase, ProxySize, ProxyStatus, ProxyWait};
pub use render_plan::{
    Animated, CompositeColorPolicy, FadeOp, LayerFx, PlanCanvas, PlanLayer, PlanMode, PlanReframe, PlanSource, PlanStream,
    PlanText, PlanTiming, ReframeInterp, RenderPlan, YuvMatrix, MAX_SHRINK, MIN_TEXT_PX,
};
pub use svg::{check_svg, render_preset, SvgCheck, SvgDoc, SvgPresetInfo, SvgPresetParamInfo, MAX_ANIMATIONS, MAX_VALUES};
