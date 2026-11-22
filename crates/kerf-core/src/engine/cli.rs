//! CLI-driven media engine: probing, analysis, frame/waveform extraction and
//! export by invoking the system `ffmpeg` / `ffprobe` binaries.
//!
//! Unlike [`super::ffmpeg`] (in-process libav, gated behind the `ffmpeg`
//! feature and the FFmpeg *development* libraries), everything here only needs
//! the FFmpeg *binaries* on `PATH`, so it compiles and runs in the
//! `--no-default-features` build. The binaries can be overridden with the
//! `KERF_FFMPEG` / `KERF_FFPROBE` environment variables.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use super::colors::{safe_color, valid_color};
use super::cpu;
use super::decode_policy::{
    export_hwaccel_request, facts_from_assets, facts_matter, looks_like_hardware_decode_failure, pix_fmt_bits_420,
    resolve_hwaccel, tune_inputs, Chroma, Codec, HostOs, InputFacts, Reading,
};
use super::peaks;
use super::source_probe::{ProbeCache, SOURCE_PROBE_TIMEOUT};
use super::ProbeResult;
use crate::clip_timing::{
    clip_input_seek, clip_seek, clip_source_window, end_frame, is_cache_proxy, is_head_padded_proxy, picture_window, ramp,
    transition_fx, ClipFx, ClipTiming, FadeEdge, FadeStep, FadeTint, Rational, HEAD_PADDED_SUFFIX,
};
use crate::error::{Error, Result};
pub use crate::model::Fit;
use crate::model::FitOrder;
use crate::model::{
    pix_fmt_layout, Asset, AudioEffect, Clip, Color, Delivery, Grade, Hdr, Mask, MaskShape, PixLayout, Projection, Property,
    Reframe, ReframeKeyframe, ResolvedReframe, SalienceMap, StreamInfo, StreamKind, TextOverlay, TimeRange, Timeline, Transform,
    VideoEffect,
};
use crate::render_plan::{still_size, CompositeColorPolicy};

mod analysis;
mod audio_graph;
mod color_policy;
mod export_args;
mod export_format;
mod export_options;
mod export_size;
mod export_size_measure;
mod extract;
mod filter_graph;
mod hardware;
mod keyframes;
mod placement;
mod preview_stream;
mod probe;
mod progress_run;
mod proxy;
mod proxy_fast;
mod render;
mod source_traits;
mod spawn;
mod still_graph;
mod stills;
mod stitch;
mod svt_lossless;
mod text;
mod variants;
mod video_chain;
mod video_fx;
mod wide_graph;

pub use analysis::*;
pub use audio_graph::*;
pub use color_policy::*;
pub use export_args::*;
pub use export_format::*;
pub use export_options::*;
pub use export_size::*;
pub use export_size_measure::{measure_export_size, SizeMeasureProgress, DEFAULT_MEASURE_BUDGET, MAX_MEASURE_BUDGET, TEMP_CAP};
pub use extract::*;
pub use filter_graph::*;
pub use hardware::*;
pub(crate) use keyframes::*;
pub use placement::*;
pub use preview_stream::*;
pub use probe::*;
use progress_run::*;
pub use proxy::*;
pub use proxy_fast::*;
pub use render::*;
pub use source_traits::*;
pub use spawn::*;
use still_graph::*;
pub use stills::*;
pub use stitch::*;
pub use svt_lossless::svt_av1_lossless_supported;
use svt_lossless::{svt_lossless_refusal, wants_svt_lossless};
pub use text::*;
pub use variants::*;
pub use video_chain::*;
pub use video_fx::*;
pub use wide_graph::*;

/// The widest and narrowest stream asked for. The top is the MCP frame tools' own cap and the
/// preview's (`preview-size.ts`): the engine never scales a picture past its own width, so the
/// bound is only on cost.
pub const MAX_STREAM_WIDTH: u32 = 1920;
pub const MIN_STREAM_WIDTH: u32 = 64;

/// The fastest a stream plays. The webview paints at the display's rate, so frames past this
/// are composited and shipped for nothing.
pub const MAX_STREAM_FPS: f64 = 60.0;
pub const MIN_STREAM_FPS: f64 = 1.0;

/// The stages of an export, the share of the bar each owns, and ffmpeg's `-progress` stream read
/// into positions.
mod progress;
pub use progress::ExportStage;
use progress::{eta_secs, metering_share, pass_position, variants_eta, Block, ProgressReader, VERIFY_SHARE};

/// What an export wrote, read back from the file: its streams and its own loudness.
mod written;
pub use written::{measure_export, ExportReport, LoudnessTarget, WrittenAudio, WrittenVideo};

