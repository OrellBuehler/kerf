//! Embedded MCP server — the app *is* the MCP server.
//!
//! Hosts the Kerf timeline / media engine as MCP tools over a streamable-HTTP
//! endpoint (`/mcp`) on localhost, sharing the **same** `Project` the Tauri
//! commands edit. A connected LLM thus operates on the project the user has
//! open; after every mutation we emit a `project-changed` Tauri event so the
//! webview re-fetches and the edit shows up live in the GUI.
//!
//! Edits made through these tools are attributed to [`EditSource::Agent`]; the
//! actor is set per-operation under the shared lock (the GUI sets
//! [`EditSource::User`] the same way), so attribution stays correct even though
//! both front doors share one `Project`.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use base64::Engine as _;
use kerf_core::{
    AudioEffect, CaptionOptions, CaptionStyle, Delivery, EditSource, ExportOptions, Fit, Keyframe, Mask, MaskShape, Project,
    Projection, ReframeKeyframe, Region, StreamKind, TextKeyframe, Transition, TransitionKind, VideoEffect,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ProgressNotificationParam, ServerCapabilities, ServerInfo};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

/// Default localhost bind address for the MCP endpoint; override with `KERF_MCP_ADDR`.
const DEFAULT_ADDR: &str = "127.0.0.1:7777";

/// Ceilings on the sizes a tool call may ask for. These numbers reach us from a
/// model reading a schema description, not from a UI slider, so they are
/// clamped rather than trusted — `skim_asset` already clamps its grid the same
/// way. A frame far wider than the model can resolve costs a decode and a
/// base64 payload nobody benefits from, and a waveform of a million buckets is
/// megabytes of JSON that would bury the answer it was fetched to support.
const MAX_PREVIEW_WIDTH: u32 = 1920;
/// JPEG `-q:v` for a zoomed frame: a zoom exists to read fine detail, and the
/// preview's `4` smears text and edges that `2` keeps. The image is small
/// anyway, so the bytes hardly move.
const ZOOM_QUALITY: u8 = 2;
const MAX_WAVEFORM_BUCKETS: usize = 4096;

#[derive(Clone)]
pub struct KerfMcp {
    project: Arc<Mutex<Project>>,
    app: AppHandle,
}

