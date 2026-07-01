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

use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine as _;
use kerf_core::{
    AudioEffect, EditSource, ExportOptions, Keyframe, Project, StreamKind, TextKeyframe, Transition, TransitionKind,
    VideoEffect,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::{session::local::LocalSessionManager, StreamableHttpService};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

/// Default localhost bind address for the MCP endpoint; override with `KERF_MCP_ADDR`.
const DEFAULT_ADDR: &str = "127.0.0.1:7777";

#[derive(Clone)]
pub struct KerfMcp {
    project: Arc<Mutex<Project>>,
    app: AppHandle,
}

// ---- tool parameter schemas ------------------------------------------------

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AssetIdParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
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
    #[schemars(description = "Uniform scale after fitting to the frame (1.0 = fit, < 1.0 = picture-in-picture); omit to leave unchanged")]
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
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TransitionParams {
    #[schemars(description = "UUID of the clip whose start blends with the clip before it on the same track")]
    clip_id: String,
    #[schemars(description = "Transition kind: \"crossfade\" or \"dip_to_black\". Omit to clear the transition")]
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
    #[schemars(
        description = "Position/opacity keyframes (replaces the overlay's animation). Each: \
                       {\"time\":seconds_from_overlay_start,\"pos_x\":0.5,\"pos_y\":0.85,\"opacity\":1.0}. Pass [] to clear."
    )]
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
    #[schemars(
        description = "Optional encode settings. Omit for the safe default (H.264 + AAC MP4). \
                       Key fields: container (mp4/mov/mkv/webm/gif/mp3/m4a/wav/flac); video_codec \
                       (libx264/libx265/libvpx-vp9/libsvtav1/prores_ks/gif, plus GPU encoders \
                       h264_nvenc/hevc_nvenc/av1_nvenc/h264_qsv/hevc_qsv/h264_videotoolbox/\
                       hevc_videotoolbox/h264_amf/hevc_amf — far faster, crf still applies); \
                       audio_codec (aac/libmp3lame/libopus/flac/alac/pcm_s16le/pcm_s24le); \
                       rate_control (crf/bitrate/two_pass/lossless); crf; video_bitrate (\"8M\"); \
                       preset; hwaccel (\"auto\"/\"cuda\"/\"vaapi\"/\"videotoolbox\"/\"qsv\" — GPU \
                       decode, composes with any encoder); resolution ([w,h]); fps; audio_bitrate \
                       (\"192k\"); include_audio; faststart; range ({start,end} timeline seconds — \
                       render only that span)."
    )]
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
    #[schemars(description = "Number of peak-magnitude buckets to return")]
    buckets: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FrameParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(description = "Time in the source asset to decode (seconds)")]
    time_secs: f64,
    #[schemars(description = "Maximum output width in pixels (default 640)")]
    max_width: Option<u32>,
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
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TimelineFrameParams {
    #[schemars(description = "Timeline position to render (seconds)")]
    time_secs: f64,
    #[schemars(description = "Maximum output width in pixels (default 640)")]
    max_width: Option<u32>,
}

#[derive(Serialize)]
struct AssetMetadata {
    asset: kerf_core::Asset,
    analysis: Option<kerf_core::AssetAnalysis>,
}

#[derive(Serialize)]
struct TrackSummary {
    id: String,
    name: String,
    kind: String,
    clip_count: usize,
    duration_secs: f64,
}

#[derive(Serialize)]
struct TimelineSummary {
    total_duration_secs: f64,
    track_count: usize,
    total_clip_count: usize,
    tracks: Vec<TrackSummary>,
}

// ---- tools -----------------------------------------------------------------

#[tool_router]
impl KerfMcp {
    #[tool(description = "List the system font family names available for text overlays (pass one as `font` to add_overlay / update_overlay)")]
    fn list_fonts(&self) -> Result<String, McpError> {
        json(&kerf_core::list_system_fonts())
    }