/// Measuring the mix (`ebur128` meters on the export's own audio graph): a child
/// module so it reaches the private graph builders.
mod levels;
pub use levels::{mix_levels, stream_mix_pcm};

/// A clip's noise reduction (`afftdn`), its delay taken back out where this ffmpeg has one.
mod denoise;
use denoise::denoise_filter;
#[cfg(test)]
use denoise::with_afftdn_delay;

/// A confined run's probe: its argv, and what the real `ffprobe` refuses (`#[ignore]`d).
#[cfg(test)]
mod confined_probe;

/// Noise reduction through the real `ffmpeg`: the sound keeps its sync and its length (`#[ignore]`d).
#[cfg(test)]
mod denoise_rendered;

/// A ducked track through the real `ffmpeg`: its bed plays past its key's end (`#[ignore]`d).
#[cfg(test)]
mod duck_rendered;

/// The golden argv oracle (see its docs): a child module so it reaches the
/// private builders.
#[cfg(test)]
pub(crate) mod golden;

/// The BT.709 label on the four builders, and the plan's mirror of it.
#[cfg(test)]
mod bt709_label;

/// What the export graph draws, pinned against rendered pixels (`#[ignore]`d).
#[cfg(test)]
mod rendered;

#[cfg(test)]
mod export_progress_rendered;

#[cfg(test)]
mod export_size_calibration;
#[cfg(test)]
mod export_size_measure_calibration;
#[cfg(test)]
mod export_size_measure_rendered;
#[cfg(test)]
mod export_size_measure_tests;
#[cfg(test)]
mod export_size_rendered;
#[cfg(test)]
mod export_size_tests;

/// A clip's curves, wheels and LUT as filters.
mod grade;
use grade::grade_filters;

/// Stabilization: a clip's frames moved against its camera's shake, and the passes that measure it.
mod stabilize;
pub(crate) use stabilize::measure_motion;
pub use stabilize::vidstab_available;
#[cfg(test)]
pub(crate) use stabilize::with_vidstab;
use stabilize::{stabilizer, Stabilizer};

/// Stabilization rendered by the real ffmpeg (`#[ignore]`d).
#[cfg(test)]
mod stabilize_rendered;

/// A clip composited with a blend mode instead of alpha-over.
mod blend;
/// The tag a blend chain names for its composite (the bottom layer's): the planner mirrors it in `PlanCanvas::blend_tag`.
pub(crate) use blend::composite_matrix as blend_composite_tag;
use blend::{blend_chains, blend_fold, composite_matrix, fold_runs, keeps_clear_bars, Blend, FoldClip, FoldStage, FOLD_MIN};
pub use blend::{blend_colour_graph, blend_probe_graph};

/// Blend modes through the real `ffmpeg`: the pixels of each mode, the composite outside the clip, and `normal`
/// through the blend graph against the plain `overlay` (`#[ignore]`d).
#[cfg(test)]
mod blend_rendered;

/// Which blended clips fold into one fork and join, and what the planner counts of the rest.
#[cfg(test)]
mod blend_fold_tests;

/// A run of blended clips through the real `ffmpeg`, folded and in parts (`#[ignore]`d).
#[cfg(test)]
mod blend_fold_rendered;

#[cfg(test)]
mod blend_fold_checker;

/// A clip's live behaviours (sway, breathe, spin, drift, shake) written onto the expressions of its
/// transform numbers.
mod behavior;
use behavior::driven_expr;

/// A still whose picture moves (keyed zoom, position, crop) drawn in one sub-pixel resample of a
/// copy scaled once.
mod still_motion;
use still_motion::StillMotion;

/// The still inside a fade, a dip, a dissolve, a slide or a push: its layers and their fades.
mod still_fx;
use still_fx::{still_layers, StillFades};

/// Animated SVG titles as PNG sequences and stills: drawn before ffmpeg is spawned, overlaid by it.
pub(crate) mod svg_layer;

/// The cut's captions and chapter markers muxed into an export, and the caption
/// files written beside it.
mod side_files;

/// Every delivery frame of a multi-format export in one ffmpeg run.
mod together;

/// A long export rendered in parts, so that what is open at once stays bounded.
mod segments;

#[cfg(test)]
mod segments_tests;

/// An export in parts through the real ffmpeg, the whole export's frames and samples (`#[ignore]`d).
#[cfg(test)]
mod segments_rendered;

/// The one run held to the variants rendered one by one, frame for frame (`#[ignore]`d).
#[cfg(test)]
mod together_rendered;

/// Titles through the real ffmpeg (`#[ignore]`d).
#[cfg(test)]
mod svg_rendered;

/// Titles on tracks: the argv, and through the real ffmpeg (`#[ignore]`d).
#[cfg(test)]
mod title_clips;