// ---- tool parameter schemas ------------------------------------------------

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct StageEditsParams {
    #[schemars(description = "One line describing what you are about to propose; it becomes the label of the applied revision")]
    note: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ApplyStagedParams {
    #[schemars(
        description = "Apply even though the user has edited the timeline since these edits were staged, replacing their newer cut. Default false."
    )]
    force: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RevisionDiffParams {
    #[schemars(description = "Revision seq to explain (see history); the diff is from the revision before it")]
    seq: i64,
    #[schemars(description = "Compare against this revision seq instead of `seq - 1`")]
    from: Option<i64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ImportParams {
    #[schemars(description = "Absolute path to the media file to import")]
    path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AssetIdParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct CaptionParams {
    #[schemars(
        description = "Look: `lines` (default) holds a few words as a subtitle line; `word_punch` puts one large word on screen at a time, the social-video style. Everything below is an override on top of the style — omit them to get the whole look."
    )]
    style: Option<CaptionStyle>,
    #[schemars(description = "Most words on one caption line (lines: 4, word_punch: 1)")]
    max_words: Option<usize>,
    #[schemars(description = "Most characters on one caption line (default 28); the tighter of the two limits wins")]
    max_chars: Option<usize>,
    #[schemars(description = "Vertical position as a fraction of frame height, 0 = top (lines: 0.88, word_punch: 0.72)")]
    pos_y: Option<f64>,
    #[schemars(description = "Font height as a fraction of frame height (lines: 0.05, word_punch: 0.11)")]
    size: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SpeechModelParams {
    #[schemars(
        description = "whisper.cpp model name (tiny, base, small, medium, large-v3-turbo, or an `.en` variant); omit for the default"
    )]
    name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CutClipParams {
    #[schemars(description = "UUID of the source asset")]
    asset_id: String,
    #[schemars(description = "In-point in the source asset (seconds)")]
    start: f64,
    #[schemars(description = "Out-point in the source asset (seconds)")]
    end: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddClipParams {
    #[schemars(description = "UUID of the source asset")]
    asset_id: String,
    #[schemars(description = "Target track UUID; omit to auto-select by asset kind")]
    track_id: Option<String>,
    #[schemars(description = "In-point in the source asset (seconds)")]
    source_in: f64,
    #[schemars(description = "Out-point in the source asset (seconds)")]
    source_out: f64,
    #[schemars(description = "Timeline position (seconds); omit to append")]
    timeline_start: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SplitParams {
    #[schemars(description = "UUID of the clip to split")]
    clip_id: String,
    #[schemars(description = "Timeline time at which to split (seconds)")]
    at: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TrimParams {
    #[schemars(description = "UUID of the clip to trim")]
    clip_id: String,
    #[schemars(description = "New source in-point (seconds)")]
    source_in: Option<f64>,
    #[schemars(description = "New source out-point (seconds)")]
    source_out: Option<f64>,
    #[schemars(
        description = "New timeline start (seconds) applied in the same edit — pass alongside source_in \
                       when trimming the left edge so the clip's right edge stays put"
    )]
    timeline_start: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ReorderParams {
    #[schemars(description = "UUID of the track containing the clip")]
    track_id: String,
    #[schemars(description = "UUID of the clip to move")]
    clip_id: String,
    #[schemars(description = "New zero-based index within the track")]
    new_index: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ClipIdParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct MoveClipParams {
    #[schemars(description = "UUID of the clip to move")]
    clip_id: String,
    #[schemars(description = "New timeline position in seconds (clamped to >= 0)")]
    timeline_start: f64,
    #[schemars(description = "Destination track UUID (must be the same kind); omit to keep the clip on its current track")]
    track_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddTrackParams {
    #[schemars(description = "Track kind: \"video\" or \"audio\"")]
    kind: String,
    #[schemars(description = "Optional track name; auto-named (V2/A2/…) when omitted")]
    name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TrackIdParams {
    #[schemars(description = "UUID of the track")]
    track_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SmartCropParams {
    #[schemars(description = "UUID of the clip to reframe; every clip on an unlocked video track when omitted")]
    clip_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SnapToBeatsParams {
    #[schemars(description = "UUID of the track to align; every unlocked video track when omitted")]
    track_id: Option<String>,
    #[schemars(
        description = "How far a cut may move to reach a beat (seconds). Defaults to half a beat, which lands every cut on the beat it is nearest"
    )]
    tolerance: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CutClipRangeParams {
    #[schemars(description = "UUID of the clip to cut")]
    clip_id: String,
    #[schemars(description = "Source-time start of the span to remove (seconds, e.g. a transcript segment's start)")]
    from: f64,
    #[schemars(description = "Source-time end of the span to remove (seconds)")]
    to: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetTrackDuckParams {
    #[schemars(description = "UUID of the track to (un)duck")]
    track_id: String,
    #[schemars(description = "true to duck this track under the others on export, false to restore a flat mix")]
    duck: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetMaskParams {
    #[schemars(description = "UUID of the clip to mask")]
    clip_id: String,
    #[schemars(
        description = "Shape outline: \"rect\" or \"ellipse\". Omit to CLEAR the mask entirely. Every other field, when omitted, keeps the clip's current value (or its default on a fresh mask)"
    )]
    shape: Option<String>,
    #[schemars(description = "Centre of the shape across the frame, 0 = left edge, 1 = right (default 0.5)")]
    x: Option<f64>,
    #[schemars(description = "Centre of the shape down the frame, 0 = top, 1 = bottom (default 0.5)")]
    y: Option<f64>,
    #[schemars(description = "Full width of the shape as a fraction of the frame, not a radius (default 0.5)")]
    width: Option<f64>,
    #[schemars(description = "Full height of the shape as a fraction of the frame (default 0.5)")]
    height: Option<f64>,
    #[schemars(
        description = "Edge softness as a fraction of the shape's own half-size, 0 = hard (default 0.15). A hard-edged mask over a face reads as a sticker"
    )]
    feather: Option<f64>,
    #[schemars(description = "Keep what is OUTSIDE the shape instead of inside it (default false)")]
    inverted: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetTrackVolumeParams {
    #[schemars(description = "UUID of the track to set the level of")]
    track_id: String,
    #[schemars(description = "Track fader as a linear gain: 1.0 is unity, 0.5 is -6 dB, 0 is silent. Clamped to 0..4")]
    volume: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetTrackPanParams {
    #[schemars(description = "UUID of the track to place")]
    track_id: String,
    #[schemars(description = "Stereo placement: -1 hard left, 0 centre, 1 hard right")]
    pan: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetDeliveryFormatParams {
    #[schemars(description = "Delivery frame width in pixels, e.g. 1080. Omit (with height) to clear the \
                              format and go back to following the footage's shape.")]
    width: Option<u32>,
    #[schemars(description = "Delivery frame height in pixels, e.g. 1920 for a 9:16 vertical cut")]
    height: Option<u32>,
    #[schemars(description = "How footage of a different shape meets the frame: \"cover\" (the default) \
                              fills and crops, \"contain\" scales the whole picture in and letterboxes")]
    fit: Option<Fit>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddMarkerParams {
    #[schemars(description = "Timeline time in seconds to mark")]
    time: f64,
    #[schemars(description = "Short label for the marker, e.g. 'best laugh' or 'chapter 2'")]
    name: String,
    #[schemars(description = "Optional CSS color for the ruler chip")]
    #[serde(default)]
    color: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct UpdateMarkerParams {
    #[schemars(description = "UUID of the marker to update")]
    marker_id: String,
    #[schemars(description = "New timeline time in seconds; omit to leave it put")]
    #[serde(default)]
    time: Option<f64>,
    #[schemars(description = "New label; omit to leave it alone")]
    #[serde(default)]
    name: Option<String>,
    #[schemars(description = "New CSS color, or an empty string to clear it")]
    #[serde(default)]
    color: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct MarkerIdParams {
    #[schemars(description = "UUID of the marker to remove")]
    marker_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct DuplicateClipsParams {
    #[schemars(description = "UUIDs of the clips to copy")]
    clip_ids: Vec<String>,
    #[schemars(description = "Timeline time the earliest copy should land at, in seconds")]
    at: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetTrackMutedParams {
    #[schemars(description = "UUID of the track to mute or unmute")]
    track_id: String,
    #[schemars(description = "true to silence (audio) or hide (video) this track, false to restore it")]
    muted: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetTrackSoloParams {
    #[schemars(description = "UUID of the track to solo or unsolo")]
    track_id: String,
    #[schemars(description = "true to solo this track, false to clear its solo")]
    solo: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetTrackLockedParams {
    #[schemars(description = "UUID of the track to lock or unlock")]
    track_id: String,
    #[schemars(description = "true to guard this track's clips against edits, false to unlock")]
    locked: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetClipEnabledParams {
    #[schemars(description = "UUID of the clip to enable or disable")]
    clip_id: String,
    #[schemars(description = "false to drop this clip from the render while leaving it on the timeline")]
    enabled: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct VolumeParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Linear gain (1.0 = unchanged, 0.0 = muted)")]
    volume: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FadeParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Fade-in duration in seconds; omit to leave unchanged, 0 to clear")]
    fade_in: Option<f64>,
    #[schemars(description = "Fade-out duration in seconds; omit to leave unchanged, 0 to clear")]
    fade_out: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SpeedParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Playback rate: 1.0 = normal, 2.0 = 2x faster, 0.5 = half speed, negative = reverse")]
    speed: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TransformParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Uniform scale after fitting to the frame (1.0 = fit, < 1.0 = picture-in-picture); omit to leave unchanged"
    )]
    scale: Option<f64>,
    #[schemars(description = "Horizontal offset as a fraction of frame width (0.0 = centered); omit to leave unchanged")]
    pos_x: Option<f64>,
    #[schemars(description = "Vertical offset as a fraction of frame height (0.0 = centered); omit to leave unchanged")]
    pos_y: Option<f64>,
    #[schemars(description = "Clockwise rotation in degrees; omit to leave unchanged")]
    rotation: Option<f64>,
    #[schemars(description = "Opacity 0.0–1.0 (1.0 = opaque); omit to leave unchanged")]
    opacity: Option<f64>,
    #[schemars(description = "Fraction cropped from the left edge (0.0–1.0); omit to leave unchanged")]
    crop_left: Option<f64>,
    #[schemars(description = "Fraction cropped from the right edge (0.0–1.0); omit to leave unchanged")]
    crop_right: Option<f64>,
    #[schemars(description = "Fraction cropped from the top edge (0.0–1.0); omit to leave unchanged")]
    crop_top: Option<f64>,
    #[schemars(description = "Fraction cropped from the bottom edge (0.0–1.0); omit to leave unchanged")]
    crop_bottom: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ColorParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Additive brightness -1.0–1.0 (0.0 = unchanged); omit to leave unchanged")]
    brightness: Option<f64>,
    #[schemars(description = "Contrast multiplier 0.0–4.0 (1.0 = unchanged); omit to leave unchanged")]
    contrast: Option<f64>,
    #[schemars(description = "Saturation multiplier 0.0–3.0 (1.0 = unchanged); omit to leave unchanged")]
    saturation: Option<f64>,
    #[schemars(description = "Gamma 0.1–10.0 (1.0 = unchanged); omit to leave unchanged")]
    gamma: Option<f64>,
    #[schemars(
        description = "Warm/cool shift -1.0–1.0 (0.0 = unchanged; positive warms, negative cools); omit to leave unchanged"
    )]
    temperature: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TransitionParams {
    #[schemars(description = "UUID of the clip whose start blends with the clip before it on the same track")]
    clip_id: String,
    #[schemars(
        description = "Transition kind. Fades: \"crossfade\", \"dip_to_black\", \"dip_to_white\". Motion, named for the direction of travel: \"slide_left\" / \"slide_right\" / \"slide_up\" / \"slide_down\" brings the new shot in over the old one, \"push_left\" / \"push_right\" / \"push_up\" / \"push_down\" carries the old one out with it. Omit to clear the transition"
    )]
    kind: Option<String>,
    #[schemars(description = "Transition duration in seconds (required when a kind is given)")]
    duration: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct VideoEffectsParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Ordered list of video effects (replaces the clip's chain). Each is an object with a \"type\": \
                       {\"type\":\"blur\",\"sigma\":8}, {\"type\":\"sharpen\",\"amount\":1.0}, {\"type\":\"grayscale\"}, \
                       {\"type\":\"invert\"}, {\"type\":\"vignette\"}, or \
                       {\"type\":\"chroma_key\",\"color\":\"green\",\"similarity\":0.1,\"blend\":0.0} (keys a color to \
                       transparency so a lower track shows through). Pass [] to clear."
    )]
    effects: Vec<VideoEffect>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AudioEffectsParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Ordered list of audio effects (replaces the clip's chain). Each is an object with a \"type\": \
                       {\"type\":\"highpass\",\"hz\":80}, {\"type\":\"lowpass\",\"hz\":12000}, \
                       {\"type\":\"equalizer\",\"hz\":3000,\"width\":1000,\"gain_db\":3}, \
                       {\"type\":\"compressor\",\"threshold_db\":-18,\"ratio\":3,\"attack_ms\":20,\"release_ms\":250,\"makeup_db\":6}, \
                       or {\"type\":\"gate\",\"threshold_db\":-40}. Pass [] to clear."
    )]
    effects: Vec<AudioEffect>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct KeyframesParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Transform keyframes (replaces the clip's animation). Each: {\"time\":seconds_from_clip_start, \
                       \"scale\":1.0,\"pos_x\":0.0,\"pos_y\":0.0,\"rotation\":0.0,\"opacity\":1.0}. Two or more animate \
                       the clip; pass [] to clear and use the static transform."
    )]
    keyframes: Vec<Keyframe>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ReframeParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Camera heading in degrees, wrapping at ±180 (0 = the source's forward direction, 90 = to the \
                       right); omit to leave unchanged"
    )]
    yaw: Option<f64>,
    #[schemars(description = "Camera elevation in degrees, -90 (straight down) to 90 (straight up); omit to leave unchanged")]
    pitch: Option<f64>,
    #[schemars(description = "Horizon tilt in degrees; omit to leave unchanged")]
    roll: Option<f64>,
    #[schemars(
        description = "Diagonal field of view in degrees, 1–359. ~100 is a natural-looking shot, lower zooms in, \
                       above ~150 goes fisheye-wide; omit to leave unchanged"
    )]
    fov: Option<f64>,
    #[schemars(
        description = "Field of view of each physical lens in degrees, for a dual_fisheye source only (default 190). \
                       Tuning this moves the stitch seam; omit to leave unchanged"
    )]
    lens_fov: Option<f64>,
    #[schemars(
        description = "Override the source's projection when detection got it wrong: \"equirect\", \"dual_fisheye\" or \
                       \"fisheye\". Required to reframe an asset Kerf did not detect as 360"
    )]
    input: Option<Projection>,
    #[schemars(
        description = "What to render: \"flat\" (default — an ordinary rectilinear shot) or \"equirect\" (stitch a \
                       dual-fisheye source without choosing a direction)"
    )]
    output: Option<Projection>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AssetProjectionParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(
        description = "The source's spherical projection: \"equirect\", \"dual_fisheye\" or \"fisheye\"; null clears \
                       the mark and treats the asset as ordinary flat footage"
    )]
    projection: Option<Projection>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ReframeKeyframesParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Camera keyframes (replaces the clip's animation). Each: {\"time\":seconds_from_clip_start, \
                       \"yaw\":0.0,\"pitch\":0.0,\"roll\":0.0,\"fov\":100.0}. Two or more pan the virtual camera over \
                       the clip; pass [] to clear and hold the static pose."
    )]
    keyframes: Vec<ReframeKeyframe>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddReframeKeyframeParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Keyframe time in seconds from the clip's start")]
    time: f64,
    #[schemars(description = "Camera heading in degrees at this time; omit to capture the current value")]
    yaw: Option<f64>,
    #[schemars(description = "Camera elevation in degrees (-90..90) at this time; omit to capture current")]
    pitch: Option<f64>,
    #[schemars(description = "Horizon tilt in degrees at this time; omit to capture current")]
    roll: Option<f64>,
    #[schemars(description = "Diagonal field of view in degrees at this time; omit to capture current")]
    fov: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddKeyframeParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Keyframe time in seconds from the clip's start")]
    time: f64,
    #[schemars(description = "Scale at this time (1.0 = fit); omit to capture the current value")]
    scale: Option<f64>,
    #[schemars(description = "Horizontal position as a frame-width fraction (0 = centered); omit to capture current")]
    pos_x: Option<f64>,
    #[schemars(description = "Vertical position as a frame-height fraction (0 = centered); omit to capture current")]
    pos_y: Option<f64>,
    #[schemars(description = "Rotation in degrees; omit to capture current")]
    rotation: Option<f64>,
    #[schemars(description = "Opacity 0.0–1.0; omit to capture current")]
    opacity: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddOverlayParams {
    #[schemars(description = "The text to display")]
    text: String,
    #[schemars(description = "When the overlay appears, in timeline seconds")]
    start: f64,
    #[schemars(description = "When the overlay disappears, in timeline seconds")]
    end: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct UpdateOverlayParams {
    #[schemars(description = "UUID of the overlay")]
    overlay_id: String,
    #[schemars(description = "New text; omit to leave unchanged")]
    text: Option<String>,
    #[schemars(description = "New start time (seconds); omit to leave unchanged")]
    start: Option<f64>,
    #[schemars(description = "New end time (seconds); omit to leave unchanged")]
    end: Option<f64>,
    #[schemars(description = "Center X as a fraction of frame width (0–1); omit to leave unchanged")]
    pos_x: Option<f64>,
    #[schemars(description = "Center Y as a fraction of frame height (0–1, ~0.85 = lower third); omit to leave unchanged")]
    pos_y: Option<f64>,
    #[schemars(description = "Font height as a fraction of frame height (e.g. 0.06); omit to leave unchanged")]
    size: Option<f64>,
    #[schemars(description = "Text color (e.g. \"white\", \"#ffcc00\", \"yellow@0.9\"); omit to leave unchanged")]
    color: Option<String>,
    #[schemars(description = "Box color behind the text (e.g. \"black@0.5\"); empty string clears it; omit to leave unchanged")]
    bg: Option<String>,
    #[schemars(
        description = "System font family name (see list_fonts); empty string reverts to the default font; omit to leave unchanged"
    )]
    font: Option<String>,
    #[schemars(description = "Bold (thickened) text; omit to leave unchanged")]
    bold: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct OverlayIdParams {
    #[schemars(description = "UUID of the overlay")]
    overlay_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct OverlayKeyframesParams {
    #[schemars(description = "UUID of the overlay")]
    overlay_id: String,
    #[schemars(description = "Position/opacity keyframes (replaces the overlay's animation). Each: \
                       {\"time\":seconds_from_overlay_start,\"pos_x\":0.5,\"pos_y\":0.85,\"opacity\":1.0}. Pass [] to clear.")]
    keyframes: Vec<TextKeyframe>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportSrtParams {
    #[schemars(description = "UUID of the asset whose transcript to export")]
    asset_id: String,
    #[schemars(description = "Output .srt file path to write")]
    output_path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ConcatParams {
    #[schemars(description = "Ordered list of asset UUIDs to stitch together")]
    asset_ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RevertParams {
    #[schemars(description = "Revision seq to jump the timeline back to (see history)")]
    seq: i64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportParams {
    #[schemars(description = "Output file path for the rendered result. Its extension should match the chosen container.")]
    output_path: String,
    #[schemars(description = "Optional encode settings. Omit for the safe default (H.264 + AAC MP4). \
                       Key fields: container (mp4/mov/mkv/webm/gif/mp3/m4a/wav/flac); video_codec \
                       (libx264/libx265/libvpx-vp9/libsvtav1/prores_ks/gif, plus GPU encoders \
                       h264_nvenc/hevc_nvenc/av1_nvenc/h264_qsv/hevc_qsv/h264_videotoolbox/\
                       hevc_videotoolbox/h264_amf/hevc_amf — far faster, crf still applies); \
                       audio_codec (aac/libmp3lame/libopus/flac/alac/pcm_s16le/pcm_s24le); \
                       rate_control (crf/bitrate/two_pass/lossless); crf; video_bitrate (\"8M\"); \
                       preset; hwaccel (\"auto\"/\"cuda\"/\"vaapi\"/\"videotoolbox\"/\"qsv\" — GPU \
                       decode, composes with any encoder); resolution ([w,h]); fps; audio_bitrate \
                       (\"192k\"); include_audio; faststart; range ({start,end} timeline seconds — \
                       render only that span).")]
    #[serde(default)]
    options: Option<ExportOptions>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AddTaskParams {
    #[schemars(description = "What the task should accomplish, in plain language")]
    prompt: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CompleteTaskParams {
    #[schemars(description = "UUID of the task to complete")]
    task_id: String,
    #[schemars(description = "Short summary of the edits made, shown to the user")]
    result: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FailTaskParams {
    #[schemars(description = "UUID of the task that could not be completed")]
    task_id: String,
    #[schemars(description = "Why the task failed")]
    error: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TaskIdParams {
    #[schemars(description = "UUID of the task")]
    task_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct WaveformParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(description = "Number of buckets to return (1–4096; a few hundred is plenty to read a shape from)")]
    buckets: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FrameParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(description = "Time in the source asset to decode (seconds)")]
    time_secs: f64,
    #[schemars(description = "Maximum output width in pixels (default 640, capped at 1920)")]
    max_width: Option<u32>,
    #[schemars(
        description = "Zoom into part of the frame instead of seeing all of it: a rectangle in fractions of the full frame (0..1), `left`/`top` its top-left corner, `width`/`height` its size. The region is cropped out first and then scaled to max_width, so a quarter of the frame shows four times the detail for the same image cost — use it to check a face, on-screen text, a caption, a mask edge. Omit for the whole frame."
    )]
    region: Option<RegionParams>,
}

/// A region of a frame to zoom into, as the schema hands it to a model: the
/// same fractions [`Region`] takes, kept as a separate type so the tool schema
/// documents each edge.
#[derive(Debug, Clone, Copy, serde::Deserialize, schemars::JsonSchema)]
struct RegionParams {
    #[schemars(description = "Left edge of the region as a fraction of the frame width (0 = left edge)")]
    left: f64,
    #[schemars(description = "Top edge of the region as a fraction of the frame height (0 = top edge)")]
    top: f64,
    #[schemars(description = "Width of the region as a fraction of the frame width")]
    width: f64,
    #[schemars(description = "Height of the region as a fraction of the frame height")]
    height: f64,
}

impl RegionParams {
    /// The engine region, pulled into the frame — a model's fractions are a
    /// request, not a proof.
    fn region(self) -> Region {
        Region {
            left: self.left,
            top: self.top,
            width: self.width,
            height: self.height,
        }
        .normalized()
    }

    /// The region as the caption echoes it back, after normalization, so the
    /// model's next crop is in the coordinates that were actually used.
    fn describe(self) -> String {
        let r = self.region();
        format!(
            "region left={:.3} top={:.3} width={:.3} height={:.3} of the full frame",
            r.left, r.top, r.width, r.height
        )
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SkimParams {
    #[schemars(description = "UUID of the asset to skim")]
    asset_id: String,
    #[schemars(description = "Start of the time range in seconds (default 0 = start of asset)")]
    start: Option<f64>,
    #[schemars(description = "End of the time range in seconds (default the asset's full duration)")]
    end: Option<f64>,
    #[schemars(description = "Grid columns (default 4, max 8)")]
    columns: Option<u32>,
    #[schemars(description = "Grid rows (default 4, max 8)")]
    rows: Option<u32>,
    #[schemars(description = "Width of each grid cell in pixels (default 240)")]
    cell_width: Option<u32>,
    #[schemars(
        description = "Zoom into one cell of the sheet instead of building it: the 1-based, row-major cell number from a previous skim with the same range and grid. Returns that cell's moment as a single full-detail frame (max_width wide, from the original source) — the shortcut from 'cell 7 looks promising' to seeing it properly."
    )]
    cell: Option<u32>,
    #[schemars(description = "Width of the zoomed cell frame in pixels when `cell` is given (default 640, capped at 1920)")]
    max_width: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TimelineFrameParams {
    #[schemars(description = "Timeline position to render (seconds)")]
    time_secs: f64,
    #[schemars(description = "Maximum output width in pixels (default 640, capped at 1920)")]
    max_width: Option<u32>,
    #[schemars(
        description = "Zoom into part of the frame instead of seeing all of it: a rectangle in fractions of the full frame (0..1), `left`/`top` its top-left corner, `width`/`height` its size. The region is cropped out first and then scaled to max_width, so a quarter of the frame shows four times the detail for the same image cost — use it to check a face, on-screen text, a caption, a mask edge. Omit for the whole frame."
    )]
    region: Option<RegionParams>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CoverParams {
    #[schemars(description = "Timeline position to capture (seconds)")]
    time_secs: f64,
    #[schemars(description = "Output image path. A .png extension writes PNG, anything else JPEG.")]
    output_path: String,
}

#[derive(Serialize)]
struct AssetMetadata {
    asset: kerf_core::Asset,
    analysis: Option<kerf_core::AssetAnalysis>,
}

/// A span of a track where nothing renders.
#[derive(Serialize)]
struct Gap {
    start_secs: f64,
    end_secs: f64,
}

#[derive(Serialize)]
struct TrackSummary {
    id: String,
    name: String,
    kind: String,
    clip_count: usize,
    duration_secs: f64,
    /// Where this track renders nothing — black picture on a video track,
    /// silence on an audio one — including a hole at the head when the first
    /// clip doesn't start at 0. Omitted entirely when the track is gapless.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    gaps: Vec<Gap>,
}

#[derive(Serialize)]
struct TimelineSummary {
    total_duration_secs: f64,
    track_count: usize,
    total_clip_count: usize,
    tracks: Vec<TrackSummary>,
    /// The frame this cut is being made for, e.g. "1080x1920 (cover)". Absent
    /// when the project follows its footage's shape. Worth reading before
    /// framing anything: under a cover delivery the crop decides what survives.
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery_format: Option<String>,
    /// Set while edits are staged: this summary describes the **proposal**, not
    /// the cut the user is looking at. `staged_diff` spells out the difference.
    #[serde(skip_serializing_if = "Option::is_none")]
    staged_changes: Option<usize>,
}

// ---- tools -----------------------------------------------------------------

#[tool_router]
impl KerfMcp {
    #[tool(
        description = "List the system font family names available for text overlays (pass one as `font` to add_overlay / update_overlay)"
    )]
    fn list_fonts(&self) -> Result<String, McpError> {
        json(&kerf_core::list_system_fonts())
    }

    #[tool(
        description = "Import a media file into the project and return the Asset (probed streams, duration, \
                       resolution, fps, codecs). Importing a file the project already holds resolves to that \
                       asset instead of duplicating it, so this is safe to re-run. One Insta360 lens file pulls \
                       in its sibling and stitches the pair into a single 360 asset — a full re-encode that can \
                       run for minutes. An import is NOT part of your staged proposal: media lands for the user \
                       immediately, because a file on disk is not an edit to their cut."
    )]
    async fn import_asset(&self, Parameters(p): Parameters<ImportParams>) -> Result<String, McpError> {
        let project = self.project.clone();
        let app = self.app.clone();
        let path = p.path;
        let asset = blocking(move || {
            // Probe (and, for a lens pair, stitch) with the lock released, taking
            // it only for the quick insert — the same shape as the GUI's import,
            // so a multi-minute stitch never freezes the user's editing.
            let mut on_progress = |pr: kerf_core::ExportProgress| {
                let _ = app.emit("import-progress", crate::ImportProgress::new(&path, pr));
            };
            let probed = Project::probe_import(std::path::Path::new(&path), &mut on_progress).map_err(core_err)?;
            lock_agent(&project).insert_or_get_asset(&probed).map_err(core_err)
        })
        .await?;
        // Preview decodes come off a proxy; queue it now so the first frame
        // anyone asks for is not a seek into a long GOP.
        crate::spawn_proxy(&self.app, &asset);
        self.changed();
        json(&asset)
    }

    #[tool(description = "List all media assets in the project")]
    fn list_assets(&self) -> Result<String, McpError> {
        let project = self.lock();
        json(&project.list_assets().map_err(core_err)?)
    }

    #[tool(
        description = "Get an asset's probed metadata and cached analysis (silence, scenes, transcript, EBU R128 loudness, onset times, tempo/beat grid, speech/music class)"
    )]
    fn get_asset_metadata(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let asset = project.require_asset(id).map_err(core_err)?;
        let analysis = project.get_analysis(id).map_err(core_err)?;
        json(&AssetMetadata { asset, analysis })
    }

    #[tool(description = "Get the full non-destructive timeline state (tracks and clips)")]
    fn get_timeline_state(&self) -> Result<String, McpError> {
        let project = self.lock();
        json(&project.working_timeline().map_err(core_err)?)
    }

    #[tool(
        description = "Which speech-to-text backend this build of Kerf will use for transcription, and whether its model is downloaded yet. Call this when an asset's transcript comes back empty: it says whether transcription is unavailable (and why), or simply that the first run still has to fetch a model."
    )]
    fn transcription_status(&self) -> Result<String, McpError> {
        json(&kerf_core::transcription_status())
    }

    #[tool(
        description = "Download a speech-to-text model (whisper.cpp ggml) into Kerf's cache, so the next analyze_asset does not wait for it. Names, smallest first: tiny, tiny.en, base, base.en, small, small.en, medium, medium.en, large-v3-turbo — the plain names are multilingual, `.en` ones are English-only but more accurate on English. Bigger is slower and more accurate; `base` is the default. This only fills the cache: transcription keeps using whichever model is *selected*, so to transcribe with a model you name here, call set_speech_model too. Blocks for the length of the download (75 MB to 1.6 GB) and is a no-op if the model is already cached."
    )]
    async fn download_speech_model(&self, Parameters(p): Parameters<SpeechModelParams>) -> Result<String, McpError> {
        let name = p.name.unwrap_or_else(|| kerf_core::DEFAULT_SPEECH_MODEL.to_string());
        let path =
            blocking(move || kerf_core::download_speech_model(&name, &mut |_: kerf_core::DownloadProgress| {}).map_err(core_err))
                .await?;
        Ok(format!("Speech model ready at {}", path.display()))
    }

    #[tool(
        description = "Analyze an asset (silence + scene detection, EBU R128 loudness, onset/transient detection, tempo/beat estimation, speech-vs-music classification, and speech-to-text transcription) and cache the result. The first call that transcribes downloads a speech model (~148 MB) and inference then runs for a good fraction of the media's duration, so expect this one to be slow; see transcription_status if the transcript comes back empty."
    )]
    async fn analyze_asset(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.project.clone();
        let analysis = blocking(move || {
            // Resolve under the lock, run the heavy ffmpeg analysis with the lock
            // released, then re-lock only to cache it — so analysis doesn't freeze
            // the GUI or stall other tools for its whole (multi-second) duration.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            let analysis = kerf_core::analyze_asset_media(&asset).map_err(core_err)?;
            lock_agent(&project).set_analysis(&analysis).map_err(core_err)?;
            Ok(analysis)
        })
        .await?;
        self.changed();
        json(&analysis)
    }

    #[tool(
        description = "Pick the speech-to-text model transcription uses, remembered in the project (omit the name to \
                       go back to the default). Same names as download_speech_model. Selecting a model does not \
                       download it — the next transcription fetches it if the cache is cold, or call \
                       download_speech_model first to get that wait out of the way. Returns the resulting \
                       transcription status."
    )]
    fn set_speech_model(&self, Parameters(p): Parameters<SpeechModelParams>) -> Result<String, McpError> {
        let name = p.name.as_deref().map(str::trim).filter(|s| !s.is_empty());
        kerf_core::set_speech_model(name);
        // Both writes the GUI's picker makes: the process setting transcription
        // reads, and the project meta that restores the choice on reopen.
        self.lock()
            .set_meta(crate::SPEECH_MODEL_KEY, name.unwrap_or(""))
            .map_err(core_err)?;
        // The GUI reads the transcription status once, at launch, so it has to be
        // told the choice moved or its picker shows the old model until the next
        // start. Deliberately not `project-changed`: that re-fetches the timeline,
        // the history and the task queue, and none of those moved.
        self.notify("speech-model-changed");
        json(&kerf_core::transcription_status())
    }

    #[tool(description = "Cut [start, end) of an asset and append it to the matching track")]
    fn cut_clip(&self, Parameters(p): Parameters<CutClipParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        self.edit(|project| {
            let out = project.cut_clip(id, p.start, p.end).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(description = "Add a clip referencing a source range of an asset to the timeline")]
    fn add_clip_to_timeline(&self, Parameters(p): Parameters<AddClipParams>) -> Result<String, McpError> {
        let asset_id = parse_id(&p.asset_id)?;
        let track_id = p.track_id.as_deref().map(parse_id).transpose()?;
        self.edit(|project| {
            let out = project
                .add_clip_to_timeline(asset_id, track_id, p.source_in, p.source_out, p.timeline_start)
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(description = "Split a timeline clip at a timeline time into two adjacent clips")]
    fn split_at(&self, Parameters(p): Parameters<SplitParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let (left, right) = project.split_at(clip_id, p.at).map_err(core_err)?;
            json(&serde_json::json!({ "left": left, "right": right }))
        })
    }

    #[tool(description = "Trim a clip's source in/out points (timeline position preserved unless timeline_start is passed)")]
    fn trim(&self, Parameters(p): Parameters<TrimParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project
                .trim(clip_id, p.source_in, p.source_out, p.timeline_start)
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(description = "Move a clip to a new index within its track (re-flows the track gaplessly)")]
    fn reorder(&self, Parameters(p): Parameters<ReorderParams>) -> Result<String, McpError> {
        let track_id = parse_id(&p.track_id)?;
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            project.reorder(track_id, clip_id, p.new_index).map_err(core_err)?;
            Ok("ok".to_string())
        })
    }

    #[tool(
        description = "Move a clip to a new timeline position (free positioning, gaps allowed), optionally onto another same-kind track; rejects overlaps"
    )]
    fn move_clip(&self, Parameters(p): Parameters<MoveClipParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let track_id = p.track_id.as_deref().map(parse_id).transpose()?;
        self.edit(|project| {
            let out = project.move_clip(clip_id, p.timeline_start, track_id).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Remove a clip and close the gap: later clips on the same track shift left by its duration (ripple delete)"
    )]
    fn ripple_delete(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            project.ripple_delete(clip_id).map_err(core_err)?;
            Ok("ok".to_string())
        })
    }

    #[tool(
        description = "Cut a source-time range out of a clip and close the gap (split + ripple in one edit) — \
                       the transcript-editing primitive: pass a transcript segment's start/end to delete that \
                       sentence from the cut"
    )]
    fn cut_clip_range(&self, Parameters(p): Parameters<CutClipRangeParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let pieces = project.cut_clip_range(clip_id, p.from, p.to).map_err(core_err)?;
            json(&pieces)
        })
    }

    #[tool(
        description = "Add a new empty track (\"video\" or \"audio\"), e.g. a B-roll lane above the interview; later video tracks composite on top at export"
    )]
    fn add_track(&self, Parameters(p): Parameters<AddTrackParams>) -> Result<String, McpError> {
        let kind = parse_kind(&p.kind)?;
        self.edit(|project| {
            let track = project.add_track(kind, p.name).map_err(core_err)?;
            json(&track)
        })
    }

    #[tool(description = "Remove a track and all of its clips (refuses to remove the last track)")]
    fn remove_track(&self, Parameters(p): Parameters<TrackIdParams>) -> Result<String, McpError> {
        let track_id = parse_id(&p.track_id)?;
        self.edit(|project| {
            project.remove_track(track_id).map_err(core_err)?;
            Ok("ok".to_string())
        })
    }

    #[tool(
        description = "Duck (or unduck) a track: on export its audio is sidechain-compressed under the \
                       other tracks, so e.g. a music bed dips automatically whenever dialogue plays"
    )]
    fn set_track_duck(&self, Parameters(p): Parameters<SetTrackDuckParams>) -> Result<String, McpError> {
        let track_id = parse_id(&p.track_id)?;
        self.edit(|project| {
            let track = project.set_track_duck(track_id, p.duck).map_err(core_err)?;
            json(&track)
        })
    }

    #[tool(description = "Cut a clip to a shape: inside the shape the clip is kept, outside it goes \
                       transparent and whatever is on a LOWER track shows through. Omit `shape` to \
                       clear the mask. This is the one masking primitive, and it composes with the \
                       track stack rather than replacing it — to blur a face, duplicate the shot onto \
                       the track above with add_clip at the same timeline_start, give the copy a blur \
                       with set_video_effects, then mask the copy to an ellipse over the face; to grade \
                       one region, do the same with set_color. Check the result with preview_timeline: \
                       positions are fractions of the frame, so you have to look to know you covered \
                       the right thing.")]
    fn set_mask(&self, Parameters(p): Parameters<SetMaskParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let mask = match p.shape {
                None => None,
                Some(ref s) => {
                    let shape = MaskShape::parse(s).ok_or_else(|| {
                        McpError::invalid_params(format!("invalid mask shape '{s}'; expected \"rect\" or \"ellipse\""), None)
                    })?;
                    // Omitted fields keep the clip's current mask, so nudging one
                    // number (move the ellipse a little left) does not reset the
                    // size and feather back to the defaults out from under it.
                    // Deliberately not `.ok()…unwrap_or_default()`: a read that
                    // failed is not the same answer as "this clip has no mask
                    // yet", and collapsing the two silently resets every field
                    // the caller did not name.
                    let timeline = project.working_timeline().map_err(core_err)?;
                    let (ti, ci) = timeline
                        .locate(clip_id)
                        .ok_or_else(|| core_err(kerf_core::Error::ClipNotFound(clip_id)))?;
                    let d = timeline.tracks[ti].clips[ci].mask.unwrap_or_default();
                    Some(Mask {
                        shape,
                        x: p.x.unwrap_or(d.x),
                        y: p.y.unwrap_or(d.y),
                        width: p.width.unwrap_or(d.width),
                        height: p.height.unwrap_or(d.height),
                        feather: p.feather.unwrap_or(d.feather),
                        inverted: p.inverted.unwrap_or(d.inverted),
                    })
                }
            };
            let clip = project.set_mask(clip_id, mask).map_err(core_err)?;
            json(&clip)
        })
    }

    #[tool(
        description = "Set a track's fader — one linear gain riding every clip on the track, applied after \
                       each clip's own volume and effects. This is how a music bed is balanced against a \
                       voiceover: put the music on its own track and pull it to roughly 0.25-0.4 under \
                       speech. Prefer it to editing every clip's volume, and to set_track_duck when the \
                       level should simply sit lower rather than dip and recover."
    )]
    fn set_track_volume(&self, Parameters(p): Parameters<SetTrackVolumeParams>) -> Result<String, McpError> {
        let track_id = parse_id(&p.track_id)?;
        self.edit(|project| {
            let track = project.set_track_volume(track_id, p.volume).map_err(core_err)?;
            json(&track)
        })
    }

    #[tool(
        description = "Place a track in the stereo field, -1 hard left to 1 hard right. A balance, so a \
                       panned track never gets louder, and a no-op on a mono delivery. Use it sparingly — \
                       a hard-panned music bed sounds broken on a phone speaker, which is what most of \
                       this footage is watched on."
    )]
    fn set_track_pan(&self, Parameters(p): Parameters<SetTrackPanParams>) -> Result<String, McpError> {
        let track_id = parse_id(&p.track_id)?;
        self.edit(|project| {
            let track = project.set_track_pan(track_id, p.pan).map_err(core_err)?;
            json(&track)
        })
    }

    #[tool(
        description = "Set the frame this project is cut for — the shape of the delivered video, e.g. \
                       1080x1920 for a vertical Reel or 1080x1080 for a square feed post. Everything \
                       that renders a picture (preview_timeline, get_frame and the export) then uses \
                       it, so what you look at while cutting is what ships. With \"cover\" (the default) \
                       a 16:9 shot is filled and cropped to the frame, which means the crop decides what \
                       survives: check preview_timeline and use set_transform's position to slide the \
                       important part of a shot back into frame. Omit width and height to clear it."
    )]
    fn set_delivery_format(&self, Parameters(p): Parameters<SetDeliveryFormatParams>) -> Result<String, McpError> {
        let format = match (p.width, p.height) {
            (Some(w), Some(h)) => Some(Delivery::new(w, h, p.fit.unwrap_or(Fit::Cover))),
            _ => None,
        };
        self.edit(|project| {
            let timeline = project.set_delivery_format(format).map_err(core_err)?;
            json(&timeline)
        })
    }

    #[tool(
        description = "Drop a named marker on the timeline. Markers render nothing — they are shared \
                       vocabulary for places in the cut, so 'the laugh at 01:12' survives as something \
                       the user can see and jump to. Good for reporting findings from skim_asset."
    )]
    fn add_marker(&self, Parameters(p): Parameters<AddMarkerParams>) -> Result<String, McpError> {
        self.edit(|project| {
            let marker = project.add_marker(p.time, p.name, p.color).map_err(core_err)?;
            json(&marker)
        })
    }

    #[tool(description = "Move, rename or recolor a marker; omitted fields are left alone")]
    fn update_marker(&self, Parameters(p): Parameters<UpdateMarkerParams>) -> Result<String, McpError> {
        let marker_id = parse_id(&p.marker_id)?;
        self.edit(|project| {
            let marker = project.update_marker(marker_id, p.time, p.name, p.color).map_err(core_err)?;
            json(&marker)
        })
    }

    #[tool(description = "Remove a marker from the timeline")]
    fn remove_marker(&self, Parameters(p): Parameters<MarkerIdParams>) -> Result<String, McpError> {
        let marker_id = parse_id(&p.marker_id)?;
        self.edit(|project| {
            project.remove_marker(marker_id).map_err(core_err)?;
            Ok("ok".to_string())
        })
    }

    #[tool(
        description = "Mute (or unmute) a track: its clips stop rendering — silent for audio, hidden for \
                       video — while keeping their place on the timeline. Use this to audition a cut \
                       without a music bed, or to park B-roll without deleting it."
    )]
    fn set_track_muted(&self, Parameters(p): Parameters<SetTrackMutedParams>) -> Result<String, McpError> {
        let track_id = parse_id(&p.track_id)?;
        self.edit(|project| {
            let track = project.set_track_muted(track_id, p.muted).map_err(core_err)?;
            json(&track)
        })
    }

    #[tool(
        description = "Solo (or unsolo) a track. While any track of a kind is soloed, the other tracks of \
                       that kind stop rendering; video and audio solo independently, so soloing a music \
                       bed does not blank the picture. Several tracks may be soloed at once."
    )]
    fn set_track_solo(&self, Parameters(p): Parameters<SetTrackSoloParams>) -> Result<String, McpError> {
        let track_id = parse_id(&p.track_id)?;
        self.edit(|project| {
            let track = project.set_track_solo(track_id, p.solo).map_err(core_err)?;
            json(&track)
        })
    }

    #[tool(
        description = "Lock (or unlock) a track against editing. A locked track still renders; the GUI \
                       refuses to drag, trim or split its clips."
    )]
    fn set_track_locked(&self, Parameters(p): Parameters<SetTrackLockedParams>) -> Result<String, McpError> {
        let track_id = parse_id(&p.track_id)?;
        self.edit(|project| {
            let track = project.set_track_locked(track_id, p.locked).map_err(core_err)?;
            json(&track)
        })
    }

    #[tool(
        description = "Enable or disable one clip. A disabled clip keeps its position, trims, effects and \
                       keyframes but drops out of the render — the reversible way to try a cut without it."
    )]
    fn set_clip_enabled(&self, Parameters(p): Parameters<SetClipEnabledParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let clip = project.set_clip_enabled(clip_id, p.enabled).map_err(core_err)?;
            json(&clip)
        })
    }

    #[tool(
        description = "Copy clips and insert the copies so the earliest lands at `at`, keeping the \
                       relative offsets between them and each one's track. Everything comes along — \
                       trims, speed, transform, color, effects, keyframes, reframe — which \
                       add_clip_to_timeline cannot do, since that builds a fresh clip. Rejected \
                       outright if any copy would overlap, so a partial paste never lands."
    )]
    fn duplicate_clips(&self, Parameters(p): Parameters<DuplicateClipsParams>) -> Result<String, McpError> {
        let ids = p.clip_ids.iter().map(|s| parse_id(s)).collect::<Result<Vec<_>, _>>()?;
        self.edit(|project| {
            let clips = project.duplicate_clips(&ids, p.at).map_err(core_err)?;
            json(&clips)
        })
    }

    #[tool(description = "Remove a clip from the timeline")]
    fn remove(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            project.remove(clip_id).map_err(core_err)?;
            Ok("ok".to_string())
        })
    }

    #[tool(description = "Set the linear volume gain of a clip")]
    fn set_volume(&self, Parameters(p): Parameters<VolumeParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project.set_volume(clip_id, p.volume).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(description = "Set a clip's fade-in / fade-out duration in seconds (omit a field to leave it unchanged, 0 to clear)")]
    fn set_fade(&self, Parameters(p): Parameters<FadeParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project.set_fade(clip_id, p.fade_in, p.fade_out).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Set a clip's playback speed (1.0 = unchanged, 2.0 = 2x faster, 0.5 = half, negative = reverse); this retimes the clip and changes its timeline duration"
    )]
    fn set_speed(&self, Parameters(p): Parameters<SpeedParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project.set_speed(clip_id, p.speed).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Set a clip's geometric transform — scale / position (pos_x, pos_y as fractions of the frame) / rotation / opacity / per-edge crop. Use a sub-1.0 scale with a position for picture-in-picture. Omit a field to leave it unchanged."
    )]
    fn set_transform(&self, Parameters(p): Parameters<TransformParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project
                .set_transform(
                    clip_id,
                    p.scale,
                    p.pos_x,
                    p.pos_y,
                    p.rotation,
                    p.opacity,
                    p.crop_left,
                    p.crop_right,
                    p.crop_top,
                    p.crop_bottom,
                )
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Set a clip's color correction — brightness / contrast / saturation / gamma / temperature (warm-cool). Omit a field to leave it unchanged."
    )]
    fn set_color(&self, Parameters(p): Parameters<ColorParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project
                .set_color(clip_id, p.brightness, p.contrast, p.saturation, p.gamma, p.temperature)
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Set or clear the transition blending a clip's start with the clip before it on the same track, with a duration in seconds; omit kind to clear. A dissolve or a motion transition plays both shots at once, so it borrows the outgoing clip's unused source — a clip trimmed to the very end of its footage has none to lend and the transition falls back to a hard cut. Dips need no handle. Reach for a fade between scenes and a motion transition between shots in a montage; a cut needs no transition at all."
    )]
    fn set_transition(&self, Parameters(p): Parameters<TransitionParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let transition = parse_transition(p.kind, p.duration)?;
        self.edit(|project| {
            let out = project.set_transition(clip_id, transition).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Replace a clip's video effect chain (applied in order at export): blur, sharpen, grayscale, invert, vignette, or chroma_key (key a color to transparency so footage on a lower track shows through). Pass an empty list to clear."
    )]
    fn set_video_effects(&self, Parameters(p): Parameters<VideoEffectsParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project.set_video_effects(clip_id, p.effects).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Replace a clip's audio effect chain (applied in order at export): highpass, lowpass, equalizer (parametric band), compressor (dynamics) or gate (noise gate). Pass an empty list to clear."
    )]
    fn set_audio_effects(&self, Parameters(p): Parameters<AudioEffectsParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project.set_audio_effects(clip_id, p.effects).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Replace a clip's transform keyframes to animate scale / position / rotation / opacity over time. Two or more keyframes animate the clip (e.g. a Ken Burns zoom, a moving picture-in-picture). Pass an empty list to clear the animation."
    )]
    fn set_keyframes(&self, Parameters(p): Parameters<KeyframesParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project.set_keyframes(clip_id, p.keyframes).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Add (or replace) one transform keyframe at a time offset from the clip's start; unspecified channels capture the clip's current pose there. Use two calls to animate between two poses."
    )]
    fn add_keyframe(&self, Parameters(p): Parameters<AddKeyframeParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project
                .add_keyframe(clip_id, p.time, p.scale, p.pos_x, p.pos_y, p.rotation, p.opacity)
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(description = "Remove all transform keyframes from a clip (back to its static transform)")]
    fn clear_keyframes(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project.clear_keyframes(clip_id).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Aim a 360 clip's virtual camera — yaw / pitch / roll / field of view — to render an ordinary \
                       rectilinear shot out of spherical footage. Clips cut from 360 assets are reframed by default, \
                       so this adjusts where the camera points. Call preview_timeline first to see the whole sphere \
                       and decide where the subject is. Omit a field to leave it unchanged."
    )]
    fn set_reframe(&self, Parameters(p): Parameters<ReframeParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project
                .set_reframe(clip_id, p.yaw, p.pitch, p.roll, p.fov, p.lens_fov, p.input, p.output)
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Mark an asset as 360 footage Kerf did not detect as such (or pass null to unmark it). \
                       Detection only trusts spherical metadata or a telltale capture layout, so a stitched \
                       equirect export that lost its metadata imports as flat video. Unlike set_reframe this is \
                       remembered on the asset, so every clip cut from it afterwards is reframed automatically."
    )]
    fn set_asset_projection(&self, Parameters(p): Parameters<AssetProjectionParams>) -> Result<String, McpError> {
        let asset_id = parse_id(&p.asset_id)?;
        let asset = self.lock().set_asset_projection(asset_id, p.projection).map_err(core_err)?;
        crate::spawn_proxy(&self.app, &asset);
        self.changed();
        json(&asset)
    }

    #[tool(
        description = "Stop reprojecting a 360 clip, leaving its raw spherical picture (equirect or dual-fisheye) on \
                       the timeline."
    )]
    fn clear_reframe(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project.clear_reframe(clip_id).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Replace a 360 clip's camera keyframes to pan the virtual camera over the clip — following a \
                       subject, whipping between two points of interest, or pushing in by narrowing the field of \
                       view. Pass an empty list to hold a static pose."
    )]
    fn set_reframe_keyframes(&self, Parameters(p): Parameters<ReframeKeyframesParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project.set_reframe_keyframes(clip_id, p.keyframes).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Add (or replace) one 360 camera keyframe at a time offset from the clip's start; unspecified \
                       channels capture the camera's current pose there. Two calls pan between two directions."
    )]
    fn add_reframe_keyframe(&self, Parameters(p): Parameters<AddReframeKeyframeParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        self.edit(|project| {
            let out = project
                .add_reframe_keyframe(clip_id, p.time, p.yaw, p.pitch, p.roll, p.fov)
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Add a text overlay (title / lower-third / caption / watermark) drawn over the composited picture between start and end (timeline seconds). Returns the overlay; style or animate it with update_overlay / set_overlay_keyframes."
    )]
    fn add_overlay(&self, Parameters(p): Parameters<AddOverlayParams>) -> Result<String, McpError> {
        self.edit(|project| {
            let out = project.add_overlay(p.text, p.start, p.end).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Update a text overlay's text, timing, position (pos_x / pos_y as 0–1 frame fractions), size (font height fraction), color, box background (bg, empty string clears), font (system font family from list_fonts, empty string clears) or bold. Omit a field to leave it unchanged."
    )]
    fn update_overlay(&self, Parameters(p): Parameters<UpdateOverlayParams>) -> Result<String, McpError> {
        let overlay_id = parse_id(&p.overlay_id)?;
        self.edit(|project| {
            let out = project
                .update_overlay(
                    overlay_id, p.text, p.start, p.end, p.pos_x, p.pos_y, p.size, p.color, p.bg, p.font, p.bold,
                )
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(description = "Remove a text overlay")]
    fn remove_overlay(&self, Parameters(p): Parameters<OverlayIdParams>) -> Result<String, McpError> {
        let overlay_id = parse_id(&p.overlay_id)?;
        self.edit(|project| {
            project.remove_overlay(overlay_id).map_err(core_err)?;
            Ok("ok".to_string())
        })
    }

    #[tool(
        description = "Set or clear (empty list) a text overlay's position/opacity keyframes, to animate it over its lifetime (e.g. a title that slides in and fades out)"
    )]
    fn set_overlay_keyframes(&self, Parameters(p): Parameters<OverlayKeyframesParams>) -> Result<String, McpError> {
        let overlay_id = parse_id(&p.overlay_id)?;
        self.edit(|project| {
            let out = project.set_overlay_keyframes(overlay_id, p.keyframes).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Caption the cut: project every clip's cached transcript (run analyze_asset first) through the current edit and write the result as text overlays, replacing any previously generated set. Captions are placed in TIMELINE time, so they follow trims, reorders, speed changes and removed silences, and words that were cut out get no caption. Long sentences are split into readable lines. Pick the look with `style`: `lines` for subtitles, `word_punch` for one big word at a time (what social captions usually look like — prefer it for a vertical cut). Hand-made titles and lower-thirds are left alone. Returns the overlays created."
    )]
    fn generate_captions(&self, Parameters(p): Parameters<CaptionParams>) -> Result<String, McpError> {
        let opts = CaptionOptions {
            style: p.style.unwrap_or_default(),
            max_words: p.max_words,
            max_chars: p.max_chars,
            pos_y: p.pos_y,
            size: p.size,
        };
        self.edit(|project| {
            let out = project.generate_captions(opts).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Remove the captions generate_captions wrote, leaving hand-made titles and lower-thirds alone. Returns how many were removed."
    )]
    fn clear_captions(&self) -> Result<String, McpError> {
        self.edit(|project| {
            let removed = project.clear_captions().map_err(core_err)?;
            Ok(format!("removed {removed} generated caption(s)"))
        })
    }

    #[tool(description = "Write an asset's cached transcript to a SubRip (.srt) subtitle file (run analyze_asset first)")]
    fn export_srt(&self, Parameters(p): Parameters<ExportSrtParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let srt = {
            let project = self.lock();
            project.transcript_srt(id).map_err(core_err)?
        };
        std::fs::write(&p.output_path, srt).map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(format!("wrote {}", p.output_path))
    }

    #[tool(description = "Append the non-silent spans of an asset as clips, using cached analysis")]
    fn remove_silence(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        self.edit(|project| {
            let out = project.remove_silence(id).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Smart crop: frame each shot for the delivery frame instead of centring it. Samples where a clip's content actually sits and writes the crop that keeps it, per clip — so 16:9 footage delivered at 9:16 keeps the subject rather than whatever happened to be in the middle. Set the delivery frame first (set_delivery_format); clips already that shape are left alone. The result is an ordinary transform crop the user can adjust or undo"
    )]
    async fn smart_crop(&self, Parameters(p): Parameters<SmartCropParams>) -> Result<String, McpError> {
        let clip = p.clip_id.as_deref().map(parse_id).transpose()?;
        let project = self.project.clone();
        let (moved, timeline) = blocking(move || {
            // Plan under the lock, sample with it released (one short ffmpeg
            // decode per clip), apply under it again — the same shape as
            // analyze_asset, for the same reason.
            let plan = lock_agent(&project).smart_crop_inputs(clip).map_err(core_err)?;
            let crops = Project::sample_smart_crops(&plan).map_err(core_err)?;
            let guard = lock_agent(&project);
            let moved = guard.apply_smart_crops(&crops).map_err(core_err)?;
            let timeline = guard.working_timeline().map_err(core_err)?;
            Ok((moved, timeline))
        })
        .await?;
        self.changed();
        json(&serde_json::json!({ "clips_reframed": moved, "timeline": timeline }))
    }

    #[tool(
        description = "Cut to the beat: ripple a track's cuts onto the beat grid of the analyzed music on the audio tracks, retrimming each clip so its outgoing cut lands on a beat. Analyze the music asset first"
    )]
    fn snap_to_beats(&self, Parameters(p): Parameters<SnapToBeatsParams>) -> Result<String, McpError> {
        let track = p.track_id.as_deref().map(parse_id).transpose()?;
        self.edit(|project| {
            let aligned = project.snap_to_beats(track, p.tolerance).map_err(core_err)?;
            // The *working* timeline, like every other read here: inside a task the
            // alignment lands in the staged proposal, and `timeline()` would hand
            // back the user's untouched cut — a timeline with none of the moves the
            // `cuts_aligned` count beside it just reported.
            let timeline = project.working_timeline().map_err(core_err)?;
            json(&serde_json::json!({ "cuts_aligned": aligned, "timeline": timeline }))
        })
    }

    #[tool(description = "Append the full audio of an asset to the first audio track")]
    fn extract_audio(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        self.edit(|project| {
            let out = project.extract_audio(id).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(description = "Stitch the full length of several assets together in order")]
    fn concatenate(&self, Parameters(p): Parameters<ConcatParams>) -> Result<String, McpError> {
        let ids = p.asset_ids.iter().map(|s| parse_id(s)).collect::<Result<Vec<Uuid>, _>>()?;
        self.edit(|project| {
            let out = project.concatenate(&ids).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Begin a staged edit: from here on your edits are held back as a proposal the user reviews \
                       instead of changing the cut they are looking at. Your own reads (get_timeline_state, \
                       preview_timeline, timeline_summary, export) follow the proposal, so you can check your work \
                       before handing it over. claim_next_task already does this for you."
    )]
    fn stage_edits(&self, Parameters(p): Parameters<StageEditsParams>) -> Result<String, McpError> {
        self.edit(|project| {
            let staged = project.begin_staging(None, p.note.as_deref()).map_err(core_err)?;
            json(&staged)
        })
    }

    #[tool(
        description = "What your staged edits would change about the user's cut — every add, cut, move, retrim and \
                       adjustment, plus the runtime before and after. Returns null when nothing is staged."
    )]
    fn staged_diff(&self) -> Result<String, McpError> {
        let project = self.lock();
        match project.staged().map_err(core_err)? {
            None => json(&serde_json::json!(null)),
            Some(staged) => {
                let summary = staged.diff.summary();
                json(&serde_json::json!({ "staged": staged, "summary": summary }))
            }
        }
    }

    #[tool(
        description = "Apply your staged edits to the live timeline yourself, as one revision. Normally leave this to \
                       the user — completing the task shows them the proposal to accept. Fails if they have edited \
                       the timeline since you staged, unless `force`."
    )]
    fn apply_staged_edits(&self, Parameters(p): Parameters<ApplyStagedParams>) -> Result<String, McpError> {
        self.edit(|project| {
            let timeline = project.apply_staged(p.force.unwrap_or(false)).map_err(core_err)?;
            json(&timeline)
        })
    }

    #[tool(description = "Throw your staged edits away, leaving the user's timeline untouched")]
    fn discard_staged_edits(&self) -> Result<String, McpError> {
        self.edit(|project| {
            let timeline = project.discard_staged().map_err(core_err)?;
            json(&timeline)
        })
    }

    #[tool(description = "Explain what one revision changed (see history), as a list of edits to the cut")]
    fn revision_diff(&self, Parameters(p): Parameters<RevisionDiffParams>) -> Result<String, McpError> {
        let project = self.lock();
        let diff = match p.from {
            Some(from) => project.diff_revisions(from, p.seq).map_err(core_err)?,
            None => project.revision_diff(p.seq).map_err(core_err)?,
        };
        let summary = diff.summary();
        json(&serde_json::json!({ "diff": diff, "summary": summary }))
    }

    #[tool(description = "List the timeline edit history (revisions, newest changes have higher seq; the current one is marked)")]
    fn history(&self) -> Result<String, McpError> {
        let project = self.lock();
        json(&project.history().map_err(core_err)?)
    }

    #[tool(description = "Undo the last timeline edit, returning the restored timeline")]
    fn undo(&self) -> Result<String, McpError> {
        self.edit(|project| {
            let out = project.undo().map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(description = "Redo the next timeline edit, returning the restored timeline")]
    fn redo(&self) -> Result<String, McpError> {
        self.edit(|project| {
            let out = project.redo().map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(description = "Revert the timeline to a specific revision seq (see history), returning the restored timeline")]
    fn revert_to(&self, Parameters(p): Parameters<RevertParams>) -> Result<String, McpError> {
        self.edit(|project| {
            let out = project.revert_to(p.seq).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "List the hardware (GPU) video encoders this machine's ffmpeg can actually use (verified by a \
                       test encode) — pass one as export's video_codec for a much faster render. Empty means software \
                       encoders only."
    )]
    async fn export_capabilities(&self) -> Result<String, McpError> {
        let encoders = blocking(|| Ok(kerf_core::hw_encoders().to_vec())).await?;
        json(&serde_json::json!({ "hw_encoders": encoders }))
    }

    #[tool(
        description = "Render the timeline to a file with full ffmpeg encode control (container, video/audio codec, \
                       rate control, resolution, fps, bitrate, faststart, gif, audio-only …). Omit `options` for the \
                       safe H.264/AAC MP4 default. A render takes minutes: send a `progressToken` in the request's \
                       `_meta` to receive progress notifications while it runs, and cancel the request \
                       (`notifications/cancelled`) to stop it — a cancelled render deletes its half-written file \
                       rather than leaving a broken one behind."
    )]
    async fn export(
        &self,
        Parameters(p): Parameters<ExportParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let opts = p.options.unwrap_or_default();
        let project = self.project.clone();
        let output_path = p.output_path;

        // A render runs for minutes. Without progress an agent cannot tell a slow
        // export from a hung one, and without cancellation it cannot abandon a
        // render whose settings it already knows are wrong — it can only wait for
        // a file it does not want. Both ride the protocol rather than a Kerf-
        // specific channel: the client's `progressToken`, and the cancellation
        // token rmcp trips when `notifications/cancelled` arrives.
        let cancel = context.ct.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<kerf_core::ExportProgress>();
        let forward = {
            let peer = context.peer.clone();
            let token = context.meta.get_progress_token();
            // The task drains the channel either way, so a client that asked for
            // no progress does not leave ffmpeg's ticks piling up in it.
            tauri::async_runtime::spawn(async move {
                while let Some(progress) = rx.recv().await {
                    let Some(token) = token.clone() else { continue };
                    let mut param = ProgressNotificationParam::new(token, progress.fraction).with_total(1.0);
                    if let Some(eta) = progress.eta_secs {
                        param = param.with_message(format!("rendering, {} to go", fmt_ts(eta)));
                    }
                    let _ = peer.notify_progress(param).await;
                }
            })
        };

        let render_path = output_path.clone();
        let status = blocking(move || {
            // Snapshot the timeline + assets under the lock, then render with the
            // lock released so a long export doesn't freeze the GUI (or block other
            // agent tools) for its whole duration.
            let (timeline, assets) = {
                let project = lock_agent(&project);
                (
                    project.working_timeline().map_err(core_err)?,
                    project.list_assets().map_err(core_err)?,
                )
            };
            let mut on_progress = |progress: kerf_core::ExportProgress| {
                let _ = tx.send(progress);
            };
            let status = kerf_core::render_with_progress(
                &timeline,
                &assets,
                std::path::Path::new(&render_path),
                &opts,
                &mut on_progress,
                &|| cancel.is_cancelled(),
            )
            .map_err(core_err)?;
            if status == kerf_core::RenderStatus::Cancelled {
                // Mirrors the GUI: a cancelled export leaves no debris, and in
                // particular no truncated file that reads as a finished render.
                // Done *here*, not after the await — cancelling the request can
                // drop this handler's future, while a blocking job runs to
                // completion whether or not anyone is still holding its handle.
                let _ = std::fs::remove_file(&render_path);
            }
            Ok(status)
        })
        .await;
        // `tx` died with the closure, so the forwarder has already run dry.
        let _ = forward.await;

        match status? {
            kerf_core::RenderStatus::Completed => json(&serde_json::json!({ "output": output_path })),
            kerf_core::RenderStatus::Cancelled => Err(McpError::internal_error(
                format!("export cancelled; {output_path} was removed"),
                None,
            )),
        }
    }

    #[tool(
        description = "Check the assembled cut against each publishing target (Instagram Reels / YouTube Shorts / \
                       TikTok / Instagram feed / YouTube): length limits, frame shape, and the reach limits a platform \
                       enforces silently — e.g. a Reel over 3 minutes uploads fine and is then shown only to existing \
                       followers. Returns errors (would be rejected), warnings (accepted but under-distributed or \
                       letterboxed) and tips. Advisory: export is never blocked. Use before reporting a cut finished."
    )]
    fn platform_check(&self) -> Result<String, McpError> {
        // One summary, not two: `Project::platform_check` resolves its own, and
        // each resolve deserializes the whole timeline and re-reads every asset
        // row to answer the identical question.
        let summary = self.lock().cut_summary(None).map_err(core_err)?;
        let targets = kerf_core::platform::check_all(&summary);
        json(&serde_json::json!({
            "cut": {
                "duration_secs": summary.duration,
                "frame": format!("{}x{}", summary.width, summary.height),
                "has_audio": summary.has_audio,
                "has_text": summary.has_text,
            },
            "targets": targets,
        }))
    }

    #[tool(
        description = "Write the composited timeline at a given time to an image file as a cover / thumbnail — full \
                       delivery resolution, rendered through the export graph, so it is a real frame of the finished \
                       video at the shape the project is cut for."
    )]
    async fn export_cover(&self, Parameters(p): Parameters<CoverParams>) -> Result<String, McpError> {
        let project = self.project.clone();
        let (time_secs, output_path) = (p.time_secs, p.output_path);
        let written = blocking(move || {
            let (timeline, assets) = lock_agent(&project).export_still_inputs().map_err(core_err)?;
            Project::render_still(&timeline, &assets, time_secs, &output_path, None).map_err(core_err)
        })
        .await?;
        json(&serde_json::json!({ "output": written.to_string_lossy() }))
    }

    // ---- agent task queue --------------------------------------------------

    #[tool(description = "List the agent task queue with each task's status (queued/working/ready/done/failed)")]
    fn list_tasks(&self) -> Result<String, McpError> {
        let project = self.lock();
        json(&project.list_tasks().map_err(core_err)?)
    }

    #[tool(description = "Enqueue a new task (status: queued) for an agent to claim")]
    fn add_task(&self, Parameters(p): Parameters<AddTaskParams>) -> Result<String, McpError> {
        self.edit(|project| {
            let task = project.add_task(&p.prompt).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(description = "Claim the oldest queued task (marks it working) and return it; returns null when the queue is empty")]
    fn claim_next_task(&self) -> Result<String, McpError> {
        self.edit(|project| {
            let task = project.claim_next_task().map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(description = "Mark a claimed task ready for the user to review, with a summary of the edits made")]
    fn complete_task(&self, Parameters(p): Parameters<CompleteTaskParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        self.edit(|project| {
            let task = project.complete_task(id, p.result).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(description = "Mark a task failed with an error message")]
    fn fail_task(&self, Parameters(p): Parameters<FailTaskParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        self.edit(|project| {
            let task = project.fail_task(id, &p.error).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(description = "Mark a task done (user accepted the staged edit), returning the updated task")]
    fn resolve_task(&self, Parameters(p): Parameters<TaskIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        self.edit(|project| {
            let task = project.resolve_task(id).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(description = "Remove a task from the queue permanently, returning the updated task list")]
    fn remove_task(&self, Parameters(p): Parameters<TaskIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        self.edit(|project| {
            project.remove_task(id).map_err(core_err)?;
            json(&project.list_tasks().map_err(core_err)?)
        })
    }

    #[tool(description = "Get peak-magnitude waveform data (0.0–1.0) for an asset's first audio stream")]
    async fn get_waveform(&self, Parameters(p): Parameters<WaveformParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let count = p.buckets.clamp(1, MAX_WAVEFORM_BUCKETS);
        let project = self.project.clone();
        let buckets = blocking(move || {
            // Resolve under the lock, decode the whole audio stream with it
            // released — bucketing a long source takes seconds and must not
            // stall the GUI's commands on the shared mutex.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            Project::decode_waveform(&asset, count).map_err(core_err)
        })
        .await?;
        json(&buckets)
    }

    #[tool(
        description = "Get an RMS energy envelope (0.0–1.0 per bucket) for an asset's first audio stream — a perceptual loudness-over-time curve. Unlike the peak waveform, it tracks how loud each slice feels, so use it to find quiet/loud passages and match cut pacing to musical energy."
    )]
    async fn get_energy(&self, Parameters(p): Parameters<WaveformParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let count = p.buckets.clamp(1, MAX_WAVEFORM_BUCKETS);
        let project = self.project.clone();
        let energy = blocking(move || {
            // Same lock-free decode shape as `get_waveform`.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            Project::decode_energy(&asset, count).map_err(core_err)
        })
        .await?;
        json(&energy)
    }

    #[tool(
        description = "Decode a single frame from an asset at a source time and return it as a low-res image the model can actually see. Use to drill into a specific moment (e.g. one cell flagged by skim_asset) before cutting."
    )]
    async fn get_frame(&self, Parameters(p): Parameters<FrameParams>) -> Result<CallToolResult, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.project.clone();
        let (time_secs, max_width) = (p.time_secs, p.max_width.unwrap_or(640).clamp(64, MAX_PREVIEW_WIDTH));
        let region = p.region;
        let jpeg = blocking(move || {
            // Resolve under the lock, decode with it released — mirrors the GUI's
            // `get_frame` so an agent drill-in can't stall the user's edits.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            match region {
                // A zoom is a request for detail, so it decodes the original at
                // the best JPEG quality rather than the proxy at the preview one.
                Some(r) => Project::decode_preview_region(&asset, time_secs, r.region(), max_width, ZOOM_QUALITY),
                None => Project::decode_preview_frame(&asset, time_secs, max_width, 4, true),
            }
            .map_err(core_err)
        })
        .await?;
        let mut caption = format!("asset {} @ {}", p.asset_id, fmt_ts(p.time_secs.max(0.0)));
        if let Some(r) = region {
            caption.push_str(&format!(", {}", r.describe()));
        }
        Ok(image_result(caption, jpeg))
    }

    #[tool(
        description = "Skim an asset: sample frames evenly across a time range (default the whole asset) into one contact-sheet image, plus a text index of which source timestamp each grid cell shows. The cheap way to survey footage and find the good parts; then pass `cell` (same range and grid) to see one cell at full detail, or call get_frame — with a `region` to zoom — to inspect a promising moment, and add_clip_to_timeline / cut_clip to use it."
    )]
    async fn skim_asset(&self, Parameters(p): Parameters<SkimParams>) -> Result<CallToolResult, McpError> {
        let id = parse_id(&p.asset_id)?;
        let columns = p.columns.unwrap_or(4).clamp(1, 8);
        let rows = p.rows.unwrap_or(4).clamp(1, 8);
        let cell_width = p.cell_width.unwrap_or(240).clamp(80, 640);
        let project = self.project.clone();
        if let Some(cell) = p.cell {
            return self.skim_cell(id, &p, columns, rows, cell).await;
        }
        let (jpeg, times) = blocking(move || {
            // Resolve under the lock, sample the (columns × rows) frames with it
            // released — a contact sheet is many seeks and would otherwise freeze
            // the GUI's commands on the shared mutex for its whole build.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            Project::decode_contact_sheet(&asset, p.start, p.end, columns, rows, cell_width, 5).map_err(core_err)
        })
        .await?;
        let index = times
            .iter()
            .enumerate()
            .map(|(i, t)| format!("  cell {}: {}", i + 1, fmt_ts(*t)))
            .collect::<Vec<_>>()
            .join("\n");
        let caption = format!("contact sheet {columns}x{rows} (row-major), cell -> source time:\n{index}");
        Ok(image_result(caption, jpeg))
    }

    /// The `cell` form of [`Self::skim_asset`]: the moment one cell of a sheet
    /// showed, decoded on its own at full detail. The sheet is never rebuilt —
    /// the cell's timestamp is pure arithmetic over the same range and grid.
    async fn skim_cell(
        &self,
        id: uuid::Uuid,
        p: &SkimParams,
        columns: u32,
        rows: u32,
        cell: u32,
    ) -> Result<CallToolResult, McpError> {
        let cells = columns * rows;
        if cell < 1 || cell > cells {
            return Err(McpError::invalid_params(
                format!("cell must be 1..={cells} for a {columns}x{rows} sheet, got {cell}"),
                None,
            ));
        }
        let max_width = p.max_width.unwrap_or(640).clamp(64, MAX_PREVIEW_WIDTH);
        let (start, end) = (p.start, p.end);
        let project = self.project.clone();
        let (jpeg, time) = blocking(move || {
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            // The same range defaults `decode_contact_sheet` applies, so the cell
            // lands on the frame the sheet showed.
            let start = start.unwrap_or(0.0).max(0.0);
            let end = end.unwrap_or(asset.duration).min(asset.duration).max(start);
            let time = kerf_core::contact_sheet_times(start, end, columns, rows)[(cell - 1) as usize];
            let jpeg = Project::decode_preview_region(&asset, time, Region::FULL, max_width, ZOOM_QUALITY).map_err(core_err)?;
            Ok::<_, McpError>((jpeg, time))
        })
        .await?;
        Ok(image_result(
            format!(
                "cell {cell} of {columns}x{rows} sheet, asset {} @ {}",
                p.asset_id,
                fmt_ts(time)
            ),
            jpeg,
        ))
    }

    #[tool(
        description = "Render the assembled timeline at a timeline time into one composite image the model can see — the actual cut on screen at that moment (footage layered in track order, picture-in-picture placement, crop, color; gaps render black). Use to verify an edit you just made; pass `region` to zoom into a detail of it. A moment inside a transition is not: dissolves, dips and slides render as the plain cut."
    )]
    async fn preview_timeline(&self, Parameters(p): Parameters<TimelineFrameParams>) -> Result<CallToolResult, McpError> {
        let project = self.project.clone();
        let (time_secs, max_width) = (p.time_secs, p.max_width.unwrap_or(640).clamp(64, MAX_PREVIEW_WIDTH));
        let region = p.region;
        let jpeg = blocking(move || {
            // Snapshot the inputs under the lock, composite with it released —
            // mirrors the GUI's `get_timeline_frame`.
            let (timeline, assets) = lock_agent(&project).timeline_frame_inputs().map_err(core_err)?;
            match region {
                Some(r) => Project::composite_timeline_region(&timeline, &assets, time_secs, r.region(), max_width, ZOOM_QUALITY),
                None => Project::composite_timeline_frame(&timeline, &assets, time_secs, max_width, 4),
            }
            .map_err(core_err)
        })
        .await?;
        let mut caption = format!("timeline composite @ {}", fmt_ts(p.time_secs.max(0.0)));
        if let Some(r) = region {
            caption.push_str(&format!(", {}", r.describe()));
        }
        Ok(image_result(caption, jpeg))
    }

    #[tool(description = "Summarise the timeline: total duration, track count, clips per track, and any per-track gaps")]
    fn timeline_summary(&self) -> Result<String, McpError> {
        let project = self.lock();
        let timeline = project.working_timeline().map_err(core_err)?;
        let tracks: Vec<TrackSummary> = timeline
            .tracks
            .iter()
            .map(|t| TrackSummary {
                id: t.id.to_string(),
                name: t.name.clone(),
                kind: format!("{:?}", t.kind).to_lowercase(),
                clip_count: t.clips.len(),
                duration_secs: t.end(),
                gaps: track_gaps(t),
            })
            .collect();
        let total_clip_count = tracks.iter().map(|t| t.clip_count).sum();
        let summary = TimelineSummary {
            total_duration_secs: timeline.duration(),
            track_count: tracks.len(),
            total_clip_count,
            tracks,
            delivery_format: timeline
                .format
                .map(|d| format!("{}x{} ({})", d.width, d.height, format!("{:?}", d.fit).to_lowercase())),
            staged_changes: project.staged().map_err(core_err)?.map(|staged| staged.diff.entries.len()),
        };
        json(&summary)
    }
}

