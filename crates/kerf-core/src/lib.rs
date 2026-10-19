//! `kerf-core` — the domain model, `.kerf` project persistence (SQLite), and
//! FFmpeg media engine for Kerf.
//!
//! Everything an editor needs that is independent of the UI shell or the MCP
//! server lives here: assets, cached analysis metadata, the non-destructive
//! timeline (EDL), and the operations that mutate it.

pub mod analysis;
pub mod captions_export;
pub mod captions_import;
mod clip_timing;
pub mod color_lut;
pub mod error;
pub mod finite;
pub mod fonts;
pub mod frame_pick;
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod hostile;
pub mod interchange;
pub mod layer_geometry;
pub mod mask_plane;
pub mod media;
pub mod model;
pub mod paths;
pub mod plan_caps;
pub mod plan_epoch;
pub mod planner;
pub mod platform;
pub mod play_plan;
pub mod project;
pub mod project_activity;
pub mod proxy;
pub mod recovery;
pub mod render_bar;
pub mod render_plan;
pub mod scopes;
pub mod svg;
pub mod template;
pub mod timecode;
pub mod transcript;

mod engine;

#[cfg(feature = "whisper")]
pub use analysis::WhisperTranscriber;
pub use analysis::{
    analysis_status, analyze_asset_media, analyze_asset_steps, analyze_steps, auto_analysis, default_kinds,
    refine_transcript_media, resolve_steps, set_auto_analysis, transcription_backend, transcription_status, AnalysisKindStatus,
    AnalysisProgress, AnalysisProviders, AnalysisState, AnalysisStatus, AutoAnalysis, CancelFn, FfmpegRhythmAnalyzer,
    FfmpegSceneDetector, FfmpegSilenceDetector, NullAnalyzer, ProgressFn, RhythmAnalyzer, SceneDetector, ScoreRhythm,
    SilenceDetector, StepsRun, Transcriber, TranscriptionStatus, WhisperFilterTranscriber,
};
pub use captions_export::{cut_captions, write_captions, CaptionCue, SubtitleFormat};
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
    band_balance, check_caption_sidecars, composite_color_policy, contact_sheet_times, decode_audio, decode_hwaccel,
    disable_decode_hwaccel, discard_partial, download_speech_model, download_speech_model_cancellable, export_seek_arg,
    export_still, export_still_cancellable, ffmpeg_command, ffmpeg_path, file_mtime, filmstrip_for, fps_mode_flag,
    generate_proxy, geq_edge, geq_penultimate_plane, hw_encoders, insta360_pair, limit_ffmpeg_args, limit_frame_args,
    music_status, playback_params, prepare_soundfont, prepare_variants, prepare_voiceover, proxy_path, proxy_width,
    render_variants, render_with, render_with_progress, seek_arg, set_speech_model, source_identity, source_is_indexed_container,
    source_seek_points_are_keyframes, speech_model_names, stems_status, stitch_insta360, stitch_insta360_with, stitched_path,
    stream_preview, stream_preview_until, stream_preview_with, text_raster_graph, text_still_filter, validate_export,
    voiceover_status, wav_bytes, waveform_pyramid, waveform_range, waveform_range_of, write_wav, AudioBuffer, BandLevel,
    BandWindow, Container, DownloadProgress, ExportOptions, ExportProgress, ExportVariant, Filmstrip, FilmstripSheet, Fit,
    GeqEdge, ImageFormat, LoudnessPreset, MusicStatus, PlaybackParams, PlaybackRequest, PreviewFrame, RateControl, Region,
    RenderStatus, ScrubConfig, ScrubFrames, ScrubStats, SpeechModelInfo, StemsStatus, StyleInfo, TextPart, TextRasterGraph,
    VariantProgress, VariantsRun, VoiceInfo, VoiceoverStatus, WaveformLevel, WaveformPyramid, WaveformRange, WindowBalance,
    DEFAULT_SPEECH_MODEL, DEFAULT_VOICE, FILMSTRIP_HEIGHT, MAX_FILMSTRIP_FRAMES, MAX_STREAM_FPS, MAX_STREAM_WIDTH,
    MAX_VOICE_SPEED, MAX_WAVEFORM_BUCKETS, MIN_STREAM_FPS, MIN_STREAM_WIDTH, MIN_VOICE_SPEED, STEM_NAMES,
};
pub use error::{Error, Result};
pub use fonts::list_system_fonts;
pub use frame_pick::{fps_pick, seek_ticks, FpsPick, Pick, PickProgress, SourceFrames};
pub use interchange::markers::MarkerListFormat;
pub use interchange::{InterchangeFormat, InterchangeOptions, Level as ReportLevel, Report, ReportEntry};
pub use mask_plane::mask_alpha_plane;
pub use media::{MediaResolver, OriginalMedia, ProxyMedia, SourceMedia};
pub use model::{
    plan_score_to_picture, AlignOptions, AlignReport, AnalysisKind, Asset, AssetAnalysis, AudioEffect, AutoIncluded, BeatReading,
    CaptionLayout, CaptionOptions, CaptionPlacement, CaptionStyle, CaptionTimeBase, Clip, ClipCut, ClipLut, ClipMove, Color,
    CropFrame, CurvePoint, Curves, CutAlignment, Delivery, DeltaRange, Detached, DetachedMany, DiffEntry, DiffKind, Easing,
    EditOutcome, EditSource, Framing, Grade, HitKind, InstrumentOverrides, Keyframe, LevelReading, Levels, Marker, MarkerKind,
    MarkerPatch, Mask, MaskShape, MasterBus, MusicScore, MusicStyle, Projection, Property, PropertyKey, PropertyTrack, Reframe,
    ReframeKeyframe, ResolvedReframe, Revision, Rhythm, SalienceMap, ScoreHit, ScoreSection, SectionKind, SectionSpan,
    SkippedChange, SkippedDetach, SourceLimits, SplitSide, StagedAssetChange, StagedEdit, StreamInfo, StreamKind, Subsampling,
    SubsetMerge, SvgOverlay, SvgOverlayPatch, SvgPreset, SvgPresetKind, SvgPresetParams, Task, TaskStatus, TextKeyframe,
    TextOverlay, TimeRange, Timeline, TimelineDiff, Track, TrackLevels, TranscriptSegment, Transform, Transition, TransitionKind,
    VideoEffect, Voiceover, Wheel, Wheels, Word, ADJACENT_EPS, DEFAULT_MAX_TEMPO_CHANGE, LEVELS_TARGET_LUFS,
    LEVELS_TRUE_PEAK_CEILING_DBTP, MASTER_DEFAULT_CEILING_DB, MASTER_MAX_VOLUME, MASTER_MIN_CEILING_DB, MAX_BPM,
    MAX_CHANNEL_VOLUME, MAX_SEED, MAX_SPEED, MAX_SVG_OVERLAYS, MAX_SVG_SECONDS, MIN_BPM, MIN_EDIT_CLIP, MIN_SPEED,
    RELEASE_TAIL_S,
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
    AlignTo, AudioSync, AudioSyncMeasure, AudioSyncPlan, BandBalance, BandBalanceRequest, BandPlan, BandSource, ComposePlan,
    ComposeRequest, Composed, DeliveryInputs, ExportedFile, FramingPlan, MissingAsset, Page, Project, RelinkCheck, Relinked,
    RemovedAsset, ScoreAlign, SmartCropJob, SmartCropPlan, StemsPlaced, SubsetApplied, SvgChecked, SvgLayout, SvgOverlayResult,
    SvgSource, SyncSide, DEFAULT_HISTORY_LIMIT, MAX_CHANNEL_KEYS, MAX_SYNC_SECONDS, MUSIC_TRACK, SCHEMA_VERSION,
    STAGED_UNDO_DEPTH, SYNC_SAMPLE_RATE, VOICEOVER_TRACK,
};
pub use proxy::{PreviewSource, ProxyInput, ProxyPhase, ProxySize, ProxyStatus, ProxyWait};
pub use recovery::{Recoverable, RecoveryInfo};
pub use render_plan::{
    Animated, CompositeColorPolicy, FadeOp, LayerFx, PlanCanvas, PlanLayer, PlanMode, PlanReframe, PlanSource, PlanStream,
    PlanText, PlanTiming, ReframeInterp, RenderPlan, YuvMatrix, MAX_SHRINK, MIN_TEXT_PX,
};
pub use svg::{check_svg, render_preset, SvgCheck, SvgDoc, SvgPresetInfo, SvgPresetParamInfo, MAX_ANIMATIONS, MAX_VALUES};
pub use template::{
    ImportOutcome, ImportStatus, Library as TemplateLibrary, PreparedTemplate, PreparedValues, Template, TemplateControl,
    TemplateEditInputs, TemplateInfo, TemplateInstance, TemplateKind, TemplateList, TemplateManifest, TemplateProblem,
    TemplateSaveRequest, TemplateSaved, TemplateSource, TemplateTarget,
};