    #[tool(description = "List all media assets in the project")]
    fn list_assets(&self) -> Result<String, McpError> {
        let project = self.lock();
        json(&project.list_assets().map_err(core_err)?)
    }

    #[tool(description = "Get an asset's probed metadata and cached analysis (silence, scenes, transcript, EBU R128 loudness, onset times, tempo/beat grid, speech/music class)")]
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
        json(&project.timeline().map_err(core_err)?)
    }

    #[tool(description = "Analyze an asset (silence + scene detection, EBU R128 loudness, onset/transient detection, tempo/beat estimation, speech-vs-music classification, and transcription when configured) and cache the result")]
    fn analyze_asset(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        // Probe under the lock, run the heavy ffmpeg analysis with the lock
        // released, then re-lock only to cache it — so analysis doesn't freeze
        // the GUI or stall other tools for its whole (multi-second) duration.
        let asset = self.lock().require_asset(id).map_err(core_err)?;
        let analysis = kerf_core::analyze_asset_media(&asset).map_err(core_err)?;
        self.lock().set_analysis(&analysis).map_err(core_err)?;
        self.changed();
        json(&analysis)
    }

    #[tool(description = "Cut [start, end) of an asset and append it to the matching track")]
    fn cut_clip(&self, Parameters(p): Parameters<CutClipParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let out = project.cut_clip(id, p.start, p.end).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Add a clip referencing a source range of an asset to the timeline")]
    fn add_clip_to_timeline(&self, Parameters(p): Parameters<AddClipParams>) -> Result<String, McpError> {
        let asset_id = parse_id(&p.asset_id)?;
        let track_id = p.track_id.as_deref().map(parse_id).transpose()?;
        let project = self.lock();
        let out = project
            .add_clip_to_timeline(asset_id, track_id, p.source_in, p.source_out, p.timeline_start)
            .map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Split a timeline clip at a timeline time into two adjacent clips")]
    fn split_at(&self, Parameters(p): Parameters<SplitParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let (left, right) = project.split_at(clip_id, p.at).map_err(core_err)?;
        self.changed();
        json(&serde_json::json!({ "left": left, "right": right }))
    }

    #[tool(description = "Trim a clip's source in/out points (timeline position preserved unless timeline_start is passed)")]
    fn trim(&self, Parameters(p): Parameters<TrimParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let out = project.trim(clip_id, p.source_in, p.source_out, p.timeline_start).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Move a clip to a new index within its track (re-flows the track gaplessly)")]
    fn reorder(&self, Parameters(p): Parameters<ReorderParams>) -> Result<String, McpError> {
        let track_id = parse_id(&p.track_id)?;
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        project.reorder(track_id, clip_id, p.new_index).map_err(core_err)?;
        self.changed();
        Ok("ok".to_string())
    }

    #[tool(description = "Move a clip to a new timeline position (free positioning, gaps allowed), optionally onto another same-kind track; rejects overlaps")]
    fn move_clip(&self, Parameters(p): Parameters<MoveClipParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let track_id = p.track_id.as_deref().map(parse_id).transpose()?;
        let project = self.lock();
        let out = project.move_clip(clip_id, p.timeline_start, track_id).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Remove a clip and close the gap: later clips on the same track shift left by its duration (ripple delete)")]
    fn ripple_delete(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        project.ripple_delete(clip_id).map_err(core_err)?;
        self.changed();
        Ok("ok".to_string())
    }

    #[tool(description = "Add a new empty track (\"video\" or \"audio\"), e.g. a B-roll lane above the interview; later video tracks composite on top at export")]
    fn add_track(&self, Parameters(p): Parameters<AddTrackParams>) -> Result<String, McpError> {
        let kind = parse_kind(&p.kind)?;
        let project = self.lock();
        let track = project.add_track(kind, p.name).map_err(core_err)?;
        self.changed();
        json(&track)
    }

    #[tool(description = "Remove a track and all of its clips (refuses to remove the last track)")]
    fn remove_track(&self, Parameters(p): Parameters<TrackIdParams>) -> Result<String, McpError> {
        let track_id = parse_id(&p.track_id)?;
        let project = self.lock();
        project.remove_track(track_id).map_err(core_err)?;
        self.changed();
        Ok("ok".to_string())
    }

    #[tool(description = "Remove a clip from the timeline")]
    fn remove(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        project.remove(clip_id).map_err(core_err)?;
        self.changed();
        Ok("ok".to_string())
    }

    #[tool(description = "Set the linear volume gain of a clip")]
    fn set_volume(&self, Parameters(p): Parameters<VolumeParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let out = project.set_volume(clip_id, p.volume).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Set a clip's fade-in / fade-out duration in seconds (omit a field to leave it unchanged, 0 to clear)")]
    fn set_fade(&self, Parameters(p): Parameters<FadeParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let out = project.set_fade(clip_id, p.fade_in, p.fade_out).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(
        description = "Set a clip's playback speed (1.0 = unchanged, 2.0 = 2x faster, 0.5 = half, negative = reverse); this retimes the clip and changes its timeline duration"
    )]
    fn set_speed(&self, Parameters(p): Parameters<SpeedParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let out = project.set_speed(clip_id, p.speed).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(
        description = "Set a clip's geometric transform — scale / position (pos_x, pos_y as fractions of the frame) / rotation / opacity / per-edge crop. Use a sub-1.0 scale with a position for picture-in-picture. Omit a field to leave it unchanged."
    )]
    fn set_transform(&self, Parameters(p): Parameters<TransformParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
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
        self.changed();
        json(&out)
    }

    #[tool(
        description = "Set a clip's color correction — brightness / contrast / saturation / gamma. Omit a field to leave it unchanged."
    )]
    fn set_color(&self, Parameters(p): Parameters<ColorParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let out = project
            .set_color(clip_id, p.brightness, p.contrast, p.saturation, p.gamma)
            .map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(
        description = "Set or clear the transition blending a clip's start with the clip before it on the same track. kind is \"crossfade\" or \"dip_to_black\" with a duration in seconds; omit kind to clear."
    )]
    fn set_transition(&self, Parameters(p): Parameters<TransitionParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let transition = parse_transition(p.kind, p.duration)?;
        let project = self.lock();
        let out = project.set_transition(clip_id, transition).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(
        description = "Replace a clip's video effect chain (applied in order at export): blur, sharpen, grayscale, invert, vignette, or chroma_key (key a color to transparency so footage on a lower track shows through). Pass an empty list to clear."
    )]
    fn set_video_effects(&self, Parameters(p): Parameters<VideoEffectsParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let out = project.set_video_effects(clip_id, p.effects).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(
        description = "Replace a clip's audio effect chain (applied in order at export): highpass, lowpass, equalizer (parametric band), compressor (dynamics) or gate (noise gate). Pass an empty list to clear."
    )]
    fn set_audio_effects(&self, Parameters(p): Parameters<AudioEffectsParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let out = project.set_audio_effects(clip_id, p.effects).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(
        description = "Replace a clip's transform keyframes to animate scale / position / rotation / opacity over time. Two or more keyframes animate the clip (e.g. a Ken Burns zoom, a moving picture-in-picture). Pass an empty list to clear the animation."
    )]
    fn set_keyframes(&self, Parameters(p): Parameters<KeyframesParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let out = project.set_keyframes(clip_id, p.keyframes).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(
        description = "Add (or replace) one transform keyframe at a time offset from the clip's start; unspecified channels capture the clip's current pose there. Use two calls to animate between two poses."
    )]
    fn add_keyframe(&self, Parameters(p): Parameters<AddKeyframeParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let out = project
            .add_keyframe(clip_id, p.time, p.scale, p.pos_x, p.pos_y, p.rotation, p.opacity)
            .map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Remove all transform keyframes from a clip (back to its static transform)")]
    fn clear_keyframes(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_id(&p.clip_id)?;
        let project = self.lock();
        let out = project.clear_keyframes(clip_id).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(
        description = "Add a text overlay (title / lower-third / caption / watermark) drawn over the composited picture between start and end (timeline seconds). Returns the overlay; style or animate it with update_overlay / set_overlay_keyframes."
    )]
    fn add_overlay(&self, Parameters(p): Parameters<AddOverlayParams>) -> Result<String, McpError> {
        let project = self.lock();
        let out = project.add_overlay(p.text, p.start, p.end).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(
        description = "Update a text overlay's text, timing, position (pos_x / pos_y as 0–1 frame fractions), size (font height fraction), color, box background (bg, empty string clears), font (system font family from list_fonts, empty string clears) or bold. Omit a field to leave it unchanged."
    )]
    fn update_overlay(&self, Parameters(p): Parameters<UpdateOverlayParams>) -> Result<String, McpError> {
        let overlay_id = parse_id(&p.overlay_id)?;
        let project = self.lock();
        let out = project
            .update_overlay(
                overlay_id, p.text, p.start, p.end, p.pos_x, p.pos_y, p.size, p.color, p.bg, p.font, p.bold,
            )
            .map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Remove a text overlay")]
    fn remove_overlay(&self, Parameters(p): Parameters<OverlayIdParams>) -> Result<String, McpError> {
        let overlay_id = parse_id(&p.overlay_id)?;
        let project = self.lock();
        project.remove_overlay(overlay_id).map_err(core_err)?;
        self.changed();
        Ok("ok".to_string())
    }

    #[tool(description = "Set or clear (empty list) a text overlay's position/opacity keyframes, to animate it over its lifetime (e.g. a title that slides in and fades out)")]
    fn set_overlay_keyframes(&self, Parameters(p): Parameters<OverlayKeyframesParams>) -> Result<String, McpError> {
        let overlay_id = parse_id(&p.overlay_id)?;
        let project = self.lock();
        let out = project.set_overlay_keyframes(overlay_id, p.keyframes).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(
        description = "Generate caption overlays from an asset's cached transcript (run analyze_asset first), one per segment, low-center with a translucent box. Captions use the transcript's timestamps, so they align when the asset sits at the start of the timeline at normal speed. Returns the overlays created."
    )]
    fn captions_from_transcript(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let out = project.captions_from_transcript(id).map_err(core_err)?;
        self.changed();
        json(&out)
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
        let project = self.lock();
        let out = project.remove_silence(id).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Append the full audio of an asset to the first audio track")]
    fn extract_audio(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let out = project.extract_audio(id).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Stitch the full length of several assets together in order")]
    fn concatenate(&self, Parameters(p): Parameters<ConcatParams>) -> Result<String, McpError> {
        let ids = p.asset_ids.iter().map(|s| parse_id(s)).collect::<Result<Vec<Uuid>, _>>()?;
        let project = self.lock();
        let out = project.concatenate(&ids).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "List the timeline edit history (revisions, newest changes have higher seq; the current one is marked)")]
    fn history(&self) -> Result<String, McpError> {
        let project = self.lock();
        json(&project.history().map_err(core_err)?)
    }

    #[tool(description = "Undo the last timeline edit, returning the restored timeline")]
    fn undo(&self) -> Result<String, McpError> {
        let project = self.lock();
        let out = project.undo().map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Redo the next timeline edit, returning the restored timeline")]
    fn redo(&self) -> Result<String, McpError> {
        let project = self.lock();
        let out = project.redo().map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(description = "Revert the timeline to a specific revision seq (see history), returning the restored timeline")]
    fn revert_to(&self, Parameters(p): Parameters<RevertParams>) -> Result<String, McpError> {
        let project = self.lock();
        let out = project.revert_to(p.seq).map_err(core_err)?;
        self.changed();
        json(&out)
    }

    #[tool(
        description = "Render the timeline to a file with full ffmpeg encode control (container, video/audio codec, \
                       rate control, resolution, fps, bitrate, faststart, gif, audio-only …). Omit `options` for the \
                       safe H.264/AAC MP4 default."
    )]
    fn export(&self, Parameters(p): Parameters<ExportParams>) -> Result<String, McpError> {
        let opts = p.options.unwrap_or_default();
        // Snapshot the timeline + assets under the lock, then render with the
        // lock released so a long export doesn't freeze the GUI (or block other
        // agent tools) for its whole duration.
        let (timeline, assets) = {
            let project = self.lock();
            (project.timeline().map_err(core_err)?, project.list_assets().map_err(core_err)?)
        };
        kerf_core::render_with(&timeline, &assets, std::path::Path::new(&p.output_path), &opts).map_err(core_err)?;
        json(&serde_json::json!({ "output": p.output_path }))
    }

    // ---- agent task queue --------------------------------------------------

    #[tool(description = "List the agent task queue with each task's status (queued/working/ready/done/failed)")]
    fn list_tasks(&self) -> Result<String, McpError> {
        let project = self.lock();
        json(&project.list_tasks().map_err(core_err)?)
    }

    #[tool(description = "Enqueue a new task (status: queued) for an agent to claim")]
    fn add_task(&self, Parameters(p): Parameters<AddTaskParams>) -> Result<String, McpError> {
        let project = self.lock();
        let task = project.add_task(&p.prompt).map_err(core_err)?;
        self.changed();
        json(&task)
    }

    #[tool(description = "Claim the oldest queued task (marks it working) and return it; returns null when the queue is empty")]
    fn claim_next_task(&self) -> Result<String, McpError> {
        let project = self.lock();
        let task = project.claim_next_task().map_err(core_err)?;
        self.changed();
        json(&task)
    }

    #[tool(description = "Mark a claimed task ready for the user to review, with a summary of the edits made")]
    fn complete_task(&self, Parameters(p): Parameters<CompleteTaskParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        let project = self.lock();
        let task = project.complete_task(id, p.result).map_err(core_err)?;
        self.changed();
        json(&task)
    }

    #[tool(description = "Mark a task failed with an error message")]
    fn fail_task(&self, Parameters(p): Parameters<FailTaskParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        let project = self.lock();
        let task = project.fail_task(id, &p.error).map_err(core_err)?;
        self.changed();
        json(&task)
    }

    #[tool(description = "Mark a task done (user accepted the staged edit), returning the updated task")]
    fn resolve_task(&self, Parameters(p): Parameters<TaskIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        let project = self.lock();
        let task = project.resolve_task(id).map_err(core_err)?;
        self.changed();
        json(&task)
    }

    #[tool(description = "Remove a task from the queue permanently, returning the updated task list")]
    fn remove_task(&self, Parameters(p): Parameters<TaskIdParams>) -> Result<String, McpError> {
        let id = parse_id(&p.task_id)?;
        let project = self.lock();
        project.remove_task(id).map_err(core_err)?;
        self.changed();
        json(&project.list_tasks().map_err(core_err)?)
    }

    #[tool(description = "Get peak-magnitude waveform data (0.0–1.0) for an asset's first audio stream")]
    fn get_waveform(&self, Parameters(p): Parameters<WaveformParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let buckets = project.waveform(id, p.buckets).map_err(core_err)?;
        json(&buckets)
    }

    #[tool(
        description = "Get an RMS energy envelope (0.0–1.0 per bucket) for an asset's first audio stream — a perceptual loudness-over-time curve. Unlike the peak waveform, it tracks how loud each slice feels, so use it to find quiet/loud passages and match cut pacing to musical energy."
    )]
    fn get_energy(&self, Parameters(p): Parameters<WaveformParams>) -> Result<String, McpError> {
        let id = parse_id(&p.asset_id)?;
        let project = self.lock();
        let energy = project.energy(id, p.buckets).map_err(core_err)?;
        json(&energy)
    }

    #[tool(
        description = "Decode a single frame from an asset at a source time and return it as a low-res image the model can actually see. Use to drill into a specific moment (e.g. one cell flagged by skim_asset) before cutting."
    )]
    fn get_frame(&self, Parameters(p): Parameters<FrameParams>) -> Result<CallToolResult, McpError> {
        let id = parse_id(&p.asset_id)?;
        let jpeg = self
            .lock()
            .frame_jpeg(id, p.time_secs, p.max_width.unwrap_or(640), 4)
            .map_err(core_err)?;
        Ok(image_result(format!("asset {} @ {}", p.asset_id, fmt_ts(p.time_secs.max(0.0))), jpeg))
    }

    #[tool(
        description = "Skim an asset: sample frames evenly across a time range (default the whole asset) into one contact-sheet image, plus a text index of which source timestamp each grid cell shows. The cheap way to survey footage and find the good parts; then call get_frame to inspect a promising moment, and add_clip_to_timeline / cut_clip to use it."
    )]
    fn skim_asset(&self, Parameters(p): Parameters<SkimParams>) -> Result<CallToolResult, McpError> {
        let id = parse_id(&p.asset_id)?;
        let columns = p.columns.unwrap_or(4).clamp(1, 8);
        let rows = p.rows.unwrap_or(4).clamp(1, 8);
        let cell_width = p.cell_width.unwrap_or(240).clamp(80, 640);
        let (jpeg, times) = self
            .lock()
            .skim_asset(id, p.start, p.end, columns, rows, cell_width, 5)
            .map_err(core_err)?;
        let index = times
            .iter()
            .enumerate()
            .map(|(i, t)| format!("  cell {}: {}", i + 1, fmt_ts(*t)))
            .collect::<Vec<_>>()
            .join("\n");
        let caption = format!("contact sheet {columns}x{rows} (row-major), cell -> source time:\n{index}");
        Ok(image_result(caption, jpeg))
    }

    #[tool(
        description = "Render the assembled timeline at a timeline time into one composite image the model can see — the actual cut on screen at that moment (footage layered in track order, picture-in-picture placement, crop, color; gaps render black). Use to verify an edit you just made. Mid-transition blends (crossfade/dip-to-black) are approximated."
    )]
    fn preview_timeline(&self, Parameters(p): Parameters<TimelineFrameParams>) -> Result<CallToolResult, McpError> {
        let jpeg = self
            .lock()
            .timeline_frame(p.time_secs, p.max_width.unwrap_or(640), 4)
            .map_err(core_err)?;
        Ok(image_result(format!("timeline composite @ {}", fmt_ts(p.time_secs.max(0.0))), jpeg))
    }

    #[tool(description = "Summarise the timeline: total duration, track count, clips per track, and any per-track gaps")]
    fn timeline_summary(&self) -> Result<String, McpError> {
        let project = self.lock();
        let timeline = project.timeline().map_err(core_err)?;
        let tracks: Vec<TrackSummary> = timeline
            .tracks
            .iter()
            .map(|t| TrackSummary {
                id: t.id.to_string(),
                name: t.name.clone(),
                kind: format!("{:?}", t.kind).to_lowercase(),
                clip_count: t.clips.len(),
                duration_secs: t.end(),
            })
            .collect();
        let total_clip_count = tracks.iter().map(|t| t.clip_count).sum();
        let summary = TimelineSummary {
            total_duration_secs: timeline.duration(),
            track_count: tracks.len(),
            total_clip_count,
            tracks,
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
        // Recover from a poisoned mutex (a panic while another op held the lock)
        // rather than panicking here too — a single failed op shouldn't brick the
        // agent endpoint for the rest of the session.
        let mut guard = self.project.lock().unwrap_or_else(|e| e.into_inner());
        guard.set_actor(EditSource::Agent);
        guard
    }

    /// Tell the webview the project changed so it re-fetches and renders live.
    fn changed(&self) {
        if let Err(e) = self.app.emit("project-changed", ()) {
            tracing::warn!(error = %e, "failed to emit project-changed");
        }
    }
}