impl KerfMcp {
    fn new(project: Arc<Mutex<Project>>, app: AppHandle) -> Self {
        Self { project, app }
    }

    /// Lock the shared project, attributing any edits made under this guard to
    /// the agent. The GUI sets `User` the same way, and the mutex keeps the two
    /// from interleaving within a single operation.
    fn lock(&self) -> MutexGuard<'_, Project> {
        lock_agent(&self.project)
    }

    /// Run one operation under the shared lock, release the lock, *then* tell the
    /// webview. Every mutating tool is this shape, and the order matters: the
    /// event makes the GUI re-fetch, and that re-fetch takes the same lock. A
    /// failed operation notifies nothing, because nothing changed.
    fn edit<T>(&self, op: impl FnOnce(&Project) -> Result<T, McpError>) -> Result<T, McpError> {
        // The guard is a temporary of this statement, so it is dropped here —
        // before the emit, and before anything the emit wakes up comes asking
        // for the lock.
        let out = op(&self.lock())?;
        self.changed();
        Ok(out)
    }

    /// Tell the webview the project changed so it re-fetches and renders live.
    fn changed(&self) {
        self.notify("project-changed");
    }

    /// Emit a webview event, logging rather than failing when there is no window
    /// listening — a headless agent session is a perfectly good way to run this.
    fn notify(&self, event: &str) {
        if let Err(e) = self.app.emit(event, ()) {
            tracing::warn!(error = %e, event, "failed to emit webview event");
        }
    }
}