/// Which slot a job takes, the cores' or the hardware's.
#[cfg(test)]
mod lane_tests;

/// ffprobe's JSON and ffmpeg's logs, mutated: nothing panics.
#[cfg(test)]
mod hostile_tests;

/// The deadline on a one-shot still's process.
#[cfg(all(test, unix))]
mod output_bounded_tests;

/// The environment every child starts in (`command`): the OpenMP binding variables are removed, the rest is inherited.
#[cfg(all(test, unix))]
mod spawn_env_tests;

/// Children that cannot be waited on for ever: the supervised run, the bounded reap, the
/// partial-output cleanup and the way a failed `silencedetect` is read.
#[cfg(all(test, unix))]
mod supervised_tests;

/// A bounded cache of composited stills, keyed by the argv that would draw each.
mod still_cache;

/// The scrubbed frame: cached, with a window of its neighbours drawn by one process.
mod scrub;
pub use scrub::{ScrubConfig, ScrubFrames, ScrubStats};

/// A contact sheet of the composited cut: a still per moment, tiled.
mod timeline_sheet;
pub use timeline_sheet::{timeline_sheet_cancellable, timeline_sheet_cells, SheetCell, SheetClip};

/// The scrub window held to the export's frames and the still's (`#[ignore]`d).
#[cfg(test)]
mod scrub_rendered;

/// The scrubbed still held to the export's frame at the same moment (`#[ignore]`d).
#[cfg(test)]
mod still_rendered;

/// The sound of detached / linked clips: that `extract_audio` used to double, that a
/// detached clip's *own* chain is the chain it had, and that its **level** survives
/// the move between a picture track's fader and an audio track's (graph level, and
/// measured on a render under `#[ignore]`). Pan, duck and mute are the destination
/// track's afterwards — see `Timeline::detach_audio`.
#[cfg(test)]
mod linked_audio;

/// The export plan against the *evaluated* export graph, on a grid of frames.
#[cfg(test)]
mod sweep;

/// The `fps` pick against the frames the export renders (`#[ignore]`d).
#[cfg(test)]
mod picked;

/// Speed ramps through the real `ffmpeg`: the frames, the scrubbed still, the sound's sync (`#[ignore]`d).
#[cfg(test)]
mod ramp_rendered;

/// The blurred fill through the real `ffmpeg` (`#[ignore]`d).
#[cfg(test)]
mod blur_rendered;

/// A zoom taken from the source in the fit's own resample, and a `Cover` clip that pans across the
/// picture: the chains, and the real `ffmpeg`'s pixels (`#[ignore]`d).
#[cfg(test)]
mod zoom_fit;

/// A source whose video starts late keeps its sync in the export (`#[ignore]`d).
#[cfg(test)]
mod late_start;

/// The master limiter through the real `ffmpeg`, the corpus the preview's port replays (`#[ignore]`d).
#[cfg(test)]
mod alimiter_corpus;

/// A keyframed zoom with every other feature of a clip, measured per output frame
/// against `transform_at` and the scrubbed still (`#[ignore]`d).
#[cfg(test)]
mod keyed_zoom;

/// Live behaviours (sway, breathe, spin, drift, shake) against the pictures the export, the scrubbed still
/// and the playback stream render (`#[ignore]`d).
#[cfg(test)]
mod behavior_rendered;

/// Per-property channels — a keyed colour and a keyed volume — against the pictures and samples
/// the export renders (`#[ignore]`d).
#[cfg(test)]
mod keyed_channels;

/// The motion presets (B8a) through the real `ffmpeg`: every preset renders, and the export agrees
/// with the scrubbed still (`#[ignore]`d).
#[cfg(test)]
mod motion_presets_rendered;

/// A keyed mask and a keyed crop against the pictures the export and the still render (`#[ignore]`d).
#[cfg(test)]
mod keyed_mask_crop;

/// Curves, wheels and a LUT against the pictures the export and the still render (`#[ignore]`d).
#[cfg(test)]
mod grade_rendered;

/// Scene detection and contact sheets on a proxy against the original (`#[ignore]`d).
#[cfg(test)]
mod scenes_on_proxy;

/// The Fast (GPU) proxy pipeline through the real `ffmpeg`, with a stand-in for the hardware it cannot
/// have here: the guard against x264's own proxies, the probe failing cleanly, the fallbacks (`#[ignore]`d).
#[cfg(test)]
mod proxy_fast_rendered;

/// A 45 MP still with a slow keyed zoom, pan and crop, measured sub-pixel per output frame at
/// 1080p50 and 2160p50, against time (`#[ignore]`d).
#[cfg(test)]
mod still_motion_rendered;

#[cfg(test)]
mod tests;