#[tool_handler]
impl ServerHandler for KerfMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Kerf MCP server. The user queues editing tasks in the desktop app; \
             call claim_next_task to take the oldest one (or list_tasks to see \
             the whole queue). To work a task, inspect loaded media with \
             list_assets / get_asset_metadata / get_timeline_state, run \
             analyze_asset to populate silence / scene / transcript / loudness \
             (EBU R128 LUFS) / onset / tempo (BPM + beat grid) / speech-vs-music \
             metadata. \
             You can also SEE the footage: skim_asset returns a contact-sheet \
             image of a clip (survey it to find the good parts), get_frame shows \
             a single moment up close, and preview_timeline renders the cut you \
             have assembled at a given time so you can check it on screen. \
             Then assemble a non-destructive edit with the \
             cut/split/trim/add/reorder/move_clip/remove/ripple_delete tools \
             (move_clip frees a clip to any position or same-kind track; \
             ripple_delete closes the gap). Layer footage with add_track / \
             remove_track — e.g. add a video track and move_clip B-roll onto it \
             over the interview (later video tracks composite on top). Polish \
             with set_volume / set_fade (fade-in/out, e.g. to smooth hard cuts), \
             set_speed, set_transform (scale / position / rotation / opacity / \
             crop — picture-in-picture), set_color and set_transition (crossfade \
             / dip-to-black). Go further: set_video_effects (blur / sharpen / \
             grayscale / invert / vignette / chroma_key — green-screen so a lower \
             track shows through), set_audio_effects (highpass / lowpass / EQ / \
             compressor / gate), and animate a clip with set_keyframes / \
             add_keyframe (scale / position / rotation / opacity over time — a Ken \
             Burns zoom, a moving picture-in-picture). Add titles, lower-thirds \
             and captions with add_overlay / update_overlay / set_overlay_keyframes \
             (drawn over the cut; list_fonts lists installed system fonts to pass \
             as update_overlay's font), or captions_from_transcript to caption an \
             analyzed asset in one call; export_srt writes a subtitle file. \
             Every edit is tracked: use \
             history to list revisions and undo / redo / revert_to to roll \
             changes back. When finished call complete_task with a short summary \
             (or fail_task on error); the user reviews and applies the staged \
             edit. Call export to render."
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

/// Serve the MCP tools over streamable HTTP at `/mcp`, sharing `project` with
/// the Tauri commands. Runs until the process exits.
pub async fn serve(project: Arc<Mutex<Project>>, app: AppHandle) -> anyhow::Result<()> {
    let addr = bind_addr();

    let service = StreamableHttpService::new(
        move || Ok(KerfMcp::new(project.clone(), app.clone())),
        LocalSessionManager::default().into(),
        Default::default(),
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
                    format!("invalid transition kind '{k}'; expected \"crossfade\" or \"dip_to_black\""),
                    None,
                )
            })?;
            let duration = duration
                .ok_or_else(|| McpError::invalid_params("transition duration is required".to_string(), None))?;
            Ok(Some(Transition { kind, duration }))
        }
    }
}

fn core_err(e: kerf_core::Error) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

/// Wrap JPEG bytes as an MCP tool result the model can *see*: a caption text
/// block followed by an image content block (rmcp expects bare base64 + MIME,
/// not a `data:` URL).
fn image_result(caption: String, jpeg: Vec<u8>) -> CallToolResult {
    let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);
    CallToolResult::success(vec![Content::text(caption), Content::image(b64, "image/jpeg")])
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
    use super::fmt_ts;

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
}