/// Lock the shared project outside `&self` (for closures moved onto the
/// blocking pool), attributing edits to the agent. Recovers from a poisoned
/// mutex (a panic while another op held the lock) rather than panicking here
/// too — a single failed op shouldn't brick the agent endpoint for the session.
fn lock_agent(project: &Mutex<Project>) -> MutexGuard<'_, Project> {
    note_agent_activity();
    let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
    guard.set_actor(EditSource::Agent);
    guard
}

// ---- agent presence --------------------------------------------------------

/// Unix seconds of the last thing an agent did — its `initialize` handshake, or
/// any tool call that reached the project — or 0 if nothing ever has.
///
/// The agent panel used to show a green "live" dot unconditionally, which said
/// the same thing whether or not anything was connected. Nothing here claims a
/// *socket* is open: a streamable-HTTP client holds no connection between
/// calls, so the only honest signal is when it last spoke. Every agent-side
/// project access goes through `lock_agent`, which makes that the one choke
/// point worth stamping.
static LAST_AGENT_ACTIVITY: AtomicI64 = AtomicI64::new(0);

fn note_agent_activity() {
    LAST_AGENT_ACTIVITY.store(unix_now(), Ordering::Relaxed);
}

/// Seconds since an agent last spoke, or `None` if none ever has.
pub fn agent_last_seen_secs() -> Option<i64> {
    let at = LAST_AGENT_ACTIVITY.load(Ordering::Relaxed);
    (at > 0).then(|| (unix_now() - at).max(0))
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Run a blocking (ffmpeg) job on the blocking thread pool and await it, so a
/// slow decode / analysis / render doesn't pin one of the shared tokio workers
/// serving the MCP endpoint (and the rest of the app) for its whole duration.
async fn blocking<T: Send + 'static>(job: impl FnOnce() -> Result<T, McpError> + Send + 'static) -> Result<T, McpError> {
    tauri::async_runtime::spawn_blocking(job)
        .await
        .map_err(|e| McpError::internal_error(e.to_string(), None))?
}

/// The tool router, built once for the life of the process.
///
/// `#[tool_handler]`'s default router expression is `Self::tool_router()`, and
/// the generated `call_tool`, `list_tools` and `get_tool` each evaluate it — so
/// every request rebuilds all ~85 routes: a schema-cache lookup, a boxed
/// handler and a map insert apiece, ~250 µs of release-build work per tool call
/// that produces the identical router every time. The routes are fixed at
/// compile time, so build them once and hand out a borrow.
fn router() -> &'static rmcp::handler::server::router::tool::ToolRouter<KerfMcp> {
    static ROUTER: OnceLock<rmcp::handler::server::router::tool::ToolRouter<KerfMcp>> = OnceLock::new();
    ROUTER.get_or_init(KerfMcp::tool_router)
}

/// How the server introduces itself to clients. `ServerInfo::default()` fills
/// `server_info` in from *rmcp's* own build env, so an untouched default has
/// every client listing this server as "rmcp".
fn server_identity() -> Implementation {
    Implementation::new("kerf", env!("CARGO_PKG_VERSION"))
}

#[tool_handler(router = router())]
impl ServerHandler for KerfMcp {
    fn get_info(&self) -> ServerInfo {
        // `initialize` is the one moment we know an agent is on the other end
        // of the socket, so it counts as being seen even before it calls a tool.
        note_agent_activity();
        let mut info = ServerInfo::default();
        info.server_info = server_identity();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Kerf MCP server. The user queues editing tasks in the desktop app; \
             call claim_next_task to take the oldest one (or list_tasks to see \
             the whole queue). To work a task, inspect loaded media with \
             list_assets / get_asset_metadata / get_timeline_state (import_asset \
             loads a file the user has not added yet), run \
             analyze_asset to populate silence / scene / transcript / loudness \
             (EBU R128 LUFS) / onset / tempo (BPM + beat grid) / speech-vs-music \
             metadata. \
             You can also SEE the footage: skim_asset returns a contact-sheet \
             image of a clip (survey it to find the good parts), get_frame shows \
             a single moment up close, and preview_timeline renders the cut you \
             have assembled at a given time so you can check it on screen. Look, \
             then look closer: skim_asset with `cell` opens one sheet cell at \
             full detail, and get_frame / preview_timeline take a `region` \
             (fractions of the frame) that is cropped out and enlarged — the way \
             to read a face, on-screen text, a caption against the safe area or \
             a mask edge, since a whole frame at the same width shows a fraction \
             of that detail. Prefer a region to a larger max_width. \
             Then assemble a non-destructive edit with the \
             cut/split/trim/add/reorder/move_clip/remove/ripple_delete tools \
             (move_clip frees a clip to any position or same-kind track; \
             ripple_delete closes the gap). Layer footage with add_track / \
             remove_track — e.g. add a video track and move_clip B-roll onto it \
             over the interview (later video tracks composite on top). Polish \
             with set_volume / set_fade (fade-in/out, e.g. to smooth hard cuts), \
             set_speed, set_transform (scale / position / rotation / opacity / \
             crop — picture-in-picture), set_color and set_transition (a fade \
             between scenes, a slide or push between the shots of a montage). \
             Go further: set_video_effects (blur / sharpen / \
             grayscale / invert / vignette / chroma_key — green-screen so a lower \
             track shows through), set_mask (cut a clip to a rectangle or ellipse so \
             a lower track shows through — with a duplicated, blurred copy above, \
             that is how a face or a number plate is blurred), \
             set_audio_effects (highpass / lowpass / EQ / \
             compressor / gate). Mix with set_track_volume — a music bed belongs \
             on its own track pulled well under the speech (~0.3), which is most \
             of what makes a cut sound finished — plus set_track_duck to dip it \
             further under dialogue and set_track_pan to place it. Animate a clip \
             with set_keyframes / \
             add_keyframe (scale / position / rotation / opacity over time — a Ken \
             Burns zoom, a moving picture-in-picture). 360 footage (Insta360 \
             .insv, equirect exports) is detected on import and clips cut from it \
             are reframed to an ordinary rectilinear shot automatically: aim the \
             virtual camera with set_reframe (yaw / pitch / roll / field of view) \
             and pan it with add_reframe_keyframe / set_reframe_keyframes, or drop \
             back to the raw sphere with clear_reframe. You can see the whole \
             sphere with skim_asset / get_frame and the framed result with \
             preview_timeline, so look first, then point the camera at whatever \
             the shot is actually about. Add titles, lower-thirds \
             and captions with add_overlay / update_overlay / set_overlay_keyframes \
             (drawn over the cut; list_fonts lists installed system fonts to pass \
             as update_overlay's font), or generate_captions to caption the whole \
             cut in one call. Its style=word_punch puts one large word on \
             screen at a time instead of a subtitle line — that is what social \
             captions look like, so prefer it for a vertical cut unless the user \
             asked for subtitles. Caption LAST, after the cutting is done: captions \
             are placed in timeline time, so a later trim or remove_silence moves \
             the words out from under them — re-run generate_captions after any \
             further edit and it replaces its own set, leaving typed titles \
             alone. export_srt writes a subtitle file. \
             When the cut is going somewhere vertical, set_delivery_format sets \
             the frame it is being made for and smart_crop then frames each shot \
             for it — reshaping 16:9 footage to 9:16 throws away most of the \
             width, and without this the middle is what survives, subject or \
             not. Look at the result with preview_timeline; the crop is an \
             ordinary transform the user can adjust. \
             Your task edits are STAGED, not applied: claiming a task opens a \
             proposal, and every edit you make goes into it instead of changing \
             the cut the user is looking at. Your own reads follow the proposal, \
             so get_timeline_state / preview_timeline / timeline_summary show \
             the cut you are building — check staged_diff to see exactly what you \
             are about to hand over, and discard_staged_edits to abandon it. When \
             finished call complete_task with a short summary (or fail_task on \
             error); the user then accepts the proposal, which applies it as one \
             revision, or dismisses it. Outside a task, stage_edits opens a \
             proposal the same way and apply_staged_edits commits one yourself. \
             Every applied edit is tracked: history lists the revisions, \
             revision_diff explains one of them, and undo / redo / revert_to roll \
             changes back (they work on the live cut, so apply or discard your \
             staged edits first). Before you report a cut finished, run \
             platform_check: it says whether the length and frame shape suit \
             where it is going, including the reach limits a platform enforces \
             silently (a Reel over 3 minutes uploads fine and then reaches only \
             existing followers). Call export to render, and export_cover to \
             write the thumbnail the platform shows before anyone presses play."
                .to_string(),
        );
        info
    }
}

// ---- server ----------------------------------------------------------------

/// The MCP bind address, honoring the `KERF_MCP_ADDR` override.
fn bind_addr() -> String {
    std::env::var("KERF_MCP_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string())
}

/// The full URL a client connects to (`http://<addr>/mcp`). The GUI shows this
/// so the user knows where to point their agent.
pub fn endpoint_url() -> String {
    format!("http://{}/mcp", bind_addr())
}

/// The `Host` headers this server will accept.
///
/// rmcp validates the inbound `Host` against an allow-list that defaults to
/// loopback, to blunt DNS-rebinding attacks against locally running servers.
/// `KERF_MCP_ADDR` exists to move the server *off* loopback, so the bare
/// default would make every such override reject its own clients. Extend the
/// list with whatever the override names: a concrete address is added as-is,
/// while a wildcard bind (`0.0.0.0` / `[::]`) cannot be enumerated at all —
/// the client's `Host` is whichever of this machine's addresses it reached us
/// on — so it yields an empty list, which is rmcp's documented "allow any".
fn allowed_hosts(addr: &str) -> Vec<String> {
    let defaults = ["localhost", "127.0.0.1", "::1"];
    let mut hosts: Vec<String> = defaults.iter().map(|h| h.to_string()).collect();
    let Some(host) = host_of(addr) else {
        return hosts;
    };
    if host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_unspecified()) {
        return Vec::new();
    }
    if !hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
        hosts.push(host);
    }
    hosts
}

/// The host part of a `host:port` bind address, tolerating a bracketed IPv6
/// literal and a missing port.
fn host_of(addr: &str) -> Option<String> {
    if let Ok(sock) = addr.parse::<std::net::SocketAddr>() {
        return Some(sock.ip().to_string());
    }
    let host = match addr.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => addr,
    };
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    (!host.is_empty()).then(|| host.to_string())
}

/// Serve the MCP tools over streamable HTTP at `/mcp`, sharing `project` with
/// the Tauri commands. Runs until the process exits.
pub async fn serve(project: Arc<Mutex<Project>>, app: AppHandle) -> anyhow::Result<()> {
    let addr = bind_addr();

    let service = StreamableHttpService::new(
        move || Ok(KerfMcp::new(project.clone(), app.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_allowed_hosts(allowed_hosts(&addr)),
    );
    let router = axum::Router::new().nest_service("/mcp", service);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "kerf MCP server listening on http://{addr}/mcp");
    axum::serve(listener, router).await?;
    Ok(())
}

// ---- helpers ---------------------------------------------------------------

fn parse_id(s: &str) -> Result<Uuid, McpError> {
    Uuid::parse_str(s).map_err(|e| McpError::invalid_params(format!("invalid uuid '{s}': {e}"), None))
}

fn parse_kind(s: &str) -> Result<StreamKind, McpError> {
    match s.to_lowercase().as_str() {
        "video" => Ok(StreamKind::Video),
        "audio" => Ok(StreamKind::Audio),
        other => Err(McpError::invalid_params(
            format!("invalid track kind '{other}'; expected \"video\" or \"audio\""),
            None,
        )),
    }
}

fn parse_transition(kind: Option<String>, duration: Option<f64>) -> Result<Option<Transition>, McpError> {
    match kind {
        None => Ok(None),
        Some(k) => {
            let kind = TransitionKind::parse(&k).ok_or_else(|| {
                McpError::invalid_params(
                    format!(
                        "invalid transition kind '{k}'; expected one of {}",
                        TransitionKind::wire_names()
                    ),
                    None,
                )
            })?;
            let duration =
                duration.ok_or_else(|| McpError::invalid_params("transition duration is required".to_string(), None))?;
            Ok(Some(Transition { kind, duration }))
        }
    }
}

/// Map a core error onto the MCP error the model should act on.
///
/// A stale id, an out-of-range value or an operation that doesn't fit the
/// current state is the *caller's* mistake, and saying so as `invalid_params`
/// is what tells the model to fix its arguments and retry. Reported as
/// `internal_error` — the old blanket mapping — a mistyped uuid reads as a
/// broken server, which an agent has no move against but giving up.
fn core_err(e: kerf_core::Error) -> McpError {
    use kerf_core::Error as E;
    match e {
        E::AssetNotFound(_)
        | E::ClipNotFound(_)
        | E::TrackNotFound(_)
        | E::OverlayNotFound(_)
        | E::RevisionNotFound(_)
        | E::TaskNotFound(_)
        | E::InvalidArgument(_)
        | E::NoStagedEdit
        | E::StagedEditPending
        | E::StagedEditStale => McpError::invalid_params(e.to_string(), None),
        // A database, io, ffmpeg or engine failure is ours, not the caller's.
        other => McpError::internal_error(other.to_string(), None),
    }
}

/// Wrap JPEG bytes as an MCP tool result the model can *see*: a caption text
/// block followed by an image content block (rmcp expects bare base64 + MIME,
/// not a `data:` URL).
fn image_result(caption: String, jpeg: Vec<u8>) -> CallToolResult {
    let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);
    CallToolResult::success(vec![ContentBlock::text(caption), ContentBlock::image(b64, "image/jpeg")])
}

/// The spans of a track that render nothing: the hole before the first clip and
/// every hole between clips. Clips are not held in start order, so this walks a
/// sorted copy of their spans, and it tolerates overlap by carrying the furthest
/// end reached rather than the previous clip's. A hole under a millisecond is
/// float noise from a retrim, not a gap.
fn track_gaps(track: &kerf_core::Track) -> Vec<Gap> {
    const EPSILON: f64 = 1e-3;
    let mut spans: Vec<(f64, f64)> = track.clips.iter().map(|c| (c.timeline_start, c.timeline_end())).collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut gaps = Vec::new();
    let mut cursor = 0.0f64;
    for (start, end) in spans {
        if start - cursor > EPSILON {
            gaps.push(Gap {
                start_secs: cursor,
                end_secs: start,
            });
        }
        cursor = cursor.max(end);
    }
    gaps
}

/// Format a seconds offset as `mm:ss.mmm` for frame / contact-sheet captions.
/// Rounds to milliseconds *before* splitting so a value just under a minute
/// carries into the minute (59.9999 → `01:00.000`, not `00:60.000`).
fn fmt_ts(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as i64;
    let minutes = ms / 60_000;
    let seconds = (ms % 60_000) as f64 / 1000.0;
    format!("{minutes:02}:{seconds:06.3}")
}

fn json<T: Serialize>(value: &T) -> Result<String, McpError> {
    serde_json::to_string_pretty(value).map_err(|e| McpError::internal_error(e.to_string(), None))
}

#[cfg(test)]
mod tests {
    use super::{allowed_hosts, core_err, fmt_ts, image_result, router, server_identity, track_gaps};

    #[test]
    fn fmt_ts_carries_at_minute_boundaries() {
        assert_eq!(fmt_ts(0.0), "00:00.000");
        assert_eq!(fmt_ts(12.5), "00:12.500");
        assert_eq!(fmt_ts(-3.0), "00:00.000");
        // Just under a minute must carry into minutes, not render ":60.000".
        assert_eq!(fmt_ts(59.9999), "01:00.000");
        assert_eq!(fmt_ts(119.9997), "02:00.000");
        assert_eq!(fmt_ts(59.9994), "00:59.999");
        assert_eq!(fmt_ts(125.25), "02:05.250");
    }

    /// Every tool the agent can call must reach it with a description and an
    /// object input schema. The router is built by the `tool_router` macro, so
    /// this is what catches an rmcp upgrade silently changing how the tool
    /// surface is generated.
    #[test]
    fn every_tool_has_a_description_and_object_schema() {
        let tools = router().list_all();
        assert!(tools.len() > 50, "expected the full tool surface, got {}", tools.len());

        for tool in &tools {
            let description = tool.description.as_deref().unwrap_or_default();
            assert!(!description.is_empty(), "tool `{}` has no description", tool.name);
            assert_eq!(
                tool.input_schema.get("type").and_then(|t| t.as_str()),
                Some("object"),
                "tool `{}` input schema is not an object",
                tool.name
            );
        }
    }

    /// `Option<T>` parameters must stay out of `required` — a great many tools
    /// document a field as "omit to …", and a schema generator that started
    /// requiring them would break that contract without failing to compile.
    #[test]
    fn optional_parameters_are_not_required() {
        let tools = router().list_all();
        let add_clip = tools
            .iter()
            .find(|t| t.name == "add_clip_to_timeline")
            .expect("add_clip_to_timeline is registered");

        let required: Vec<&str> = add_clip
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        // Documented as required by the tool.
        assert!(required.contains(&"asset_id"), "asset_id should be required: {required:?}");
        assert!(required.contains(&"source_in"), "source_in should be required: {required:?}");
        // Documented as "omit to auto-select" / "omit to append".
        assert!(!required.contains(&"track_id"), "track_id must stay optional: {required:?}");
        assert!(
            !required.contains(&"timeline_start"),
            "timeline_start must stay optional: {required:?}"
        );

        let properties = add_clip
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("input schema has properties");
        for field in ["asset_id", "track_id", "source_in", "source_out", "timeline_start"] {
            assert!(properties.contains_key(field), "`{field}` missing from schema");
        }
    }
    /// The default bind is loopback, which rmcp's own defaults already cover.
    #[test]
    fn loopback_binds_keep_the_default_allow_list() {
        for addr in ["127.0.0.1:7777", "localhost:7777", "[::1]:7777"] {
            let hosts = allowed_hosts(addr);
            assert!(hosts.iter().any(|h| h == "127.0.0.1"), "{addr} -> {hosts:?}");
            assert!(hosts.iter().any(|h| h == "localhost"), "{addr} -> {hosts:?}");
            assert!(hosts.iter().any(|h| h == "::1"), "{addr} -> {hosts:?}");
            // No duplicates from re-adding a host the defaults already list.
            assert_eq!(hosts.len(), 3, "{addr} -> {hosts:?}");
        }
    }

    /// `KERF_MCP_ADDR` pointed at a concrete non-loopback address has to allow
    /// that address, or the override rejects every client it just enabled.
    #[test]
    fn a_concrete_override_is_allowed() {
        let hosts = allowed_hosts("192.168.1.5:7777");
        assert!(hosts.iter().any(|h| h == "192.168.1.5"), "{hosts:?}");
        // The loopback defaults survive alongside it.
        assert!(hosts.iter().any(|h| h == "127.0.0.1"), "{hosts:?}");

        let named = allowed_hosts("kerf.local:7777");
        assert!(named.iter().any(|h| h == "kerf.local"), "{named:?}");
    }

    /// A wildcard bind can be reached on any of this machine's addresses, so
    /// there is no list to write: an empty list is rmcp's "allow any".
    #[test]
    fn a_wildcard_bind_disables_host_validation() {
        for addr in ["0.0.0.0:7777", "[::]:7777"] {
            assert!(allowed_hosts(addr).is_empty(), "{addr} should allow any host");
        }
    }

    /// A malformed or port-less override must not silently drop the loopback
    /// defaults and lock the user out of the default endpoint.
    #[test]
    fn odd_addresses_keep_loopback_reachable() {
        for addr in ["", ":7777", "127.0.0.1", "not a host:port"] {
            let hosts = allowed_hosts(addr);
            assert!(hosts.iter().any(|h| h == "127.0.0.1"), "{addr} must keep loopback: {hosts:?}");
        }
    }
    /// Clients list a server by its `serverInfo`, and rmcp's default fills that
    /// in from its own crate identity — so the app has to name itself.
    #[test]
    fn the_server_introduces_itself_as_kerf() {
        let info = server_identity();
        assert_eq!(info.name, "kerf");
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_ne!(info.name, "rmcp", "server_info must not be rmcp's own identity");
    }

    /// The visual tools hand the model a caption plus a raw-base64 image block.
    /// This asserts the shape that actually goes over the wire — rmcp wants
    /// bare base64 under `data` with a sibling `mimeType`, never a `data:` URL,
    /// and getting that wrong degrades silently into an image the model cannot
    /// see rather than into a build failure.
    #[test]
    fn image_results_serialize_as_caption_plus_bare_base64() {
        let jpeg = [0xFF, 0xD8, 0xFF, 0xD9];
        let value =
            serde_json::to_value(image_result("at 00:02.000".to_string(), jpeg.to_vec())).expect("CallToolResult serializes");

        let content = value["content"].as_array().expect("content is an array");
        assert_eq!(content.len(), 2, "expected caption + image: {content:?}");

        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "at 00:02.000");

        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["mimeType"], "image/jpeg");
        let data = content[1]["data"].as_str().expect("image data is a string");
        assert_eq!(data, "/9j/2Q==");
        assert!(!data.starts_with("data:"), "must be bare base64, not a data: URL");

        assert_eq!(value["isError"], false);
    }

    /// `timeline_summary` promises the agent "any per-track gaps", and a gap is
    /// a real defect in a cut — the picture goes black there. The head of the
    /// track counts: a cut that opens two seconds late opens on black.
    #[test]
    fn track_gaps_finds_holes_including_the_one_at_the_head() {
        use kerf_core::{Clip, StreamKind, Track};
        let asset = uuid::Uuid::new_v4();
        let mut track = Track::new(StreamKind::Video, "V1");
        // Deliberately out of start order — clips are not stored sorted.
        track.clips.push(Clip::new(asset, 0.0, 1.0, 6.0));
        track.clips.push(Clip::new(asset, 0.0, 2.0, 2.0));

        let gaps = track_gaps(&track);
        assert_eq!(gaps.len(), 2, "head gap + the hole between the clips");
        assert_eq!((gaps[0].start_secs, gaps[0].end_secs), (0.0, 2.0));
        assert_eq!((gaps[1].start_secs, gaps[1].end_secs), (4.0, 6.0));
    }

    /// A gapless track reports nothing, and float noise from a retrim is not a
    /// gap — otherwise every summary would be a wall of sub-millisecond holes.
    #[test]
    fn track_gaps_ignores_a_gapless_track_and_float_noise() {
        use kerf_core::{Clip, StreamKind, Track};
        let asset = uuid::Uuid::new_v4();
        let mut track = Track::new(StreamKind::Video, "V1");
        track.clips.push(Clip::new(asset, 0.0, 2.0, 0.0));
        track.clips.push(Clip::new(asset, 0.0, 2.0, 2.000_04));
        assert!(
            track_gaps(&track).is_empty(),
            "40 microseconds of drift is not a gap, got {:?}",
            track_gaps(&track).len()
        );

        // An overlap must not be read as a gap by the clip that follows it.
        let mut overlapping = Track::new(StreamKind::Video, "V1");
        overlapping.clips.push(Clip::new(asset, 0.0, 5.0, 0.0));
        overlapping.clips.push(Clip::new(asset, 0.0, 1.0, 1.0));
        assert!(track_gaps(&overlapping).is_empty());
    }

    /// A stale id or an out-of-range value is the caller's mistake, and the
    /// model can only act on that if it is told so: `invalid_params` means "fix
    /// the arguments", `internal_error` means "the server is broken".
    #[test]
    fn caller_mistakes_are_reported_as_invalid_params() {
        use rmcp::model::ErrorCode;
        let id = uuid::Uuid::new_v4();
        for e in [
            kerf_core::Error::ClipNotFound(id),
            kerf_core::Error::AssetNotFound(id),
            kerf_core::Error::TrackNotFound(id),
            kerf_core::Error::InvalidArgument("marker time must be >= 0".to_string()),
            kerf_core::Error::StagedEditStale,
        ] {
            let rendered = e.to_string();
            assert_eq!(core_err(e).code, ErrorCode::INVALID_PARAMS, "{rendered}");
        }
        // Ours, not theirs.
        assert_eq!(
            core_err(kerf_core::Error::Engine("ffmpeg exited 1".to_string())).code,
            ErrorCode::INTERNAL_ERROR
        );
    }

    /// The router is fixed at compile time but the `#[tool_handler]` default
    /// rebuilds it per request. This pins that it is built once — a regression
    /// here is invisible except as latency on every single tool call.
    #[test]
    fn the_tool_router_is_built_once() {
        assert!(std::ptr::eq(router(), router()));
    }

    /// `export` takes a `RequestContext` alongside its `Parameters` so it can
    /// report progress and honour cancellation. That second argument must stay
    /// out of the *input schema* — a context extractor leaking in would ask the
    /// model to invent a request context, and the tool would be uncallable.
    #[test]
    fn the_context_extractor_stays_out_of_the_export_schema() {
        let tools = router().list_all();
        let export = tools.iter().find(|t| t.name == "export").expect("export is registered");

        let properties = export
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("input schema has properties");
        assert!(properties.contains_key("output_path"), "{properties:?}");
        assert!(properties.contains_key("options"), "{properties:?}");
        assert_eq!(properties.len(), 2, "only the declared params belong here: {properties:?}");

        let required: Vec<&str> = export
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(required, ["output_path"], "options is optional");
    }

    /// An agent with no way to load media can only rearrange what it was handed.
    #[test]
    fn media_can_be_imported_over_mcp() {
        let tools = router().list_all();
        let import = tools
            .iter()
            .find(|t| t.name == "import_asset")
            .expect("import_asset is registered");
        let required: Vec<&str> = import
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(required, ["path"]);
    }
}
