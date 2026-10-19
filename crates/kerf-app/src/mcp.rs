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
    AudioEffect, CaptionFormat, CaptionImportRequest, CaptionOptions, CaptionStyle, CaptionTimeBase, ClipCut, ClipLut, ClipMove,
    CurvePoint, Curves, Delivery, EditSource, ExportOptions, Fit, Keyframe, Mask, MaskShape, Project, Projection, Property,
    PropertyKey, PropertyTrack, ReframeKeyframe, Region, SplitSide, StreamKind, SvgCheck, SvgDoc, SvgLayout, SvgOverlay,
    SvgOverlayPatch, SvgOverlayResult, SvgPreset, SvgPresetKind, SvgPresetParams, SvgSource, TextKeyframe, TimeRange, Timeline,
    Transition, TransitionKind, VideoEffect, Wheel, Wheels,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

mod args;
mod hints;
mod jobs;
mod project_tools;
mod reads;
mod server;
mod templates;

use jobs::Run;
pub(crate) use server::bind_problem;
pub use server::{
    check_addr, check_token, connection_info, endpoint_url, generate_token, server_error, settings_changed, start,
    ConnectionInfo, DEFAULT_ADDR,
};

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
/// The widest cell of an SVG contact sheet (a sheet holds up to 36 of them).
const MAX_SVG_CELL_WIDTH: u32 = 960;
/// An SVG source longer than this is left out of `get_timeline_state` unless asked for.
const SVG_INLINE_LIMIT: usize = 4 * 1024;

#[derive(Clone)]
pub struct KerfMcp {
    project: Arc<Mutex<Project>>,
    app: AppHandle,
    /// The rmcp session this server instance belongs to (the `Mcp-Session-Id` the client sends).
    /// rmcp makes one instance per session, so this is the agent's identity: it owns the
    /// proposal its edits open, and no other session may touch it. `None` for a client that
    /// keeps no session.
    session: Arc<Mutex<Option<String>>>,
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
struct ApplySubsetParams {
    #[schemars(
        description = "The `id`s of the staged_diff entries to apply (for example `clip_added:<clip id>`); the rest stay in the proposal"
    )]
    entry_ids: Vec<String>,
    #[schemars(description = "Apply even though the user has edited the timeline since these edits were staged. Default false.")]
    force: Option<bool>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct PageParams {
    #[schemars(description = "Skip this many of the newest entries (default 0), to page back through older ones")]
    offset: Option<usize>,
    #[schemars(description = "Most entries to return (default 50, at most 500)")]
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RevisionDiffParams {
    #[schemars(description = "Revision seq to explain (see history); the diff is from the revision before it")]
    seq: i64,
    #[schemars(description = "Compare against this revision seq instead of `seq - 1`")]
    from: Option<i64>,
    #[schemars(description = "Skip this many changes (default 0), to page on; `entries.next_offset` is the next page")]
    offset: Option<usize>,
    #[schemars(description = "Most changes to return (default 100, at most 500)")]
    limit: Option<usize>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct DiffPageParams {
    #[schemars(description = "Skip this many changes (default 0), to page on; `diff.entries.next_offset` is the next page")]
    offset: Option<usize>,
    #[schemars(description = "Most changes to return (default 100, at most 500)")]
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ImportParams {
    #[schemars(description = "Absolute path to the media file to import")]
    path: String,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result, stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AssetIdParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AnalyzeParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(
        description = "Which analyses to run: any of `silence`, `scenes`, `loudness`, `rhythm` (onsets, tempo, \
                       speech/music class), `transcript`, or `all`. Only these run, and their results are merged \
                       into what is already cached — the other kinds are left as they were. Omit it for the \
                       analyses the user has left switched on in Settings › Analysis (so an agent does not fetch \
                       a speech model for someone who turned transcription off); name `transcript` yourself to \
                       transcribe anyway. Run only what you need: `silence` before remove_silence, `rhythm` \
                       before snap_to_beats, `transcript` before generate_captions."
    )]
    steps: Option<Vec<String>>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result, stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct ProxyStatusParams {
    #[schemars(description = "UUID of the asset; omit for every asset in the project")]
    asset_id: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct VoiceoverParams {
    #[schemars(
        description = "The script to read aloud, in English. Sentences are read one at a time; a blank line is a longer pause (a paragraph break)."
    )]
    text: String,
    #[schemars(
        description = "Kokoro voice id (default af_heart). American: af_heart, af_bella, af_nicole, af_sarah, af_sky, af_nova, am_michael, am_fenrir, am_puck, am_adam, am_eric, am_liam, am_onyx; British: bf_emma, bf_isabella, bf_alice, bf_lily, bm_george, bm_fable, bm_lewis, bm_daniel. voiceover_status lists them all."
    )]
    voice: Option<String>,
    #[schemars(description = "Speaking rate, 0.5–2.0 (default 1.0)")]
    speed: Option<f64>,
    #[schemars(description = "Audio track to place it on; omitted, it goes on the `VO` track (created on first use)")]
    track_id: Option<String>,
    #[schemars(
        description = "Timeline time to start at, in seconds; omitted, it is appended after what the track already holds"
    )]
    timeline_start: Option<f64>,
    #[schemars(
        description = "Caption the cut afterwards in this style (`lines` or `word_punch`), the same as generate_captions. Omit to leave captions alone."
    )]
    caption_style: Option<CaptionStyle>,
    #[schemars(
        description = "Ripple override for placing the narration. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, narration placed over footage already on the track pushes that footage and everything after it later by its length; appending, or filling free space, moves nothing either way. false places it without moving anything. Tracks ripple independently. Ripple is an attempt, not a guarantee: it is skipped on a locked track, and a lane it would leave overlapping (or before 0) is kept as the edit made it — check get_timeline_state afterwards rather than assuming the later clips moved."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "With caption_style: most words on one caption line (lines: 4, word_punch: 1); see generate_captions"
    )]
    caption_max_words: Option<usize>,
    #[schemars(description = "With caption_style: most characters on one caption line (default 28)")]
    caption_max_chars: Option<usize>,
    #[schemars(
        description = "With caption_style: vertical position as a fraction of frame height, 0 = top (lines: 0.88, word_punch: 0.72)"
    )]
    caption_pos_y: Option<f64>,
    #[schemars(description = "With caption_style: font height as a fraction of frame height (lines: 0.05, word_punch: 0.11)")]
    caption_size: Option<f64>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result, stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ComposeAlignParams {
    #[schemars(
        description = "What to fit the music to: `cuts` (the boundaries between the clips of a video track; the picture's first start and last end are not cuts) or `markers` (every timeline marker). Read from the cut you are proposing, staged edits included."
    )]
    to: kerf_core::AlignTo,
    #[schemars(description = "With `cuts`: the video track whose cuts to use (default the first video track)")]
    track_id: Option<String>,
    #[schemars(
        description = "The most the tempo may be nudged, as a fraction of it: 0.06 (the default) is up to 6 % faster or slower, 0 keeps the tempo and only adds the hits, 0.25 at most"
    )]
    max_tempo_change: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ComposeMusicParams {
    #[schemars(
        description = "The piece, as a score: {bpm, key, style, sections, hits?, seed?, instruments?}. See the tool description for what each field takes."
    )]
    score: kerf_core::MusicScore,
    #[schemars(
        description = "Audio track to put the music on; omitted, it goes on the `M` track (created on first use). Ignored with replace_clip_id."
    )]
    track_id: Option<String>,
    #[schemars(
        description = "Timeline time to start at, in seconds; omitted, it is appended after what the track already holds. With `align` this is the start the music is fitted from, so pass it when the music should start after 0. Ignored with replace_clip_id (the clip keeps its place)."
    )]
    at: Option<f64>,
    #[schemars(
        description = "Fit the score to the picture before rendering: nudge the tempo so cuts land on beats and add impact / dropout / riser hits at the cuts. Omit to render the score as written."
    )]
    align: Option<ComposeAlignParams>,
    #[schemars(
        description = "UUID of a music clip made by compose_music: replace its music with the new render instead of adding a clip. This is how a score is edited and composed again. The WHOLE bed goes: every clip of that clip's asset on its track (what fit_music made of one piece, in segments) becomes ONE clip of the new render, at the earliest of their starts, with the gain, fade-in and effects of the first and the fade-out of the last; its length becomes the render's. Refused: a clip that is not composed music, a locked track, music linked to a picture (unlink_clips first). The answer's `replaced` lists the clip ids that were replaced."
    )]
    replace_clip_id: Option<String>,
    #[schemars(
        description = "Ripple override for placing the music, and for the check made before it renders. Omitted, the project's ripple mode applies (get_ripple_mode). It matters when the music lands on or runs into other clips of its track: with ripple on they are pushed later (a start inside a clip is still refused, ripple never splits a clip); with it off the request is refused before anything renders. Appending to the track moves nothing either way. Ripple is an attempt: it is skipped on a locked track, and a lane it would leave overlapping is refused."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result, stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
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

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct ImportCaptionsParams {
    #[schemars(
        description = "Absolute path of a .srt, .vtt, .ass or .ssa subtitle file (at most 5 MiB). Give this or `text`, not both."
    )]
    path: Option<String>,
    #[schemars(
        description = "The subtitle file's text itself, for captions you wrote rather than found on disk (SubRip is the easy one to write). Give this or `path`, not both."
    )]
    text: Option<String>,
    #[schemars(description = "`srt`, `vtt` or `ass`. Omit to detect it from the content, which is almost always right.")]
    format: Option<String>,
    #[schemars(
        description = "Which clock the cue times are on. `timeline` (default): the file is a subtitle track for the finished cut, so a cue at 0:12 shows at 0:12 of the timeline (cues after the cut ends are dropped). `source`: the file times one asset's own footage — a transcript, or subtitles for the uncut recording — and needs asset_id; each cue is projected through every clip of that asset, so it follows trims, reorders and speed changes and a cue whose footage was cut gets no caption. Giving asset_id alone implies `source`."
    )]
    base: Option<String>,
    #[schemars(description = "The asset whose footage the cue times belong to; only with base `source`")]
    asset_id: Option<String>,
    #[schemars(
        description = "Look: `lines` (default) holds a few words as a subtitle line; `word_punch` puts one large word on screen at a time. Long cues are split into readable lines either way. Everything below is an override on top of the style."
    )]
    style: Option<CaptionStyle>,
    #[schemars(
        description = "Most words on one caption line (lines: 4, word_punch: 1). Raise it, with max_chars, to keep each cue of a professionally timed file as one caption."
    )]
    max_words: Option<usize>,
    #[schemars(description = "Most characters on one caption line (default 28); the tighter of the two limits wins")]
    max_chars: Option<usize>,
    #[schemars(description = "Vertical position as a fraction of frame height, 0 = top (lines: 0.88, word_punch: 0.72)")]
    pos_y: Option<f64>,
    #[schemars(description = "Font height as a fraction of frame height (lines: 0.05, word_punch: 0.11)")]
    size: Option<f64>,
    #[schemars(
        description = "Seconds added to every cue before it is placed; negative moves them earlier. For a file whose clock does not start where the picture does — a broadcast .srt that begins at 01:00:00 wants -3600. Default 0."
    )]
    offset: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SpeechModelParams {
    #[schemars(
        description = "whisper.cpp model name (tiny, base, small, medium, large-v3-turbo, or an `.en` variant); omit for the default"
    )]
    name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct DownloadModelParams {
    #[schemars(
        description = "whisper.cpp model name (tiny, base, small, medium, large-v3-turbo, or an `.en` variant); omit for the default"
    )]
    name: Option<String>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result, stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
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
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, a clip dropped onto footage that is already there pushes that footage and everything after it later by the clip's length; appending, or filling a gap it fits in, moves nothing either way. false places the clip exactly as asked. Tracks ripple independently. A clip dropped INSIDE an existing clip would need that clip split, so ripple cannot make room: it leaves the two overlapping — place clips on the boundary between two, or split first, and check get_timeline_state. More generally ripple is an attempt, not a guarantee: it is skipped on a locked track, and a lane it would leave overlapping (or before 0) is kept as the edit made it — check get_timeline_state afterwards rather than assuming the later clips moved."
    )]
    ripple: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SplitParams {
    #[schemars(description = "UUID of the clip to split")]
    clip_id: String,
    #[schemars(description = "Timeline time at which to split (seconds)")]
    at: f64,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). A split changes no length, so ripple never moves anything here; the parameter exists so a call can state its intent either way."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: each partner that has the split time inside it is split at the same moment; the clips on each side of the cut form a link group of their own (a partner that lies wholly after the cut goes with the right half). false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
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
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, the later clips on the same track follow the change in this clip's length, keeping their gaps, and a left-edge trim keeps the clip's start (so timeline_start is not needed). false trims in place and leaves later clips where they are. Tracks ripple independently (trimming on V1 never moves A1). Ripple is an attempt, not a guarantee: it is skipped on a locked track, and a lane it would leave overlapping (or before 0) is kept as the edit made it — check get_timeline_state afterwards rather than assuming the later clips moved."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: a partner that shares the edge you move (within 1 ms) has that edge moved by the same amount, clamped to its own footage; a pure move carries it by the same time; a partner that would then land on a clip outside its group refuses the call (ripple may make room). false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
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
struct RippleDeleteParams {
    #[schemars(description = "UUID of the clip to remove")]
    clip_id: String,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: each partner is removed too, the named clip's track closes the gap by that clip's length, and the linked clips after it follow by the same amount on their own tracks (unlinked clips on a partner's track stay put). false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RemoveClipParams {
    #[schemars(description = "UUID of the clip to remove")]
    clip_id: String,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). true closes the gap the clip leaves: the later clips on its track shift left by its length, gaps kept (ripple_delete is the same thing as a tool of its own). false leaves the gap. Tracks ripple independently. Ripple is an attempt, not a guarantee: it is skipped on a locked track, and a lane it would leave overlapping (or before 0) is kept as the edit made it — check get_timeline_state afterwards rather than assuming the later clips moved."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: they are removed too (and gaps close per ripple). false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RemoveClipsParams {
    #[schemars(description = "UUIDs of the clips to remove, on any tracks; a clip named twice is removed once")]
    clip_ids: Vec<String>,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). true is a multi-clip ripple delete: every track closes up behind what it lost, each by its own removed length, gaps kept. false leaves the gaps. Tracks ripple independently. The result's `rippled` / `clips_shifted` say whether anything actually moved."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: every partner of a named clip is removed with it and counted in `removed`. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ClipMoveParams {
    #[schemars(description = "UUID of the clip to move")]
    clip_id: String,
    #[schemars(description = "Where the clip starts afterwards, in absolute timeline seconds (not an offset); must be >= 0")]
    timeline_start: f64,
    #[schemars(description = "Destination track UUID (must be the same kind); omit to keep the clip on its current track")]
    track_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct MoveClipsParams {
    #[schemars(
        description = "The clips to move and where each lands. Every clip may appear once. The group is checked as a group: clips moving together may pass through the places they are leaving, but may not overlap each other or a clip that stays."
    )]
    moves: Vec<ClipMoveParams>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: each partner of a named clip moves by the same time on its OWN track — a track change you ask for applies to the clip you named only. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct LinkClipsParams {
    #[schemars(
        description = "UUIDs of the clips to link — at least two, each on a DIFFERENT track (a picture and its sound), none on a locked track"
    )]
    clip_ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct DetachClipsParams {
    #[schemars(
        description = "UUIDs of the picture clips whose own sound to detach (on video tracks); one that cannot be is skipped and reported"
    )]
    clip_ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ReattachClipsParams {
    #[schemars(
        description = "UUIDs of the clips whose detached sound to reattach — each names a picture or its sound (a pair named twice counts once); all or nothing: one that cannot be reattached refuses the lot"
    )]
    clip_ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct UnlinkClipsParams {
    #[schemars(
        description = "UUIDs of the clips to unlink; each leaves its group, and a group left with a single clip dissolves (so naming either half of a pair unlinks the pair)"
    )]
    clip_ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetRippleModeParams {
    #[schemars(description = "true to turn ripple mode on for the project, false to turn it off")]
    on: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct MoveClipParams {
    #[schemars(description = "UUID of the clip to move")]
    clip_id: String,
    #[schemars(description = "New timeline position in seconds (clamped to >= 0)")]
    timeline_start: f64,
    #[schemars(description = "Destination track UUID (must be the same kind); omit to keep the clip on its current track")]
    track_id: Option<String>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: each partner moves by the same time on its OWN track — a track change you ask for applies to the clip you named only. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
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
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result, stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
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
struct SeparateStemsParams {
    #[schemars(description = "UUID of the asset whose sound to separate (music, or footage with dialogue over a bed)")]
    asset_id: String,
    #[schemars(
        description = "UUID of a clip of that asset to lay the stems under: each stem goes on its own new audio track at the clip's position and span, and the clip's own sound is switched off. Omit to only add the stems to the library"
    )]
    clip_id: Option<String>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result, stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct PlanMusicFitParams {
    #[schemars(description = "UUID of the music clip (on an audio track, at normal speed)")]
    clip_id: String,
    #[schemars(
        description = "How long the music should last, in seconds. Omit to fit it to the picture: from the clip's start to the end of the last video clip"
    )]
    target: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FitMusicParams {
    #[schemars(description = "UUID of the music clip (on an audio track, at normal speed, not linked to a picture)")]
    clip_id: String,
    #[schemars(
        description = "How long the music should last, in seconds. Omit to fit it to the picture: from the clip's start to the end of the last video clip"
    )]
    target: Option<f64>,
    #[schemars(
        description = "When the nearest bar-aligned arrangement runs over the target, cut it there and fade out over the last 2 s (default true). False keeps the whole arrangement, ending included"
    )]
    fade_out: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetMasterDuckParams {
    #[schemars(
        description = "Speech-gate depth in dB (negative, clamped to -40..-1; -12 is a good bed under dialogue). Omit to go back to the sidechain compressor"
    )]
    depth_db: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CutClipRangeParams {
    #[schemars(description = "UUID of the clip to cut")]
    clip_id: String,
    #[schemars(description = "Source-time start of the span to remove (seconds, e.g. a transcript segment's or a word's start)")]
    from: f64,
    #[schemars(description = "Source-time end of the span to remove (seconds)")]
    to: f64,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: the stretch of TIMELINE the cut removes is taken out of every partner it overlaps too — a partner whose start was inside the stretch resumes at the cut, one spanning it is cut in two — and the linked clips after it follow by the same amount. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RollEditParams {
    #[schemars(description = "UUID of the EARLIER clip of the pair — the one whose end is the cut")]
    clip_a: String,
    #[schemars(
        description = "UUID of the LATER clip of the pair, on the same track and touching clip_a — the one whose start is the cut"
    )]
    clip_b: String,
    #[schemars(
        description = "Seconds to move the cut: positive moves it later (clip_a gains footage at its end, clip_b loses it at its start), negative earlier. Clamped to the footage each clip has left and to 0.05s for the clip that shrinks; the reply's `applied` is how far it really went."
    )]
    delta: f64,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: each pair of partners that share the cut rolls by the same amount, and the roll clamps to the tightest pair. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SlipClipParams {
    #[schemars(description = "UUID of the clip to slip")]
    clip_id: String,
    #[schemars(
        description = "Source-footage seconds (NOT timeline seconds — at 2x speed 1 second of slip moves the picture half a second): positive makes the clip start later in its own footage, so you see material that comes later; negative earlier. A reversed clip is mirrored for you (positive moves its source window down). Clamped to the asset's footage; the reply's `applied` is how far it really went."
    )]
    delta: f64,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: partners slip by the same moment of footage (a still has none and is skipped), clamped to the tightest. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SlideClipParams {
    #[schemars(description = "UUID of the clip to slide")]
    clip_id: String,
    #[schemars(
        description = "Timeline seconds to move the clip: positive later, negative earlier. A neighbour that touches the clip gives way (previous clip's end and next clip's start move by the same amount); one across a gap is left alone and the clip stops where it would meet it. Clamped to the neighbours' footage and a 0.05s floor, and to 0 at the start of the track; the reply's `applied` is how far it really went."
    )]
    delta: f64,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: partners slide by the same time, their own neighbours giving way, clamped to the tightest. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SplitRemoveParams {
    #[schemars(description = "UUID of the clip to cut")]
    clip_id: String,
    #[schemars(
        description = "Timeline time to cut at (seconds); must lie inside the clip and leave at least 0.05s of the half you keep"
    )]
    at: f64,
    #[schemars(description = "Which half to REMOVE: \"left\" (everything before `at`) or \"right\" (everything after it)")]
    side: SplitSide,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, the later clips on the track close the gap the removed half leaves (a left removal keeps the clip's own start and pulls the rest in); false leaves the gap, as after a plain split and delete. Ripple is an attempt, not a guarantee: it is skipped on a locked track, and a lane it would leave overlapping is kept as the edit made it."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: partners that span `at` are trimmed to it as well. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ClipCutParams {
    #[schemars(description = "UUID of the clip to cut")]
    clip_id: String,
    #[schemars(
        description = "Timeline time to cut it at (seconds); must lie inside the clip and leave at least 0.05s of the half you keep"
    )]
    at: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SplitRemoveClipsParams {
    #[schemars(
        description = "The clips to cut and where. At most one clip per track, each clip once; different tracks may be cut at different times (usually the same time: a picture and its sound)."
    )]
    cuts: Vec<ClipCutParams>,
    #[schemars(
        description = "Which half to REMOVE from every clip: \"left\" (everything before its `at`) or \"right\" (everything after it)"
    )]
    side: SplitSide,
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, each track closes the gap its own clip leaves (a left removal keeps the clip's own start); tracks ripple independently. false leaves the gaps."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: a partner of a named clip that you did not name is cut at the same time if the time is inside it. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
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
struct SetMasterVolumeParams {
    #[schemars(
        description = "Master fader as a linear gain on the finished mix: 1.0 is unity, 0.5 is -6 dB, 0 is silent. Clamped to 0..4 (+12 dB)"
    )]
    volume: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetMasterLimiterParams {
    #[schemars(description = "true switches the limiter on, false switches it off")]
    enabled: bool,
    #[schemars(
        description = "Where the limiter stops the signal, in dBFS: -1.5 keeps the mix a decibel and a half under full scale. Clamped to -24..0. Omit to keep the current ceiling (-1.5 until one is chosen)"
    )]
    ceiling_db: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetMasterTruePeakParams {
    #[schemars(
        description = "true makes the master limiter hold the true peak (it runs 4x oversampled, so the peaks between samples stay under the ceiling too); false is the plain sample-peak limiter"
    )]
    enabled: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SyncByAudioParams {
    #[schemars(description = "UUID of the clip that stays put: the timing reference")]
    reference_clip_id: String,
    #[schemars(description = "UUID of the clip to move into sync with the reference (its linked partners move with it)")]
    clip_id: String,
    #[schemars(
        description = "Move the clip even when the match is not reliable (confidence under 8 or distinct under 1.5). Default false: an unreliable match is reported and nothing moves"
    )]
    force: Option<bool>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once; poll job_status for progress and the result, stop it with cancel_job. Default false: the call stays open until the sync is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RefineTranscriptParams {
    #[schemars(description = "UUID of the asset whose cached transcript to re-time")]
    asset_id: String,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once; poll job_status for the result, stop it with cancel_job. Default false: the call stays open until it is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct GetLevelsParams {
    #[schemars(description = "Measure only this span of the cut, {start, end} in timeline seconds. Omit for the whole cut")]
    range: Option<TimeRange>,
    #[schemars(
        description = "Measure the mix as an export with loudnorm=true would write it (default false: the mix as it stands, which is what tells you whether to turn loudnorm on)"
    )]
    loudnorm: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct BandBalanceParams {
    #[schemars(description = "UUID of an asset: measure its whole sound. Times are the asset's own seconds.")]
    asset_id: Option<String>,
    #[schemars(
        description = "UUID of a clip: measure its source range at the clip's own volume (its fader and effects are not applied). Times are source seconds."
    )]
    clip_id: Option<String>,
    #[schemars(
        description = "Measure the finished mix of the cut over this span, {start, end} in timeline seconds — the audio an export would render, faders, ducking and master bus included (what get_levels measures)."
    )]
    range: Option<TimeRange>,
    #[schemars(
        description = "The stretches to compare, [{start, end, label?}] in the target's seconds (at most 100, each at least 0.25 s). Omitted: a composed asset's score sections, else the four-bar phrases of analyzed music, else eight equal windows; `windows_from` in the answer says which."
    )]
    sections: Option<Vec<kerf_core::BandWindow>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct PlatformCheckParams {
    #[schemars(
        description = "Judge only this span of the cut, {start, end} in timeline seconds — what an export with the same `range` would write. Omit for the whole cut"
    )]
    range: Option<TimeRange>,
    #[schemars(
        description = "Judge the cut at this frame width in pixels instead of the project's own — the size an export with `options.resolution` would write. Give it with `height`. Omit both to use the project's frame"
    )]
    width: Option<u32>,
    #[schemars(description = "Frame height in pixels; see `width`")]
    height: Option<u32>,
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
    #[schemars(description = "How long the marked span runs from `time`, seconds; 0 is a point marker")]
    #[serde(default)]
    duration: Option<f64>,
    #[schemars(description = "A free-form note (a review comment, a chapter description); an empty string clears it")]
    #[serde(default)]
    comment: Option<String>,
    #[schemars(
        description = "`comment` (the default: a note on the cut) or `chapter` (starts a chapter: written into the export's chapter list and the YouTube chapter text)"
    )]
    #[serde(default)]
    kind: Option<kerf_core::MarkerKind>,
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
    #[schemars(description = "How long the marked span runs from `time`, seconds; 0 is a point marker")]
    #[serde(default)]
    duration: Option<f64>,
    #[schemars(description = "A free-form note (a review comment, a chapter description); an empty string clears it")]
    #[serde(default)]
    comment: Option<String>,
    #[schemars(
        description = "`comment` (the default: a note on the cut) or `chapter` (starts a chapter: written into the export's chapter list and the YouTube chapter text)"
    )]
    #[serde(default)]
    kind: Option<kerf_core::MarkerKind>,
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
    #[schemars(
        description = "Ripple override for this call. Omitted, the project's ripple mode applies (get_ripple_mode). With ripple on, the later clips on the same track follow the change in this clip's duration, keeping their gaps; false retimes the clip and leaves later clips where they are (a slowed clip then runs into its neighbour). Tracks ripple independently. Ripple is an attempt, not a guarantee: it is skipped on a locked track, and a lane it would leave overlapping (or before 0) is kept as the edit made it — check get_timeline_state afterwards rather than assuming the later clips moved."
    )]
    ripple: Option<bool>,
    #[schemars(
        description = "Linked-clip override for this call. Omitted, clips linked to the ones named — a picture and its detached sound (detach_audio, link_clips; `link_id` in get_timeline_state shows the groups) — are edited with them: partners are retimed by the same ratio and re-placed about the clip you named (a sound that leads its picture still leads it, scaled); the linked clips after follow it. false edits only what you named and ignores links, which can leave a picture and its sound out of sync. A linked clip on a locked track refuses the whole call."
    )]
    link: Option<bool>,
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
struct TransformAtParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Seconds from the clip's start: where an animated number gets its key")]
    time: f64,
    #[schemars(description = "Uniform scale after fitting to the frame (1.0 = fit); omit to leave unchanged")]
    scale: Option<f64>,
    #[schemars(description = "Horizontal offset as a fraction of frame width (0.0 = centered); omit to leave unchanged")]
    pos_x: Option<f64>,
    #[schemars(description = "Vertical offset as a fraction of frame height (0.0 = centered); omit to leave unchanged")]
    pos_y: Option<f64>,
    #[schemars(description = "Clockwise rotation in degrees; omit to leave unchanged")]
    rotation: Option<f64>,
    #[schemars(description = "Opacity 0.0–1.0 (1.0 = opaque); omit to leave unchanged")]
    opacity: Option<f64>,
    #[schemars(description = "Fraction cropped from the left edge (0.0–1.0, never animated); omit to leave unchanged")]
    crop_left: Option<f64>,
    #[schemars(description = "Fraction cropped from the right edge (0.0–1.0, never animated); omit to leave unchanged")]
    crop_right: Option<f64>,
    #[schemars(description = "Fraction cropped from the top edge (0.0–1.0, never animated); omit to leave unchanged")]
    crop_top: Option<f64>,
    #[schemars(description = "Fraction cropped from the bottom edge (0.0–1.0, never animated); omit to leave unchanged")]
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
struct GradeParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "Tone curves, replacing the clip's: `master`, `red`, `green`, `blue`, each a list of {x, y} points in 0..1 (input -> output, at most 16; a monotone cubic is drawn through them and held flat past the first and last point). A channel's curve runs first, then the master. [{x:0,y:1},{x:1,y:0}] inverts; [{x:0,y:0},{x:0.25,y:0.2},{x:0.75,y:0.8},{x:1,y:1}] is a gentle S. Pass {} to clear the curves; omit to leave them unchanged"
    )]
    curves: Option<CurvesArg>,
    #[schemars(
        description = "Colour wheels, replacing the clip's: `shadows`, `midtones`, `highlights`, each {x, y, lightness}. (x, y) is a point in the unit disc whose angle is the hue pushed into that range (0 deg red, 120 deg green, 240 deg blue, counter-clockwise; e.g. teal shadows are x -0.3, y 0) and whose distance is the strength; lightness (-1..1) lifts or lowers the range. 0.1 to 0.3 is a visible grade, 1 is extreme. Pass {} to clear; omit to leave them unchanged"
    )]
    wheels: Option<WheelsArg>,
    #[schemars(
        description = "Absolute path of a .cube 3D LUT file to look the clip up through (after the color numbers, before the curves). The file is checked now: a 3D table of size 2 to 65 with a 0..1 domain. Pass \"\" to remove the LUT; omit to leave it unchanged"
    )]
    lut_path: Option<String>,
    #[schemars(
        description = "How much of the LUT, 0..1 (1 = all of it, the default for a new LUT); alone, changes the current LUT's"
    )]
    lut_intensity: Option<f64>,
}

/// `set_grade`'s curves: the model's [`Curves`], refusing a key it does not name (`mastre`), which the
/// persisted type quietly drops.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CurvesArg {
    #[serde(default)]
    master: Vec<PointArg>,
    #[serde(default)]
    red: Vec<PointArg>,
    #[serde(default)]
    green: Vec<PointArg>,
    #[serde(default)]
    blue: Vec<PointArg>,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PointArg {
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WheelsArg {
    #[serde(default)]
    shadows: WheelArg,
    #[serde(default)]
    midtones: WheelArg,
    #[serde(default)]
    highlights: WheelArg,
}

#[derive(Debug, Clone, Copy, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WheelArg {
    #[serde(default)]
    x: f64,
    #[serde(default)]
    y: f64,
    #[serde(default)]
    lightness: f64,
}

impl From<CurvesArg> for Curves {
    fn from(c: CurvesArg) -> Self {
        let points = |v: Vec<PointArg>| v.into_iter().map(|p| CurvePoint { x: p.x, y: p.y }).collect();
        Self {
            master: points(c.master),
            red: points(c.red),
            green: points(c.green),
            blue: points(c.blue),
        }
    }
}

impl From<WheelArg> for Wheel {
    fn from(w: WheelArg) -> Self {
        Self {
            x: w.x,
            y: w.y,
            lightness: w.lightness,
        }
    }
}

impl From<WheelsArg> for Wheels {
    fn from(w: WheelsArg) -> Self {
        Self {
            shadows: w.shadows.into(),
            midtones: w.midtones.into(),
            highlights: w.highlights.into(),
        }
    }
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
        description = "Whole-transform keyframes (replaces the clip's bundle of them). Each: {\"time\":seconds_from_clip_start, \
                       \"scale\":1.0,\"pos_x\":0.0,\"pos_y\":0.0,\"rotation\":0.0,\"opacity\":1.0}. Two or more animate \
                       the clip; pass [] to clear these keyframes (a number with keys of its own, from \
                       set_property_keyframes, keeps them — clear_keyframes clears everything of the transform)."
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
struct PropertyKeyframesParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "The number to animate: scale, pos_x, pos_y, rotation, opacity (the transform), brightness, \
                       contrast, saturation, gamma, temperature (the colour grade), volume (the clip's own gain), \
                       mask_x, mask_y, mask_width, mask_height, mask_feather (the clip's mask — it must have one, \
                       set_mask) or crop_zoom, crop_x, crop_y (the animated crop: a zoom into the picture and where \
                       its window sits)"
    )]
    prop: Property,
    #[schemars(
        description = "The keys (replaces this number's animation; pass [] to make it static again). Each: \
                       {\"time\":seconds_from_clip_start, \"value\":number, \"easing\":optional, same values as \
                       set_keyframe_easing}. A key's easing shapes the segment that leaves it. Ranges are the static \
                       setters': opacity 0–1, brightness and temperature -1–1, contrast 0–4, saturation 0–3, gamma \
                       0.1–10, volume 0–4 (linear gain), scale > 0, mask_x / mask_y / mask_feather 0–1, mask_width / \
                       mask_height 0.01–2 (fractions of the frame), crop_zoom 1–10 (1 = the whole picture, 2 = half \
                       of each side), crop_x / crop_y -1–1 (-1 = the window against the left / top edge, 0 = centred, \
                       1 = right / bottom). At most 1000 keys."
    )]
    keys: Vec<PropertyKey>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct PropertyTracksParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(
        description = "The numbers to re-key, each {\"prop\":name, \"keys\":[...]} exactly as set_property_keyframes \
                       takes its prop and keys (pass [] keys to make that number static). Each number at most once; \
                       numbers not listed are untouched."
    )]
    tracks: Vec<PropertyTrackArg>,
}

/// One number's keys for `set_property_tracks`: the model's [`PropertyTrack`] with `keys` required
/// and a key it does not name (`key`) refused — the persisted type reads either as no keys, which
/// would silently make the number static.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PropertyTrackArg {
    prop: Property,
    keys: Vec<PropertyKey>,
}

impl From<PropertyTrackArg> for PropertyTrack {
    fn from(a: PropertyTrackArg) -> Self {
        Self {
            prop: a.prop,
            keys: a.keys,
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CopyKeyframesParams {
    #[schemars(description = "UUID of the clip to copy the animation from")]
    from_clip_id: String,
    #[schemars(description = "UUID of the clip to give it to (its own keys for the copied numbers are replaced)")]
    to_clip_id: String,
    #[schemars(
        description = "The numbers to copy (any set_property_keyframes takes: scale, pos_x, ..., volume, mask_x, ..., \
                       crop_zoom, crop_x, crop_y); omit or [] for every number the source animates"
    )]
    props: Option<Vec<Property>>,
    #[schemars(
        description = "Seconds to shift the keys later (negative: earlier, cutting off what falls before the \
                       destination's start and pinning the pose it opens on). Default 0."
    )]
    offset: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SetKeyframeEasingParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
    #[schemars(description = "Time of the keyframe, in seconds from the clip's start (within a millisecond)")]
    time: f64,
    #[schemars(
        description = "One number whose segment to shape (see set_property_keyframes); omit to shape the transform \
                       keyframe at that time (the whole-transform keys and any transform number keyed on its own)"
    )]
    prop: Option<Property>,
    #[schemars(
        description = "How the clip travels from this keyframe's pose to the next: \"linear\" (default), \"hold\" \
                       (stay, then jump at the next key), \"ease_in\", \"ease_out\", \"ease_in_out\", or \
                       {\"bezier\": {\"x1\", \"y1\", \"x2\", \"y2\"}} with every control point in 0..=1 (CSS cubic-bezier, \
                       no overshoot)"
    )]
    easing: kerf_core::Easing,
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
struct AddSvgOverlayParams {
    #[schemars(
        description = "Your own SVG document: the text of an <svg ...>...</svg> file, animated with SMIL (see the tool description). Give this OR `preset`, not both"
    )]
    svg: Option<String>,
    #[schemars(description = "A ready-made animated title instead of `svg` (see list_svg_presets). Needs `params`")]
    preset: Option<SvgPresetKind>,
    #[schemars(
        description = "What fills the preset: `text` (required), and optionally `subtitle`, `color`, `accent` (any SVG colour like \"#ffffff\") and `font` (a system family from list_fonts). Only with `preset`"
    )]
    params: Option<SvgPresetParams>,
    #[schemars(description = "When the title appears, in timeline seconds")]
    start: f64,
    #[schemars(
        description = "When it disappears, in timeline seconds. Omit it to run for the SVG's own length when its animations end (clamped to 2–30 s), else 4 s"
    )]
    end: Option<f64>,
    #[schemars(description = "Left edge of the placement rectangle as a fraction of the frame width; default 0")]
    x: Option<f64>,
    #[schemars(description = "Top edge of the placement rectangle as a fraction of the frame height; default 0")]
    y: Option<f64>,
    #[schemars(description = "Width of the placement rectangle as a fraction of the frame width; default 1 (the whole frame)")]
    width: Option<f64>,
    #[schemars(description = "Height of the placement rectangle as a fraction of the frame height; default 1 (the whole frame)")]
    height: Option<f64>,
    #[schemars(description = "Opacity of the whole title, 0–1; default 1")]
    opacity: Option<f64>,
    #[schemars(description = "A label for the title (shown in the timeline); default is the preset's text")]
    name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct UpdateSvgOverlayParams {
    #[schemars(description = "UUID of the SVG overlay (see timeline_summary)")]
    overlay_id: String,
    #[schemars(description = "New label; omit to leave unchanged")]
    name: Option<String>,
    #[schemars(
        description = "Replace the artwork with your own SVG (the title stops being a preset). Not together with `preset_params`; omit to leave unchanged"
    )]
    svg: Option<String>,
    #[schemars(
        description = "Re-fill a PRESET title: the full set of params (`text` required, optional `subtitle`, `color`, `accent`, `font`). Refused on a custom SVG title; omit to leave unchanged"
    )]
    preset_params: Option<SvgPresetParams>,
    #[schemars(
        description = "Switch a PRESET title to another kind (see list_svg_presets), keeping its id, name, timing, placement and place in the draw order; its params carry over unless `preset_params` is given too. Refused on a custom SVG title and together with `svg`; omit to leave unchanged"
    )]
    preset_kind: Option<SvgPresetKind>,
    #[schemars(description = "New start time (timeline seconds); omit to leave unchanged")]
    start: Option<f64>,
    #[schemars(description = "New end time (timeline seconds); omit to leave unchanged")]
    end: Option<f64>,
    #[schemars(description = "Left edge of the placement rectangle, fraction of the frame width; omit to leave unchanged")]
    x: Option<f64>,
    #[schemars(description = "Top edge of the placement rectangle, fraction of the frame height; omit to leave unchanged")]
    y: Option<f64>,
    #[schemars(description = "Width of the placement rectangle, fraction of the frame width; omit to leave unchanged")]
    width: Option<f64>,
    #[schemars(description = "Height of the placement rectangle, fraction of the frame height; omit to leave unchanged")]
    height: Option<f64>,
    #[schemars(description = "Opacity of the whole title, 0–1; omit to leave unchanged")]
    opacity: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ValidateSvgParams {
    #[schemars(description = "The SVG document to check: the text of an <svg ...>...</svg> file")]
    svg: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct PreviewSvgOverlayParams {
    #[schemars(
        description = "UUID of an SVG title already on the timeline (see timeline_summary's `svg_overlays`). Give this OR `svg`, not both"
    )]
    overlay_id: Option<String>,
    #[schemars(
        description = "A draft SVG document to look at before adding it (the text of an <svg ...>...</svg> file). Give this OR `overlay_id`, not both"
    )]
    svg: Option<String>,
    #[schemars(
        description = "The moments to draw, in seconds AFTER THE TITLE APPEARS (0 = its first frame; for a title already on the timeline, a sliced one's offset is added for you). At most 36. Not together with `count`"
    )]
    times: Option<Vec<f64>>,
    #[schemars(
        description = "How many moments to draw, spread evenly from the first frame to the end of the title (default 6, at most 36). Not together with `times`"
    )]
    count: Option<u32>,
    #[schemars(description = "Width in pixels of each cell of the sheet (default 480, 64 to 960)")]
    max_width: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct PreviewSvgPresetParams {
    #[schemars(description = "The ready-made title to draw (see list_svg_presets)")]
    preset: SvgPresetKind,
    #[schemars(description = "What fills it, as for add_svg_overlay (`text` is required)")]
    params: SvgPresetParams,
    #[schemars(description = "How long the title would run, in seconds (default 4): a preset times its animation to its length")]
    duration: Option<f64>,
    #[schemars(description = "The moments to draw, in seconds after the title appears. At most 36. Not together with `count`")]
    times: Option<Vec<f64>>,
    #[schemars(
        description = "How many moments to draw, spread evenly from the first frame to the end (default 6, at most 36). Not together with `times`"
    )]
    count: Option<u32>,
    #[schemars(description = "Width in pixels of each cell of the sheet (default 480, 64 to 960)")]
    max_width: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct TimelineStateParams {
    #[schemars(
        description = "Return every animated SVG title's source however long it is. By default a source over 4 KiB is replaced by a note with its size"
    )]
    full_svg: Option<bool>,
    #[schemars(
        description = "Return every overlay inline, generated captions included, and every SVG source whole. By default the generated captions (often hundreds) are left out of `overlays` and shown as a window under `captions`."
    )]
    full: Option<bool>,
    #[schemars(
        description = "Which generated caption the `captions` window starts at (default 0); `captions.next_offset` is the next page"
    )]
    captions_offset: Option<usize>,
    #[schemars(description = "How many generated captions the window holds (default 20, at most 500)")]
    captions_limit: Option<usize>,
    #[schemars(
        description = "Which clip (in track order) the clip window starts at (default 0). A cut of up to 200 clips is shown whole; a longer one is windowed and `clips_window.next_offset` is the next page"
    )]
    clips_offset: Option<usize>,
    #[schemars(description = "How many clips the window holds (default 200, at most 1000)")]
    clips_limit: Option<usize>,
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
struct ExportCaptionsParams {
    #[schemars(description = "Absolute local .srt or .vtt file path to write — no URLs or ffmpeg protocol sinks")]
    output_path: String,
    #[schemars(description = "`srt` or `vtt`; omit to follow the path's extension")]
    #[serde(default)]
    format: Option<kerf_core::SubtitleFormat>,
    #[schemars(
        description = "Overwrite an existing file at output_path (default false — the call fails naming the path so you can pick a new one or opt in)"
    )]
    overwrite: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportInterchangeParams {
    #[schemars(description = "Absolute local output path (.otio or .edl) — no URLs or ffmpeg protocol sinks")]
    output_path: String,
    #[schemars(description = "`otio` (OpenTimelineIO, every track) or `edl` (CMX 3600, one video track)")]
    format: kerf_core::InterchangeFormat,
    #[schemars(description = "EDL only: which video track to write, 0 = the first (V1). Default 0.")]
    #[serde(default)]
    video_track: Option<usize>,
    #[schemars(
        description = "Overwrite an existing file at output_path (default false — the call fails naming the path so you can pick a new one or opt in)"
    )]
    overwrite: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportMarkersParams {
    #[schemars(description = "Absolute local output path (.csv or .txt) — no URLs or ffmpeg protocol sinks")]
    output_path: String,
    #[schemars(
        description = "`csv` (every marker as a spreadsheet row, formula-safe) or `youtube` (chapter lines for a video description, from the chapter markers — every marker when none is a chapter)"
    )]
    format: kerf_core::MarkerListFormat,
    #[schemars(
        description = "Overwrite an existing file at output_path (default false — the call fails naming the path so you can pick a new one or opt in)"
    )]
    overwrite: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportSrtParams {
    #[schemars(description = "UUID of the asset whose transcript to export")]
    asset_id: String,
    #[schemars(description = "Absolute local .srt file path to write — no URLs or ffmpeg protocol sinks")]
    output_path: String,
    #[schemars(
        description = "Overwrite an existing file at output_path (default false — the call fails naming the path so you can pick a new one or opt in)"
    )]
    overwrite: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SpeakerRangesParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(description = "Only this speaker's ranges (e.g. \"S1\"); omit for every speaker")]
    speaker: Option<String>,
    #[schemars(description = "Words of one speaker closer than this many seconds are one range (default 0.75, at most 10)")]
    join_gap: Option<f64>,
    #[schemars(
        description = "Skip this many of each speaker's ranges (default 0), to page on; `more` in the answer says there are more"
    )]
    offset: Option<usize>,
    #[schemars(description = "Most ranges to return per speaker (default 200, at most 2000)")]
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FillerWordsParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
    #[schemars(
        description = "Words or short phrases to find, case and punctuation ignored (default: um, uh, umm, uhm, erm, er, ah, hmm, mm, you know, i mean). At most 100."
    )]
    fillers: Option<Vec<String>>,
    #[schemars(description = "Skip this many hits (default 0), to page on; `next_offset` in the answer is the next page")]
    offset: Option<usize>,
    #[schemars(description = "Most hits to return (default 200, at most 1000)")]
    limit: Option<usize>,
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
    #[schemars(
        description = "Absolute local output file path for the rendered result — no URLs or ffmpeg protocol \
                       sinks. Its extension should match the chosen container."
    )]
    output_path: String,
    #[schemars(description = "Optional encode settings. Omit for the safe default (H.264 + AAC MP4). \
                       Key fields: container (mp4/mov/mkv/webm/gif/mp3/m4a/wav/flac); video_codec \
                       (libx264/libx265/libvpx-vp9/libsvtav1/prores_ks/gif, plus GPU encoders \
                       h264_nvenc/hevc_nvenc/av1_nvenc/h264_qsv/hevc_qsv/av1_qsv/h264_videotoolbox/\
                       hevc_videotoolbox/h264_amf/hevc_amf/av1_amf/h264_vaapi/hevc_vaapi/av1_vaapi — \
                       far faster, crf still applies; use only the ones export_capabilities lists — \
                       a VAAPI codec on a machine nothing has probed yet waits up to 10 s for the probe \
                       to find the render node); \
                       audio_codec (aac/libmp3lame/libopus/flac/alac/pcm_s16le/pcm_s24le); \
                       rate_control (crf/bitrate/two_pass/lossless); crf; video_bitrate (\"8M\"); \
                       preset; hwaccel (\"auto\"/\"cuda\"/\"vaapi\"/\"videotoolbox\"/\"qsv\" — GPU \
                       decode, composes with any encoder); resolution ([w,h]); fps; audio_bitrate \
                       (\"192k\"); include_audio; faststart; range ({start,end} timeline seconds — \
                       render only that span); embed_captions (mux the cut's captions in as a soft \
                       subtitle stream — mp4/mov/mkv/webm only, not shown by default); \
                       skip_burned_captions (leave the captions out of the picture); caption_sidecars \
                       ([\"srt\",\"vtt\"] — caption files beside the output, written once the render \
                       completes; an existing one is refused unless overwrite is true). Chapter markers \
                       become the file's chapters.")]
    #[serde(default)]
    options: Option<ExportOptions>,
    #[schemars(
        description = "Overwrite an existing file at output_path (default false — the call fails naming the path so you can pick a new one or opt in)"
    )]
    overwrite: Option<bool>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result, stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExportVariantsParams {
    #[schemars(
        description = "Absolute local base output path (no URLs or ffmpeg protocol sinks); each delivery lands \
                              beside it with its shape in the name — `/renders/cut.mp4` at 9:16 and 1:1 writes \
                              `cut-9x16.mp4` and `cut-1x1.mp4`."
    )]
    output_path: String,
    #[schemars(
        description = "The delivery frames to render, one file each: \"9:16\" (1080x1920, Reels / Shorts / \
                              TikTok), \"1:1\" (1080x1080, feed), \"4:5\" (1080x1350, Instagram portrait), \"16:9\" \
                              (1920x1080, YouTube), or an explicit \"WxH\"."
    )]
    formats: Vec<String>,
    #[schemars(
        description = "Frame each shot for every shape first (default true): samples where each clip's \
                              content sits and keeps a crop per shape on the clip, so a 9:16 and a 1:1 delivery \
                              each keep the subject rather than the middle. The project frame's own crop is never \
                              touched. false renders whatever crop each clip already has."
    )]
    smart_crop: Option<bool>,
    #[schemars(description = "Encode settings shared by every variant — the same fields as `export`. Its \
                              resolution and fit are replaced per variant by the delivery frame.")]
    #[serde(default)]
    options: Option<ExportOptions>,
    #[schemars(
        description = "Overwrite any variant file that already exists (default false — the call fails naming the \
                              first existing path so you can pick a new base path or opt in)"
    )]
    overwrite: Option<bool>,
    #[schemars(
        description = "Render every file in one pass (default true): the sources are decoded and the mix is built \
                              once, then split into each frame — about one export's decode for all of them. Falls back \
                              to one file after another where one pass cannot (two-pass or hardware encoders, an \
                              audio-only container). A cancelled or failed single pass removes every file. false \
                              renders the files one after another, keeping the finished ones if a later one is cancelled."
    )]
    single_pass: Option<bool>,
    #[schemars(
        description = "Run as a background job: the call answers {job_id} at once and the work carries on; poll job_status for progress and the result, stop it with cancel_job. Default false: the call stays open until the work is done (send a progressToken for progress)."
    )]
    background: Option<bool>,
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
struct WaveformRangeParams {
    #[schemars(description = "UUID of the asset (it must have an audio stream)")]
    asset_id: String,
    #[schemars(
        description = "Start of the window in SOURCE seconds — time within the asset's own audio, from 0 at the start of the file, not timeline time. Part of the window before 0 reads as silence."
    )]
    start: f64,
    #[schemars(
        description = "End of the window in SOURCE seconds; must be greater than start. Part of the window past the end of the audio reads as silence."
    )]
    end: f64,
    #[schemars(
        description = "Number of buckets to split the window into (1–4096, capped). A few hundred shows the shape of a whole file; to see exactly where a word or beat begins, ask for a short window instead of more buckets."
    )]
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
struct ScopesParams {
    #[schemars(description = "Timeline position to measure (seconds)")]
    time_secs: f64,
    #[schemars(
        description = "Grids to return besides the numeric summary: any of `waveform` (luma), `parade` (red, green, blue), \
                       `vectorscope`, `histogram`. Omit for the summary alone, which is usually all you need."
    )]
    grids: Option<Vec<kerf_core::scopes::ScopeKind>>,
    #[schemars(
        description = "Grid size, each field optional: `columns` / `rows` of the waveform and parade (default 64 x 64, \
                       at most 512 x 256), `vector` the vectorscope's side (default 48, at most 256)"
    )]
    grid_size: Option<GridSizeParams>,
    #[schemars(description = "Width to measure the frame at (default the delivery's, at most 3840)")]
    max_width: Option<u32>,
    #[schemars(
        description = "The Y'CbCr matrix to read the frame with: `auto` (default: the delivery's, BT.709 for HD and up, \
                       BT.601 for SD), `bt601`, `bt709`, `bt2020`"
    )]
    matrix: Option<kerf_core::scopes::ScopeMatrix>,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct GridSizeParams {
    columns: Option<u32>,
    rows: Option<u32>,
    vector: Option<u32>,
}

impl GridSizeParams {
    fn resolve(self) -> kerf_core::scopes::GridSize {
        kerf_core::scopes::GridSize {
            columns: self.columns.unwrap_or(64),
            rows: self.rows.unwrap_or(64),
            vector: self.vector.unwrap_or(48),
        }
        .clamped()
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CoverParams {
    #[schemars(description = "Timeline position to capture (seconds)")]
    time_secs: f64,
    #[schemars(
        description = "Absolute local output image path (no URLs or ffmpeg protocol sinks). A .png \
                       extension writes PNG, anything else JPEG."
    )]
    output_path: String,
    #[schemars(
        description = "Overwrite an existing file at output_path (default false — the call fails naming the path so you can pick a new one or opt in)"
    )]
    overwrite: Option<bool>,
}

/// What `analyze_asset` returns: a summary of the cached analysis (counts, not lists — see
/// `get_transcript` / `get_analysis_detail`) with what the run did beside it.
#[derive(Serialize)]
struct AnalyzeResult {
    #[serde(flatten)]
    analysis: reads::AnalysisSummary,
    analysis_status: kerf_core::AnalysisStatus,
    /// Steps that failed, with why; the others still landed.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    failed: Vec<FailedStep>,
}

#[derive(Serialize)]
struct FailedStep {
    step: kerf_core::AnalysisKind,
    reason: String,
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
    /// How many clips on this track are linked to a partner on another track (a
    /// picture and its detached sound). Omitted when none are — which is every
    /// track of a project that never linked anything.
    #[serde(skip_serializing_if = "is_zero")]
    linked_clips: usize,
    /// How many picture clips here have had their own sound detached (muted; the
    /// sound lives on an audio clip). Omitted when none.
    #[serde(skip_serializing_if = "is_zero")]
    detached_sound_clips: usize,
    /// How many clips here are cut from media whose preview proxy is not ready (queued, building,
    /// failed, missing or switched off): previews of them decode the original and are slower.
    /// Omitted when none are.
    #[serde(skip_serializing_if = "is_zero")]
    clips_without_proxy: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// An SVG overlay as an agent reads it: everything but the source, which can be 128 KiB of
/// markup (a preset's is generated, a custom one the agent wrote itself).
#[derive(Serialize)]
struct SvgOverlaySummary {
    id: Uuid,
    name: String,
    /// `custom`, or the preset's kind (`fade_up`, `typewriter`, `lower_third`, `word_pop`). A title
    /// made from a template is `custom` and has `template`.
    kind: &'static str,
    start: f64,
    end: f64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    opacity: f64,
    /// What a preset title was filled with; absent on a custom one.
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<SvgPresetParams>,
    /// The template a title came from, with the controls it has and what they hold now; absent on a
    /// preset or a custom title.
    #[serde(skip_serializing_if = "Option::is_none")]
    template: Option<TemplateSummary>,
    svg_bytes: usize,
}

/// What a title remembers of its template, as an agent needs it to call `set_template_values`: the
/// controls without the elements they point at.
#[derive(Serialize)]
struct TemplateSummary {
    template_id: String,
    name: String,
    controls: Vec<ControlSummary>,
    /// Each control's current value; an `image` control has none (its picture is in the artwork).
    values: std::collections::BTreeMap<String, String>,
}

#[derive(Serialize)]
struct ControlSummary {
    id: String,
    label: String,
    kind: kerf_core::TemplateKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max: Option<f64>,
}

impl From<&kerf_core::TemplateInstance> for TemplateSummary {
    fn from(t: &kerf_core::TemplateInstance) -> Self {
        Self {
            template_id: t.template_id.clone(),
            name: t.name.clone(),
            controls: t
                .controls
                .iter()
                .map(|c| ControlSummary {
                    id: c.id.clone(),
                    label: c.label.clone(),
                    kind: c.kind,
                    min: c.min,
                    max: c.max,
                })
                .collect(),
            values: t.values.clone(),
        }
    }
}

impl From<&SvgOverlay> for SvgOverlaySummary {
    fn from(o: &SvgOverlay) -> Self {
        Self {
            id: o.id,
            name: o.display_name(),
            kind: o.preset.as_ref().map_or("custom", |p| p.kind.as_str()),
            start: o.start,
            end: o.end,
            x: o.x,
            y: o.y,
            width: o.width,
            height: o.height,
            opacity: o.opacity,
            params: o.preset.as_ref().map(|p| p.params.clone()),
            template: o.template.as_ref().map(TemplateSummary::from),
            svg_bytes: o.svg.len(),
        }
    }
}

/// What `add_svg_overlay` / `update_svg_overlay` answer.
#[derive(Serialize)]
struct SvgOverlayOutcome {
    overlay: SvgOverlaySummary,
    /// What the sanitiser or the SMIL reader dropped or could not honour.
    warnings: Vec<String>,
}

impl From<SvgOverlayResult> for SvgOverlayOutcome {
    fn from(r: SvgOverlayResult) -> Self {
        Self {
            overlay: SvgOverlaySummary::from(&r.overlay),
            warnings: r.warnings,
        }
    }
}

/// `svg` xor `preset` + `params`, as the one source `add_svg_overlay` takes.
fn svg_source(
    svg: Option<String>,
    preset: Option<SvgPresetKind>,
    params: Option<SvgPresetParams>,
) -> Result<SvgSource, McpError> {
    let bad = |msg: &str| McpError::invalid_params(msg.to_string(), None);
    match (svg, preset, params) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(bad("pass either `svg` or `preset` with `params`, not both")),
        (Some(svg), None, None) => Ok(SvgSource::Custom(svg)),
        (None, Some(kind), Some(params)) => Ok(SvgSource::Preset(SvgPreset { kind, params })),
        (None, Some(_), None) => Err(bad("a preset needs `params` (at least `text`)")),
        (None, None, Some(_)) => Err(bad("`params` fill a `preset`; name one (see list_svg_presets)")),
        (None, None, None) => Err(bad("pass `svg` (your own document) or a `preset` with `params`")),
    }
}

/// What `validate_svg` answers. No source: the agent has it, and sanitising only removes things.
#[derive(Serialize)]
struct SvgValidation {
    /// The size of the sanitised document, the part that is stored (limit 128 KiB).
    svg_bytes: usize,
    /// What the sanitiser or the SMIL reader would drop or could not honour.
    warnings: Vec<String>,
    /// When every animation has finished, in seconds after the title appears; null for a still
    /// or a document that loops forever.
    natural_duration: Option<f64>,
}

impl From<SvgCheck> for SvgValidation {
    fn from(c: SvgCheck) -> Self {
        Self {
            svg_bytes: c.svg.len(),
            warnings: c.warnings,
            natural_duration: c.natural_duration,
        }
    }
}

/// Which moments of a title a preview sheet shows.
#[derive(Debug, PartialEq)]
enum SvgMoments {
    /// These seconds after the title appears.
    At(Vec<f64>),
    /// This many, evenly from the first frame to the end.
    Spread(usize),
}

/// `times` xor `count` as the moments of a preview sheet. A list the sheet cannot hold is refused
/// rather than cut short, because the caption would then not match what was asked for.
fn svg_moments(times: Option<Vec<f64>>, count: Option<u32>) -> Result<SvgMoments, McpError> {
    let bad = |msg: String| McpError::invalid_params(msg, None);
    let most = kerf_core::svg::raster::MAX_SHEET_CELLS;
    match (times, count) {
        (Some(_), Some(_)) => Err(bad("pass either `times` or `count`, not both".to_string())),
        (Some(times), None) => {
            if times.is_empty() || times.len() > most {
                return Err(bad(format!("`times` takes 1 to {most} moments, got {}", times.len())));
            }
            if let Some(t) = times.iter().find(|t| !t.is_finite() || **t < 0.0) {
                return Err(bad(format!("`times` are seconds after the title appears, not {t}")));
            }
            Ok(SvgMoments::At(times))
        }
        (None, count) => Ok(SvgMoments::Spread((count.unwrap_or(6) as usize).clamp(1, most))),
    }
}

/// The moments of a title that runs `span` seconds, in seconds after it appears.
fn svg_sheet_times(moments: &SvgMoments, span: f64) -> Vec<f64> {
    match moments {
        SvgMoments::At(times) => times.clone(),
        SvgMoments::Spread(1) => vec![span / 2.0],
        SvgMoments::Spread(n) => (0..*n).map(|i| span * i as f64 / (*n - 1) as f64).collect(),
    }
}

/// The text beside a preview sheet: what it shows and which moment each cell is.
fn svg_sheet_caption(label: &str, times: &[f64], offset: f64, warnings: &[String]) -> String {
    let mut out = format!(
        "{label}: {} moment{} (row-major), cell -> seconds after it appears:",
        times.len(),
        if times.len() == 1 { "" } else { "s" }
    );
    for (i, t) in times.iter().enumerate() {
        out.push_str(&format!("\n  cell {}: {t:.2}s", i + 1));
    }
    if offset > 0.0 {
        out.push_str(&format!(
            "\nthis title was cut into, so its animation is {offset:.2}s further on than these times: the labels on the image are that clock"
        ));
    }
    if !warnings.is_empty() {
        out.push_str(&format!("\nwarnings: {}", warnings.join("; ")));
    }
    out
}

/// A title to look at: what the agent named, with how long it runs and how far in it starts.
struct SvgPreviewTitle {
    svg: String,
    /// How it is introduced in the caption.
    label: String,
    /// Seconds it is on screen; `None` for a draft, which runs for the length `add_svg_overlay` would give it.
    span: Option<f64>,
    /// Seconds of the document's own clock already elapsed when it appears.
    offset: f64,
    /// A draft is read for the first time here, so what the sanitiser dropped is news.
    report_warnings: bool,
}

/// Draw the contact sheet of `title` at `moments` into a `frame`-shaped cell `cell_w` wide:
/// the PNG and its caption. Reads the document (so it belongs on the blocking pool) and touches
/// no project.
fn svg_preview(
    title: &SvgPreviewTitle,
    moments: &SvgMoments,
    cell_w: u32,
    frame: (u32, u32),
) -> Result<(Vec<u8>, String), McpError> {
    let doc = SvgDoc::parse(&title.svg).map_err(core_err)?;
    let span = title.span.unwrap_or_else(|| doc.default_span());
    let times = svg_sheet_times(moments, span);
    let clock: Vec<f64> = times.iter().map(|t| t + title.offset).collect();
    let png = kerf_core::svg::raster::contact_sheet(&doc, &clock, cell_w, frame).map_err(core_err)?;
    let warnings = if title.report_warnings { doc.warnings() } else { &[] };
    Ok((png, svg_sheet_caption(&title.label, &times, title.offset, warnings)))
}

/// Leave the sources of large SVG titles out of a timeline read: a title can carry 128 KiB and a
/// timeline 50 of them. The note says how to get the source.
fn elide_large_svg(timeline: &mut Timeline) {
    for o in &mut timeline.svg_overlays {
        if o.svg.len() > SVG_INLINE_LIMIT {
            o.svg = format!(
                "[{} bytes of SVG left out; call get_timeline_state with full_svg=true to read it]",
                o.svg.len()
            );
        }
    }
}

#[derive(Serialize)]
struct TimelineSummary {
    total_duration_secs: f64,
    track_count: usize,
    total_clip_count: usize,
    tracks: Vec<TrackSummary>,
    /// The animated SVG titles, in draw order, without their source. Absent when there are none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    svg_overlays: Vec<SvgOverlaySummary>,
    /// The frame this cut is being made for, e.g. "1080x1920 (cover)". Absent
    /// when the project follows its footage's shape. Worth reading before
    /// framing anything: under a cover delivery the crop decides what survives.
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery_format: Option<String>,
    /// Set while edits are staged: this summary describes the **proposal**, not
    /// the cut the user is looking at. `staged_diff` spells out the difference.
    #[serde(skip_serializing_if = "Option::is_none")]
    staged_changes: Option<usize>,
    /// The media this cut uses whose preview proxy is not ready, or whose source file is gone.
    /// Absent when everything is ready.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    media_notes: Vec<reads::MediaNote>,
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
                       resolution, fps, codecs). `path` must be an absolute path to a media file on this computer \
                       (a URL, an ffmpeg protocol such as concat: or pipe:, a network share (UNC path), a relative \
                       path or a missing file is refused at once). Importing a file the project already holds resolves to that \
                       asset instead of duplicating it, so this is safe to re-run. One Insta360 lens file pulls \
                       in its sibling and stitches the pair into a single 360 asset — a full re-encode that can \
                       run for minutes (send a `progressToken` for progress, cancel the request to stop it, or pass \
                       `background: true` to run it as a background job). An import is NOT part of your staged proposal: media lands for the user \
                       immediately, because a file on disk is not an edit to their cut."
    )]
    async fn import_asset(
        &self,
        Parameters(p): Parameters<ImportParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        // Absolute, an existing regular file, no URL, ffmpeg protocol or network share: said now, on
        // the call, not by a background job a moment later. The network check goes first, before any
        // stat: opening a UNC path makes this machine authenticate to the host it names.
        crate::require_local_input_path(&p.path).map_err(|e| McpError::invalid_params(e, None))?;
        kerf_core::paths::validate_import_path(std::path::Path::new(&p.path)).map_err(core_err)?;
        let me = self.clone();
        let owner = self.owner();
        let generation = self.lock().generation();
        // The GUI's Stop on an import ends an agent's import too.
        let gui_stop = self.app.state::<crate::AppState>().begin_import();
        self.long_tool("import_asset", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let (project, app) = (me.project.clone(), me.app.clone());
            let path = p.path;
            let asset = blocking(move || {
                // Probe (and, for a lens pair, stitch) with the lock released, taking
                // it only for the quick insert — the same shape as the GUI's import,
                // so a multi-minute stitch never freezes the user's editing.
                let mut on_progress = |pr: kerf_core::ExportProgress| {
                    let _ = app.emit("import-progress", crate::ImportProgress::new(&path, pr));
                    run.progress(Some(pr.fraction), "stitching the 360 pair");
                };
                let probed = Project::probe_import_with(std::path::Path::new(&path), &mut on_progress, &|| run.cancelled())
                    .map_err(core_err)?;
                let asset = {
                    let guard = lock_agent_as(&project, owner.as_deref());
                    // Imported into the project it was asked of, or not at all.
                    require_generation(&guard, generation, "it was being imported")?;
                    guard.insert_or_get_asset(&probed).map_err(core_err)?
                };
                // Preview decodes come off a proxy; queue it now so the first frame anyone asks for
                // is not a seek into a long GOP. It takes its place in front of any analysis the
                // agent starts next (`kerf_core::proxy`), and may run a cached ffprobe, which is why
                // it is here on the blocking pool and not in the async tool body.
                crate::spawn_proxy(&app, &asset);
                Ok(asset)
            })
            .await?;
            me.changed();
            json(&asset)
        })
        .await
    }

    #[tool(
        description = "The preview proxy of one asset (asset_id) or of every asset: `state` is ready, building (with \
                       `fraction` 0..1 and `eta_secs`), queued, failed (with the `reason`), missing, off (proxies \
                       are off, previews use the original, or the user deleted this one) or not_needed (a still or an \
                       audio-only file). Previews decode the original until the proxy is ready; export always reads \
                       the original. A proxy is built before any analysis waiting for the machine."
    )]
    async fn proxy_status(&self, Parameters(p): Parameters<ProxyStatusParams>) -> Result<String, McpError> {
        let wanted = p.asset_id.as_deref().map(|s| parse_arg("asset_id", s)).transpose()?;
        let project = self.project.clone();
        let statuses = blocking(move || {
            let mut inputs = lock_agent(&project).proxy_inputs().map_err(core_err)?;
            if let Some(id) = wanted {
                inputs.retain(|i| i.asset.id == id);
                if inputs.is_empty() {
                    return Err(core_err(kerf_core::Error::AssetNotFound(id)));
                }
            }
            Ok(kerf_core::proxy::statuses(&inputs))
        })
        .await?;
        json(&statuses)
    }

    #[tool(
        description = "Which analyses have run for one asset (asset_id) or for every asset: per kind (silence, scenes, \
                       loudness, rhythm, transcript) `state` is done, not_run, running, failed (with the `reason`) or \
                       off (switched off in Settings › Analysis, or no speech-to-text backend). Use it to see what \
                       analyze_asset still has to do before an edit that reads the result."
    )]
    async fn analysis_status(&self, Parameters(p): Parameters<ProxyStatusParams>) -> Result<String, McpError> {
        let wanted = p.asset_id.as_deref().map(|s| parse_arg("asset_id", s)).transpose()?;
        let (project, owner) = (self.project.clone(), self.owner());
        let statuses = blocking(move || {
            // The transcription backend may need an ffmpeg start the first time it is asked: before
            // the lock, never inside it.
            crate::warm_transcription_backend();
            let project = lock_agent_as(&project, owner.as_deref());
            match wanted {
                Some(id) => {
                    project.require_asset(id).map_err(core_err)?;
                    Ok(vec![project.analysis_status(id).map_err(core_err)?])
                }
                None => project.analysis_statuses().map_err(core_err),
            }
        })
        .await?;
        json(&statuses)
    }

    #[tool(
        description = "Build an asset's preview proxy again from the original: the current one (and any build in \
                       progress) is dropped first, and a proxy the user had deleted is wanted again. Use it when \
                       previews of an asset look wrong or stale. Returns where the build starts; poll proxy_status \
                       for its progress. Not part of your staged proposal — a proxy is a cache, not an edit."
    )]
    async fn rebuild_proxy(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let (project, app) = (self.project.clone(), self.app.clone());
        let status = blocking(move || {
            crate::rebuild_proxy_now(&app, &|| lock_agent(&project), id).map_err(|e| McpError::invalid_params(e, None))
        })
        .await?;
        json(&status)
    }

    #[tool(
        description = "Delete an asset's preview proxy files (at every size) and stop any build of them, freeing \
                       the disk. Previews decode the original until it is rebuilt (rebuild_proxy), and it is not \
                       built again by itself on a later open. Returns the bytes freed and the asset's new status."
    )]
    async fn delete_proxy(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let (project, app) = (self.project.clone(), self.app.clone());
        let (freed, status) = blocking(move || {
            crate::delete_proxy_now(&app, &|| lock_agent(&project), id).map_err(|e| McpError::invalid_params(e, None))
        })
        .await?;
        json(&serde_json::json!({ "bytes_freed": freed, "proxy": status }))
    }

    #[tool(
        description = "Get the non-destructive timeline state (tracks and clips). A clip with `link_id` is \
                       linked to the other clips sharing it (a picture and its detached sound); one with \
                       `source_audio: false` is a picture whose own sound was detached and is silent — its \
                       sound is on the linked audio clip. Both fields are absent on an ordinary clip. Typed titles \
                       are in `overlays`; the GENERATED captions (generate_captions / import_captions — often hundreds) \
                       are not: `captions` is a window of them (`total`, `returned`, `next_offset`, `items`), moved with \
                       captions_offset / captions_limit, or pass `full: true` for everything inline. Animated SVG titles \
                       (`svg_overlays`) carry their SVG source here, but a source over 4 KiB is replaced by a note with \
                       its size (a title can be 128 KiB): pass `full_svg: true` (or `full`) to get every source whole. \
                       timeline_summary is much smaller (durations, gaps, proxies); preview_svg_overlay shows how a title looks."
    )]
    fn get_timeline_state(&self, Parameters(p): Parameters<TimelineStateParams>) -> Result<String, McpError> {
        let mut timeline = self.lock().working_timeline().map_err(core_err)?;
        let full = p.full.unwrap_or(false);
        if !(full || p.full_svg.unwrap_or(false)) {
            elide_large_svg(&mut timeline);
        }
        let view = reads::StateView {
            full,
            captions_offset: p.captions_offset,
            captions_limit: p.captions_limit,
            clips_offset: p.clips_offset,
            clips_limit: p.clips_limit,
        };
        json(&reads::timeline_state(&timeline, &view)?)
    }

    #[tool(
        description = "What the user is looking at in the Kerf window: `playhead` (timeline seconds), \
                       `selected_clip_ids`, `mark_in` / `mark_out` (the in/out marks, null when unset), \
                       `visible_range` (the timeline seconds on screen), `focused_panel` (timeline, preview, \
                       inspector, library, mixer, agent or deliver) and `selected_asset_id` (the library's \
                       pick). Call it when the user says \"this clip\", \"here\" or \"the selection\". The ids \
                       are the user's live timeline, which your staged edits may since have changed. \
                       `age_secs` says how long ago the window last reported; `stale` is true when it has \
                       stopped reporting, and `reported` is false when no window ever has (Kerf headless)."
    )]
    fn get_editor_context(&self) -> Result<String, McpError> {
        use tauri::Manager;
        match self.app.try_state::<crate::editor_context::EditorContextState>() {
            Some(state) => json(&state.read()),
            None => json(&crate::editor_context::EditorContextState::default().read()),
        }
    }

    #[tool(
        description = "Which speech-to-text backend this build of Kerf will use for transcription, and whether its model is downloaded yet. Call this when an asset's transcript comes back empty: it says whether transcription is unavailable (and why), or simply that the first run still has to fetch a model."
    )]
    fn transcription_status(&self) -> Result<String, McpError> {
        json(&kerf_core::transcription_status())
    }

    #[tool(
        description = "Download a speech-to-text model (whisper.cpp ggml) into Kerf's cache, so the next analyze_asset does not wait for it. Names, smallest first: tiny, tiny.en, base, base.en, small, small.en, medium, medium.en, large-v3-turbo — the plain names are multilingual, `.en` ones are English-only but more accurate on English. Bigger is slower and more accurate; `base` is the default. This only fills the cache: transcription keeps using whichever model is *selected*, so to transcribe with a model you name here, call set_speech_model too. The download is 75 MB to 1.6 GB and a no-op if the model is already cached; send a `progressToken` for progress (Kerf's own download bar moves too), cancel the request to stop it, or pass `background: true` and poll job_status."
    )]
    async fn download_speech_model(
        &self,
        Parameters(p): Parameters<DownloadModelParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let name = p.name.unwrap_or_else(|| kerf_core::DEFAULT_SPEECH_MODEL.to_string());
        let known = kerf_core::transcription_status().models;
        if !known.iter().any(|m| m.name == name) {
            return Err(McpError::invalid_params(
                format!(
                    "unknown speech model '{name}'; expected one of: {}",
                    known.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")
                ),
                None,
            ));
        }
        let app = self.app.clone();
        // The GUI's Cancel on the model download ends an agent's download too.
        let gui_stop = app.state::<crate::AppState>().begin_model_download();
        self.long_tool("download_speech_model", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let model = name.clone();
            let path = blocking(move || {
                let mut on_progress = |d: kerf_core::DownloadProgress| {
                    let fraction = d.fraction();
                    // The same event the GUI's own download streams, so its progress bar moves too.
                    let _ = app.emit(
                        "model-progress",
                        serde_json::json!({
                            "model": model,
                            "downloaded_bytes": d.downloaded,
                            "total_bytes": d.total,
                            "fraction": fraction,
                        }),
                    );
                    let mb = |bytes: u64| bytes as f64 / 1_048_576.0;
                    run.progress(
                        fraction,
                        match d.total {
                            Some(total) => format!("downloading {model}: {:.0} of {:.0} MB", mb(d.downloaded), mb(total)),
                            None => format!("downloading {model}: {:.0} MB", mb(d.downloaded)),
                        },
                    );
                };
                kerf_core::download_speech_model_cancellable(&model, &mut on_progress, &|| run.cancelled()).map_err(core_err)
            })
            .await?;
            json(&serde_json::json!({ "model": name, "path": path.to_string_lossy() }))
        })
        .await
    }

    #[tool(
        description = "Analyze an asset and cache the result: silence, scene changes, EBU R128 loudness, rhythm \
                       (onsets, tempo/beat grid, speech-vs-music class) and speech-to-text transcript. Pass `steps` to \
                       run only some of them — the results are merged into what is cached and the other kinds are left \
                       alone, so run just what the next edit needs. Omit `steps` for the analyses the user has \
                       switched on in Settings. A cancelled or failed step caches nothing; a step that fails does not \
                       stop the others (see `failed` and `analysis_status` in the result). Transcribing downloads a \
                       speech model on first use (~148 MB) and then runs for a good fraction of the media's duration, so \
                       expect that step to be slow; see transcription_status if the transcript comes back empty. A \
                       preview proxy that is queued is built first. Send a `progressToken` for progress (the step and how \
                       far it is), cancel the request — or press Stop in Kerf — to stop it, or pass `background: true` and \
                       poll job_status. The result is a SUMMARY (counts, durations, the head of the transcript), not the \
                       lists: read those with get_transcript and get_analysis_detail."
    )]
    async fn analyze_asset(
        &self,
        Parameters(p): Parameters<AnalyzeParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let steps = crate::parse_steps(p.steps).map_err(|e| McpError::invalid_params(e, None))?;
        // Nothing to run (everything switched off, a silent file asked for its speech) is a bad
        // request, said so before anything starts.
        let generation = {
            let project = self.lock();
            let asset = project.require_asset(id).map_err(core_err)?;
            kerf_core::resolve_steps(&asset, steps.as_deref()).map_err(core_err)?;
            project.generation()
        };
        // The GUI's Stop button stops an agent's analysis too: this run polls the same stop the
        // GUI's own does (`begin_analysis`), and a Stop pressed before it began does not end it.
        let gui_stop = self.app.state::<crate::AppState>().begin_analysis();
        let (me, owner) = (self.clone(), self.owner());
        self.long_tool("analyze_asset", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let (project, app) = (me.project.clone(), me.app.clone());
            let outcome = blocking(move || {
                // Heavy ffmpeg analysis with the lock released, re-locking only to merge each
                // finished step into the cache (`run_analysis`) — so analysis doesn't freeze the
                // GUI or stall other tools for its whole (multi-second) duration.
                let _progress = AnalysisProgressRelay::new(&app, id, &run);
                let outcome = crate::run_analysis(
                    &app,
                    &|| lock_agent_as(&project, owner.as_deref()),
                    id,
                    steps.as_deref(),
                    &|| run.cancelled(),
                )
                .map_err(|e| McpError::internal_error(e, None))?;
                // Whatever finished was merged as it did; a project swapped in meanwhile is not
                // told it was analysed.
                require_generation(
                    &lock_agent_as(&project, owner.as_deref()),
                    generation,
                    "it was being analysed",
                )?;
                Ok(outcome)
            })
            .await?;
            me.changed();
            json(&AnalyzeResult {
                analysis: reads::AnalysisSummary::of(&outcome.analysis),
                analysis_status: outcome.status,
                failed: outcome
                    .failed
                    .into_iter()
                    .map(|(step, reason)| FailedStep { step, reason })
                    .collect(),
            })
        })
        .await
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

    #[tool(
        description = "Cut [start, end) of an asset (SOURCE seconds) and append it to the first track of the asset's main kind — the first video track for footage, the first audio track for audio-only media — after the last clip already there. It does not take a track or a position: add_clip_to_timeline does. Returns the clip."
    )]
    fn cut_clip(&self, Parameters(p): Parameters<CutClipParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        self.edit(|project| {
            let out = project.cut_clip(id, p.start, p.end).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Add a clip referencing a source range of an asset to the timeline. Appended after the \
                       track's last clip unless timeline_start says where. In ripple mode (get_ripple_mode, or \
                       pass `ripple`) a clip dropped onto footage that is already there pushes it later."
    )]
    fn add_clip_to_timeline(&self, Parameters(p): Parameters<AddClipParams>) -> Result<String, McpError> {
        let asset_id = parse_arg("asset_id", &p.asset_id)?;
        let track_id = p.track_id.as_deref().map(|s| parse_arg("track_id", s)).transpose()?;
        self.edit(|project| {
            let out = project
                .with_ripple(p.ripple, |project| {
                    project.add_clip_to_timeline(asset_id, track_id, p.source_in, p.source_out, p.timeline_start)
                })
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Split a timeline clip at a timeline time into two adjacent clips. Linked clips are split \
                       with it (a picture's detached sound is cut at the same moment, and the new halves are \
                       linked to each other); pass link=false to split only this clip."
    )]
    fn split_at(&self, Parameters(p): Parameters<SplitParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let (left, right) = project
                .with_links(p.link, |project| {
                    project.with_ripple(p.ripple, |project| project.split_at(clip_id, p.at))
                })
                .map_err(core_err)?;
            json(&serde_json::json!({ "left": left, "right": right }))
        })
    }

    #[tool(
        description = "Trim a clip's source in/out points (timeline position preserved unless timeline_start is \
                       passed). In ripple mode (get_ripple_mode, or pass `ripple`) the later clips on the track \
                       follow the change in the clip's length and a left-edge trim keeps the clip's start. \
                       A linked partner that shares the edge you move (its detached sound or picture) has \
                       that edge moved with it — and the trim is refused if that would put the partner on a clip \
                       outside its group (as move_clip refuses), unless ripple makes room; pass link=false to \
                       trim only this clip. Returns the clip as it ended up."
    )]
    fn trim(&self, Parameters(p): Parameters<TrimParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project
                .with_links(p.link, |project| {
                    project.with_ripple(p.ripple, |project| {
                        project.trim(clip_id, p.source_in, p.source_out, p.timeline_start)
                    })
                })
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Move a clip to a new index within its track (re-flows the track gaplessly). The re-flow \
                       re-lays this one track and the clips linked to what it moved follow (a picture's sound goes where \
                       its picture went), unless an unlinked clip on their track is in the way, which refuses it; \
                       link=false is not offered here — use move_clip to reposition one clip."
    )]
    fn reorder(&self, Parameters(p): Parameters<ReorderParams>) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            project.reorder(track_id, clip_id, p.new_index).map_err(core_err)?;
            Ok("ok".to_string())
        })
    }

    #[tool(
        description = "Move a clip to a new timeline position (free positioning, gaps allowed), optionally onto another \
                       same-kind track; rejects overlaps. Linked clips move with it by the same time, each on its \
                       own track (a picture's detached sound follows it; a track change applies to the clip you \
                       named only); pass link=false to move only this clip. All or nothing — a partner that would \
                       start before 0, land on another clip or sit on a locked track refuses the move."
    )]
    fn move_clip(&self, Parameters(p): Parameters<MoveClipParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let track_id = p.track_id.as_deref().map(|s| parse_arg("track_id", s)).transpose()?;
        self.edit(|project| {
            let out = project
                .with_links(p.link, |project| project.move_clip(clip_id, p.timeline_start, track_id))
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Move several clips in ONE edit (one revision, one undo step): each entry names a clip, \
                       where it starts afterwards (absolute timeline seconds, not an offset) and optionally \
                       another track of the same kind. All or nothing — an unknown clip, a start before 0, a \
                       clip named twice, a locked track or a landing spot that overlaps a clip that stays (or \
                       another clip in the group) refuses the whole call and changes nothing. Clips moving \
                       together may pass through the places they are leaving, so nudging a run of abutting \
                       clips by a second is fine, and two clips can swap. Never ripples, whatever the ripple \
                       mode. Linked clips of the ones named move with them (by the same time, on their own \
                       tracks) unless link=false, and are checked in the same group. Returns the moved clips in \
                       the order given, then the linked partners carried along."
    )]
    fn move_clips(&self, Parameters(p): Parameters<MoveClipsParams>) -> Result<String, McpError> {
        let moves = clip_moves(&p.moves)?;
        self.edit(|project| {
            let out = project
                .with_links(p.link, |project| project.move_clips(&moves))
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Remove a clip and close the gap: later clips on the same track shift left by its duration \
                       (ripple delete). Linked clips are removed too, each closing the gap on its own track; pass \
                       link=false to remove only this clip."
    )]
    fn ripple_delete(&self, Parameters(p): Parameters<RippleDeleteParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            project
                .with_links(p.link, |project| project.ripple_delete(clip_id))
                .map_err(core_err)?;
            Ok("ok".to_string())
        })
    }

    #[tool(
        description = "Cut a source-time range out of a clip and close the gap (split + ripple in one edit) — \
                       the transcript-editing primitive: pass a transcript segment's start/end to delete that \
                       sentence from the cut, or a word's (get_transcript, find_filler_words) to delete one word. Linked clips lose the same stretch of timeline (a picture's \
                       detached sound is cut with it); pass link=false to cut only this clip."
    )]
    fn cut_clip_range(&self, Parameters(p): Parameters<CutClipRangeParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let pieces = project
                .with_links(p.link, |project| project.cut_clip_range(clip_id, p.from, p.to))
                .map_err(core_err)?;
            json(&pieces)
        })
    }

    #[tool(
        description = "ROLL an edit: move the cut between two adjacent clips of one track (clip_a ends where clip_b \
                       begins) by `delta` seconds, positive = later. Both clips' footage shifts with it — clip_a's end \
                       and clip_b's start move together — so the pair covers the same stretch of timeline and nothing \
                       after it moves. Use it to adjust where a cut falls without touching the length of the cut. \
                       Clamps (does not fail) to the footage each clip has left and to a 0.05s floor for the clip that \
                       shrinks; `applied` in the reply says how far it went and `clamped` whether that is short of \
                       `delta`. Errors if the clips are not adjacent, on different tracks, in the wrong order, or the \
                       track is locked. One revision. Never ripples, whatever the ripple mode. The cut of each \
                       linked partner pair that shares this one rolls too (clamped to the tightest) unless link=false."
    )]
    fn roll_edit(&self, Parameters(p): Parameters<RollEditParams>) -> Result<String, McpError> {
        let (a, b) = (parse_arg("clip_a", &p.clip_a)?, parse_arg("clip_b", &p.clip_b)?);
        self.edit(|project| {
            let out = project
                .with_links(p.link, |project| project.roll_edit(a, b, p.delta))
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "SLIP a clip: show a different part of its footage without moving it or changing its length — \
                       the clip keeps its place and duration on the timeline and only its source window shifts. \
                       `delta` is in SOURCE seconds; positive makes the clip start later in its own footage, negative \
                       earlier (a reversed clip is mirrored for you). Clamps to the asset's footage — `applied` says how \
                       far it went — and errors when there is none left, for a still image (nothing to slip) or a \
                       locked track. Keyframes and fades are timed to the clip, so they stay put. One revision. Never \
                       ripples. Linked partners slip by the same moment of footage unless link=false."
    )]
    fn slip_clip(&self, Parameters(p): Parameters<SlipClipParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project
                .with_links(p.link, |project| project.slip_clip(clip_id, p.delta))
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "SLIDE a clip along its track, keeping its content: it moves by `delta` timeline seconds \
                       (positive = later) while the clips touching it give way — the previous clip's end and the next \
                       clip's start move by the same amount, so the clip's footage and the overall length of the three \
                       are unchanged. A neighbour across a gap is not touched: with a gap on one side only the touching \
                       neighbour is trimmed (or extended), and the clip stops where it would meet a clip it is not \
                       touching. The last clip of a track has no next clip to give way, so sliding it later extends the \
                       track. Clamps (does not fail) to the neighbours' footage and a 0.05s floor, and to 0 at the start \
                       of the track; `applied` says how far it went. Errors if nothing can move or the track is locked. \
                       One revision. Never ripples. Linked partners slide with it, each with its own neighbours \
                       giving way and the whole slide clamped to the tightest, unless link=false."
    )]
    fn slide_clip(&self, Parameters(p): Parameters<SlideClipParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project
                .with_links(p.link, |project| project.slide_clip(clip_id, p.delta))
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Split a clip at a timeline time and REMOVE one half in a single edit — trim the start (side \
                       \"left\") or the end (side \"right\") to that time. The half that stays keeps the clip's id, and \
                       loses what belonged to the removed half: removing the left drops fade_in and the transition into \
                       it, removing the right drops fade_out. In ripple mode (get_ripple_mode, or pass `ripple`) the \
                       later clips on the track close the gap; otherwise the gap stays. Linked partners that span \
                       `at` are trimmed to it as well (link=false: this clip only) — but only the named clip comes \
                       back: read the partners from get_timeline_state, or use split_remove_clips, which returns \
                       every clip it cut. Returns the clip that remains. \
                       Errors if `at` is not inside the clip or would leave under 0.05s, or the track is locked."
    )]
    fn split_remove(&self, Parameters(p): Parameters<SplitRemoveParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project
                .with_links(p.link, |project| {
                    project.with_ripple(p.ripple, |project| project.split_remove(clip_id, p.at, p.side))
                })
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Split and REMOVE one half of SEVERAL clips as ONE edit (one revision, one undo step) — trim \
                       the starts (side \"left\") or the ends (side \"right\") of, say, a video clip and its audio \
                       to the same time. Each cut is split_remove's: the half that stays keeps its clip's id and \
                       loses what belonged to the removed half. All or nothing — an unknown clip, a locked track, a \
                       cut outside its clip or one that would leave under 0.05s refuses the whole call and changes \
                       nothing — and at most one clip per track. In ripple mode (get_ripple_mode, or pass `ripple`) \
                       each track closes its own gap. A linked partner of a named clip that you did not name is cut \
                       at the same time when the time is inside it (link=false: exactly what you named). Returns \
                       the clips that remain, in the order given, then the partners'."
    )]
    fn split_remove_clips(&self, Parameters(p): Parameters<SplitRemoveClipsParams>) -> Result<String, McpError> {
        let cuts = clip_cuts(&p.cuts)?;
        self.edit(|project| {
            let out = project
                .with_links(p.link, |project| {
                    project.with_ripple(p.ripple, |project| project.split_remove_clips(&cuts, p.side))
                })
                .map_err(core_err)?;
            json(&out)
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
        let track_id = parse_arg("track_id", &p.track_id)?;
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
        let track_id = parse_arg("track_id", &p.track_id)?;
        self.edit(|project| {
            let track = project.set_track_duck(track_id, p.duck).map_err(core_err)?;
            json(&reads::track_brief(&track))
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
                       the right thing. To make it follow something, key its numbers with \
                       set_property_keyframes (mask_x, mask_y, mask_width, mask_height, mask_feather): a \
                       keyed number is read from its keys, not from this static value, and clearing the \
                       mask clears its keys.")]
    fn set_mask(&self, Parameters(p): Parameters<SetMaskParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
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
        let track_id = parse_arg("track_id", &p.track_id)?;
        self.edit(|project| {
            let track = project.set_track_volume(track_id, p.volume).map_err(core_err)?;
            json(&reads::track_brief(&track))
        })
    }

    #[tool(
        description = "Place a track in the stereo field, -1 hard left to 1 hard right. A balance, so a \
                       panned track never gets louder, and a no-op on a mono delivery. Use it sparingly — \
                       a hard-panned music bed sounds broken on a phone speaker, which is what most of \
                       this footage is watched on."
    )]
    fn set_track_pan(&self, Parameters(p): Parameters<SetTrackPanParams>) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        self.edit(|project| {
            let track = project.set_track_pan(track_id, p.pan).map_err(core_err)?;
            json(&reads::track_brief(&track))
        })
    }

    #[tool(
        description = "Set the master fader — one linear gain on the finished mix, after every track and the duck \
                       bus and before loudnorm. Use it to bring the whole cut down (or up) without touching a \
                       track: when get_levels says the mix is hot, this and set_master_limiter are the fixes, and \
                       a quiet cut is raised here or by exporting with loudnorm. Clamped to 0..4 (+12 dB). \
                       Returns the master bus."
    )]
    fn set_master_volume(&self, Parameters(p): Parameters<SetMasterVolumeParams>) -> Result<String, McpError> {
        self.edit(|project| {
            let master = project.set_master_volume(p.volume).map_err(core_err)?;
            json(&master)
        })
    }

    #[tool(
        description = "Switch the master limiter on or off, optionally moving its ceiling (dBFS, default -1.5). A \
                       lookahead limiter on the finished mix that holds the loudest peaks under the ceiling \
                       instead of letting them clip — turn it on for a cut destined for a platform, which asks for \
                       a true peak under -1 dBTP. It is a sample-peak ceiling unless set_master_true_peak is on: the \
                       true peak between samples can read above it (up to about 1 dB on a pure high tone), which is \
                       why the default sits half a decibel under that line. Confirm the true peak with get_levels, and \
                       if it is still over -1 dBTP with the limiter on, turn on set_master_true_peak or lower the \
                       ceiling by the overshoot plus half a decibel (the notes say to what) instead of switching it on \
                       again. A limiter has nothing to do on a mix \
                       already under its ceiling and leaves it alone. Returns the master bus."
    )]
    fn set_master_limiter(&self, Parameters(p): Parameters<SetMasterLimiterParams>) -> Result<String, McpError> {
        self.edit(|project| {
            let master = project.set_master_limiter(p.enabled, p.ceiling_db).map_err(core_err)?;
            json(&master)
        })
    }

    #[tool(
        description = "Make the master limiter (set_master_limiter) hold the true peak instead of only the sample \
                       peak: it then runs on the mix upsampled four times, so the peaks between samples — what \
                       get_levels and the platforms measure as dBTP — stay under its ceiling too, and a ceiling of \
                       -1 really means -1 dBTP. Costs a little render time; does nothing while the limiter is off \
                       (the setting is kept). Returns the master bus."
    )]
    fn set_master_true_peak(&self, Parameters(p): Parameters<SetMasterTruePeakParams>) -> Result<String, McpError> {
        self.edit(|project| {
            let master = project.set_master_true_peak(p.enabled).map_err(core_err)?;
            json(&master)
        })
    }

    #[tool(
        description = "Choose how ducked tracks (set_track_duck) dip under the rest of the mix on export. With \
                       depth_db, a speech gate: whenever the other tracks speak the ducked tracks drop by exactly \
                       that much (ramping down in ~50 ms and back ~300 ms after the speech stops) — -12 is a good \
                       music bed under dialogue. Without it, the default sidechain compressor, which dips by how loud \
                       the speech is. Returns the master bus."
    )]
    fn set_master_duck(&self, Parameters(p): Parameters<SetMasterDuckParams>) -> Result<String, McpError> {
        self.edit(|project| {
            let master = project.set_master_duck(p.depth_db).map_err(core_err)?;
            json(&master)
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
            json(&serde_json::json!({ "format": timeline.format, "timeline": reads::timeline_brief(&timeline) }))
        })
    }

    #[tool(
        description = "Drop a named marker on the timeline. Markers render nothing — they are shared \
                       vocabulary for places in the cut, so 'the laugh at 01:12' survives as something \
                       the user can see and jump to. Good for reporting findings from skim_asset."
    )]
    fn add_marker(&self, Parameters(p): Parameters<AddMarkerParams>) -> Result<String, McpError> {
        let patch = kerf_core::MarkerPatch {
            color: p.color,
            duration: p.duration,
            comment: p.comment,
            kind: p.kind,
            ..kerf_core::MarkerPatch::default()
        };
        self.edit(|project| {
            let marker = project.add_marker_with(p.time, p.name, patch).map_err(core_err)?;
            json(&marker)
        })
    }

    #[tool(
        description = "Move, rename, recolor or re-kind a marker, or set its duration or comment; omitted fields are left alone"
    )]
    fn update_marker(&self, Parameters(p): Parameters<UpdateMarkerParams>) -> Result<String, McpError> {
        let marker_id = parse_arg("marker_id", &p.marker_id)?;
        let patch = kerf_core::MarkerPatch {
            time: p.time,
            name: p.name,
            color: p.color,
            duration: p.duration,
            comment: p.comment,
            kind: p.kind,
        };
        self.edit(|project| {
            let marker = project.edit_marker(marker_id, patch).map_err(core_err)?;
            json(&marker)
        })
    }

    #[tool(description = "Remove a marker from the timeline")]
    fn remove_marker(&self, Parameters(p): Parameters<MarkerIdParams>) -> Result<String, McpError> {
        let marker_id = parse_arg("marker_id", &p.marker_id)?;
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
        let track_id = parse_arg("track_id", &p.track_id)?;
        self.edit(|project| {
            let track = project.set_track_muted(track_id, p.muted).map_err(core_err)?;
            json(&reads::track_brief(&track))
        })
    }

    #[tool(
        description = "Solo (or unsolo) a track. While any track of a kind is soloed, the other tracks of \
                       that kind stop rendering; video and audio solo independently, so soloing a music \
                       bed does not blank the picture. Several tracks may be soloed at once."
    )]
    fn set_track_solo(&self, Parameters(p): Parameters<SetTrackSoloParams>) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        self.edit(|project| {
            let track = project.set_track_solo(track_id, p.solo).map_err(core_err)?;
            json(&reads::track_brief(&track))
        })
    }

    #[tool(
        description = "Lock (or unlock) a track against editing. A locked track still renders; the GUI \
                       refuses to drag, trim or split its clips."
    )]
    fn set_track_locked(&self, Parameters(p): Parameters<SetTrackLockedParams>) -> Result<String, McpError> {
        let track_id = parse_arg("track_id", &p.track_id)?;
        self.edit(|project| {
            let track = project.set_track_locked(track_id, p.locked).map_err(core_err)?;
            json(&reads::track_brief(&track))
        })
    }

    #[tool(
        description = "Enable or disable one clip. A disabled clip keeps its position, trims, effects and \
                       keyframes but drops out of the render — the reversible way to try a cut without it. \
                       A picture whose sound was detached (`source_audio: false`) carries no sound: disabling it \
                       leaves its linked audio clip playing, so disable that clip too."
    )]
    fn set_clip_enabled(&self, Parameters(p): Parameters<SetClipEnabledParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
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
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<_>, _>>()?;
        self.edit(|project| {
            let clips = project.duplicate_clips(&ids, p.at).map_err(core_err)?;
            json(&clips)
        })
    }

    #[tool(
        description = "Remove a clip from the timeline. Leaves a gap where it was, unless the project is in ripple \
                       mode (get_ripple_mode) or `ripple` is true, which closes it. Linked clips are removed with \
                       it (a picture's detached sound goes too) unless link=false."
    )]
    fn remove(&self, Parameters(p): Parameters<RemoveClipParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            project
                .with_links(p.link, |project| {
                    project.with_ripple(p.ripple, |project| project.remove(clip_id))
                })
                .map_err(core_err)?;
            Ok("ok".to_string())
        })
    }

    #[tool(
        description = "Remove several clips in ONE edit (one revision, one undo step) — a multi-select delete. \
                       All or nothing: an unknown clip, or one on a locked track, refuses the whole call. Leaves \
                       gaps, unless the project is in ripple mode (get_ripple_mode) or `ripple` is true, which \
                       closes every track up behind what it lost (a multi-clip ripple delete). Linked partners of \
                       the named clips are removed too unless link=false. Returns \
                       `removed` (how many clips, partners included), `ripple_active` (whether ripple applied to this call) and, \
                       measured rather than assumed, `rippled` / `clips_shifted` (whether any later clip actually \
                       moved, and how many — ripple on with nothing after the removed clips moves nothing)."
    )]
    fn remove_clips(&self, Parameters(p): Parameters<RemoveClipsParams>) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<_>, _>>()?;
        self.edit(|project| {
            // What the agent sees is the cut it is building: the proposal while it has
            // one staged, the live timeline otherwise.
            let before = project.working_timeline().map_err(core_err)?;
            let (removed, ripple) = project
                .with_links(p.link, |project| {
                    project.with_ripple(p.ripple, |project| {
                        let ripple = project.ripple_active()?;
                        project.remove_clips(&ids).map(|removed| (removed, ripple))
                    })
                })
                .map_err(core_err)?;
            // Ripple on is a promise to try, not a result: a locked track never moves
            // and a lane the shift would leave overlapping is declined, silently.
            // So say what happened.
            let shifted = project.working_timeline().map_err(core_err)?.clips_moved_since(&before);
            json(&serde_json::json!({
                "removed": removed,
                "ripple_active": ripple,
                "rippled": shifted > 0,
                "clips_shifted": shifted,
            }))
        })
    }

    #[tool(
        description = "Whether the project is in ripple mode. In ripple mode an edit that changes how much footage \
                       sits ahead of a clip — a trim, a speed change, a remove, an add onto footage that is there, \
                       a voiceover placed over footage — carries the later clips on the same track along, keeping \
                       their gaps. Tracks ripple on their own, except that a clip the ripple moved takes its \
                       LINKED partners along by the same amount (a picture's detached sound keeps with it, a \
                       J/L-cut offset included; other clips on that audio track stay put), a locked track never \
                       moves (and a lane the shift would leave overlapping is left as the edit made it), and titles \
                       and markers stay put. Off by default; it is the \
                       user's toolbar setting and is saved with the project. Check it before trimming or \
                       removing, and pass `ripple` on those tools to override it for one call."
    )]
    fn get_ripple_mode(&self) -> Result<String, McpError> {
        let on = self.lock().ripple_mode().map_err(core_err)?;
        json(&serde_json::json!({ "ripple_mode": on }))
    }

    #[tool(
        description = "Turn ripple mode on or off for the project (see get_ripple_mode). This flips the USER's \
                       setting, which shows in their toolbar and stays until changed again, and it applies at \
                       once — also to your staged edits. It is not an edit: no revision, and the timeline does \
                       not move. While the user has agent review switched on (the default) it is refused: pass \
                       `ripple` to trim / set_speed / remove / remove_clips / add_clip_to_timeline to ripple (or not) \
                       for one call, or ask the user to switch modes."
    )]
    fn set_ripple_mode(&self, Parameters(p): Parameters<SetRippleModeParams>) -> Result<String, McpError> {
        let on = {
            let project = self.lock();
            project.set_ripple_mode(p.on).map_err(core_err)?;
            project.ripple_mode().map_err(core_err)?
        };
        // The toolbar toggle reads the flag when a project loads, so it has to be
        // told. Not `project-changed`: nothing it re-fetches moved.
        self.notify("ripple-mode-changed");
        json(&serde_json::json!({ "ripple_mode": on }))
    }

    #[tool(
        description = "Set the linear volume gain of a clip. A picture whose sound was detached (`source_audio: \
                       false`) carries no sound, so its volume changes nothing audible — its linked audio clip \
                       (see `link_id`) is a separate clip to set. While the clip's volume is animated \
                       (set_property_keyframes with prop `volume`) this static value is not used: clear those \
                       keys (pass []) for it to apply."
    )]
    fn set_volume(&self, Parameters(p): Parameters<VolumeParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project.set_volume(clip_id, p.volume).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Set a clip's fade-in / fade-out duration in seconds (omit a field to leave it unchanged, 0 to \
                       clear). On a picture whose sound was detached (`source_audio: false`) the fade only fades the \
                       picture — it carries no sound, so nothing audible changes; its linked audio clip (see \
                       `link_id`) is a separate clip to fade."
    )]
    fn set_fade(&self, Parameters(p): Parameters<FadeParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project.set_fade(clip_id, p.fade_in, p.fade_out).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Set a clip's playback speed (1.0 = unchanged, 2.0 = 2x faster, 0.5 = half, negative = reverse); this retimes the clip and changes its timeline duration. In ripple mode (get_ripple_mode, or pass `ripple`) the later clips on the track follow the change in duration. Linked partners are retimed by the same ratio so a picture and its sound stay in step (link=false: this clip only)."
    )]
    fn set_speed(&self, Parameters(p): Parameters<SpeedParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project
                .with_links(p.link, |project| {
                    project.with_ripple(p.ripple, |project| project.set_speed(clip_id, p.speed))
                })
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Set a clip's geometric transform — scale / position (pos_x, pos_y as fractions of the frame) / rotation / opacity / per-edge crop. Use a sub-1.0 scale with a position for picture-in-picture. Omit a field to leave it unchanged. A number that is animated (set_keyframes / add_keyframe, or set_property_keyframes) is read from its keys, not from this static value, which only applies once its keys are cleared; the crop edges are never animated."
    )]
    fn set_transform(&self, Parameters(p): Parameters<TransformParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
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
        description = "Set how a clip looks at one moment of it (`time`, seconds from the clip's start), in one edit, whether or not it is animated: each named number that has keyframes (set_keyframes / add_keyframe, or set_property_keyframes) gets a key at that time — the rest of the animation is kept — and every other named number, the crop edges always, is set as set_transform sets it. Use it to change a pose without first finding out which numbers are animated. Returns the clip."
    )]
    fn set_transform_at(&self, Parameters(p): Parameters<TransformAtParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let patch = kerf_core::TransformPatch {
            scale: p.scale,
            pos_x: p.pos_x,
            pos_y: p.pos_y,
            rotation: p.rotation,
            opacity: p.opacity,
            crop_left: p.crop_left,
            crop_right: p.crop_right,
            crop_top: p.crop_top,
            crop_bottom: p.crop_bottom,
        };
        self.edit(|project| {
            let out = project.set_transform_at(clip_id, p.time, patch).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Set a clip's color correction — brightness / contrast / saturation / gamma / temperature (warm-cool). Omit a field to leave it unchanged. A number that is animated (set_property_keyframes) is read from its keys, not from this static value, which only applies once its keys are cleared (pass [])."
    )]
    fn set_color(&self, Parameters(p): Parameters<ColorParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project
                .set_color(clip_id, p.brightness, p.contrast, p.saturation, p.gamma, p.temperature)
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Grade a clip beyond set_color: tone curves (master and per channel), shadows / midtones / highlights colour wheels, and a .cube 3D LUT with an intensity. Each part is optional and omitted parts are left as they are. They apply after set_color's numbers, in the order LUT, curves, wheels. Check the result with preview_timeline."
    )]
    fn set_grade(&self, Parameters(p): Parameters<GradeParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        // A new LUT file is read before the project is locked — an existing local file, not a URL,
        // an ffmpeg protocol or a network share (reading one would make this machine reach out).
        if let Some(path) = p.lut_path.as_deref().filter(|s| !s.is_empty()) {
            crate::require_local_input_path(path).map_err(|e| McpError::invalid_params(e, None))?;
            kerf_core::paths::validate_import_path(std::path::Path::new(path)).map_err(core_err)?;
            kerf_core::color_lut::inspect_lut(path).map_err(core_err)?;
        }
        self.edit(|project| {
            let timeline = project.working_timeline().map_err(core_err)?;
            let (ti, ci) = timeline
                .locate(clip_id)
                .ok_or_else(|| core_err(kerf_core::Error::ClipNotFound(clip_id)))?;
            let mut grade = timeline.tracks[ti].clips[ci].grade.clone();
            if let Some(curves) = p.curves.clone() {
                grade.curves = Some(curves.into());
            }
            if let Some(wheels) = p.wheels {
                grade.wheels = Some(wheels.into());
            }
            match (p.lut_path.as_deref(), p.lut_intensity) {
                (Some(""), _) => grade.lut = None,
                (Some(path), intensity) => {
                    let kept = grade.lut.as_ref().filter(|l| l.path == path).map(|l| l.intensity);
                    grade.lut = Some(ClipLut {
                        path: path.to_string(),
                        intensity: intensity.or(kept).unwrap_or(1.0),
                    });
                }
                (None, Some(intensity)) => {
                    let lut = grade
                        .lut
                        .as_mut()
                        .ok_or_else(|| McpError::invalid_params("the clip has no LUT whose intensity to change", None))?;
                    lut.intensity = intensity;
                }
                (None, None) => {}
            }
            let out = project.set_grade(clip_id, grade).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Set or clear the transition blending a clip's start with the clip before it on the same track, with a duration in seconds; omit kind to clear. A dissolve or a motion transition plays both shots at once, so it borrows the outgoing clip's unused source — a clip trimmed to the very end of its footage has none to lend and the transition falls back to a hard cut. Dips need no handle. Reach for a fade between scenes and a motion transition between shots in a montage; a cut needs no transition at all."
    )]
    fn set_transition(&self, Parameters(p): Parameters<TransitionParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
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
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project.set_video_effects(clip_id, p.effects).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Replace a clip's audio effect chain (applied in order at export): highpass, lowpass, equalizer (parametric band), compressor (dynamics) or gate (noise gate). Pass an empty list to clear."
    )]
    fn set_audio_effects(&self, Parameters(p): Parameters<AudioEffectsParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project.set_audio_effects(clip_id, p.effects).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Replace a clip's whole-transform keyframes — the keys that animate scale / position / rotation / opacity together over time. Two or more keyframes animate the clip (e.g. a Ken Burns zoom, a moving picture-in-picture). An empty list clears these keyframes only: a number that has keys of its own (set_property_keyframes) keeps them, so the clip can still be animated afterwards — clear_keyframes makes the whole transform static. Colour and volume keys are never touched."
    )]
    fn set_keyframes(&self, Parameters(p): Parameters<KeyframesParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project.set_keyframes(clip_id, p.keyframes).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Add (or replace) one transform keyframe at a time offset from the clip's start; unspecified channels capture the clip's current pose there. Use two calls to animate between two poses. A key added inside an eased segment splits it (a hold stays held, a curve stays the same curve); re-keying a moment keeps how it leaves."
    )]
    fn add_keyframe(&self, Parameters(p): Parameters<AddKeyframeParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project
                .add_keyframe(clip_id, p.time, p.scale, p.pos_x, p.pos_y, p.rotation, p.opacity)
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Set the easing of the segment leaving one keyframe: how the clip moves from that pose \
                       to the next (ease in / out / in-out, a hold, or a custom cubic bezier). Keys are linear until set. \
                       Without prop it shapes the transform keyframe at that time; with prop, that one number's key \
                       (a colour number, the volume, or a transform number). Returns the clip."
    )]
    fn set_keyframe_easing(&self, Parameters(p): Parameters<SetKeyframeEasingParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = match p.prop {
                Some(prop) => project.set_property_easing(clip_id, prop, p.time, p.easing),
                None => project.set_keyframe_easing(clip_id, p.time, p.easing),
            }
            .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Animate ONE number of a clip with keys of its own: a transform number (scale, pos_x, pos_y, \
                       rotation, opacity), a colour number (brightness, contrast, saturation, gamma, temperature), \
                       the clip's volume (a linear gain — a swell, a duck, an audio fade that is not at the clip's ends), \
                       a number of its mask (a mask that follows a face, grows or softens; the clip needs a mask first) \
                       or the animated crop (crop_zoom / crop_x / crop_y: a punch-in or a pan across the shot that \
                       stays inside the picture; on top of any static crop edges). Numbers are independent: key the opacity at other moments than the zoom, grade a shot that \
                       warms over its length, ride the dialogue's level. Replaces that number's keys (pass [] to make \
                       it static); the clip's other numbers are untouched, and a number that set_keyframes was \
                       animating is taken over by its own keys while the rest of the transform keeps them. Each key: \
                       time (seconds from the clip's start), value, and the easing of the segment leaving it. While a \
                       number is animated its static value (set_color, set_volume, set_transform) is not used. \
                       Returns the clip."
    )]
    fn set_property_keyframes(&self, Parameters(p): Parameters<PropertyKeyframesParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project.set_property_keyframes(clip_id, p.prop, p.keys).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Re-key SEVERAL numbers of one clip in a single edit — the same keys set_property_keyframes \
                       takes, for each number listed: retime a move and its fade together, or set a whole look's \
                       colour keys at once, as one undo step. Numbers not listed are untouched. Returns the clip."
    )]
    fn set_property_tracks(&self, Parameters(p): Parameters<PropertyTracksParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let tracks = p.tracks.into_iter().map(PropertyTrack::from).collect();
            let out = project.set_property_tracks(clip_id, tracks).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Copy the animation of a clip's numbers (all of them, or the props listed) to another clip, \
                       shifted by offset seconds — give a second shot the same push-in, the same warm-up, the same \
                       volume swell. The destination's own keys for those numbers are replaced. Returns the \
                       destination clip."
    )]
    fn copy_keyframes(&self, Parameters(p): Parameters<CopyKeyframesParams>) -> Result<String, McpError> {
        let (from, to) = (
            parse_arg("from_clip_id", &p.from_clip_id)?,
            parse_arg("to_clip_id", &p.to_clip_id)?,
        );
        self.edit(|project| {
            let out = project
                .copy_keyframes(from, to, &p.props.unwrap_or_default(), p.offset.unwrap_or(0.0))
                .map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Remove all transform keyframes from a clip (back to its static transform): the whole-transform keys and any scale / position / rotation / opacity keys of their own. Colour and volume keys stay — clear those with set_property_keyframes and an empty list."
    )]
    fn clear_keyframes(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
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
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
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
    async fn set_asset_projection(&self, Parameters(p): Parameters<AssetProjectionParams>) -> Result<String, McpError> {
        let asset_id = parse_arg("asset_id", &p.asset_id)?;
        let (project, app, owner) = (self.project.clone(), self.app.clone(), self.owner());
        // On the blocking pool: queueing the proxy at the new size runs the cached ffprobe that
        // keys it, which must not stall an async worker. The change to the asset is held back
        // with the agent's proposal (the core does that, for the session named here).
        let asset = blocking(move || {
            let asset = lock_agent_as(&project, owner.as_deref())
                .set_asset_projection(asset_id, p.projection)
                .map_err(core_err)?;
            crate::spawn_proxy(&app, &asset);
            Ok(asset)
        })
        .await?;
        self.changed();
        json(&asset)
    }

    #[tool(
        description = "Stop reprojecting a 360 clip, leaving its raw spherical picture (equirect or dual-fisheye) on \
                       the timeline."
    )]
    fn clear_reframe(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
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
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
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
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
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
        let overlay_id = parse_arg("overlay_id", &p.overlay_id)?;
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
        let overlay_id = parse_arg("overlay_id", &p.overlay_id)?;
        self.edit(|project| {
            project.remove_overlay(overlay_id).map_err(core_err)?;
            Ok("ok".to_string())
        })
    }

    #[tool(
        description = "Add an animated SVG title (kinetic text, lower-third, logo sting, shape animation) drawn over the \
                       picture from start to end (timeline seconds), above the clips and below text overlays and \
                       captions. Pick a ready-made `preset` with `params` (list_svg_presets), or write your own `svg`. \
                       Your own SVG is animated with SMIL only: `animate`, `set`, `animateTransform` (translate / scale / \
                       rotate / skewX / skewY) and `animateColor`, with begin / dur / from / to / by / values / keyTimes / \
                       calcMode (linear, discrete, paced, spline + keySplines) / repeatCount / fill=\"freeze\", and \
                       `id.begin` / `id.end` chains. Times in the SVG are seconds RELATIVE TO THE OVERLAY'S START \
                       (begin=\"0s\" is the moment it appears, so the animation does not depend on where you place it). \
                       The document needs a viewBox (or width and height) and is scaled to fit, centred, inside the \
                       placement rectangle x / y / width / height (fractions of the frame; default the whole frame): for a \
                       full-frame title draw it in the delivery frame's own coordinates (viewBox=\"0 0 1920 1080\" for \
                       1080p; timeline_summary shows `delivery_format`), for a corner bug use a small rectangle. Dropped \
                       with a warning in the result: CSS animations and transitions, scripts and event handlers, \
                       foreignObject, links and external images or fonts (system fonts by name work; the only image that \
                       is kept is an <image> whose href is a data:image/png, data:image/jpeg, data:image/gif or \
                       data:image/webp URI), animateMotion, and `animate` on `transform` (use animateTransform). Limits: \
                       128 KiB of SVG, 50 SVG overlays. Returns the overlay (without its source) and `warnings`. Draft your \
                       own SVG with validate_svg and preview_svg_overlay (`svg`) first; once added, check it in the cut \
                       with preview_timeline at a few times between start and end."
    )]
    async fn add_svg_overlay(&self, Parameters(p): Parameters<AddSvgOverlayParams>) -> Result<String, McpError> {
        let source = svg_source(p.svg, p.preset, p.params)?;
        let layout = SvgLayout {
            name: p.name,
            x: p.x,
            y: p.y,
            width: p.width,
            height: p.height,
            opacity: p.opacity,
        };
        // Reading the document takes up to a second and a half: on the blocking pool, and before the
        // project lock, which then only places it.
        let checked = blocking(move || source.check().map_err(core_err)).await?;
        self.edit(|project| {
            let out = project
                .add_svg_overlay_parsed(checked, p.start, p.end, layout)
                .map_err(core_err)?;
            json(&SvgOverlayOutcome::from(out))
        })
    }

    #[tool(
        description = "Change an animated SVG title: its timing (start / end), placement (x / y / width / height as frame \
                       fractions), opacity or name, or its artwork. For a PRESET title pass `preset_params` (the full set; \
                       `text` required) to re-fill it and/or `preset_kind` to switch it to another preset (one edit; the \
                       title keeps its id, name, timing and place in the draw order); its animation re-times itself to \
                       the new length when you change start / end. To swap the artwork for your own, pass `svg` (the \
                       title then stops being a preset); `svg` is exclusive with `preset_params` and `preset_kind`, \
                       which a custom SVG title refuses. Omit a field to leave it unchanged. Returns the overlay \
                       (without its source) and `warnings`."
    )]
    async fn update_svg_overlay(&self, Parameters(p): Parameters<UpdateSvgOverlayParams>) -> Result<String, McpError> {
        let overlay_id = parse_arg("overlay_id", &p.overlay_id)?;
        let patch = SvgOverlayPatch {
            name: p.name,
            svg: p.svg,
            preset_params: p.preset_params,
            preset_kind: p.preset_kind,
            start: p.start,
            end: p.end,
            x: p.x,
            y: p.y,
            width: p.width,
            height: p.height,
            opacity: p.opacity,
        };
        // As in add_svg_overlay: read the artwork before the project lock is taken.
        let (patch, doc) = blocking(move || {
            let doc = patch.check().map_err(core_err)?;
            Ok((patch, doc))
        })
        .await?;
        self.edit(|project| {
            let out = project.update_svg_overlay_parsed(overlay_id, patch, doc).map_err(core_err)?;
            json(&SvgOverlayOutcome::from(out))
        })
    }

    #[tool(description = "Remove an animated SVG title (its id is in timeline_summary's `svg_overlays`)")]
    fn remove_svg_overlay(&self, Parameters(p): Parameters<OverlayIdParams>) -> Result<String, McpError> {
        let overlay_id = parse_arg("overlay_id", &p.overlay_id)?;
        self.edit(|project| {
            project.remove_svg_overlay(overlay_id).map_err(core_err)?;
            Ok("ok".to_string())
        })
    }

    #[tool(
        description = "List the ready-made animated titles add_svg_overlay can make with `preset` + `params`, and what each \
                       takes (name, whether required, default). fade_up: the text rises and fades in, then fades out; \
                       typewriter: letters appear one by one; lower_third: an accent bar wipes in above a name (and \
                       subtitle) at the lower left; word_pop: one big word at a time pops in, centred — the social-video \
                       look. A preset is drawn for the delivery frame (set_delivery_format) and times its animation to \
                       the overlay's length, so it also follows a trim; the text is centred or left-anchored and long \
                       text shrinks to fit, never wraps."
    )]
    fn list_svg_presets(&self) -> Result<String, McpError> {
        json(&kerf_core::svg::preset_infos())
    }

    #[tool(
        description = "Check an SVG document you wrote for add_svg_overlay / update_svg_overlay without adding it: it is \
                       sanitised and its SMIL animations read, exactly as an add would. Returns `svg_bytes` (the size of \
                       the sanitised document; the limit is 128 KiB), `warnings` (what would be dropped or could not be \
                       honoured: CSS animations, scripts, external images, animateMotion, an unreadable value, ...) and \
                       `natural_duration` (seconds after the title appears when every animation has finished; null for \
                       a still or something that loops forever, which add_svg_overlay then gives 4 s). A document that \
                       cannot be used at all (not XML, no <svg> root, no viewBox or width and height, over a limit) is \
                       an error saying why. Nothing is changed and the source is not echoed back. To see how it \
                       looks, use preview_svg_overlay."
    )]
    async fn validate_svg(&self, Parameters(p): Parameters<ValidateSvgParams>) -> Result<String, McpError> {
        let check = blocking(move || kerf_core::check_svg(&p.svg).map_err(core_err)).await?;
        json(&SvgValidation::from(check))
    }

    #[tool(
        description = "See an animated SVG title without rendering the cut: a PNG contact sheet of it at several moments, \
                       each cell the title drawn over a dark grey frame of the delivery shape with its time written \
                       under it, plus a text index of cell -> seconds. Pass `svg` to look at a draft before you add \
                       it (its warnings come back too) or `overlay_id` for a title already on the timeline (reads the \
                       cut you are proposing, staged edits included). By default 6 moments spread from its first frame \
                       to its end (the last is usually the title gone or at rest); pass `times` (seconds after it \
                       appears) to look at exact moments, e.g. around a transition, or `count` for more or fewer. \
                       It shows the title alone, not over the footage: to judge placement and legibility against the \
                       picture, add it and use preview_timeline at a few times between its start and end."
    )]
    async fn preview_svg_overlay(&self, Parameters(p): Parameters<PreviewSvgOverlayParams>) -> Result<CallToolResult, McpError> {
        let overlay_id = p.overlay_id.as_deref().map(|s| parse_arg("overlay_id", s)).transpose()?;
        if overlay_id.is_some() == p.svg.is_some() {
            return Err(McpError::invalid_params(
                "pass either `overlay_id` (a title on the timeline) or `svg` (a draft), exactly one".to_string(),
                None,
            ));
        }
        let moments = svg_moments(p.times, p.count)?;
        let cell_w = p.max_width.unwrap_or(480).clamp(64, MAX_SVG_CELL_WIDTH);
        let (project, draft) = (self.project.clone(), p.svg);
        let (png, caption) = blocking(move || {
            // Resolve the title and the frame under the lock, draw with it released.
            let (title, frame) = {
                let project = lock_agent(&project);
                let timeline = project.working_timeline().map_err(core_err)?;
                let frame = timeline.preset_frame();
                let title = match (overlay_id, draft) {
                    (Some(id), _) => {
                        let o = timeline
                            .svg_overlay(id)
                            .ok_or_else(|| core_err(kerf_core::Error::SvgOverlayNotFound(id)))?;
                        SvgPreviewTitle {
                            svg: o.svg.clone(),
                            label: format!("SVG title \"{}\" ({id})", o.display_name()),
                            span: Some(o.end - o.start),
                            offset: o.offset,
                            report_warnings: false,
                        }
                    }
                    (None, svg) => SvgPreviewTitle {
                        svg: svg.unwrap_or_default(),
                        label: "draft SVG (not on the timeline)".to_string(),
                        span: None,
                        offset: 0.0,
                        report_warnings: true,
                    },
                };
                (title, frame)
            };
            svg_preview(&title, &moments, cell_w, frame)
        })
        .await?;
        Ok(png_result(caption, png))
    }

    #[tool(
        description = "See a ready-made animated title (list_svg_presets) filled with your `params` before adding it: a PNG \
                       contact sheet of it at several moments, drawn for the project's delivery frame, plus a text index \
                       of cell -> seconds. Like preview_svg_overlay but for a preset that is not on the timeline yet; \
                       nothing is added and nothing is staged. `duration` (default 4 s) is how long the title would run. \
                       Then add_svg_overlay with the same `preset` and `params`."
    )]
    async fn preview_svg_preset(&self, Parameters(p): Parameters<PreviewSvgPresetParams>) -> Result<CallToolResult, McpError> {
        kerf_core::svg::check_params(&p.params).map_err(core_err)?;
        let moments = svg_moments(p.times, p.count)?;
        let duration = p.duration.unwrap_or(4.0);
        if !duration.is_finite() || duration <= 0.0 || duration > kerf_core::MAX_SVG_SECONDS {
            return Err(McpError::invalid_params(
                format!(
                    "`duration` must be a positive number of seconds (at most {}), got {duration}",
                    kerf_core::MAX_SVG_SECONDS
                ),
                None,
            ));
        }
        let cell_w = p.max_width.unwrap_or(480).clamp(64, MAX_SVG_CELL_WIDTH);
        let (project, owner) = (self.project.clone(), self.owner());
        let (png, caption) = blocking(move || {
            // The frame the title would be drawn for, under the lock; the drawing with it released.
            let frame = lock_agent_as(&project, owner.as_deref())
                .working_timeline()
                .map_err(core_err)?
                .preset_frame();
            let title = SvgPreviewTitle {
                svg: kerf_core::render_preset(p.preset, &p.params, duration, frame),
                label: format!("preset {} (not on the timeline)", p.preset.as_str()),
                span: Some(duration),
                offset: 0.0,
                report_warnings: true,
            };
            svg_preview(&title, &moments, cell_w, frame)
        })
        .await?;
        Ok(png_result(caption, png))
    }

    #[tool(
        description = "Set or clear (empty list) a text overlay's position/opacity keyframes, to animate it over its lifetime (e.g. a title that slides in and fades out)"
    )]
    fn set_overlay_keyframes(&self, Parameters(p): Parameters<OverlayKeyframesParams>) -> Result<String, McpError> {
        let overlay_id = parse_arg("overlay_id", &p.overlay_id)?;
        self.edit(|project| {
            let out = project.set_overlay_keyframes(overlay_id, p.keyframes).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Caption the cut: project every clip's cached transcript (run analyze_asset first) through the current edit and write the result as text overlays, replacing any previously generated (or imported) set. Captions are placed in TIMELINE time, so they follow trims, reorders, speed changes and removed silences, and words that were cut out get no caption. Long sentences are split into readable lines. Pick the look with `style`: `lines` for subtitles, `word_punch` for one big word at a time (what social captions usually look like — prefer it for a vertical cut). Hand-made titles and lower-thirds are left alone. Returns how many captions were written, the first few in full and the ids of the first hundred (get_timeline_state's `captions` window pages through all of them)."
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
            json(&reads::caption_result(&out))
        })
    }

    #[tool(
        description = "Caption the cut from a subtitle file: SubRip (.srt), WebVTT (.vtt) or ASS/SSA (.ass, .ssa), by path or as text you pass in. Cue times are taken as TIMELINE time by default (a subtitle track made for the finished cut); pass base=source with asset_id when the file instead times one asset's own footage (a transcript, subtitles for the uncut recording) and each cue is projected through that asset's clips like a transcript, following trims, reorders and speed changes. Long cues are split into readable lines, sized to fit the delivery frame, never two on screen at once — the same machinery as generate_captions, in the `style` you pick (`word_punch` for a vertical cut). Markup, styles and positions in the file are ignored; unreadable entries are skipped and counted. The captions REPLACE any previously generated or imported ones (a later generate_captions replaces these too — captions are one lane of text), and hand-made titles are left alone. Caption last, like generate_captions: a later edit does not move them. Pass offset (seconds, may be negative) when the file's clock does not start where the picture does. Returns a summary: cues read, placed, skipped_lines, dropped_outside (not on the cut at all: past its end or before its start, or footage that was cut out), dropped_short (on the cut but too brief to read), dropped_overlap (lost its slot to another cue), captions written and replaced. At most 10,000 cues / 100,000 words are read and 20,000 captions written per import — a refused import changes nothing."
    )]
    async fn import_captions(&self, Parameters(p): Parameters<ImportCaptionsParams>) -> Result<String, McpError> {
        let asset = p.asset_id.as_deref().map(|s| parse_arg("asset_id", s)).transpose()?;
        let base = CaptionTimeBase::resolve(p.base.as_deref(), asset).map_err(core_err)?;
        let format = CaptionFormat::from_arg(p.format.as_deref()).map_err(core_err)?;
        let req = CaptionImportRequest {
            base,
            options: CaptionOptions {
                style: p.style.unwrap_or_default(),
                max_words: p.max_words,
                max_chars: p.max_chars,
                pos_y: p.pos_y,
                size: p.size,
            },
            offset: p.offset.unwrap_or(0.0),
        };
        let (project, owner) = (self.project.clone(), self.owner());
        let (path, text) = (p.path, p.text);
        let summary = blocking(move || {
            // Read, decode and parse with the project lock released — a few MB of
            // text is milliseconds, but never milliseconds the user's edits wait
            // on — and take it only to place the result.
            let text = caption_source(path.as_deref(), text)?;
            let file = kerf_core::parse_captions(&text, format).map_err(core_err)?;
            lock_agent_edit(&project, owner.as_deref())?
                .import_captions(&file, req)
                .map_err(core_err)
        })
        .await?;
        self.changed();
        json(&summary)
    }

    #[tool(
        description = "Whether voiceovers can be generated on this machine, whether the voice model is downloaded yet (the first voiceover fetches ~100 MB), and the voices on offer with their accent and gender."
    )]
    fn voiceover_status(&self) -> Result<String, McpError> {
        json(&kerf_core::voiceover_status())
    }

    #[tool(
        description = "Read a script aloud with the Kokoro text-to-speech model and put the narration on the timeline: on the `VO` audio track unless track_id names another, appended after what is already there unless timeline_start says where. The audio is English only. The first call downloads the voice model (~100 MB) and each voice is fetched on first use; synthesis then takes a few seconds per sentence — send a `progressToken` for progress and cancel the request to stop it. Each sentence is synthesized on its own, so the voiceover's transcript is its script with exact timings: pass caption_style to caption the cut straight away (or run generate_captions later), and never analyze_asset it for a transcript. The generated audio lands for the user immediately like an import; placing it on the timeline (and the captions) is an ordinary staged edit. `caption_style` plus `caption_max_words` / `caption_max_chars` / `caption_pos_y` / `caption_size` set the look of those captions as in generate_captions; if the narration is placed but the captions fail, the result still succeeds with a `warning`. `track_id` is checked before anything is synthesized. Pass `background: true` for a job_id. Returns the asset (without its per-sentence timings: get_transcript has them) and the clip."
    )]
    async fn generate_voiceover(
        &self,
        Parameters(p): Parameters<VoiceoverParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let track = p.track_id.as_deref().map(|s| parse_arg("track_id", s)).transpose()?;
        let generation = {
            let project = self.lock();
            // Said before a minute of synthesis, not after it: the track the narration is for must
            // exist, and be an audio track.
            if let Some(t) = track {
                let timeline = project.working_timeline().map_err(core_err)?;
                match timeline.track(t) {
                    None => return Err(core_err(kerf_core::Error::TrackNotFound(t))),
                    Some(found) if found.kind != StreamKind::Audio => {
                        return Err(McpError::invalid_params(
                            format!("`track_id` {t} is a {:?} track; narration goes on an audio track", found.kind)
                                .to_lowercase(),
                            None,
                        ))
                    }
                    Some(_) => {}
                }
            }
            project.generation()
        };
        let voice = p.voice.unwrap_or_else(|| kerf_core::DEFAULT_VOICE.to_string());
        // Any caption option asks for captions; the style alone is the shorthand.
        let captioned = p.caption_style.is_some()
            || p.caption_max_words.is_some()
            || p.caption_max_chars.is_some()
            || p.caption_pos_y.is_some()
            || p.caption_size.is_some();
        let captions = captioned.then(|| CaptionOptions {
            style: p.caption_style.unwrap_or_default(),
            max_words: p.caption_max_words,
            max_chars: p.caption_max_chars,
            pos_y: p.caption_pos_y,
            size: p.caption_size,
        });
        let (me, owner) = (self.clone(), self.owner());
        // The GUI's Stop on a voiceover ends an agent's too (`begin_voiceover`).
        let gui_stop = self.app.state::<crate::AppState>().begin_voiceover();
        self.long_tool("generate_voiceover", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let (project, app) = (me.project.clone(), me.app.clone());
            let placed = blocking(move || {
                let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
                    let message = match &detail {
                        Some(d) => format!("{stage}: {d}"),
                        None => stage.to_string(),
                    };
                    run.progress(fraction, message);
                    // The GUI shows the same progress it shows for its own voiceovers.
                    let _ = app.emit(
                        "voiceover-progress",
                        crate::VoiceoverProgressEvent {
                            stage: stage.to_string(),
                            fraction,
                            detail,
                        },
                    );
                };
                let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                let asset = Project::synthesize_voiceover(&p.text, &voice, p.speed.unwrap_or(1.0), &mut on_progress, &stop)
                    .map_err(core_err)?;
                // Landing it is an edit of the cut, so it goes into the proposal.
                let project = lock_agent_edit(&project, owner.as_deref())?;
                require_generation(&project, generation, "the narration was being made")?;
                let (asset, clip) = project
                    .with_ripple(p.ripple, |project| project.place_voiceover(&asset, track, p.timeline_start))
                    .map_err(core_err)?;
                // The narration is down. Captions failing after that is a warning on a success,
                // not an error that leaves the agent thinking nothing was placed.
                let captions = captions.map(|opts| project.generate_captions(opts).map(|c| c.len()));
                Ok((asset, clip, captions))
            })
            .await;
            // A synthesis that fails places nothing; one that got as far as placing it has
            // changed the proposal whatever came after.
            let (asset, clip, captions) = placed?;
            me.changed();
            let mut answer = serde_json::json!({ "asset": reads::compact_asset(&asset), "clip": clip });
            match captions {
                Some(Ok(count)) => answer["captions"] = serde_json::json!(count),
                Some(Err(e)) => {
                    answer["captions"] = serde_json::json!(0);
                    answer["warning"] = serde_json::json!(format!(
                        "the narration is on the timeline, but captioning it failed: {e}. Fix that and run generate_captions."
                    ));
                }
                None => {}
            }
            json(&answer)
        })
        .await
    }

    #[tool(
        description = "Whether music can be composed on this machine: whether the SoundFont (the instrument samples, ~32 MB, downloaded by the first compose_music) is already on disk, how many compositions are rendering now, and the styles on offer with their usual tempo."
    )]
    fn music_status(&self) -> Result<String, McpError> {
        json(&kerf_core::music_status())
    }

    #[tool(
        description = "Compose original music from a score and put it on the timeline. You write the score — tempo, key, style and sections of chords — and Kerf plays it through a General MIDI SoundFont synthesizer (drums, bass, pad, keys, a hook and effects, with the kick pumping the bass and pad), so it is deterministic: the same score is the same audio. The result is a 48 kHz stereo clip on the `M` audio track (created on first use) at `at`, else appended after what the track holds; peaks at -1 dBFS, loudness left to the master bus and export. \
                       SCORE = {bpm 40-240, key, style, sections, hits?, seed?, instruments?}, 4/4 only. \
                       key: a tonic and an optional m — G, F#m, Bb, Ebm. \
                       style: tropical_house (four-on-the-floor, marimba stabs, pan-flute hook, 100-120 BPM), pop (backbeat, fingered bass, piano, strings, 100-124), lofi (lazy swung boom-bap, Rhodes sevenths, vibraphone, 70-90), cinematic (strings, horns, timpani and big hits, 60-100), ambient (pad and harp, no real beat, 55-80), rock (power-chord guitar, picked bass, live drums, 100-150); music_status lists them with a tempo. \
                       sections = [{kind, bars, chords, energy?}] played in order, bars 1-128 each, 400 bars and 10 minutes at most. kind shapes the arrangement: intro (sparse: pad and a hint of the groove), verse (the steady groove), build (pulsing bass, a snare roll that quickens, a riser into whatever follows), drop (everything at once, with a crash and impact), breakdown (no kick or bass: pad, keys and hook), outro (thins out and its last bar resolves on the tonic chord). A typical piece: intro, build, drop, breakdown, build, drop, outro. energy 0-1 sets how loud and busy a section is when its kind's default is not what you want (intro 0.3, verse 0.5, build 0.7, drop 1, breakdown 0.3, outro 0.3). \
                       chords: ONE per bar; a list shorter than bars cycles, so [\"vi\",\"IV\",\"I\",\"V\"] over 8 bars plays the four chords twice. Write chord names (C, Em, F#m7, Gsus4, D/F#, Bbmaj7, E5, Cadd9, Am7b5 — types: maj min dim aug sus2 sus4 5 7 maj7 m7 m7b5 dim7 6 m6 add9 madd9 9 maj9 m9) or roman numerals in the score's key (I, V, vi, IV, ii7, bVII): upper case is major, lower case minor, a trailing 7 / maj7 / sus4 etc. adds the type, a leading b or # moves the root a semitone, and numerals count the key's own scale (in A minor, VI is F and VII is G). lofi turns plain triads into sevenths and ambient adds a ninth. \
                       hits = [{at_s, kind}] — moments in the picture, at_s in seconds from the START OF THE MUSIC (not the timeline): impact (a crash, timpani and low boom), riser (a rising sweep about a bar long that peaks at at_s), dropout (everything but the effects goes silent for one beat from at_s), whoosh (a ~1 s sweep peaking at at_s). A drop already opens with its own crash and impact, so hits are for moments in the picture the sections do not cover (align adds them at cuts for you). seed (an integer 0-9007199254740991, i.e. 2^53 - 1; a bigger one is refused) varies the humanization and the hook; instruments overrides a role's General MIDI program (drums, bass, pad, keys, lead: 0-127). A score that is wrong (an unknown chord, bars 0, a hit past the end) is refused with the offending section or chord named, and so is a field the score does not have: a misspelled `chord`, `enrgy` or `at` is an error, not a silent default. \
                       ALIGN: with align {to: \"cuts\" | \"markers\", track_id?, max_tempo_change?} the score is fitted to the picture of the cut you are proposing before it is rendered. The tempo is nudged (within max_tempo_change, default 0.06 = 6 %, at most 0.25; never to one that ends the piece before a hit you wrote) so as many cuts as possible land on beats (a downbeat counts double). Then hits are added: an impact on every cut that lands on a downbeat; and for each `drop` that starts more than a beat in and that a cut lands within a beat of, a dropout (a beat of silence) one beat before it, plus a riser peaking at the drop's start — except when the drop follows a build (the build ends in its own riser) or starts earlier than half a bar, where only the dropout is added. Hits within 50 ms of one you wrote are not added twice, and a score stops at 256 hits (`align.notes` says if some were left out). To make a cut hit hard, lay out the sections so a drop starts near it. Cuts are the boundaries between the clips of a video track (default: the first); give `at` if the music does not start at 0. Align after the picture is cut: a later edit moves the cuts, and the music is then composed again with replace_clip_id. The answer's `align` lists the cuts on a beat / downbeat before and after and the hits added, and `score` is the score actually rendered (new bpm, added hits). \
                       RECOMPOSE: the score rides on its asset (list_assets shows `music_score`). To change a piece, edit that score and call again with replace_clip_id set to any clip of the music: the WHOLE bed is replaced — every clip of that asset on its track (fit_music turns one piece into several segments) becomes ONE clip of the new render, at the earliest of their starts, with the named clip's id, the first one's gain, fade-in and effects, the last one's fade-out and the new length (fit_music it again if the piece should be another length). Refused before anything renders: a clip that is not composed music, a locked track, music linked to a picture (unlink_clips first), and — with ripple off — a longer piece that would run into the next clip. The answer's `replaced` lists the clip ids that were replaced (empty when a clip was added). \
                       The asset's tempo and bar structure are written from the score, so get_music_structure, fit_music and snap_to_beats work at once (no analyze_asset needed). Then: fit_music if the picture is longer or shorter than the piece (or compose to the length you need), set_track_duck plus set_master_duck to dip it under dialogue, and check it with get_levels and get_band_balance. \
                       The first call downloads the SoundFont (~32 MB). A piece renders in seconds to about a minute — send a `progressToken` for progress (stages download_soundfont, waiting, render) and cancel the request to stop it, or pass `background: true` for a job_id. The rendered audio lands for the user at once like an import; placing it on the timeline is an ordinary staged edit. Before anything renders it is checked that the score is valid, that the track, the replaced clip and the align target are usable and not locked, and — by rehearsing the placement under the ripple in force — that the music will not land on another clip of its track (the refusal says where); if the cut changes while it renders, placing it is checked again and, if refused, stores nothing. Returns {asset, clip, score, align, replaced: [clip ids]}."
    )]
    async fn compose_music(
        &self,
        Parameters(p): Parameters<ComposeMusicParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let id = |name: &str, value: &Option<String>| value.as_deref().map(|s| parse_arg(name, s)).transpose();
        let align = match p.align {
            None => None,
            Some(a) => Some(kerf_core::ScoreAlign {
                to: a.to,
                track_id: id("align.track_id", &a.track_id)?,
                max_tempo_change: a.max_tempo_change,
            }),
        };
        let request = kerf_core::ComposeRequest {
            score: p.score,
            track_id: id("track_id", &p.track_id)?,
            at: p.at,
            align,
            replace_clip_id: id("replace_clip_id", &p.replace_clip_id)?,
        };
        let owner = self.owner();
        // Checked now, before a minute of rendering, under the ripple this call will have: the
        // score, the track, clip and align target, a locked track and a rehearsal of the
        // placement (landing on another clip of the track is refused as `edit_timeline` refuses
        // it). A proposal another session owns refuses here too. What changes while it renders
        // is checked again when the music is placed, and a refusal there stores nothing.
        let (plan, generation) = {
            let project = lock_agent_edit(&self.project, owner.as_deref())?;
            (
                project
                    .with_ripple(p.ripple, |project| project.compose_music_inputs(&request))
                    .map_err(core_err)?,
                project.generation(),
            )
        };
        let (me, ripple) = (self.clone(), p.ripple);
        // The GUI's Stop on a composition ends an agent's too (`begin_music`).
        let gui_stop = self.app.state::<crate::AppState>().begin_music();
        self.long_tool("compose_music", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let (project, app) = (me.project.clone(), me.app.clone());
            let composed = blocking(move || {
                let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
                    let message = match &detail {
                        Some(d) => format!("{stage}: {d}"),
                        None => stage.to_string(),
                    };
                    run.progress(fraction, message);
                    // The GUI shows the same progress it shows for its own compositions.
                    let _ = app.emit(
                        "music-progress",
                        crate::VoiceoverProgressEvent {
                            stage: stage.to_string(),
                            fraction,
                            detail,
                        },
                    );
                };
                let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                let asset = Project::synthesize_music(&plan.score, &mut on_progress, &stop).map_err(core_err)?;
                // Landing it is an edit of the cut, so it goes into the proposal.
                let project = lock_agent_edit(&project, owner.as_deref())?;
                require_generation(&project, generation, "the music was being composed")?;
                let (asset, clip) = project
                    .with_ripple(ripple, |project| {
                        project.place_music(&asset, plan.track_id, plan.at, plan.replace_clip_id)
                    })
                    .map_err(core_err)?;
                Ok(kerf_core::Composed {
                    asset,
                    clip,
                    score: plan.score,
                    align: plan.align,
                    replaced: plan.replaces,
                })
            })
            .await?;
            me.changed();
            // `score` carries the score; the asset need not repeat it.
            let mut asset = reads::compact_asset(&composed.asset);
            asset.music_score = None;
            json(&serde_json::json!({
                "asset": asset,
                "clip": composed.clip,
                "score": composed.score,
                "align": composed.align,
                "replaced": composed.replaced,
            }))
        })
        .await
    }

    #[tool(
        description = "Whether stem separation is ready on this machine: whether the ONNX runtime and the ~170 MB Demucs model still have to be downloaded, and the stems it makes."
    )]
    fn stems_status(&self) -> Result<String, McpError> {
        json(&kerf_core::stems_status())
    }

    #[tool(
        description = "Split an asset's sound into four stems — drums, bass, other, vocals — with the Demucs htdemucs model, \
                       added to the library as FLAC assets (\"<name> · drums\" …). With clip_id they are also laid under \
                       that clip on four new audio tracks with the clip's own sound switched off, so each part can be \
                       mixed, muted or re-cut on its own: drums from one section under another, the vocals pulled down \
                       under a voiceover, or dialogue separated from the background of a clip (its speech lands in \
                       vocals). The first call downloads the runtime and model; separation takes a fraction of the audio's length on a \
                       modern CPU (minutes on an old one) — send a `progressToken` for progress and cancel the request to \
                       stop it, or pass `background: true` for a job_id. Cached per file. Returns the stem assets and the placed clips."
    )]
    async fn separate_stems(
        &self,
        Parameters(p): Parameters<SeparateStemsParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let asset_id = parse_arg("asset_id", &p.asset_id)?;
        let clip_id = p.clip_id.as_deref().map(|s| parse_arg("clip_id", s)).transpose()?;
        let (asset, generation) = {
            let project = self.lock();
            let asset = project.require_asset(asset_id).map_err(core_err)?;
            if let Some(clip) = clip_id {
                let timeline = project.working_timeline().map_err(core_err)?;
                if !timeline.tracks.iter().any(|t| t.clips.iter().any(|c| c.id == clip)) {
                    return Err(core_err(kerf_core::Error::ClipNotFound(clip)));
                }
            }
            (asset, project.generation())
        };
        let (me, owner) = (self.clone(), self.owner());
        // The GUI's Stop on a separation ends an agent's too (`begin_stems`).
        let gui_stop = self.app.state::<crate::AppState>().begin_stems();
        self.long_tool("separate_stems", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let (project, app) = (me.project.clone(), me.app.clone());
            let placed = blocking(move || {
                let mut on_progress = |stage: &str, fraction: Option<f64>, detail: Option<String>| {
                    let message = match &detail {
                        Some(d) => format!("{stage}: {d}"),
                        None => stage.to_string(),
                    };
                    run.progress(fraction, message);
                    let _ = app.emit(
                        "stems-progress",
                        crate::VoiceoverProgressEvent {
                            stage: stage.to_string(),
                            fraction,
                            detail,
                        },
                    );
                };
                let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                let stems = Project::separate_stems_media(&asset, &mut on_progress, &stop).map_err(core_err)?;
                // The stems land in the library either way; laying them under a clip is an edit.
                let project = match clip_id {
                    Some(_) => lock_agent_edit(&project, owner.as_deref())?,
                    None => lock_agent_as(&project, owner.as_deref()),
                };
                require_generation(&project, generation, "the stems were being made")?;
                project.place_stems(asset_id, &stems, clip_id).map_err(core_err)
            })
            .await?;
            me.changed();
            json(&placed)
        })
        .await
    }

    #[tool(
        description = "Remove the captions generate_captions or import_captions wrote, leaving hand-made titles and lower-thirds alone. Returns how many were removed."
    )]
    fn clear_captions(&self) -> Result<String, McpError> {
        self.edit(|project| {
            let removed = project.clear_captions().map_err(core_err)?;
            Ok(format!("removed {removed} generated caption(s)"))
        })
    }

    #[tool(
        description = "Write an asset's cached transcript to a SubRip (.srt) subtitle file (run analyze_asset first). \
                       Refuses to overwrite an existing file unless overwrite is true."
    )]
    async fn export_srt(&self, Parameters(p): Parameters<ExportSrtParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        crate::require_local_output_path(&p.output_path).map_err(|e| McpError::invalid_params(e, None))?;
        let path = std::path::PathBuf::from(&p.output_path);
        refuse_overwrite(&path, p.overwrite.unwrap_or(false))?;
        // Not onto a file the project still needs (a source, a proxy, the project itself).
        self.check_outputs(std::slice::from_ref(&path)).await?;
        let srt = self.lock().transcript_srt(id).map_err(core_err)?;
        blocking(move || std::fs::write(&path, srt).map_err(|e| McpError::internal_error(e.to_string(), None))).await?;
        Ok(format!("wrote {}", p.output_path))
    }

    #[tool(
        description = "Write the cut's captions (the generated / imported caption overlays, timed as the export draws them) to a SubRip (.srt) or WebVTT (.vtt) file. Fails when the cut has no captions. Refuses to overwrite an existing file unless overwrite is true."
    )]
    async fn export_captions(&self, Parameters(p): Parameters<ExportCaptionsParams>) -> Result<String, McpError> {
        crate::require_local_output_path(&p.output_path).map_err(|e| McpError::invalid_params(e, None))?;
        let path = std::path::PathBuf::from(&p.output_path);
        refuse_overwrite(&path, p.overwrite.unwrap_or(false))?;
        // Not onto a file the project still needs (a source, a proxy, the project itself).
        self.check_outputs(std::slice::from_ref(&path)).await?;
        let format = match p.format {
            Some(f) => f,
            None => kerf_core::SubtitleFormat::from_path(&path).map_err(core_err)?,
        };
        let timeline = self.lock().working_timeline().map_err(core_err)?;
        blocking(move || {
            let text = Project::captions_document(&timeline, format).map_err(core_err)?;
            Project::write_document(&path, &text).map_err(core_err)
        })
        .await?;
        Ok(format!("wrote {}", p.output_path))
    }

    #[tool(
        description = "Hand the cut to another editor: write it as OpenTimelineIO (.otio: every track, clips, gaps, transitions, speed as LinearTimeWarp, markers, media references to the original files, each clip's full Kerf state in metadata.kerf) or a CMX 3600 EDL (.edl: one video track, frame-quantized at the timeline rate, 8-character reels, dissolves, M2 speed lines, drop-frame timecode at 29.97/59.94). Returns the path and a report of everything the format dropped or approximated (merged, with counts). Refuses to overwrite an existing file unless overwrite is true."
    )]
    async fn export_interchange(&self, Parameters(p): Parameters<ExportInterchangeParams>) -> Result<String, McpError> {
        crate::require_local_output_path(&p.output_path).map_err(|e| McpError::invalid_params(e, None))?;
        let path = std::path::PathBuf::from(&p.output_path);
        refuse_overwrite(&path, p.overwrite.unwrap_or(false))?;
        // Not onto a file the project still needs (a source, a proxy, the project itself).
        self.check_outputs(std::slice::from_ref(&path)).await?;
        let inputs = self.lock().delivery_inputs().map_err(core_err)?;
        let (format, track) = (p.format, p.video_track.unwrap_or(0));
        let report = blocking(move || {
            let (text, report) = Project::interchange_document(&inputs, format, track).map_err(core_err)?;
            Project::write_document(&path, &text).map_err(core_err)?;
            Ok(report)
        })
        .await?;
        json(&kerf_core::ExportedFile {
            path: p.output_path,
            report,
        })
    }

    #[tool(
        description = "Write the markers as a CSV (every marker: name, kind, start/end timecode, seconds, comment, colour; cells that look like spreadsheet formulas are defused) or as YouTube chapter text (`0:00 Intro` lines from the chapter markers; the report says when YouTube would refuse them). Refuses to overwrite an existing file unless overwrite is true."
    )]
    async fn export_markers(&self, Parameters(p): Parameters<ExportMarkersParams>) -> Result<String, McpError> {
        crate::require_local_output_path(&p.output_path).map_err(|e| McpError::invalid_params(e, None))?;
        let path = std::path::PathBuf::from(&p.output_path);
        refuse_overwrite(&path, p.overwrite.unwrap_or(false))?;
        // Not onto a file the project still needs (a source, a proxy, the project itself).
        self.check_outputs(std::slice::from_ref(&path)).await?;
        let inputs = self.lock().delivery_inputs().map_err(core_err)?;
        let format = p.format;
        let report = blocking(move || {
            let (text, report) = Project::markers_document(&inputs, format).map_err(core_err)?;
            Project::write_document(&path, &text).map_err(core_err)?;
            Ok(report)
        })
        .await?;
        json(&kerf_core::ExportedFile {
            path: p.output_path,
            report,
        })
    }

    #[tool(
        description = "When each speaker talks in an asset, from its speaker-labelled transcript: per speaker (S1, S2, … \
                       in order of first appearance) the SOURCE-time ranges of their words, joined across pauses \
                       shorter than join_gap, and their total speaking time. Use it to cut to one speaker (keep or \
                       remove their ranges with cut_clip_range / add_clip, which take source times) or to find who \
                       talks most. Ranges go by each word's own label, so a line get_transcript labels S1 (most of \
                       its words) can still lend a word to S2's ranges. Labels come from clustering voices without a \
                       model: distinct voices in clean turns separate well; overlapping speech or similar voices may \
                       not. At most `limit` ranges per speaker (default 200) from `offset`; `total_ranges` says how many \
                       there are and `more` whether any speaker has more. Errors if the transcript has no speaker \
                       labels (refine_transcript adds them)."
    )]
    fn speaker_ranges(&self, Parameters(p): Parameters<SpeakerRangesParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let join_gap = p.join_gap.filter(|g| g.is_finite()).unwrap_or(0.75).clamp(0.0, 10.0);
        let speaker = p.speaker.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let (offset, limit) = reads::page_of(p.offset, p.limit, 200, 2000);
        let ranges = self.lock().speaker_ranges(id, speaker, join_gap).map_err(core_err)?;
        // Each speaker's ranges are windowed alike: a long recording joins into hundreds.
        let more = ranges.iter().any(|s| offset.saturating_add(limit) < s.ranges.len());
        let speakers: Vec<serde_json::Value> = ranges
            .iter()
            .map(|s| {
                serde_json::json!({
                    "speaker": s.speaker,
                    "speaking": s.speaking,
                    "total_ranges": s.ranges.len(),
                    "ranges": s.ranges.iter().skip(offset).take(limit).collect::<Vec<_>>(),
                })
            })
            .collect();
        json(&serde_json::json!({ "speakers": speakers, "offset": offset, "limit": limit, "more": more }))
    }

    #[tool(
        description = "Find filler words in an asset's transcript by their word bounds: each hit's text, SOURCE-time \
                       start/end, the index of its line, and `estimated: true` when the word times were estimated \
                       rather than reported. Speech models often leave fillers out of a transcript, so few hits does \
                       not prove the speech has none. At most `limit` hits (default 200) from `offset`, with `total` \
                       and `next_offset`. Remove one with cut_clip_range (from/to = its start/end) on the \
                       clip that shows it; cut the latest first so earlier times do not shift."
    )]
    fn find_filler_words(&self, Parameters(p): Parameters<FillerWordsParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let fillers = p.fillers.unwrap_or_default();
        if fillers.len() > 100 {
            return Err(McpError::invalid_params("at most 100 fillers".to_string(), None));
        }
        let (offset, limit) = reads::page_of(p.offset, p.limit, 200, 1000);
        let found = self.lock().filler_words(id, &fillers).map_err(core_err)?;
        let items: Vec<_> = found.iter().skip(offset).take(limit).collect();
        let end = offset.saturating_add(items.len());
        json(&serde_json::json!({
            "total": found.len(),
            "offset": offset,
            "returned": items.len(),
            "next_offset": (end < found.len()).then_some(end),
            "fillers": items,
        }))
    }

    #[tool(
        description = "Re-time the words of an asset's cached transcript to the speech energy and label its speakers \
                       (S1, S2, …), without re-transcribing — for a transcript made before Kerf did this; a \
                       transcription already does it. No model is involved: it decodes the asset's audio once, as \
                       background work that waits behind exports and preview proxies, and the user can stop it like \
                       an analysis (or cancel the request, or pass `background: true` for a job_id). Refused if a \
                       newer transcript landed while it ran. Returns how many lines, words and speakers the \
                       transcript has now."
    )]
    async fn refine_transcript(
        &self,
        Parameters(p): Parameters<RefineTranscriptParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let (me, owner) = (self.clone(), self.owner());
        // The GUI's Stop on an analysis ends this too (`begin_analysis`).
        let gui_stop = self.app.state::<crate::AppState>().begin_analysis();
        self.long_tool("refine_transcript", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let (project, app) = (me.project.clone(), me.app.clone());
            let summary = blocking(move || {
                let stored = crate::run_refine(&app, &|| lock_agent_as(&project, owner.as_deref()), id, &|| run.cancelled())
                    .map_err(|e| McpError::internal_error(e, None))?;
                let mut speakers: Vec<&str> = stored.transcript.iter().filter_map(|s| s.speaker.as_deref()).collect();
                speakers.sort_unstable();
                speakers.dedup();
                Ok(serde_json::json!({
                    "segments": stored.transcript.len(),
                    "words": stored.transcript.iter().map(|s| s.words.len()).sum::<usize>(),
                    "speakers": speakers,
                }))
            })
            .await?;
            json(&summary)
        })
        .await
    }

    #[tool(
        description = "Append the non-silent spans of an asset as clips, end to end after what the first track of the asset's main kind already holds (the first video track for footage, the first audio track for audio-only media), as one revision. It needs the asset's `silence` analysis (analyze_asset with steps [\"silence\"]): with no analysis at all it fails, and if the silence step was never run it keeps the whole file. The silent spans are dropped, not rippled: nothing already on the track moves. Returns the clips."
    )]
    fn remove_silence(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        self.edit(|project| {
            let out = project.remove_silence(id).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Smart crop: frame each shot for the delivery frame instead of centring it. Samples where a clip's content actually sits and writes the crop that keeps it, per clip — so 16:9 footage delivered at 9:16 keeps the subject rather than whatever happened to be in the middle. Set the delivery frame first (set_delivery_format); clips already that shape are left alone. The result is an ordinary transform crop the user can adjust or undo. Sampling takes a moment per clip (send a `progressToken`, cancel the request, or pass `background: true` to run it as a job). Returns how many clips were reframed and a summary of the timeline."
    )]
    async fn smart_crop(
        &self,
        Parameters(p): Parameters<SmartCropParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let clip = p.clip_id.as_deref().map(|s| parse_arg("clip_id", s)).transpose()?;
        let (me, owner) = (self.clone(), self.owner());
        // The GUI's Stop on a smart crop ends an agent's too (`begin_smart_crop`).
        let gui_stop = self.app.state::<crate::AppState>().begin_smart_crop();
        self.long_tool("smart_crop", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let project = me.project.clone();
            let (moved, timeline) = blocking(move || {
                // Plan under the lock, sample with it released (one short ffmpeg decode per
                // clip), apply under it again — the same shape as analyze_asset, for the same
                // reason.
                let (plan, generation) = {
                    let project = lock_agent_as(&project, owner.as_deref());
                    (project.smart_crop_inputs(clip).map_err(core_err)?, project.generation())
                };
                let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                let crops = Project::sample_smart_crops_with(
                    &plan,
                    &mut |f| run.progress(Some(f64::from(f)), "sampling where each shot's content sits"),
                    &stop,
                )
                .map_err(core_err)?;
                let guard = lock_agent_edit(&project, owner.as_deref())?;
                require_generation(&guard, generation, "the shots were being framed")?;
                let moved = guard.apply_smart_crops(&crops).map_err(core_err)?;
                let timeline = guard.working_timeline().map_err(core_err)?;
                Ok((moved, timeline))
            })
            .await?;
            me.changed();
            json(&serde_json::json!({ "clips_reframed": moved, "timeline": reads::timeline_brief(&timeline) }))
        })
        .await
    }

    #[tool(
        description = "Sync a clip to another by their sound: decodes both clips' audio, finds the offset where \
                       they match (GCC-PHAT cross-correlation, to a fraction of a millisecond) and moves the clip \
                       so the same moment lands at the same timeline time — a camera's scratch audio against the \
                       recorder, two cameras on one stage. Only each clip's used source window (source_in..source_out, \
                       at most 20 minutes) is compared, so trim both to the stretch they share for a fast, clean \
                       match. Both clips need sound and normal forward speed, must not be linked to each other or \
                       share a track, and the moving clip's track must be unlocked — checked before anything is \
                       decoded. Send a `progressToken` to hear when it waits for another job; cancelling the request \
                       stops it, or pass `background: true` for a job. Returns `offset` (seconds the clip moved; positive is later), \
                       `timeline_start`, `confidence` (peak over noise, >= 8 is clear), `distinct` (peak over the \
                       next-best match, >= 1.5 is unambiguous — music can match at several offsets; null when nothing \
                       else matched at all), `reliable`, \
                       `inverted` (one recording has flipped polarity) and `applied`. An unreliable match does not \
                       move anything unless force is true."
    )]
    async fn sync_clips_by_audio(
        &self,
        Parameters(p): Parameters<SyncByAudioParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let (reference, clip) = (
            parse_arg("reference_clip_id", &p.reference_clip_id)?,
            parse_arg("clip_id", &p.clip_id)?,
        );
        let force = p.force.unwrap_or(false);
        let (me, owner) = (self.clone(), self.owner());
        self.long_tool("sync_clips_by_audio", p.background, &context, move |run| async move {
            let project = me.project.clone();
            let sync = blocking(move || {
                // Resolve under the lock, decode and correlate without it, move under it again.
                let (plan, generation) = {
                    let guard = lock_agent_as(&project, owner.as_deref());
                    (
                        guard.audio_sync_inputs(reference, clip).map_err(core_err)?,
                        guard.generation(),
                    )
                };
                let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                let measured =
                    Project::measure_audio_sync(&plan, &mut |w| run.progress(None, w.message()), &stop).map_err(core_err)?;
                // The sync moves a clip: it stages like any agent edit.
                let guard = lock_agent_edit(&project, owner.as_deref())?;
                require_generation(&guard, generation, "the clips were being synced")?;
                guard.apply_audio_sync(&measured, force).map_err(core_err)
            })
            .await?;
            if sync.applied {
                me.changed();
            }
            json(&sync)
        })
        .await
    }

    #[tool(
        description = "Cut to the beat: ripple a track's cuts onto the beat grid of the analyzed music on the audio tracks, retrimming each clip so its outgoing cut lands on a beat. Analyze the music asset first"
    )]
    fn snap_to_beats(&self, Parameters(p): Parameters<SnapToBeatsParams>) -> Result<String, McpError> {
        let track = p.track_id.as_deref().map(|s| parse_arg("track_id", s)).transpose()?;
        self.edit(|project| {
            let aligned = project.snap_to_beats(track, p.tolerance).map_err(core_err)?;
            // The *working* timeline, like every other read here: inside a task the
            // alignment lands in the staged proposal, and `timeline()` would hand
            // back the user's untouched cut — a timeline with none of the moves the
            // `cuts_aligned` count beside it just reported.
            let timeline = project.working_timeline().map_err(core_err)?;
            json(&serde_json::json!({ "cuts_aligned": aligned, "timeline": reads::timeline_brief(&timeline) }))
        })
    }

    #[tool(
        description = "Read the bar structure of an analyzed music asset, compactly: tempo, the fitted beat grid \
                       (period, phase, which beat is the downbeat), every bar start, and the repeating 8- and 4-bar \
                       phrases (bar indices counted from the first downbeat) that fit_music may splice between. \
                       Analyze the asset first; `music` is null when it has no steady pulse."
    )]
    fn get_music_structure(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let analysis = self.lock().get_analysis(id).map_err(core_err)?;
        let Some(analysis) = analysis else {
            return Err(McpError::invalid_params(
                "the asset has not been analyzed yet — call analyze_asset first",
                None,
            ));
        };
        let music = analysis.music.map(|m| {
            serde_json::json!({
                "bpm": m.grid.bpm(),
                "grid": m.grid,
                "duration": m.duration,
                "bar_seconds": m.grid.bar_s(),
                "bars": m.bar_chroma.len(),
                "bar_starts": (0..m.bar_chroma.len()).map(|k| m.grid.bar_start(k)).collect::<Vec<_>>(),
                "intro_end": m.grid.first_downbeat().max(0.0),
                "phrases": m.phrases,
            })
        });
        json(&serde_json::json!({ "asset_id": id, "music": music, "loudness": analysis.loudness }))
    }

    #[tool(
        description = "Preview fit_music without changing anything: the bar-aligned arrangement (source segments in \
                       output order), how long it lasts, the remainder (target minus duration: positive is short, \
                       negative runs over and fit_music would fade it out) and how many splices it needs. The intro \
                       and ending always play; only whole repeating phrases are repeated or dropped."
    )]
    fn plan_music_fit(&self, Parameters(p): Parameters<PlanMusicFitParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        let fit = self.lock().plan_music_fit(clip_id, p.target).map_err(core_err)?;
        json(&fit)
    }

    #[tool(
        description = "Fit music to length: replace a music clip with a bar-aligned edit list that lasts as long as \
                       the target (by default the picture from the clip's start). Keeps the intro and ending, repeats \
                       or drops whole repeating phrases, and crossfades each splice over 10 ms centred on it — \
                       non-destructive clips on the original file, nothing is rendered. Needs analyze_asset on the \
                       music first (a steady beat). If no arrangement hits the target on bar lines the nearest one is \
                       used and, with fade_out (default), cut at the target and faded. Check plan_music_fit first if \
                       the remainder matters. Returns the new clip ids, the plan and a summary of the timeline (get_timeline_state has all of it)."
    )]
    fn fit_music(&self, Parameters(p): Parameters<FitMusicParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let report = project
                .fit_music(clip_id, p.target, p.fade_out.unwrap_or(true))
                .map_err(core_err)?;
            let timeline = project.working_timeline().map_err(core_err)?;
            json(&serde_json::json!({ "fit": report, "timeline": reads::timeline_brief(&timeline) }))
        })
    }

    #[tool(
        description = "Give an asset's sound its own clip on an audio track, for every clip of the asset on a video \
                       track that still plays it: each is DETACHED (see detach_audio: an audio clip with the same span \
                       and position, linked to the picture, whose own sound is muted) so nothing is heard twice — in \
                       one revision. A clip that cannot be detached (its track is locked) is skipped and listed in \
                       `skipped` with the reason. Errors when there is nothing to extract (no clip of the asset on a \
                       video track, or its sound already detached) and does NOT append the asset's audio as a \
                       fallback: for the asset's whole audio as a clip of its own, use add_asset_audio. Returns \
                       `detached` (each: the audio `clip`, its `track_id`, `created_track`) and `skipped`."
    )]
    fn extract_audio(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        self.edit(|project| {
            let out = project.extract_audio(id).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Add an asset's WHOLE audio, from its start, as a new clip at the end of the first audio track \
                       (created when there is none). It touches no picture clip, so footage that is also on a video \
                       track playing its own sound is heard twice where the two overlap — to give a clip already cut its \
                       sound as an audio clip use extract_audio / detach_audio, which mute the picture. For music and \
                       other audio-only assets this is how they reach the timeline. One revision. Returns the clip."
    )]
    fn add_asset_audio(&self, Parameters(p): Parameters<AssetIdParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        self.edit(|project| {
            let out = project.add_asset_audio(id).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "DETACH a picture clip's own sound onto an audio track: a new audio clip with the SAME source \
                       span, speed and timeline position goes on the audio track at the picture's own position (V1 → \
                       A1) when it has room, else the first that does, else a new one; it is LINKED to the picture \
                       (so they move, trim, split and delete together — see link_clips) and the picture's own sound \
                       is muted, so the sound is heard once, from the audio track. The LEVEL is kept: the picture \
                       track's fader is folded into the new clip's volume (volume × picture fader ÷ audio fader) — \
                       except under a compressor or gate, which react to level: such a clip goes to an audio \
                       track whose fader EQUALS the picture track's (an existing one with room, else a new \
                       track at that fader) and its volume is left alone. \
                       What cannot be kept is the rest of the audio track's strip — its pan, duck and mute/solo \
                       decide the mix now, not the picture track's. Audio effects, fades and the transition's sound \
                       fade go with the audio clip. One revision. Errors if the clip is not on a video track, its \
                       asset has no audio, its sound is already detached, or a track involved is locked. Returns \
                       `clip` (the audio clip), `track_id` and `created_track`. reattach_audio undoes it; \
                       detach_audio_clips does several in one revision."
    )]
    fn detach_audio(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project.detach_audio(clip_id).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "DETACH the own sound of SEVERAL picture clips in ONE revision (one undo step) — detach_audio for \
                       each. A clip that cannot be (not a video clip, no audio, already detached, a locked track) is \
                       skipped and listed in `skipped` with the reason; errors only when none could be. Returns \
                       `detached` (each: the audio `clip`, `track_id`, `created_track`) and `skipped`."
    )]
    fn detach_audio_clips(&self, Parameters(p): Parameters<DetachClipsParams>) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        self.edit(|project| {
            let out = project.detach_audio_clips(&ids).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "REATTACH detached sound: delete the linked audio clip(s) carrying a picture's sound and let the \
                       picture play its own again, as it was before detach_audio (edits made to the audio clip are \
                       not carried back). Name either clip of the pair. A picture whose audio clip is already gone \
                       is just unmuted — unless another audio clip is already playing the same footage in step with \
                       it (unmuting would double the sound), which is refused. One revision. Returns the picture clip. \
                       reattach_audio_clips does several in one revision."
    )]
    fn reattach_audio(&self, Parameters(p): Parameters<ClipIdParams>) -> Result<String, McpError> {
        let clip_id = parse_arg("clip_id", &p.clip_id)?;
        self.edit(|project| {
            let out = project.reattach_audio(clip_id).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "REATTACH the detached sound of SEVERAL pictures in ONE revision (one undo step) — reattach_audio \
                       for each; name either clip of each pair (a pair named twice counts once). ALL OR NOTHING, unlike \
                       detach_audio_clips: a pair that cannot be reattached (a locked track, or the picture's sound \
                       already playing from another audio clip, which would double it) refuses the whole call and the \
                       error names the clip, changing nothing. Returns the picture clips."
    )]
    fn reattach_audio_clips(&self, Parameters(p): Parameters<ReattachClipsParams>) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        self.edit(|project| {
            let out = project.reattach_audio_clips(&ids).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "LINK clips into one group — a picture and the sound that goes with it, or any clips that must \
                       stay together. From then on move_clip(s) / trim / split_at / remove(_clips) / ripple_delete / \
                       cut_clip_range / set_speed / roll_edit / slip_clip / slide_clip / split_remove(_clips) carry the \
                       edit to the others (pass link=false on those to edit one alone) and any ripple, cut or delete \
                       that moves a linked clip moves its partners by the same amount — a sound that leads or trails \
                       its picture (a J- or L-cut) stays that far ahead or behind. Only linked clips follow: an \
                       unlinked clip on a partner's track stays where it was, and one in the way refuses the edit \
                       (as does a locked partner, a PICTURE a linked clip would run into or start before 0 — pictures are \
                       never trimmed to fit — or a clip that would be left under 0.05 s) with a reason; a linked SOUND \
                       in the way is trimmed back instead, and the revision label says so (\"… (trimmed sound on \
                       A2)\"). Two linked clips you name and move apart yourself are refused as \"out of step\" — \
                       unlink them first (naming both and cutting them at the same moment, as a trim to the \
                       playhead does, is not moving them apart). \
                       At least two clips, on different tracks, none on a locked track. detach_audio links for you. One revision. Returns the group's `link_id`."
    )]
    fn link_clips(&self, Parameters(p): Parameters<LinkClipsParams>) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        self.edit(|project| {
            let group = project.link_clips(&ids).map_err(core_err)?;
            json(&serde_json::json!({ "link_id": group, "clips": ids.len() }))
        })
    }

    #[tool(
        description = "UNLINK clips: each leaves its group, and a group left with a single clip dissolves (naming either \
                       half of a pair unlinks the pair). Errors when none of them was linked. One revision. Returns \
                       how many of the named clips were linked."
    )]
    fn unlink_clips(&self, Parameters(p): Parameters<UnlinkClipsParams>) -> Result<String, McpError> {
        let ids = p
            .clip_ids
            .iter()
            .map(|s| parse_arg("clip_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        self.edit(|project| {
            let unlinked = project.unlink_clips(&ids).map_err(core_err)?;
            json(&serde_json::json!({ "unlinked": unlinked }))
        })
    }

    #[tool(
        description = "Append the FULL length of each asset in `asset_ids`, in order, to the end of the first track of its kind (footage on the first video track, audio-only media on the first audio track), as one revision — so a video clip and an audio-only clip do not play in sequence with each other, each kind has its own run. For part of an asset use cut_clip or add_clip_to_timeline. Returns the clips."
    )]
    fn concatenate(&self, Parameters(p): Parameters<ConcatParams>) -> Result<String, McpError> {
        let ids = p
            .asset_ids
            .iter()
            .map(|s| parse_arg("asset_ids", s))
            .collect::<Result<Vec<Uuid>, _>>()?;
        self.edit(|project| {
            let out = project.concatenate(&ids).map_err(core_err)?;
            json(&out)
        })
    }

    #[tool(
        description = "Begin a staged edit: from here on your edits are held back as a proposal the user reviews \
                       instead of changing the cut they are looking at. Your own reads (get_timeline_state, \
                       preview_timeline, timeline_summary, export) follow the proposal, so you can check your work \
                       before handing it over. claim_next_task already does this for you, and while the user has \
                       review switched on (the default) your first edit opens the proposal by itself, so you rarely \
                       need this. The proposal belongs to your session: another agent connected at the same time is \
                       refused while it is open."
    )]
    fn stage_edits(&self, Parameters(p): Parameters<StageEditsParams>) -> Result<String, McpError> {
        let owner = self.owner();
        self.act(|project| {
            let staged = project
                .begin_staging(None, p.note.as_deref(), owner.as_deref())
                .map_err(core_err)?;
            json(&reads::staged_brief(&staged))
        })
    }

    #[tool(
        description = "What your staged edits would change about the user's cut: `diff.entries` — every add, cut, move, \
                       retrim and adjustment (each has an `id`), a page at a time (`limit` default 100, at most 500, from \
                       `offset`; `diff.entries.next_offset` pages on) — with the runtime and clip count before and after, \
                       and `staged` (the headline, your note, `undo_depth` — how many steps undo_staged_edit can take back \
                       — and whether it is `stale`) and any held-back library changes under `asset_changes`. Returns null \
                       when nothing is staged."
    )]
    fn staged_diff(&self, Parameters(p): Parameters<DiffPageParams>) -> Result<String, McpError> {
        let project = self.lock();
        match project.staged().map_err(core_err)? {
            None => json(&serde_json::json!(null)),
            Some(staged) => json(&serde_json::json!({
                "staged": reads::staged_brief(&staged),
                "asset_changes": staged.asset_changes,
                "diff": reads::diff_page(&staged.diff, p.offset, p.limit),
            })),
        }
    }

    #[tool(
        description = "Apply your staged edits to the live timeline, as one revision — ONLY while the user has agent \
                       review switched off (Settings › Agent). With review on (the default) this is refused: the user \
                       applies agent edits in Kerf, so finish with complete_task (or just tell them the proposal is \
                       ready) and ask them to review it; `force` does not get round that. With review off it fails if \
                       the user has edited the timeline since you staged, unless `force`. Returns a summary of the \
                       timeline afterwards (get_timeline_state has all of it)."
    )]
    fn apply_staged_edits(&self, Parameters(p): Parameters<ApplyStagedParams>) -> Result<String, McpError> {
        self.act(|project| {
            let timeline = project.apply_staged(p.force.unwrap_or(false)).map_err(core_err)?;
            json(&reads::timeline_brief(&timeline))
        })
    }

    #[tool(
        description = "Throw your staged edits away, leaving the user's timeline untouched. The proposal is kept aside, so \
                       restore_discarded_edits can bring it back if this was a mistake (the user can also restore it in \
                       Kerf). Returns a summary of the user's timeline."
    )]
    fn discard_staged_edits(&self) -> Result<String, McpError> {
        self.act(|project| {
            let timeline = project.discard_staged().map_err(core_err)?;
            json(&reads::timeline_brief(&timeline))
        })
    }

    #[tool(
        description = "Take back the last step of your staged edits (the proposal remembers its last 20), leaving the \
                       user's timeline alone. This is how to undo a mistake you made in the proposal; undo works on the \
                       live cut instead. Returns the label of the step taken back and how the proposal stands."
    )]
    fn undo_staged_edit(&self) -> Result<String, McpError> {
        self.act(|project| {
            let (label, staged) = project.undo_staged().map_err(core_err)?;
            json(&serde_json::json!({ "undone": label, "staged": reads::staged_brief(&staged) }))
        })
    }

    #[tool(
        description = "Bring back the proposal YOU last discarded (discard_staged_edits), once, and only while no proposal \
                       is open. It returns as it was, without its undo history, and may be `stale` if the user edited the \
                       timeline since. A proposal the user discarded is theirs to bring back, in Kerf: this refuses it."
    )]
    fn restore_discarded_edits(&self) -> Result<String, McpError> {
        self.act(|project| {
            let staged = project.restore_discarded().map_err(core_err)?;
            json(&reads::staged_brief(&staged))
        })
    }

    #[tool(
        description = "Apply only some of your staged changes (the `id`s of staged_diff's entries) to the live timeline, \
                       leaving the rest in the proposal — ONLY while agent review is switched off; with it on (the \
                       default) the user picks which changes to accept in Kerf and this is refused. A linked partner of a \
                       picked clip comes with it (the answer's `auto_included` says so); a pick that would collide with \
                       other clips or break a link is refused with where. Fails if the timeline moved on since you staged, \
                       unless `force`."
    )]
    fn apply_staged_subset(&self, Parameters(p): Parameters<ApplySubsetParams>) -> Result<String, McpError> {
        if p.entry_ids.is_empty() {
            return Err(McpError::invalid_params(
                "`entry_ids` must name at least one change (the `id`s in staged_diff)",
                None,
            ));
        }
        self.act(|project| {
            let applied = project
                .apply_staged_subset(&p.entry_ids, p.force.unwrap_or(false))
                .map_err(core_err)?;
            json(&serde_json::json!({
                "message": applied.message,
                "applied": applied.applied,
                "auto_included": applied.auto_included,
                "skipped": applied.skipped,
                "remaining": applied.remaining,
                "closed": applied.closed,
                "timeline": reads::timeline_brief(&applied.timeline),
            }))
        })
    }

    #[tool(
        description = "Explain what one revision changed (see history), as a list of edits to the cut: `diff.entries` a \
                       page at a time (`limit` default 100, at most 500, from `offset`; `diff.entries.next_offset` pages on), \
                       with the headline and the runtime and clip count before and after."
    )]
    fn revision_diff(&self, Parameters(p): Parameters<RevisionDiffParams>) -> Result<String, McpError> {
        let project = self.lock();
        let diff = match p.from {
            Some(from) => project.diff_revisions(from, p.seq).map_err(core_err)?,
            None => project.revision_diff(p.seq).map_err(core_err)?,
        };
        json(&serde_json::json!({ "diff": reads::diff_page(&diff, p.offset, p.limit) }))
    }

    #[tool(
        description = "List the timeline edit history, newest last: `revisions` (each with its `seq` — higher is newer — a \
                       label, who made it, and `current` on the one the timeline is at) and the `total` count. Returns \
                       the newest `limit` revisions (default 50, at most 500) after skipping `offset` of the newest; \
                       `next_offset` pages back through older ones."
    )]
    fn history(&self, Parameters(p): Parameters<PageParams>) -> Result<String, McpError> {
        let (offset, limit) = reads::page_of(p.offset, p.limit, 50, 500);
        let (revisions, total) = self
            .lock()
            .history_page(kerf_core::Page {
                limit: Some(limit),
                offset,
            })
            .map_err(core_err)?;
        let end = offset + revisions.len();
        json(&serde_json::json!({
            "total": total,
            "offset": offset,
            "returned": revisions.len(),
            "next_offset": (end < total).then_some(end),
            "revisions": revisions,
        }))
    }

    #[tool(
        description = "Undo the last edit to the user's LIVE timeline, returning a summary of the restored timeline. Only \
                       while agent review is off: with review on (the default) the live cut is the user's — take back your \
                       own staged work with undo_staged_edit instead."
    )]
    fn undo(&self) -> Result<String, McpError> {
        self.refuse_live_history()?;
        self.act(|project| {
            let out = project.undo().map_err(core_err)?;
            json(&reads::timeline_brief(&out))
        })
    }

    #[tool(
        description = "Redo the next edit on the user's LIVE timeline, returning a summary of the restored timeline. Only \
                       while agent review is off (see undo)."
    )]
    fn redo(&self) -> Result<String, McpError> {
        self.refuse_live_history()?;
        self.act(|project| {
            let out = project.redo().map_err(core_err)?;
            json(&reads::timeline_brief(&out))
        })
    }

    #[tool(
        description = "Revert the user's LIVE timeline to a specific revision seq (see history), returning a summary of the \
                       restored timeline. Only while agent review is off (see undo)."
    )]
    fn revert_to(&self, Parameters(p): Parameters<RevertParams>) -> Result<String, McpError> {
        self.refuse_live_history()?;
        self.act(|project| {
            let out = project.revert_to(p.seq).map_err(core_err)?;
            json(&reads::timeline_brief(&out))
        })
    }

    #[tool(
        description = "List the hardware (GPU) video encoders this machine's ffmpeg can actually use (verified by a \
                       test encode) — pass one as export's video_codec for a much faster render. Empty means software \
                       encoders only. libx264 stays the safe default: a hardware encoder trades file size for speed."
    )]
    async fn export_capabilities(&self) -> Result<String, McpError> {
        let encoders = blocking(|| Ok(kerf_core::hw_encoders().to_vec())).await?;
        json(&serde_json::json!({ "hw_encoders": encoders }))
    }

    #[tool(
        description = "Render the timeline to a file with full ffmpeg encode control (container, video/audio codec, \
                       rate control, resolution, fps, bitrate, faststart, gif, audio-only …). Omit `options` for the \
                       safe H.264/AAC MP4 default. `options.loudness` normalizes the mix to a platform target: \
                       youtube / spotify (-14 LUFS), apple (-16), broadcast (EBU R128, -23), all at -1 dBTP; add \
                       `options.loudness_static: true` to get there with one measured gain and a true-peak limiter \
                       (the mix's dynamics untouched; the render measures it first) instead of loudnorm's riding gain. A render takes minutes: send a `progressToken` in the request's \
                       `_meta` to receive progress notifications while it runs, and cancel the request \
                       (`notifications/cancelled`) to stop it — a cancelled render deletes its half-written file \
                       rather than leaving a broken one behind. Or pass `background: true` to get a job_id at once \
                       and poll job_status (cancel_job stops it). Refuses to overwrite an existing file unless \
                       overwrite is true, and refuses to write over a file the project still uses (a source, a \
                       proxy, the project itself)."
    )]
    async fn export(
        &self,
        Parameters(p): Parameters<ExportParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        crate::require_local_output_path(&p.output_path).map_err(|e| McpError::invalid_params(e, None))?;
        let output = std::path::PathBuf::from(&p.output_path);
        refuse_overwrite(&output, p.overwrite.unwrap_or(false))?;
        // Not onto a file the project still needs (a source, a proxy, the project itself).
        self.check_outputs(std::slice::from_ref(&output)).await?;
        let opts = ExportOptions {
            overwrite_sidecars: p.overwrite.unwrap_or(false),
            ..p.options.unwrap_or_default()
        };
        let (project, owner) = (self.project.clone(), self.owner());
        // A render runs for minutes. Without progress an agent cannot tell a slow export from a
        // hung one, and without cancellation it cannot abandon a render whose settings it already
        // knows are wrong. Both ride the protocol (`progressToken`, `notifications/cancelled`) —
        // or, as a background job, `job_status` and `cancel_job`.
        // ...and the GUI's Stop on an export ends this render too (`begin_export`).
        let gui_stop = self.app.state::<crate::AppState>().begin_export();
        self.long_tool("export", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let status = blocking(move || {
                // Snapshot the timeline + assets under the lock, then render with the
                // lock released so a long export doesn't freeze the GUI (or block other
                // agent tools) for its whole duration.
                let (timeline, assets) = {
                    let project = lock_agent_as(&project, owner.as_deref());
                    (
                        project.working_timeline().map_err(core_err)?,
                        project.list_assets().map_err(core_err)?,
                    )
                };
                let mut on_progress = |progress: kerf_core::ExportProgress| {
                    run.progress(
                        Some(progress.fraction),
                        match progress.eta_secs {
                            Some(eta) => format!("rendering, {} to go", fmt_ts(eta)),
                            None => "rendering".to_string(),
                        },
                    );
                };
                let before = kerf_core::file_mtime(&output);
                let status =
                    kerf_core::render_with_progress(&timeline, &assets, &output, &opts, &mut on_progress, &|| run.cancelled())
                        .map_err(|e| {
                            // A failed render must not leave a truncated file that reads as a
                            // finished one — and must not touch an earlier export it never wrote over.
                            kerf_core::discard_partial(&output, before);
                            core_err(e)
                        })?;
                if status == kerf_core::RenderStatus::Cancelled {
                    // Done *here*, not after the await — cancelling the request can drop this
                    // handler's future, while a blocking job runs to completion whether or not
                    // anyone is still holding its handle.
                    kerf_core::discard_partial(&output, before);
                }
                Ok((status, output))
            })
            .await?;
            match status {
                (kerf_core::RenderStatus::Completed, output) => json(&serde_json::json!({ "output": output })),
                (kerf_core::RenderStatus::Cancelled, output) => Err(McpError::internal_error(
                    format!("export cancelled; the partial file at {} was removed", output.display()),
                    None,
                )),
            }
        })
        .await
    }

    #[tool(
        description = "Export the same cut at several delivery frames in one call — one file per shape, each shot \
                       framed for each: a 9:16 Reel, a 1:1 post and a 16:9 upload from one timeline. Shots are \
                       smart-cropped per shape first (unless smart_crop is false), which is recorded on the clips as \
                       one revision and reused by later exports; generated captions are re-fit to each frame. The \
                       project's own frame is untouched. Files land beside output_path named by shape \
                       (`cut-9x16.mp4`). Reports each file with the platforms it is ready for and any issue, so \
                       there is no need to run platform_check per variant afterwards. Progress and cancellation \
                       work as in `export` (and `background: true` returns a job_id). The files render in one pass by \
                       default (single_pass): one decode of the sources split into every frame, so cancelling removes \
                       them all; with single_pass false they render one after another and cancelling keeps the files \
                       already finished and deletes the one in flight. Refuses to overwrite any existing variant file unless overwrite is true."
    )]
    async fn export_variants(
        &self,
        Parameters(p): Parameters<ExportVariantsParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        if p.formats.is_empty() {
            return Err(McpError::invalid_params(
                "formats must name at least one delivery frame",
                None,
            ));
        }
        crate::require_local_output_path(&p.output_path).map_err(|e| McpError::invalid_params(e, None))?;
        let mut deliveries: Vec<Delivery> = Vec::new();
        for name in &p.formats {
            let d = Delivery::parse(name).ok_or_else(|| {
                McpError::invalid_params(
                    format!("unknown delivery format {name:?}: expected 9:16, 1:1, 4:5, 16:9 or WxH"),
                    None,
                )
            })?;
            if !deliveries.contains(&d) {
                deliveries.push(d);
            }
        }
        let opts = p.options.unwrap_or_default();
        let base = std::path::PathBuf::from(&p.output_path);
        let variants: Vec<kerf_core::ExportVariant> = deliveries
            .iter()
            .map(|d| kerf_core::ExportVariant::beside(&base, *d))
            .collect();
        let overwrite = p.overwrite.unwrap_or(false);
        let opts = ExportOptions {
            overwrite_sidecars: overwrite,
            ..opts
        };
        for variant in &variants {
            refuse_overwrite(&variant.output, overwrite)?;
            kerf_core::check_caption_sidecars(&variant.output, &opts).map_err(core_err)?;
        }
        self.check_outputs(&variants.iter().map(|v| v.output.clone()).collect::<Vec<_>>())
            .await?;
        let smart_crop = p.smart_crop.unwrap_or(true);
        let single_pass = p.single_pass.unwrap_or(true);
        let (me, owner) = (self.clone(), self.owner());
        let generation = self.lock().generation();
        // The GUI's Stop on an export ends the framing pass and every file of this one too.
        let gui_stop = self.app.state::<crate::AppState>().begin_export();
        self.long_tool("export_variants", p.background, &context, move |run| async move {
            let run = run.also_cancelled_by(gui_stop);
            let project = me.project.clone();
            // The framing pass is the one write this tool makes. It is announced whether or not the
            // render after it succeeds: the proposal changed either way.
            let framed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let wrote = framed.clone();
            let labels: Vec<String> = deliveries.iter().map(Delivery::ratio_label).collect();
            let render_variants = variants.clone();
            let result = blocking(move || {
                // Frame first: plan under the lock, sample with it released (one
                // short decode per clip, shared by every shape), apply under it again.
                if smart_crop {
                    let plan = lock_agent_as(&project, owner.as_deref())
                        .framing_inputs(&deliveries)
                        .map_err(core_err)?;
                    if !plan.jobs.is_empty() {
                        let stop = crate::stop_or_replaced(|| run.cancelled(), &project, generation);
                        let framings = Project::sample_framings_with(
                            &plan,
                            &mut |f| run.progress(Some(f64::from(f) * 0.1), "framing each shot"),
                            &stop,
                        )
                        .map_err(core_err)?;
                        let guard = lock_agent_edit(&project, owner.as_deref())?;
                        require_generation(&guard, generation, "the shots were being framed")?;
                        wrote.store(guard.apply_framings(&framings).map_err(core_err)?, Ordering::SeqCst);
                    }
                }
                let (timeline, assets) = {
                    let project = lock_agent_as(&project, owner.as_deref());
                    (
                        project.working_timeline().map_err(core_err)?,
                        project.list_assets().map_err(core_err)?,
                    )
                };
                let mut on_progress = |progress: kerf_core::VariantProgress| {
                    let mut message = match progress.waiting {
                        Some(why) => why.to_string(),
                        None if progress.together => format!("rendering {} together ({})", progress.total, labels.join(", ")),
                        None => format!(
                            "rendering {} of {} ({})",
                            progress.variant + 1,
                            progress.total,
                            labels.get(progress.variant).cloned().unwrap_or_default()
                        ),
                    };
                    if let Some(eta) = progress.eta_secs {
                        message.push_str(&format!(", {} to go", fmt_ts(eta)));
                    }
                    run.progress(Some(progress.fraction), message);
                };
                // Core removes the file of a variant that failed or was cancelled in flight.
                let ended = kerf_core::render_variants(
                    &timeline,
                    &assets,
                    &render_variants,
                    &opts,
                    single_pass,
                    &mut on_progress,
                    &|| run.cancelled(),
                )
                .map_err(core_err)?;
                // Judged per file, at the frame that file actually is and over the span it writes.
                let range = opts.range.map(|r| (r.start, r.end));
                let mut outputs = Vec::new();
                let project = lock_agent_as(&project, owner.as_deref());
                for v in render_variants.iter().take(ended.completed) {
                    outputs.push(variant_output(&project, v, range)?);
                }
                Ok((ended.status, outputs, ended.together))
            })
            .await;
            let framed = framed.load(Ordering::SeqCst);
            if framed > 0 {
                me.changed();
            }
            let (status, outputs, together) = result?;
            match status {
                kerf_core::RenderStatus::Completed => json(&serde_json::json!({
                    "clips_framed": framed,
                    "single_pass": together,
                    "outputs": outputs,
                })),
                kerf_core::RenderStatus::Cancelled if together => Err(McpError::internal_error(
                    format!(
                        "export cancelled; the {} files rendering together were removed",
                        variants.len()
                    ),
                    None,
                )),
                kerf_core::RenderStatus::Cancelled => Err(McpError::internal_error(
                    format!(
                        "export cancelled after {} of {} files; the one in flight was removed",
                        outputs.len(),
                        variants.len()
                    ),
                    None,
                )),
            }
        })
        .await
    }

    #[tool(
        description = "Check the assembled cut against each publishing target (Instagram Reels / YouTube Shorts / \
                       TikTok / Instagram feed / YouTube): length limits, frame shape, and the reach limits a platform \
                       enforces silently — e.g. a Reel over 3 minutes uploads fine and is then shown only to existing \
                       followers. Returns errors (would be rejected), warnings (accepted but under-distributed or \
                       letterboxed) and tips. Advisory: export is never blocked. Pass `range` to judge only the span an \
                       export of that range would write (a Reel cut from the middle of a long timeline is judged at \
                       the Reel's length, frame and sound, not the timeline's), and `width` + `height` to judge it at the frame an export with a different `options.resolution` would write. Use before reporting a cut finished."
    )]
    fn platform_check(&self, Parameters(p): Parameters<PlatformCheckParams>) -> Result<String, McpError> {
        let frame = check_frame(p.width, p.height)?;
        // One summary, not two: `Project::platform_report` resolves its own, and
        // each resolve deserializes the whole timeline and re-reads every asset
        // row to answer the identical question.
        let summary = self
            .lock()
            .cut_summary_in(frame, p.range.map(|r| (r.start, r.end)))
            .map_err(core_err)?;
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
                       video at the shape the project is cut for. Refuses to overwrite an existing file unless \
                       overwrite is true."
    )]
    async fn export_cover(
        &self,
        Parameters(p): Parameters<CoverParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        crate::require_local_output_path(&p.output_path).map_err(|e| McpError::invalid_params(e, None))?;
        let output = std::path::PathBuf::from(&p.output_path);
        refuse_overwrite(&output, p.overwrite.unwrap_or(false))?;
        self.check_outputs(std::slice::from_ref(&output)).await?;
        let (project, owner) = (self.project.clone(), self.owner());
        let time_secs = p.time_secs;
        // The request's own cancellation, and the GUI's Cancel on a cover; progress rides the
        // request's `progressToken` in the stages a one-frame render has.
        let gui_stop = self.app.state::<crate::AppState>().begin_cover_export();
        let (run, relay) = jobs::foreground(&context);
        let run = run.also_cancelled_by(gui_stop);
        let written = blocking(move || {
            run.progress(Some(0.1), "reading the cut");
            let (timeline, assets) = lock_agent_as(&project, owner.as_deref())
                .export_still_inputs()
                .map_err(core_err)?;
            run.progress(Some(0.2), "rendering the cover frame");
            let written = Project::render_still_cancellable(&timeline, &assets, time_secs, &output, None, &|| run.cancelled())
                .map_err(core_err)?;
            run.progress(Some(1.0), "cover written");
            Ok(written)
        })
        .await;
        relay.finish().await;
        let written = written?;
        json(&serde_json::json!({ "output": written.to_string_lossy() }))
    }

    #[tool(
        description = "Measure the picture at a timeline time without looking at it: video scopes of the composited \
                       frame the export would write (originals, full delivery resolution). Returns a summary: per channel \
                       (red, green, blue, luma) min / max / mean in 0..1 and how many pixels are clipped at 0 and at 1 \
                       (`clipped_low` / `clipped_high`, counted over every pixel, so a single blown pixel shows), the mean \
                       chroma (`cb`, `cr` in -0.5..0.5; `hue_deg` counter-clockwise from +Cb, so red is ~103, skin tones \
                       ~123, blue ~355; `saturation` of the mean — a colour cast — and `mean_saturation`, how colourful \
                       the frame is, both as |CbCr| / 0.5) and the densest vectorscope spots. Use it to check exposure, \
                       clipping, a colour cast or a grade's effect after an edit. Grids are optional (counts, row 0 the \
                       lowest level)."
    )]
    async fn get_scopes(
        &self,
        Parameters(p): Parameters<ScopesParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        if !p.time_secs.is_finite() {
            return Err(McpError::invalid_params("time_secs must be a number", None));
        }
        let (project, owner) = (self.project.clone(), self.owner());
        let cancel = context.ct.clone();
        let req = kerf_core::scopes::ScopeRequest {
            time_secs: p.time_secs,
            max_width: p.max_width.map(|w| w.clamp(64, kerf_core::scopes::MAX_SCOPE_WIDTH)),
            matrix: p.matrix.unwrap_or_default(),
            options: kerf_core::scopes::ScopeOptions {
                grids: p.grids.unwrap_or_default(),
                size: p.grid_size.unwrap_or_default().resolve(),
            },
        };
        let scopes = blocking(move || {
            let (timeline, assets) = lock_agent_as(&project, owner.as_deref())
                .export_still_inputs()
                .map_err(core_err)?;
            kerf_core::scopes::timeline_scopes_cancellable(&timeline, &assets, &req, &|| cancel.is_cancelled()).map_err(core_err)
        })
        .await?;
        json(&scopes)
    }

    // ---- agent task queue --------------------------------------------------

    #[tool(
        description = "List the agent task queue with each task's status (queued/working/ready/done/failed), oldest first: \
                       `tasks` and the `total` count. Returns the newest `limit` tasks (default 50, at most 500) after \
                       skipping `offset` of the newest; `next_offset` pages back through older ones."
    )]
    fn list_tasks(&self, Parameters(p): Parameters<PageParams>) -> Result<String, McpError> {
        let (offset, limit) = reads::page_of(p.offset, p.limit, 50, 500);
        let (tasks, total) = self
            .lock()
            .list_tasks_page(kerf_core::Page {
                limit: Some(limit),
                offset,
            })
            .map_err(core_err)?;
        let end = offset + tasks.len();
        json(&serde_json::json!({
            "total": total,
            "offset": offset,
            "returned": tasks.len(),
            "next_offset": (end < total).then_some(end),
            "tasks": tasks,
        }))
    }

    #[tool(description = "Enqueue a new task (status: queued) for an agent to claim")]
    fn add_task(&self, Parameters(p): Parameters<AddTaskParams>) -> Result<String, McpError> {
        self.act(|project| {
            let task = project.add_task(&p.prompt).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(
        description = "Claim the oldest queued task (marks it working) and return it; returns null when the queue is empty. \
                       Claiming opens a proposal for the task, whether or not the user has review switched on: your edits go \
                       into it instead of the live cut, and the user reviews and accepts it in Kerf (only with review off \
                       may you apply_staged_edits yourself)."
    )]
    fn claim_next_task(&self) -> Result<String, McpError> {
        let owner = self.owner();
        self.act(|project| {
            let task = project.claim_next_task(owner.as_deref()).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(description = "Mark a claimed task ready for the user to review, with a summary of the edits made")]
    fn complete_task(&self, Parameters(p): Parameters<CompleteTaskParams>) -> Result<String, McpError> {
        let id = parse_arg("task_id", &p.task_id)?;
        self.act(|project| {
            let task = project.complete_task(id, p.result).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(description = "Mark a task failed with an error message")]
    fn fail_task(&self, Parameters(p): Parameters<FailTaskParams>) -> Result<String, McpError> {
        let id = parse_arg("task_id", &p.task_id)?;
        self.act(|project| {
            let task = project.fail_task(id, &p.error).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(
        description = "Mark a task done, returning it. This is what the user's accepting the proposal does; if the task still has a staged proposal it is applied with it, which an agent is refused while review is on (the user applies agent edits in Kerf), so do not call it to finish a task — complete_task is how you hand work over."
    )]
    fn resolve_task(&self, Parameters(p): Parameters<TaskIdParams>) -> Result<String, McpError> {
        let id = parse_arg("task_id", &p.task_id)?;
        self.act(|project| {
            let task = project.resolve_task(id).map_err(core_err)?;
            json(&task)
        })
    }

    #[tool(
        description = "Remove a task from the queue permanently, and with it the proposal staged under it (dismissing a task \
                       is how the user says no to its edits; the proposal can still be brought back with \
                       restore_discarded_edits). Refused, with the task left in the queue, when another agent session owns \
                       that proposal. Returns the id removed and how many tasks remain (list_tasks has them)."
    )]
    fn remove_task(&self, Parameters(p): Parameters<TaskIdParams>) -> Result<String, McpError> {
        let id = parse_arg("task_id", &p.task_id)?;
        self.act(|project| {
            project.remove_task(id).map_err(core_err)?;
            let remaining = project.list_tasks_page(kerf_core::Page::default()).map_err(core_err)?.1;
            json(&serde_json::json!({ "removed": id, "remaining_tasks": remaining }))
        })
    }

    #[tool(
        description = "Peak magnitude per bucket (0.0–1.0, unsigned) over the WHOLE of an asset's first audio stream, as a bare JSON array of `buckets` numbers (1–4096; a few hundred shows the shape). To look at a window, with a sign and per channel, use get_waveform_range; for how loud each slice FEELS use get_energy."
    )]
    async fn get_waveform(
        &self,
        Parameters(p): Parameters<WaveformParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let count = p.buckets.clamp(1, MAX_WAVEFORM_BUCKETS);
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let buckets = blocking(move || {
            // Resolve under the lock, decode the whole audio stream with it
            // released — bucketing a long source takes seconds and must not
            // stall the GUI's commands on the shared mutex. A dropped request
            // stops the decode.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            Project::decode_waveform_cancellable(&asset, count, &|| cancel.is_cancelled()).map_err(core_err)
        })
        .await?;
        json(&buckets)
    }

    #[tool(
        description = "Read what an asset's audio looks like over a window: the lowest and highest sample value in each of `buckets` equal slices of source time [start, end), per channel. Unlike get_waveform (one unsigned magnitude per bucket across the whole file) it is signed, one lane per channel (stereo gives two, mono one) and zoomable: ask for the whole asset in a few hundred buckets to find the loud, quiet and silent stretches, then ask again for a short window around one of them to find the exact moment a word, beat or click starts — the way to pick a cut point. Returns compact JSON {channels, buckets, duration, peaks_per_second, min, max}. `min` and `max` are each an array of `channels` arrays of `buckets` numbers in -1.0..1.0; bucket j covers source time start + j * (end - start) / buckets. `duration` is the length of the audio in seconds: a bucket before 0 or past it reads 0.0 / 0.0, so use `duration` to tell where the audio ends from silence. A value of exactly 1.0 or -1.0 is a clipped sample, and a run of them is distortion already baked into the source. `peaks_per_second` is the stored resolution the buckets were read from (10, 25, 100 or 500); when it is below buckets / (end - start) the request was finer than the stored 500 per second and neighbouring buckets repeat, so a shorter window will not reveal more. An asset with no audio stream (video-only footage, a still image) is rejected. The first call for a long file decodes it once, which can take seconds; every later window of that file is instant."
    )]
    async fn get_waveform_range(
        &self,
        Parameters(p): Parameters<WaveformRangeParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        require_window(p.start, p.end)?;
        let (start, end) = (p.start, p.end);
        let count = p.buckets.clamp(1, MAX_WAVEFORM_BUCKETS);
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let range = blocking(move || {
            // Resolve under the lock, read the pyramid with it released — the
            // first call for a file decodes all of its audio, which must not
            // stall the GUI's commands on the shared mutex. A dropped request
            // stops that decode (and caches nothing of it).
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            Project::decode_waveform_range_cancellable(&asset, start, end, count, &|| cancel.is_cancelled()).map_err(core_err)
        })
        .await?;
        json(&range)
    }

    #[tool(
        description = "Get an RMS energy envelope (0.0–1.0 per bucket) for an asset's first audio stream — a perceptual loudness-over-time curve. Unlike the peak waveform, it tracks how loud each slice feels, so use it to find quiet/loud passages and match cut pacing to musical energy."
    )]
    async fn get_energy(
        &self,
        Parameters(p): Parameters<WaveformParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let count = p.buckets.clamp(1, MAX_WAVEFORM_BUCKETS);
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let energy = blocking(move || {
            // Same lock-free, cancellable decode shape as `get_waveform`.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            Project::decode_energy_cancellable(&asset, count, &|| cancel.is_cancelled()).map_err(core_err)
        })
        .await?;
        json(&energy)
    }

    #[tool(
        description = "Measure how loud the cut is: the integrated loudness (LUFS), loudness range (LU), \
                       short-term maximum (LUFS), sample peak (dBFS) and true peak (dBTP), for the mix and \
                       for each track that feeds it, in one pass over the audio the \
                       export would render (so the master reading includes the master fader and limiter, and \
                       loudnorm when asked for). A track's reading is its strip: after its fader and pan, before \
                       the duck bus and the master. Platforms play social video at about -14 LUFS and ask for a \
                       true peak under -1 dBTP; `notes` says in words what is off and which tool fixes it \
                       (set_master_volume, set_master_limiter, set_track_volume, or exporting with loudnorm). \
                       Run it before you report a cut finished. A muted track is `heard: false` with no level, \
                       and a value of null means there was nothing to measure (silence; a short-term maximum \
                       needs a span of 3 seconds). It reads the whole span, so it takes a few seconds for a long \
                       cut; cancel the request to stop it. Reads the cut you are proposing, staged edits included."
    )]
    async fn get_levels(
        &self,
        Parameters(p): Parameters<GetLevelsParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let levels = blocking(move || {
            // Resolve under the lock, run ffmpeg over the whole span with it
            // released — a long cut takes seconds and must not stall the GUI.
            let (timeline, assets) = lock_agent(&project).levels_inputs().map_err(core_err)?;
            Project::measure_levels(&timeline, &assets, p.range, p.loudnorm.unwrap_or(false), &|| {
                cancel.is_cancelled()
            })
            .map_err(core_err)
        })
        .await?;
        json(&levels)
    }

    #[tool(
        description = "Measure where sound keeps its energy: six frequency bands — sub 20-60 Hz, bass 60-250, low_mid 250-500, mid 500-2k, high_mid 2k-6k, air 6k-20k — per window, each as an energy level (dBFS) and a `share_pct` of the window's energy across the bands, plus the window's RMS (dBFS), K-weighted loudness (LUFS, ungated, so a window reads like a loudness meter) and `dominant` band. \
                       Name exactly one target: `asset_id` (the asset's whole sound, in its own seconds), `clip_id` (that clip's source range at the clip's volume, in source seconds) or `range` {start, end} (the finished mix of the cut over that span of the timeline — faders, ducking, master bus and all, the audio get_levels measures; it reads the cut you are proposing, staged edits included). \
                       Windows: `sections` [{start, end, label?}] in the target's seconds; omitted, a composed asset's score sections (intro, drop 1, …), else the four-bar phrases of analyzed music, else eight equal windows — `windows_from` says which. `overall` is the whole stretch as one window, and `notes` says in words what stands out: a heavy low end, a thin mid range (500 Hz-6 kHz, where speech lives), a dull top, a loudness swing between sections, a section that is bassier than the whole. \
                       Use it after compose_music to see whether the drop is much louder than the intro, and under a voiceover: music with much in mid and high_mid fights speech, and a big sub / bass share eats headroom and vanishes on a phone. Fix with set_audio_effects (a highpass or EQ on the music clip), set_track_volume or set_track_duck, or compose again with another style / energy. Decodes the audio, so it takes a few seconds for a long one; cancel the request to stop it."
    )]
    async fn get_band_balance(
        &self,
        Parameters(p): Parameters<BandBalanceParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<String, McpError> {
        let request = kerf_core::BandBalanceRequest {
            asset_id: p.asset_id.as_deref().map(|s| parse_arg("asset_id", s)).transpose()?,
            clip_id: p.clip_id.as_deref().map(|s| parse_arg("clip_id", s)).transpose()?,
            range: p.range,
            sections: p.sections,
        };
        let project = self.project.clone();
        let owner = self.owner();
        let cancel = context.ct.clone();
        let balance = blocking(move || {
            // Resolve under the lock, decode with it released — a long cut takes seconds and must
            // not stall the GUI. A dropped request, or a project replaced meanwhile, stops it.
            let (plan, generation) = {
                let project = lock_agent_as(&project, owner.as_deref());
                (project.band_balance_inputs(&request).map_err(core_err)?, project.generation())
            };
            let stop = crate::stop_or_replaced(|| cancel.is_cancelled(), &project, generation);
            Project::measure_band_balance(&plan, &stop).map_err(core_err)
        })
        .await?;
        json(&balance)
    }

    #[tool(
        description = "Decode a single frame from an asset at a source time and return it as a low-res image the model can actually see. Use to drill into a specific moment (e.g. one cell flagged by skim_asset) before cutting."
    )]
    async fn get_frame(
        &self,
        Parameters(p): Parameters<FrameParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let (time_secs, max_width) = (p.time_secs, p.max_width.unwrap_or(640).clamp(64, MAX_PREVIEW_WIDTH));
        let region = p.region;
        let jpeg = blocking(move || {
            // Resolve under the lock, decode with it released — mirrors the GUI's
            // `get_frame` so an agent drill-in can't stall the user's edits.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            match region {
                // A zoom is a request for detail, so it decodes the original at
                // the best JPEG quality rather than the proxy at the preview one.
                Some(r) => {
                    Project::decode_preview_region_cancellable(&asset, time_secs, r.region(), max_width, ZOOM_QUALITY, &|| {
                        cancel.is_cancelled()
                    })
                }
                None => {
                    Project::decode_preview_frame_cancellable(&asset, time_secs, max_width, 4, true, &|| cancel.is_cancelled())
                }
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
    async fn skim_asset(
        &self,
        Parameters(p): Parameters<SkimParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let columns = p.columns.unwrap_or(4).clamp(1, 8);
        let rows = p.rows.unwrap_or(4).clamp(1, 8);
        let cell_width = p.cell_width.unwrap_or(240).clamp(80, 640);
        let project = self.project.clone();
        if let Some(cell) = p.cell {
            return self.skim_cell(id, &p, (columns, rows, cell), context).await;
        }
        let cancel = context.ct.clone();
        let (jpeg, times) = blocking(move || {
            // Resolve under the lock, sample the (columns × rows) frames with it
            // released — a contact sheet is many seeks and would otherwise freeze
            // the GUI's commands on the shared mutex for its whole build. A dropped
            // request stops the sampling.
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            Project::decode_contact_sheet_cancellable(&asset, p.start, p.end, columns, rows, cell_width, 5, &|| {
                cancel.is_cancelled()
            })
            .map_err(core_err)
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
        (columns, rows, cell): (u32, u32, u32),
        context: RequestContext<RoleServer>,
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
        let cancel = context.ct.clone();
        let (jpeg, time) = blocking(move || {
            let asset = lock_agent(&project).require_asset(id).map_err(core_err)?;
            // The same range defaults `decode_contact_sheet` applies, so the cell
            // lands on the frame the sheet showed.
            let start = start.unwrap_or(0.0).max(0.0);
            let end = end.unwrap_or(asset.duration).min(asset.duration).max(start);
            let time = kerf_core::contact_sheet_times(start, end, columns, rows)[(cell - 1) as usize];
            let jpeg = Project::decode_preview_region_cancellable(&asset, time, Region::FULL, max_width, ZOOM_QUALITY, &|| {
                cancel.is_cancelled()
            })
            .map_err(core_err)?;
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
        description = "Render the assembled timeline at a timeline time into one composite image the model can see — the actual cut on screen at that moment (footage layered in track order, picture-in-picture placement, crop, color; gaps render black). Use to verify an edit you just made; pass `region` to zoom into a detail of it. A moment inside a fade, dip, dissolve, slide or push is drawn as the export draws it: the outgoing clip under the incoming one's ramp, the dip's fade, the clips part-way across the frame."
    )]
    async fn preview_timeline(
        &self,
        Parameters(p): Parameters<TimelineFrameParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let project = self.project.clone();
        let cancel = context.ct.clone();
        let (time_secs, max_width) = (p.time_secs, p.max_width.unwrap_or(640).clamp(64, MAX_PREVIEW_WIDTH));
        let region = p.region;
        let jpeg = blocking(move || {
            // Snapshot the inputs under the lock, composite with it released —
            // mirrors the GUI's `get_timeline_frame`. A dropped request stops the render.
            let (timeline, assets) = lock_agent(&project).timeline_frame_inputs().map_err(core_err)?;
            match region {
                Some(r) => Project::composite_timeline_region_cancellable(
                    &timeline,
                    &assets,
                    time_secs,
                    r.region(),
                    max_width,
                    ZOOM_QUALITY,
                    &|| cancel.is_cancelled(),
                ),
                None => Project::composite_timeline_frame_cancellable(&timeline, &assets, time_secs, max_width, 4, &|| {
                    cancel.is_cancelled()
                }),
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

    #[tool(
        description = "Summarise the timeline: total duration, track count, clips per track, any per-track gaps, \
                       and per track how many clips are linked to a partner (`linked_clips`) and how many pictures \
                       have had their own sound detached (`detached_sound_clips`), and the animated SVG titles \
                       (`svg_overlays`: id, kind, timing, placement; no source). `media_notes` lists the media the cut \
                       uses whose preview proxy is not ready (`proxy` says queued / building / failed / missing / off, \
                       `clips_without_proxy` counts the clips per track — previews of those decode the original and are \
                       slower, export is not affected) or whose source file is gone (`source_missing`)"
    )]
    async fn timeline_summary(&self) -> Result<String, McpError> {
        let (project, owner) = (self.project.clone(), self.owner());
        let summary = blocking(move || {
            let (timeline, staged_changes, inputs) = {
                let project = lock_agent_as(&project, owner.as_deref());
                (
                    project.working_timeline().map_err(core_err)?,
                    project.staged().map_err(core_err)?.map(|staged| staged.diff.entries.len()),
                    project.proxy_inputs().map_err(core_err)?,
                )
            };
            // The proxies' state and the sources' presence come from the disk: lock released.
            let used: std::collections::HashSet<Uuid> = timeline
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .map(|c| c.asset_id)
                .collect();
            let used_inputs: Vec<_> = inputs.into_iter().filter(|i| used.contains(&i.asset.id)).collect();
            let proxies: std::collections::HashMap<Uuid, kerf_core::ProxyStatus> = kerf_core::proxy::statuses(&used_inputs)
                .into_iter()
                .map(|s| (s.asset_id, s))
                .collect();
            let assets: Vec<kerf_core::Asset> = used_inputs.into_iter().map(|i| i.asset).collect();
            let missing: std::collections::HashSet<Uuid> = assets
                .iter()
                .filter(|a| !std::path::Path::new(&a.path).exists())
                .map(|a| a.id)
                .collect();
            let media = reads::media_notes(&timeline, &assets, &proxies, &missing, reads::proxies_switched_off());
            let linked = timeline.linked_clip_ids();
            let tracks: Vec<TrackSummary> = timeline
                .tracks
                .iter()
                .enumerate()
                .map(|(i, t)| TrackSummary {
                    id: t.id.to_string(),
                    name: t.name.clone(),
                    kind: format!("{:?}", t.kind).to_lowercase(),
                    clip_count: t.clips.len(),
                    duration_secs: t.end(),
                    gaps: track_gaps(t),
                    linked_clips: t.clips.iter().filter(|c| linked.contains(&c.id)).count(),
                    detached_sound_clips: t.clips.iter().filter(|c| !c.source_audio).count(),
                    clips_without_proxy: media.per_track.get(i).copied().unwrap_or(0),
                })
                .collect();
            let total_clip_count = tracks.iter().map(|t| t.clip_count).sum();
            Ok(TimelineSummary {
                total_duration_secs: timeline.duration(),
                track_count: tracks.len(),
                total_clip_count,
                tracks,
                svg_overlays: timeline.svg_overlays.iter().map(SvgOverlaySummary::from).collect(),
                delivery_format: timeline
                    .format
                    .map(|d| format!("{}x{} ({})", d.width, d.height, format!("{:?}", d.fit).to_lowercase())),
                staged_changes,
                media_notes: media.notes,
            })
        })
        .await?;
        json(&summary)
    }
}

impl KerfMcp {
    fn new(project: Arc<Mutex<Project>>, app: AppHandle) -> Self {
        Self {
            project,
            app,
            session: Arc::new(Mutex::new(None)),
        }
    }

    /// Note which rmcp session a call arrived on (called for every tool call, before it runs).
    fn note_session(&self, context: &RequestContext<RoleServer>) {
        let session = session_id_of(context);
        if let Some(id) = &session {
            server::note_session_seen(id);
        }
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = session;
    }

    /// The session this agent is, for the proposal ownership rules.
    fn owner(&self) -> Option<String> {
        self.session.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Lock the shared project, attributing any edits made under this guard to
    /// the agent and saying which session it is. The GUI sets `User` the same way,
    /// and the mutex keeps the two from interleaving within a single operation.
    fn lock(&self) -> MutexGuard<'_, Project> {
        lock_agent_as(&self.project, self.owner().as_deref())
    }

    /// Run one timeline edit under the shared lock, release the lock, *then* tell the
    /// webview. Every mutating tool is this shape, and the order matters: the
    /// event makes the GUI re-fetch, and that re-fetch takes the same lock. A
    /// failed operation notifies nothing, because nothing changed.
    ///
    /// Before the edit, the project is told which session is acting
    /// ([`Project::ensure_agent_staging`]): a proposal another session owns refuses the edit
    /// there and then. The edit itself, with review on, opens the proposal it goes into — for
    /// this session, in its own transaction, so one that fails leaves nothing behind.
    fn edit<T>(&self, op: impl FnOnce(&Project) -> Result<T, McpError>) -> Result<T, McpError> {
        let owner = self.owner();
        // The guard is a temporary of this statement, so it is dropped here —
        // before the emit, and before anything the emit wakes up comes asking
        // for the lock.
        let out = {
            let project = lock_agent_edit(&self.project, owner.as_deref())?;
            op(&project)?
        };
        self.changed();
        Ok(out)
    }

    /// [`KerfMcp::edit`] for a change that is not an edit of the cut — a task moving along, a
    /// proposal being applied or discarded, the history stepping. It names the session but opens
    /// no proposal.
    fn act<T>(&self, op: impl FnOnce(&Project) -> Result<T, McpError>) -> Result<T, McpError> {
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

impl KerfMcp {
    /// Refuse any of `paths` as a place to write: the source of an asset, a capture file, a proxy,
    /// the project's own file. What the project holds is read under the lock; canonicalising and
    /// comparing the paths — filesystem calls, slow on a share that went away — happen on the
    /// blocking pool with the lock released.
    async fn check_outputs(&self, paths: &[std::path::PathBuf]) -> Result<(), McpError> {
        check_output_paths(self.project.clone(), self.owner(), paths.to_vec()).await
    }

    /// The user's live timeline is theirs while their review of agent edits is on: an agent
    /// stepping its history back or forward would change the cut with nothing to review.
    fn refuse_live_history(&self) -> Result<(), McpError> {
        if self.lock().agent_review() {
            return Err(core_err(kerf_core::Error::AgentReviewRequired));
        }
        Ok(())
    }
}

/// [`KerfMcp::check_outputs`] without the server around it.
async fn check_output_paths(
    project: Arc<Mutex<Project>>,
    owner: Option<String>,
    paths: Vec<std::path::PathBuf>,
) -> Result<(), McpError> {
    blocking(move || check_outputs_now(&project, owner.as_deref(), &paths)).await
}

/// [`check_output_paths`] for a caller already on the blocking pool. What the project protects is
/// read under the lock; comparing paths against it — and against Kerf's own settings and logs —
/// canonicalises and stats, which is done with the lock released.
fn check_outputs_now(project: &Mutex<Project>, owner: Option<&str>, paths: &[std::path::PathBuf]) -> Result<(), McpError> {
    let protected = lock_agent_as(project, owner).output_path_inputs().map_err(core_err)?;
    for path in paths {
        crate::check_output(path, &protected).map_err(core_err)?;
    }
    Ok(())
}

/// Forwards the `analysis-progress` events the GUI's bin listens to — for one asset — to a [`Run`],
/// so an agent hears what the user sees: the step and how far it is. Stops listening when dropped.
struct AnalysisProgressRelay {
    app: AppHandle,
    id: tauri::EventId,
}

impl AnalysisProgressRelay {
    fn new(app: &AppHandle, asset: Uuid, run: &Run) -> Self {
        use tauri::Listener;
        let (run, asset) = (run.clone(), asset.to_string());
        let id = app.listen("analysis-progress", move |event| {
            let Ok(p) = serde_json::from_str::<serde_json::Value>(event.payload()) else {
                return;
            };
            if p["asset_id"] != asset.as_str() {
                return;
            }
            let stage = p["stage"].as_str().unwrap_or("analysing");
            let message = match p["detail"].as_str() {
                Some(detail) => format!("{stage}: {detail}"),
                None => stage.to_string(),
            };
            run.progress(p["fraction"].as_f64(), message);
        });
        Self { app: app.clone(), id }
    }
}

impl Drop for AnalysisProgressRelay {
    fn drop(&mut self) {
        use tauri::Listener;
        self.app.unlisten(self.id);
    }
}

/// The rmcp session a request belongs to: the `Mcp-Session-Id` header of the HTTP request it
/// rode in on. `None` for a client that keeps no session, and for anything that did not come
/// over HTTP.
fn session_id_of(context: &RequestContext<RoleServer>) -> Option<String> {
    session_from_parts(context.extensions.get::<axum::http::request::Parts>()?)
}

fn session_from_parts(parts: &axum::http::request::Parts) -> Option<String> {
    let value = parts.headers.get("mcp-session-id")?.to_str().ok()?;
    (!value.is_empty()).then(|| value.to_string())
}

/// Lock the shared project outside `&self` (for closures moved onto the
/// blocking pool), attributing edits to the agent. Recovers from a poisoned
/// mutex (a panic while another op held the lock) rather than panicking here
/// too — a single failed op shouldn't brick the agent endpoint for the session.
fn lock_agent(project: &Mutex<Project>) -> MutexGuard<'_, Project> {
    note_agent_activity();
    let mut guard = project.lock().unwrap_or_else(|e| e.into_inner());
    guard.set_actor(EditSource::Agent);
    // The user's setting, which a project that was opened or created since startup does not
    // carry: read at every agent lock, so a change applies to the next call.
    guard.set_agent_review(crate::settings::agent_review());
    // A proposal whose agent session has ended (it closed, expired, or the server restarted with a
    // new token) is nobody's: the agent that reconnected, or the next one to edit, takes it over
    // instead of being told another agent owns it.
    if let Err(e) = guard.release_orphaned_staging(|owner| !server::session_alive(owner)) {
        tracing::warn!(error = %e, "could not look at who owns the staged proposal");
    }
    guard
}

/// [`lock_agent`] for a particular agent session (`owner`, its rmcp session id) — what the
/// proposal rules compare. Use it wherever a tool applies, discards or undoes staged work.
fn lock_agent_as<'a>(project: &'a Mutex<Project>, owner: Option<&str>) -> MutexGuard<'a, Project> {
    let guard = lock_agent(project);
    guard.declare_agent(owner);
    guard
}

/// [`lock_agent_as`] for an edit of the cut: it refuses at once a proposal another session owns.
/// (The edit then stages into the session's own proposal, opening it when review is on.)
fn lock_agent_edit<'a>(project: &'a Mutex<Project>, owner: Option<&str>) -> Result<MutexGuard<'a, Project>, McpError> {
    let guard = lock_agent_as(project, owner);
    guard.ensure_agent_staging(owner).map_err(core_err)?;
    Ok(guard)
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
/// every request rebuilds every route (well over a hundred tools): a schema-cache lookup, a boxed
/// handler and a map insert apiece, ~250 µs of release-build work per tool call
/// that produces the identical router every time. The routes are fixed at
/// compile time, so build them once and hand out a borrow.
fn router() -> &'static rmcp::handler::server::router::tool::ToolRouter<KerfMcp> {
    static ROUTER: OnceLock<rmcp::handler::server::router::tool::ToolRouter<KerfMcp>> = OnceLock::new();
    ROUTER.get_or_init(|| {
        let mut router = KerfMcp::tool_router()
            + KerfMcp::tool_router_jobs()
            + KerfMcp::tool_router_reads()
            + KerfMcp::tool_router_project()
            + KerfMcp::tool_router_templates();
        hints::annotate(&mut router);
        router
    })
}

/// Refuse a call naming an argument its tool does not take (`args`).
fn reject_unknown_arguments(request: &rmcp::model::CallToolRequestParams) -> Result<(), McpError> {
    let Some(tool) = router().get(&request.name) else {
        return Ok(());
    };
    match args::unknown_argument(tool, request.arguments.as_ref()) {
        Some(message) => Err(McpError::invalid_params(message, None)),
        None => Ok(()),
    }
}

/// How the server introduces itself to clients. `ServerConfig::default()` fills
/// `server_info` in from *rmcp's* own build env, so an untouched default has
/// every client listing this server as "rmcp".
fn server_identity() -> Implementation {
    Implementation::new("kerf", env!("CARGO_PKG_VERSION"))
}

/// What the server tells a client about itself and how to work (the MCP
/// `instructions`): the workflow an agent cannot infer from the tool list.
const INSTRUCTIONS: &str = "Kerf MCP server. The user queues editing tasks in the desktop app; \
             call claim_next_task to take the oldest one (or list_tasks to see \
             the whole queue). To work a task, inspect loaded media with \
             list_assets / get_asset_metadata / get_timeline_state (import_asset \
             loads a file the user has not added yet), run \
             analyze_asset to populate silence / scene / transcript / loudness \
             (EBU R128 LUFS) / onset / tempo (BPM + beat grid) / speech-vs-music \
             metadata. When the user points at something in the window — \
             \"this clip\", \"here\", \"the selection\", \"between the marks\" — \
             get_editor_context says what they have selected, where the playhead \
             and the in/out marks are and which part of the timeline is on screen. \
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
             ripple_delete closes the gap; move_clips / remove_clips do the \
             same to several clips as one revision, all or nothing). To adjust \
             a cut rather than build one: roll_edit moves the cut between two \
             adjacent clips (both shift, the pair's length is unchanged), \
             slip_clip shows other footage in the same place and length, \
             slide_clip moves a clip between its neighbours (they give way), \
             and split_remove trims a clip's start or end to a time (split_remove_clips \
             does that to a picture and its sound as one revision) — the first \
             three clamp to the footage and report `applied`, and never ripple. \
             The project may be in ripple mode, so check get_ripple_mode before you trim, \
             remove or retime: with it on, trim / set_speed / remove / \
             split_remove(_clips) / add_clip_to_timeline onto footage / \
             generate_voiceover / compose_music (replacing a clip) carry the \
             later clips on that track along by the change in length, gaps kept \
             (track by track — a V1 ripple leaves A1 where it is), and with it \
             off they leave the later clips where they were. Those tools take an \
             optional `ripple` (omitted follows the mode, false is the escape \
             hatch) to override it for one call; prefer that to set_ripple_mode, \
             which flips the user's own setting. A picture and its sound can be \
             LINKED: detach_audio splits a clip's own sound onto an audio track \
             (muting the picture's, so it is not heard twice — extract_audio does the \
             same for every use of an asset, detach_audio_clips for several clips \
             in one revision; reattach_audio / reattach_audio_clips undo it) and links the two, and from then on \
             move / trim / split / remove / speed / roll-slip-slide / ripple carry \
             a linked partner along (link_clips / unlink_clips edit the groups; \
             `link_id` in get_timeline_state shows them; pass `link: false` on any \
             of those tools to edit one clip alone, which can leave the pair out \
             of sync; a partner on a locked track refuses the edit, and so does \
             an unlinked clip, or a picture, that a linked clip would run into — the \
             error names the lane; a linked sound in the way is trimmed back and \
             the revision label says \"(trimmed sound on A2)\"). A ripple, a delete or a cut moves linked clips together by the \
             same amount, so a sound that leads or trails its picture stays that far \
             off; add_asset_audio puts an asset's whole audio on an audio track. Layer footage with add_track / \
             remove_track — e.g. add a video track and move_clip B-roll onto it \
             over the interview (later video tracks composite on top). Polish \
             with set_volume / set_fade (fade-in/out, e.g. to smooth hard cuts), \
             set_speed, set_transform (scale / position / rotation / opacity / \
             crop — picture-in-picture; set_transform_at for an animated clip, which keys \
             what is animated at a moment), set_color and set_transition (a fade \
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
             Burns zoom, a moving picture-in-picture); keys move linearly until \
             set_keyframe_easing gives a segment an ease or a hold, which is what \
             makes motion read as deliberate rather than mechanical. Any one number \
             can have keys of its own with set_property_keyframes — a colour that warms \
             over a shot, an opacity ramp that starts later than the zoom, the clip's \
             volume swelling or ducking mid-clip, a mask that moves, a crop that pushes \
             in or pans (crop_zoom / crop_x / crop_y) — and copy_keyframes gives another \
             clip the same animation. 360 footage (Insta360 \
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
             alone. import_captions brings in a .srt / .vtt / .ass / .ssa file instead \
             (base=timeline for a subtitle track made for the finished cut, \
             base=source with asset_id for a file that times one asset's own \
             footage) and replaces that same caption set — so it is the caption \
             step too: do it last, and re-run it after further edits. export_srt \
             writes an asset's transcript as a subtitle file; export_captions writes \
             the cut's captions (.srt / .vtt), and export's embed_captions / \
             caption_sidecars carry them with the render. A marker of kind `chapter` \
             becomes a chapter of the exported file; export_markers writes the \
             markers as CSV or YouTube chapters, and export_interchange hands the \
             cut to another editor as OTIO or a CMX 3600 EDL. \
             For motion graphics beyond a text overlay, add_svg_overlay puts an \
             animated SVG title on the cut: a ready-made preset (list_svg_presets: \
             fade_up, typewriter, lower_third, word_pop, filled in with `params`) \
             or your own `svg`. Your SVG is animated with SMIL only — animate, \
             set, animateTransform and animateColor — and its times are seconds \
             relative to the overlay's start. Give it a viewBox (or width and \
             height); it is fitted inside the overlay's x / y / width / height \
             rectangle (default the whole frame), so for artwork that covers the \
             frame use the delivery frame's own size as the viewBox. CSS \
             animations, scripts, external images and animateMotion are not \
             rendered (the result's `warnings` say what was dropped). SVG titles \
             sit above the footage and below text overlays and captions, are \
             placed in timeline time like any overlay (a later trim does not move \
             them, so add them once the cut is settled) and are listed in \
             timeline_summary; update_svg_overlay retimes or re-fills one, \
             remove_svg_overlay removes it. Write your own SVG in a loop: \
             validate_svg (its warnings say what would be dropped) and \
             preview_svg_overlay with `svg` (a contact sheet of the animation at \
             several moments) until it looks right, then add_svg_overlay, then \
             preview_timeline at a few moments between its start and end to \
             judge it against the footage. \
             To reuse a title, keep it as a template: when you write the SVG give \
             the elements you want to change `id` attributes, then validate_svg, \
             add_svg_overlay, preview, and save_template with `controls` (a \
             tagline, a brand colour, a logo: each names an element id and a kind \
             of text, color, number, font or image). list_templates shows the \
             user's library; apply_template puts one on the cut as an animated \
             title with `values` for its controls (a staged edit, like any other) \
             and set_template_values changes them later; a title made from a \
             template lists its controls and current values under `template` in \
             timeline_summary. update_svg_overlay with `svg` replaces the artwork \
             and ends the template. save_template, import_template and \
             export_template change the user's library or a file, not your \
             proposal, and delete_template is refused while the user reviews your \
             edits. \
             To narrate a cut, write the script and generate_voiceover it (English; \
             it lands on its own VO track with a transcript that is its script, \
             exactly timed), cut the picture to the narration, then caption last. \
             When the cut is going somewhere vertical, set_delivery_format sets \
             the frame it is being made for and smart_crop then frames each shot \
             for it — reshaping 16:9 footage to 9:16 throws away most of the \
             width, and without this the middle is what survives, subject or \
             not. Look at the result with preview_timeline; the crop is an \
             ordinary transform the user can adjust. When the same cut is \
             going to several places, export_variants renders it at every \
             shape (9:16, 1:1, 4:5, 16:9) in one call and one pass, framing each shot for \
             each shape and reporting what each file is ready for — prefer it \
             to exporting three times by hand. \
             Your edits are STAGED, not applied. While the user has review switched \
             on (the default; get_settings shows `agent_review`), claiming a task — \
             or simply making your first edit — opens a proposal that is yours \
             alone, and every edit goes into it instead of changing the cut the \
             user is looking at. Your own reads follow the proposal, so \
             get_timeline_state / preview_timeline / timeline_summary show the cut \
             you are building — check staged_diff to see exactly what you are about \
             to hand over, undo_staged_edit to take back the last step, \
             discard_staged_edits to abandon it (restore_discarded_edits brings the \
             last discarded one back). When finished call complete_task with a \
             short summary (or fail_task on error). Then the USER accepts the \
             proposal in Kerf — all of it or only some changes — or dismisses it. \
             You cannot accept it for them: apply_staged_edits, apply_staged_subset \
             and resolve_task are refused while review is on (`force` does not get \
             round that), and so are undo / redo / revert_to, which would rewrite \
             their live cut; say the proposal is ready and ask them to review it. \
             Review is the user's switch (Settings › Agent); with it off, those \
             tools work and your edits land as you make them — except while a \
             proposal is open (claim_next_task always opens one for its task, and \
             stage_edits opens one outside a task), when they go into it until you \
             apply_staged_edits or the user accepts it. One agent's proposal is \
             closed to another connected at the same time (it is yours again if \
             your connection drops and you reconnect). \
             Long tools (analyze_asset, export, export_variants, \
             generate_voiceover, compose_music, separate_stems, download_speech_model, \
             import_asset, smart_crop, refine_transcript, sync_clips_by_audio) take \
             `background: true`: they answer a \
             `job_id` at once and keep working, and job_status reports progress \
             and, at the end, the result (cancel_job stops one, list_jobs lists \
             them). Without it the call stays open — send a progressToken for \
             progress. Results are kept small: get_asset_metadata and analyze_asset \
             summarise an analysis (counts, durations, the head of the transcript), \
             and get_transcript (lines with their speakers; words=true adds each \
             line's timed words, best over a from/to window) / get_analysis_detail \
             page through the rest; \
             get_timeline_state shows generated captions as a window \
             (captions_offset / captions_limit) and, for a very long cut, its clips \
             (clips_offset / clips_limit); history, list_tasks, list_assets, \
             staged_diff and revision_diff page with offset / limit. list_assets shows each asset's preview proxy (ready, \
             queued, building with an ETA, failed, off) and which analyses have \
             run, and timeline_summary's `media_notes` lists the cut's media whose \
             proxy is not ready (previews of it are slower; export is unaffected) \
             or whose source file is missing (missing_assets lists every asset whose file \
             is gone; relinking or removing media is the user's, in Kerf's library). \
             project_info says whether the open \
             project is saved; open_project / new_project refuse to throw away \
             unsaved work, and `discard_unsaved` only works while the user's review is \
             off (otherwise ask them). save_project_as will not replace an existing file while \
             review is on. \
             Every applied edit is tracked: history lists the revisions, \
             revision_diff explains one of them, and undo / redo / revert_to roll \
             the live cut back (only while review is off; your own staged steps \
             are taken back with undo_staged_edit). Before you report a cut finished, run \
             platform_check: it says whether the length and frame shape suit \
             where it is going, including the reach limits a platform enforces \
             silently (a Reel over 3 minutes uploads fine and then reaches only \
             existing followers). For a cut going to social, also run get_levels: \
             platforms play at about -14 LUFS and want a true peak under -1 dBTP, \
             so check it, then fix a hot mix with set_master_volume / \
             set_master_limiter or export with loudnorm (which normalises to \
             -14 LUFS) or a `loudness` preset. For music under a cut: \
             analyze_asset the track, then fit_music fits it to the picture \
             with bar-aligned phrase splices (plan_music_fit / \
             get_music_structure to look first), and set_track_duck plus \
             set_master_duck dips it under dialogue. When there is no track \
             to use, or the user wants music made for the cut, write it: \
             compose_music takes a score (tempo, key, a style, sections of \
             chords, optional hits) and renders it to the M track, with its \
             tempo and bars already known (no analyze_asset). Cut the picture \
             first, then compose with align {to: \"cuts\"} so the tempo and the \
             impact / riser hits land on the cuts; fit_music if it is the \
             wrong length; set_track_duck plus set_master_duck under dialogue; \
             then get_levels for the loudness and get_band_balance to see \
             whether its low end or mids fight the voice. To change it, edit \
             the score (list_assets shows it as music_score) and compose again \
             with replace_clip_id. Call export to render, and export_cover to \
             write the thumbnail the platform shows before anyone presses play. \
             export / export_srt / export_captions / export_interchange / \
             export_markers / export_cover / export_variants all take an \
             absolute local output path and refuse to overwrite a file that's \
             already there unless you pass overwrite=true.";

#[tool_handler(router = router())]
impl ServerHandler for KerfMcp {
    /// The `#[tool_handler]` default behind the strict-argument check, plus the
    /// one place every failing tool call — whichever helper built the error —
    /// reaches the logfile.
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, McpError> {
        let tool = request.name.to_string();
        self.note_session(&context);
        let result = match reject_unknown_arguments(&request) {
            Ok(()) => {
                let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
                router().call(tcc).await
            }
            Err(e) => Err(e),
        };
        if let Err(e) = &result {
            log_tool_error(&tool, e);
        }
        result
    }

    fn get_info(&self) -> ServerConfig {
        // `initialize` is the one moment we know an agent is on the other end
        // of the socket, so it counts as being seen even before it calls a tool.
        note_agent_activity();
        let mut info = ServerConfig::default();
        info.server_info = server_identity();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(INSTRUCTIONS.to_string());
        info
    }
}

// ---- helpers ---------------------------------------------------------------

/// An id argument, parsed. A model that mistypes one is told **which** argument it was, not only
/// that "a uuid" is wrong: a call can carry half a dozen.
fn parse_arg(name: &str, s: &str) -> Result<Uuid, McpError> {
    Uuid::parse_str(s).map_err(|e| {
        McpError::invalid_params(
            format!("`{name}` must be a UUID as the tools return them (e.g. 3f2b8c1e-…), got '{s}': {e}"),
            None,
        )
    })
}

/// The group move a tool call asked for, with its ids parsed: a mistyped uuid is
/// the caller's, and says which one.
fn clip_moves(moves: &[ClipMoveParams]) -> Result<Vec<ClipMove>, McpError> {
    moves
        .iter()
        .map(|m| {
            Ok(ClipMove {
                clip_id: parse_arg("moves[].clip_id", &m.clip_id)?,
                timeline_start: m.timeline_start,
                track_id: m.track_id.as_deref().map(|s| parse_arg("moves[].track_id", s)).transpose()?,
            })
        })
        .collect()
}

/// The group cut a tool call asked for, with its ids parsed: a mistyped uuid is the
/// caller's, and says which one.
fn clip_cuts(cuts: &[ClipCutParams]) -> Result<Vec<ClipCut>, McpError> {
    cuts.iter()
        .map(|c| {
            Ok(ClipCut {
                clip_id: parse_arg("cuts[].clip_id", &c.clip_id)?,
                at: c.at,
            })
        })
        .collect()
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
        | E::SvgOverlayNotFound(_)
        | E::RevisionNotFound(_)
        | E::TaskNotFound(_)
        | E::InvalidArgument(_)
        | E::NoStagedEdit
        | E::StagedEditPending
        | E::StagedEditStale
        | E::StagedEditOwnedByOther
        | E::AgentReviewRequired
        | E::NoDiscardedEdit
        | E::AssetInUse { .. } => McpError::invalid_params(e.to_string(), None),
        // A database, io, ffmpeg or engine failure is ours, not the caller's.
        other => McpError::internal_error(other.to_string(), None),
    }
}

/// The project the call was made on is gone: the user opened or started another while `doing`
/// ran. The check is the one the GUI's jobs make (`crate::require_generation`); what a model
/// needs on top of it is what to do next.
fn require_generation(project: &Project, expected: u64, doing: &str) -> Result<(), McpError> {
    crate::require_generation(project, expected, doing)
        .map_err(|why| McpError::invalid_params(format!("{why}. Run it again on the project that is open now"), None))
}

/// A caller's mistake (a stale id, a bad argument) is a `warn`; anything else
/// is ours, and an `error`.
fn log_tool_error(tool: &str, e: &McpError) {
    if matches!(
        e.code,
        rmcp::model::ErrorCode::INVALID_PARAMS | rmcp::model::ErrorCode::METHOD_NOT_FOUND
    ) {
        tracing::warn!(target: "mcp", tool, "tool call rejected: {}", e.message);
    } else {
        tracing::error!(target: "mcp", tool, code = e.code.0, "tool call failed: {}", e.message);
    }
}

/// Wrap JPEG bytes as an MCP tool result the model can *see*: a caption text
/// block followed by an image content block (rmcp expects bare base64 + MIME,
/// not a `data:` URL).
fn image_result(caption: String, jpeg: Vec<u8>) -> CallToolResult {
    image_block(caption, &jpeg, "image/jpeg")
}

/// [`image_result`] for PNG bytes: a drawing with flat colour and sharp edges, where JPEG smears.
fn png_result(caption: String, png: Vec<u8>) -> CallToolResult {
    image_block(caption, &png, "image/png")
}

fn image_block(caption: String, bytes: &[u8], mime: &str) -> CallToolResult {
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    CallToolResult::success(vec![ContentBlock::text(caption), ContentBlock::image(b64, mime)])
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

/// The frame a `platform_check` is asked about: both sides or neither, each a sane size.
fn check_frame(width: Option<u32>, height: Option<u32>) -> Result<Option<(u32, u32)>, McpError> {
    const MAX_SIDE: u32 = 16_384;
    match (width, height) {
        (None, None) => Ok(None),
        (Some(w), Some(h)) if (1..=MAX_SIDE).contains(&w) && (1..=MAX_SIDE).contains(&h) => Ok(Some((w, h))),
        (Some(_), Some(_)) => Err(McpError::invalid_params(
            format!("`width` and `height` must each be 1 to {MAX_SIDE} pixels"),
            None,
        )),
        _ => Err(McpError::invalid_params(
            "give `width` and `height` together, or neither for the project's own frame",
            None,
        )),
    }
}

/// A window of source time a tool can read. The engine answers an empty,
/// inverted or non-finite one with `buckets` silent buckets — right for the
/// timeline, whose windows are clips it already holds, but to a model a wall of
/// zeros reads as "this part is silent", so a swapped `start` and `end` is
/// reported as the mistake it is.
fn require_window(start: f64, end: f64) -> Result<(), McpError> {
    if start.is_finite() && end.is_finite() && end > start {
        Ok(())
    } else {
        Err(McpError::invalid_params(
            format!("the window must have end greater than start, got start={start}, end={end}"),
            None,
        ))
    }
}

/// One rendered variant as `export_variants` reports it: the file, and the platforms it is ready for
/// with every issue that is more than a tip — judged at *that* file's frame and over the span it wrote
/// (`range`, the export's own), so a Reel cut from a long timeline is not held to the timeline's length.
fn variant_output(
    project: &Project,
    v: &kerf_core::ExportVariant,
    range: Option<(f64, f64)>,
) -> Result<serde_json::Value, McpError> {
    let summary = project
        .cut_summary_in(Some((v.delivery.width, v.delivery.height)), range)
        .map_err(core_err)?;
    let checks = kerf_core::platform::check_all(&summary);
    let is_tip = |i: &kerf_core::platform::DeliveryIssue| i.severity == kerf_core::platform::Severity::Tip;
    let ready_for: Vec<&str> = checks
        .iter()
        .filter(|c| c.issues.iter().all(is_tip))
        .map(|c| c.label.as_str())
        .collect();
    let issues: Vec<serde_json::Value> = checks
        .iter()
        .flat_map(|c| {
            c.issues
                .iter()
                .filter(|i| !is_tip(i))
                .map(move |i| serde_json::json!({ "target": c.label, "severity": i.severity, "message": i.message }))
        })
        .collect();
    Ok(serde_json::json!({
        "format": v.delivery.ratio_label(),
        "width": v.delivery.width,
        "height": v.delivery.height,
        "output": v.output.to_string_lossy(),
        "ready_for": ready_for,
        "issues": issues,
    }))
}

/// Refuse to silently clobber an existing file. A model that hallucinates or
/// mis-remembers a path would otherwise destroy whatever is already there with
/// no recourse — this is the write side of `require_local_output_path`.
fn refuse_overwrite(path: &std::path::Path, overwrite: bool) -> Result<(), McpError> {
    if !overwrite && path.exists() {
        return Err(McpError::invalid_params(
            format!(
                "{} already exists; pass overwrite=true to replace it or choose a new path",
                path.display()
            ),
            None,
        ));
    }
    Ok(())
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

/// The subtitle text an `import_captions` call carries: a file, read with the
/// engine's guards (`.srt` / `.ass` / `.ssa`, a regular file, at most 5 MiB), or
/// the text passed inline — exactly one of the two. A path must be absolute: a
/// relative one would resolve against wherever the app happened to start.
fn caption_source(path: Option<&str>, text: Option<String>) -> Result<String, McpError> {
    match (path, text) {
        (Some(path), None) => {
            crate::require_local_input_path(path).map_err(|e| McpError::invalid_params(e, None))?;
            let path = std::path::Path::new(path);
            if !path.is_absolute() {
                return Err(McpError::invalid_params(
                    format!("path {} must be an absolute file path", path.display()),
                    None,
                ));
            }
            kerf_core::read_caption_file(path).map_err(core_err)
        }
        (None, Some(text)) => Ok(text),
        _ => Err(McpError::invalid_params(
            "give exactly one of `path` (a .srt / .ass / .ssa file) or `text` (its contents)",
            None,
        )),
    }
}

/// A tool's answer as compact JSON: a model reads every byte, and indentation is a third of
/// what a pretty-printed timeline weighs.
fn json<T: Serialize>(value: &T) -> Result<String, McpError> {
    serde_json::to_string(value).map_err(|e| McpError::internal_error(e.to_string(), None))
}

#[cfg(test)]
mod tests {
    use super::{
        caption_source, clip_moves, core_err, elide_large_svg, fmt_ts, image_result, log_tool_error, png_result,
        refuse_overwrite, reject_unknown_arguments, require_window, router, server_identity, svg_moments, svg_preview,
        svg_sheet_caption, svg_sheet_times, svg_source, track_gaps, variant_output, ClipMoveParams, SvgMoments,
        SvgOverlaySummary, SvgPreviewTitle, SvgValidation,
    };

    #[test]
    fn set_grade_refuses_a_misspelt_key_inside_the_curves_or_wheels() {
        let parse = |v: serde_json::Value| serde_json::from_value::<super::GradeParams>(v);
        let ok = parse(serde_json::json!({
            "clip_id": "c",
            "curves": {"master": [{"x": 0, "y": 1}, {"x": 1, "y": 0}]},
            "wheels": {"shadows": {"x": 0.2}}
        }))
        .unwrap();
        let curves = kerf_core::Curves::from(ok.curves.unwrap());
        assert_eq!(curves.master.len(), 2);
        assert_eq!(kerf_core::Wheels::from(ok.wheels.unwrap()).shadows.x, 0.2);
        for bad in [
            serde_json::json!({"clip_id": "c", "curves": {"mastre": []}}),
            serde_json::json!({"clip_id": "c", "curves": {"master": [{"x": 0, "y": 1, "z": 2}]}}),
            serde_json::json!({"clip_id": "c", "wheels": {"midtone": {}}}),
            serde_json::json!({"clip_id": "c", "wheels": {"shadows": {"lightnes": 0.1}}}),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn set_property_tracks_refuses_a_track_without_its_keys() {
        let parse = |v: serde_json::Value| serde_json::from_value::<super::PropertyTracksParams>(v);
        let ok = parse(serde_json::json!({
            "clip_id": "c",
            "tracks": [{"prop": "opacity", "keys": []}, {"prop": "volume", "keys": [{"time": 0, "value": 1}]}]
        }))
        .unwrap();
        let tracks: Vec<kerf_core::PropertyTrack> = ok.tracks.into_iter().map(Into::into).collect();
        assert_eq!(tracks[1].keys.len(), 1);
        for bad in [
            serde_json::json!({"clip_id": "c", "tracks": [{"prop": "opacity"}]}),
            serde_json::json!({"clip_id": "c", "tracks": [{"prop": "opacity", "key": [{"time": 0, "value": 1}]}]}),
            serde_json::json!({"clip_id": "c", "tracks": [{"prop": "opacity", "keys": [], "easing": "hold"}]}),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn log_tool_error_handles_both_kinds() {
        log_tool_error("t", &rmcp::ErrorData::invalid_params("bad id", None));
        log_tool_error("t", &rmcp::ErrorData::internal_error("boom", None));
    }

    #[test]
    fn refuse_overwrite_blocks_an_existing_file_unless_opted_in() {
        let path = std::env::temp_dir().join(format!("kerf-refuse-overwrite-test-{}.tmp", std::process::id()));
        std::fs::write(&path, b"existing").unwrap();

        let err = refuse_overwrite(&path, false).expect_err("must refuse an existing file by default");
        assert!(err.message.contains("overwrite=true"), "{}", err.message);

        assert!(refuse_overwrite(&path, true).is_ok(), "overwrite=true must let it through");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn refuse_overwrite_allows_a_path_that_does_not_exist() {
        let path = std::env::temp_dir().join(format!("kerf-refuse-overwrite-missing-{}.tmp", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(refuse_overwrite(&path, false).is_ok());
    }

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

    #[test]
    fn a_caption_import_takes_exactly_one_of_a_path_or_text() {
        let invalid = |r: Result<String, rmcp::ErrorData>| {
            let err = r.expect_err("must be refused");
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{}", err.message);
            err.message.to_string()
        };
        // Inline text passes straight through.
        assert_eq!(caption_source(None, Some("1\n".into())).unwrap(), "1\n");
        // Neither, or both.
        assert!(invalid(caption_source(None, None)).contains("exactly one"));
        assert!(invalid(caption_source(Some("/a/b.srt"), Some("x".into()))).contains("exactly one"));
        // A path is absolute, and the engine's guards still apply to it.
        assert!(invalid(caption_source(Some("subs/movie.srt"), None)).contains("absolute"));
        let dir = std::env::temp_dir().join(format!("kerf-mcp-caption-source-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let srt = dir.join("cues.srt");
        std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,000\nHi\n").unwrap();
        assert!(caption_source(srt.to_str(), None).unwrap().contains("Hi"));
        let txt = dir.join("cues.txt");
        std::fs::write(&txt, "x").unwrap();
        assert!(invalid(caption_source(txt.to_str(), None)).contains(".srt, .vtt, .ass or .ssa"));
        assert!(invalid(caption_source(dir.join("missing.srt").to_str(), None)).contains("cannot read"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The import tool takes a file *or* text and neither is mandatory: the
    /// schema must not claim one is required, or a caller sending the other is
    /// rejected before the tool can say which it wanted.
    #[test]
    fn caption_import_parameters_are_all_optional() {
        let tools = router().list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == "import_captions")
            .expect("import_captions is registered");
        let required = tool.input_schema.get("required").and_then(|r| r.as_array());
        assert!(required.is_none_or(|r| r.is_empty()), "{required:?}");
        let props = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("properties");
        for name in ["path", "text", "format", "base", "asset_id", "style"] {
            assert!(props.contains_key(name), "{name} is missing from the schema");
        }
    }

    /// The transcript tools are registered and need only the asset: words, speaker, gap and
    /// fillers are all optional.
    #[test]
    fn transcript_tools_take_only_the_asset() {
        let tools = router().list_all();
        for name in ["get_transcript", "speaker_ranges", "find_filler_words", "refine_transcript"] {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is registered"));
            let required: Vec<&str> = tool
                .input_schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            assert_eq!(required, ["asset_id"], "{name}");
        }
    }

    /// `analyze_asset` runs only the steps it is asked for, and an agent that names none gets
    /// the user's own choice: the schema keeps `steps` optional, names every kind, and a bad name
    /// is refused before anything runs. The proxy tools an agent steers a cache with exist.
    #[test]
    fn analyze_asset_takes_optional_steps_and_the_proxy_tools_are_registered() {
        let tools = router().list_all();
        let tool = |name: &str| {
            tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is registered"))
        };
        let analyze = tool("analyze_asset");
        let required: Vec<&str> = analyze
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(required, ["asset_id"], "`steps` must stay optional");
        let schema = serde_json::to_string(&*analyze.input_schema).unwrap();
        for kind in ["silence", "scenes", "loudness", "rhythm", "transcript", "all"] {
            assert!(schema.contains(kind), "`steps` does not name `{kind}`: {schema}");
        }
        for name in ["proxy_status", "rebuild_proxy", "delete_proxy", "analysis_status"] {
            let tool = tool(name);
            assert!(!tool.description.as_deref().unwrap_or_default().is_empty(), "{name}");
        }
        let status_required = tool("proxy_status")
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.len());
        assert!(
            status_required.is_none_or(|n| n == 0),
            "proxy_status takes no required argument"
        );
        assert!(tool("get_asset_metadata")
            .description
            .as_deref()
            .unwrap()
            .contains("analysis_status"));

        // The names an agent sends are parsed the way the tool parses them.
        let parsed = crate::parse_steps(Some(vec!["transcript".into(), "silence".into()]))
            .unwrap()
            .unwrap();
        assert_eq!(
            parsed,
            [kerf_core::AnalysisKind::Silence, kerf_core::AnalysisKind::Transcript]
        );
        assert!(crate::parse_steps(None).unwrap().is_none(), "no steps means the default set");
        assert!(crate::parse_steps(Some(vec!["colour".into()]))
            .unwrap_err()
            .contains("colour"));
        assert!(
            crate::parse_steps(Some(Vec::new())).is_err(),
            "an empty list is a mistake, not a no-op"
        );
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

    /// The edit modes are documented in the units a model can act on: deltas in
    /// seconds, and the half to remove a closed set — a typo in `side` is the
    /// caller's to fix at the schema, not a silent default.
    #[test]
    fn the_edit_mode_tools_take_a_delta_and_a_closed_side() {
        let tools = router().list_all();
        let required = |name: &str| {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is registered"));
            let mut required: Vec<String> = tool
                .input_schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            required.sort();
            (required, serde_json::to_string(&*tool.input_schema).unwrap())
        };
        assert_eq!(required("roll_edit").0, ["clip_a", "clip_b", "delta"]);
        assert_eq!(required("slip_clip").0, ["clip_id", "delta"]);
        assert_eq!(required("slide_clip").0, ["clip_id", "delta"]);
        assert_eq!(required("split_remove_clips").0, ["cuts", "side"]);
        let (split, schema) = required("split_remove");
        assert_eq!(split, ["at", "clip_id", "side"]);
        assert!(schema.contains("left") && schema.contains("right"), "{schema}");

        let ok = |side: &str| {
            serde_json::from_value::<super::SplitRemoveParams>(serde_json::json!({ "clip_id": "x", "at": 1.0, "side": side }))
        };
        assert!(ok("left").is_ok() && ok("right").is_ok());
        assert!(ok("middle").is_err());
    }

    /// A detached picture carries no sound, so the property edits an agent would try on it
    /// (volume, fade, disabling) change nothing audible: the tools say so, and where its
    /// sound is instead.
    #[test]
    fn the_property_tools_say_a_detached_picture_carries_no_sound() {
        let tools = router().list_all();
        for name in ["set_volume", "set_fade", "set_clip_enabled"] {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is registered"));
            let description = tool.description.as_deref().unwrap_or_default();
            assert!(
                description.contains("source_audio: false") && description.contains("linked audio clip"),
                "`{name}` should warn about a detached picture: {description}"
            );
        }
    }

    /// A keyed number ignores its static value, and `set_keyframes` clears the whole-transform
    /// keys only: the tools an agent would reach for say so, rather than the agent finding out by
    /// a clip that is still moving after "clearing" it.
    #[test]
    fn the_static_setters_and_set_keyframes_say_what_an_animated_number_does_to_them() {
        let tools = router().list_all();
        let text = |name: &str| {
            tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is registered"))
                .description
                .as_deref()
                .unwrap_or_default()
                .to_string()
        };
        for name in ["set_volume", "set_color", "set_transform"] {
            let d = text(name);
            assert!(
                d.contains("set_property_keyframes") && (d.contains("not used") || d.contains("not from this static value")),
                "`{name}` should say its static value is ignored while the number is animated: {d}"
            );
        }
        let d = text("set_keyframes");
        assert!(
            d.contains("only") && d.contains("keeps them") && d.contains("clear_keyframes"),
            "`set_keyframes` clears the bundle only: {d}"
        );
        let d = text("clear_keyframes");
        assert!(d.contains("of their own") && d.contains("Colour and volume keys stay"), "{d}");
    }

    /// The per-call `ripple` override belongs on the edits that follow the
    /// project's ripple mode and on none that decide their own layout — an
    /// override on `move_clip` or `ripple_delete` would promise something the
    /// core never does. Optional everywhere: omitted means "follow the mode".
    #[test]
    fn ripple_is_an_optional_argument_on_exactly_the_edits_that_follow_the_mode() {
        let tools = router().list_all();
        let schema = |name: &str| {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is registered"));
            let properties = tool
                .input_schema
                .get("properties")
                .and_then(|p| p.as_object())
                .cloned()
                .unwrap_or_default();
            let required: Vec<String> = tool
                .input_schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            (properties, required)
        };

        for name in [
            "trim",
            "set_speed",
            "remove",
            "remove_clips",
            "add_clip_to_timeline",
            "split_at",
            "split_remove",
            "split_remove_clips",
            "generate_voiceover",
            "compose_music",
        ] {
            let (properties, required) = schema(name);
            assert!(properties.contains_key("ripple"), "`{name}` should take `ripple`");
            assert!(!required.iter().any(|r| r == "ripple"), "`{name}`: ripple must stay optional");
        }
        for name in [
            "ripple_delete",
            "cut_clip_range",
            "snap_to_beats",
            "move_clip",
            "move_clips",
            "roll_edit",
            "slip_clip",
            "slide_clip",
            "reorder",
            "duplicate_clips",
        ] {
            let (properties, _) = schema(name);
            assert!(
                !properties.contains_key("ripple"),
                "`{name}` never ripples, so takes no `ripple`"
            );
        }

        // The mode itself, and the group edits, are on the surface.
        assert!(schema("get_ripple_mode").0.is_empty());
        assert_eq!(schema("set_ripple_mode").1, ["on"]);
        assert_eq!(schema("move_clips").1, ["moves"]);
        assert_eq!(schema("remove_clips").1, ["clip_ids"]);
    }

    /// The per-call `link` override belongs on the edits that carry linked clips
    /// along and on none that do not — an override on `add_clip_to_timeline` or
    /// `set_volume` would promise a propagation the core never does. Optional
    /// everywhere: omitted means "edit the group".
    #[test]
    fn link_is_an_optional_argument_on_exactly_the_edits_that_carry_linked_clips() {
        let tools = router().list_all();
        let schema = |name: &str| {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is registered"));
            let properties = tool
                .input_schema
                .get("properties")
                .and_then(|p| p.as_object())
                .cloned()
                .unwrap_or_default();
            let required: Vec<String> = tool
                .input_schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            (properties, required)
        };
        for name in [
            "move_clip",
            "move_clips",
            "trim",
            "split_at",
            "remove",
            "remove_clips",
            "ripple_delete",
            "cut_clip_range",
            "set_speed",
            "roll_edit",
            "slip_clip",
            "slide_clip",
            "split_remove",
            "split_remove_clips",
        ] {
            let (properties, required) = schema(name);
            assert!(properties.contains_key("link"), "`{name}` should take `link`");
            assert!(!required.iter().any(|r| r == "link"), "`{name}`: link must stay optional");
        }
        for name in [
            "add_clip_to_timeline",
            "cut_clip",
            "reorder",
            "set_volume",
            "set_fade",
            "snap_to_beats",
            "duplicate_clips",
            "extract_audio",
            "add_asset_audio",
            "detach_audio",
            "detach_audio_clips",
            "reattach_audio",
            "reattach_audio_clips",
            "link_clips",
            "unlink_clips",
        ] {
            let (properties, _) = schema(name);
            assert!(
                !properties.contains_key("link"),
                "`{name}` does not follow links, so takes no `link`"
            );
        }
        assert_eq!(schema("detach_audio").1, ["clip_id"]);
        assert_eq!(schema("detach_audio_clips").1, ["clip_ids"]);
        assert_eq!(schema("add_asset_audio").1, ["asset_id"]);
        assert_eq!(schema("reattach_audio").1, ["clip_id"]);
        assert_eq!(schema("reattach_audio_clips").1, ["clip_ids"]);
        assert_eq!(schema("link_clips").1, ["clip_ids"]);
        assert_eq!(schema("unlink_clips").1, ["clip_ids"]);
    }

    /// A group move reaches the core with every id parsed; one mistyped uuid
    /// refuses the lot and is the caller's to fix.
    #[test]
    fn a_group_move_parses_its_ids_and_names_the_bad_one() {
        use rmcp::model::ErrorCode;
        let (clip, track) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let item = |clip_id: String, track_id: Option<String>| ClipMoveParams {
            clip_id,
            timeline_start: 2.5,
            track_id,
        };

        let moves = clip_moves(&[item(clip.to_string(), None), item(clip.to_string(), Some(track.to_string()))]).unwrap();
        assert_eq!(moves.len(), 2);
        assert_eq!(
            (moves[0].clip_id, moves[0].timeline_start, moves[0].track_id),
            (clip, 2.5, None)
        );
        assert_eq!(moves[1].track_id, Some(track));

        let e = clip_moves(&[item(clip.to_string(), None), item("not-a-uuid".to_string(), None)]).unwrap_err();
        assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
        assert!(e.message.contains("not-a-uuid"), "{}", e.message);
        let e = clip_moves(&[item(clip.to_string(), Some("nope".to_string()))]).unwrap_err();
        assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
    }

    /// The group cut reaches the core with every id parsed; one mistyped uuid refuses
    /// the lot and is the caller's to fix.
    #[test]
    fn a_group_cut_parses_its_ids_and_names_the_bad_one() {
        use super::{clip_cuts, ClipCutParams};
        use rmcp::model::ErrorCode;
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let item = |clip_id: String, at: f64| ClipCutParams { clip_id, at };
        let cuts = clip_cuts(&[item(a.to_string(), 2.5), item(b.to_string(), 3.0)]).unwrap();
        assert_eq!(
            cuts.iter().map(|c| (c.clip_id, c.at)).collect::<Vec<_>>(),
            vec![(a, 2.5), (b, 3.0)]
        );
        let e = clip_cuts(&[item(a.to_string(), 1.0), item("not-a-uuid".to_string(), 1.0)]).unwrap_err();
        assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
        assert!(e.message.contains("not-a-uuid"), "{}", e.message);
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
            kerf_core::Error::SvgOverlayNotFound(id),
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
        assert!(properties.contains_key("overwrite"), "{properties:?}");
        assert!(properties.contains_key("background"), "{properties:?}");
        assert_eq!(properties.len(), 4, "only the declared params belong here: {properties:?}");

        let required: Vec<&str> = export
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(required, ["output_path"], "options and overwrite are optional");
    }

    /// The mixer's three tools: the master fader and limiter are staged edits like
    /// any other, and `get_levels` is a read that takes a `RequestContext` (so a
    /// long measurement can be cancelled) which must stay out of its schema.
    #[test]
    fn the_master_and_levels_tools_are_on_the_surface() {
        let tools = router().list_all();
        let schema = |name: &str| {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is registered"));
            let mut properties: Vec<String> = tool
                .input_schema
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|p| p.keys().cloned().collect())
                .unwrap_or_default();
            properties.sort();
            let required: Vec<String> = tool
                .input_schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            (properties, required)
        };
        assert_eq!(
            schema("set_master_volume"),
            (vec!["volume".to_string()], vec!["volume".to_string()])
        );
        assert_eq!(
            schema("set_master_limiter"),
            (
                vec!["ceiling_db".to_string(), "enabled".to_string()],
                vec!["enabled".to_string()]
            )
        );
        assert_eq!(
            schema("set_master_duck"),
            (vec!["depth_db".to_string()], Vec::<String>::new())
        );
        assert_eq!(
            schema("fit_music"),
            (
                vec!["clip_id".to_string(), "fade_out".to_string(), "target".to_string()],
                vec!["clip_id".to_string()]
            )
        );
        assert_eq!(
            schema("plan_music_fit"),
            (vec!["clip_id".to_string(), "target".to_string()], vec!["clip_id".to_string()])
        );
        assert_eq!(
            schema("get_music_structure"),
            (vec!["asset_id".to_string()], vec!["asset_id".to_string()])
        );
        // Both optional, and no request context leaking into the schema.
        assert_eq!(
            schema("get_levels"),
            (vec!["loudnorm".to_string(), "range".to_string()], Vec::<String>::new())
        );
        // The check takes an optional range and frame and nothing else: a bare call still judges the whole cut.
        assert_eq!(
            schema("platform_check"),
            (
                vec!["height".to_string(), "range".to_string(), "width".to_string()],
                Vec::<String>::new()
            )
        );
    }

    /// The words an agent reads have to name the ceiling the engine actually defaults to
    /// (it moved from -1 to -1.5 dBFS once the limiter was measured to overshoot).
    #[test]
    fn the_limiter_tool_names_the_default_ceiling_it_really_has() {
        let default = format!("{}", kerf_core::MASTER_DEFAULT_CEILING_DB);
        let tools = router().list_all();
        let limiter = tools.iter().find(|t| t.name == "set_master_limiter").expect("registered");
        let text = limiter.description.as_deref().unwrap_or_default();
        assert!(text.contains(&format!("default {default}")), "{text}");
        let param = limiter.input_schema["properties"]["ceiling_db"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(param.contains(&format!("({default} until")), "{param}");
    }

    /// A Reel written from the middle of a long timeline is judged at the Reel's length: the per-file
    /// verdict `export_variants` reports reads the range the export was given, not the whole cut.
    #[test]
    fn an_exported_variant_is_judged_over_the_span_it_wrote() {
        let project = kerf_core::Project::sample().unwrap();
        let interview = project
            .list_assets()
            .unwrap()
            .into_iter()
            .find(|a| a.name == "interview.mp4")
            .unwrap();
        // 20.5 s of sample cut, then two more full interviews: 260.5 s, past the 3 min Shorts cap.
        project
            .add_clip_to_timeline(interview.id, None, 0.0, 120.0, Some(20.5))
            .unwrap();
        project
            .add_clip_to_timeline(interview.id, None, 0.0, 120.0, Some(140.5))
            .unwrap();
        let reel = kerf_core::ExportVariant::beside(
            std::path::Path::new("/renders/cut.mp4"),
            kerf_core::Delivery::parse("9:16").unwrap(),
        );

        let whole = variant_output(&project, &reel, None).unwrap();
        let issues = whole["issues"].as_array().unwrap();
        assert!(
            issues
                .iter()
                .any(|i| i["target"] == "YouTube Shorts" && i["severity"] == "error"),
            "{whole}"
        );

        // Over the 20 s span the three vertical feeds are ready; what remains is the other platforms'
        // shape, never a length.
        let span = variant_output(&project, &reel, Some((10.0, 30.0))).unwrap();
        let ready: Vec<&str> = span["ready_for"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(ready, ["Instagram Reels", "YouTube Shorts", "TikTok"]);
        assert!(
            span["issues"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| !ready.contains(&i["target"].as_str().unwrap())),
            "{span}"
        );
        assert_eq!(span["format"], "9:16");
    }

    #[test]
    fn the_server_instructions_point_at_the_editor_context() {
        let text = super::INSTRUCTIONS;
        assert!(text.contains("get_editor_context"), "{text}");
        assert!(text.contains("\"this clip\""), "{text}");
    }

    fn call(name: &'static str, args: serde_json::Value) -> rmcp::model::CallToolRequestParams {
        rmcp::model::CallToolRequestParams::new(name).with_arguments(args.as_object().unwrap().clone())
    }

    /// serde drops a key a parameter struct does not name, so a misspelt override
    /// would run the edit without it and report success.
    #[test]
    fn an_unknown_argument_is_refused_naming_the_accepted_ones() {
        use rmcp::model::ErrorCode;
        let e = reject_unknown_arguments(&call("trim", serde_json::json!({"clip_id": "x", "rippel": false})))
            .expect_err("a misspelt ripple must not be dropped silently");
        assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(e.code.0, -32602);
        for name in ["\"rippel\"", "trim", "ripple", "clip_id", "source_in", "link"] {
            assert!(e.message.contains(name), "{name}: {}", e.message);
        }

        let e = reject_unknown_arguments(&call("get_editor_context", serde_json::json!({"verbose": true}))).unwrap_err();
        assert!(e.message.contains("takes no arguments"), "{}", e.message);

        assert!(reject_unknown_arguments(&call("trim", serde_json::json!({"clip_id": "x", "ripple": false}))).is_ok());
        assert!(reject_unknown_arguments(&call("get_editor_context", serde_json::json!({}))).is_ok());
        assert!(reject_unknown_arguments(&rmcp::model::CallToolRequestParams::new("get_editor_context")).is_ok());
        assert!(
            reject_unknown_arguments(&call("no_such_tool", serde_json::json!({"x": 1}))).is_ok(),
            "an unknown tool is the router's to report"
        );
    }

    /// The strict-argument check accepts what the schema lists, and schemars does not
    /// list a `#[serde(alias)]` name, so an alias on a parameter would be refused.
    #[test]
    fn mcp_parameters_take_no_serde_alias() {
        // The tools' parameter structs live in this file and in the adapter's modules.
        for (file, source) in [
            ("mcp.rs", include_str!("mcp.rs")),
            ("mcp/jobs.rs", include_str!("mcp/jobs.rs")),
            ("mcp/project_tools.rs", include_str!("mcp/project_tools.rs")),
            ("mcp/reads.rs", include_str!("mcp/reads.rs")),
            ("mcp/server.rs", include_str!("mcp/server.rs")),
            ("mcp/templates.rs", include_str!("mcp/templates.rs")),
        ] {
            assert!(
                !source.replace(' ', "").contains(concat!("alias", "=\"")),
                "an MCP parameter in {file} uses #[serde(alias)]"
            );
        }
    }

    /// Every argument a tool's schema lists is accepted — the check reads the same
    /// schema the client was given, so it can only refuse what the client was not offered —
    /// and no tool admits a key it does not list.
    #[test]
    fn every_listed_argument_is_accepted_and_nothing_else() {
        for tool in router().list_all() {
            let props = tool
                .input_schema
                .get("properties")
                .and_then(|p| p.as_object())
                .cloned()
                .unwrap_or_default();
            let args: serde_json::Map<String, serde_json::Value> =
                props.keys().map(|k| (k.clone(), serde_json::Value::Null)).collect();
            let mut request = rmcp::model::CallToolRequestParams::new(tool.name.clone()).with_arguments(args);
            assert!(reject_unknown_arguments(&request).is_ok(), "{}", tool.name);
            request
                .arguments
                .as_mut()
                .unwrap()
                .insert("not_an_argument".into(), serde_json::Value::Null);
            assert!(reject_unknown_arguments(&request).is_err(), "{} admits any key", tool.name);
        }
    }

    /// A client decides from these hints what to run unasked and what to confirm,
    /// so a tool without them is a tool it has to guess about.
    #[test]
    fn every_tool_is_titled_and_annotated() {
        let tools = router().list_all();
        for tool in &tools {
            assert!(
                tool.title.as_deref().is_some_and(|t| !t.is_empty()),
                "{} has no title",
                tool.name
            );
            let a = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{} has no annotations", tool.name));
            for (hint, value) in [
                ("readOnlyHint", a.read_only_hint),
                ("destructiveHint", a.destructive_hint),
                ("idempotentHint", a.idempotent_hint),
                ("openWorldHint", a.open_world_hint),
            ] {
                assert!(value.is_some(), "{} lacks {hint}", tool.name);
            }
        }
        for (name, ..) in super::hints::TOOLS {
            assert!(tools.iter().any(|t| t.name == *name), "hints name {name}, which is no tool");
        }
        let hint = |name: &str| tools.iter().find(|t| t.name == name).unwrap().annotations.clone().unwrap();
        for read in [
            "get_timeline_state",
            "preview_timeline",
            "staged_diff",
            "get_editor_context",
            "get_levels",
            "list_templates",
        ] {
            assert_eq!(hint(read).read_only_hint, Some(true), "{read}");
        }
        for staged in [
            "trim",
            "remove_clips",
            "remove_track",
            "set_volume",
            "stage_edits",
            "apply_template",
            "set_template_values",
            "save_template",
        ] {
            let a = hint(staged);
            assert_eq!((a.read_only_hint, a.destructive_hint), (Some(false), Some(false)), "{staged}");
        }
        for destructive in [
            "apply_staged_edits",
            "discard_staged_edits",
            "revert_to",
            "export",
            "export_variants",
            "export_cover",
            "export_srt",
            "export_template",
            "delete_template",
            "delete_proxy",
            "remove_task",
        ] {
            assert_eq!(hint(destructive).destructive_hint, Some(true), "{destructive}");
        }
        for downloads in [
            "download_speech_model",
            "generate_voiceover",
            "compose_music",
            "separate_stems",
        ] {
            assert_eq!(hint(downloads).open_world_hint, Some(true), "{downloads}");
        }
    }

    #[test]
    fn the_server_instructions_send_a_social_cut_through_get_levels() {
        let text = super::INSTRUCTIONS;
        assert!(text.contains("get_levels"), "{text}");
        assert!(text.contains("-14 LUFS") && text.contains("loudnorm"), "{text}");
        assert!(text.contains("set_master_limiter"), "{text}");
    }

    /// An agent that has never seen the GUI writes the score from the tool's description alone, so
    /// the description has to carry every style, every section kind, the chord syntax (names and
    /// roman numerals), the hits and how to align or recompose.
    #[test]
    fn compose_music_describes_the_score_and_the_music_tools_are_on_the_surface() {
        let tools = router().list_all();
        let tool = |name: &str| {
            tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is registered"))
        };
        let text = |name: &str| tool(name).description.as_deref().unwrap_or_default().to_string();
        let compose = text("compose_music");
        for style in kerf_core::MusicStyle::ALL {
            assert!(
                compose.contains(style.name()),
                "the description names the style {}",
                style.name()
            );
        }
        for needle in [
            "intro",
            "verse",
            "build",
            "drop",
            "breakdown",
            "outro",
            "roman numerals",
            "F#m7",
            "D/F#",
            "bVII",
            "at_s",
            "impact",
            "riser",
            "dropout",
            "whoosh",
            "align",
            "replace_clip_id",
            "get_band_balance",
            "set_master_duck",
            "fit_music",
            "ONE per bar",
            "progressToken",
            "9007199254740991",
            "WHOLE bed",
            "unlink_clips",
            "replaced",
            "half a bar",
            "build",
            "256 hits",
            "misspelled",
        ] {
            assert!(compose.contains(needle), "the description should mention {needle}");
        }

        let schema = &tool("compose_music").input_schema;
        assert_eq!(schema["required"], serde_json::json!(["score"]));
        for field in ["score", "track_id", "at", "align", "replace_clip_id", "ripple", "background"] {
            assert!(schema["properties"].get(field).is_some(), "compose_music takes {field}");
        }
        let shown = serde_json::to_string(schema).unwrap();
        for needle in [
            "tropical_house",
            "breakdown",
            "chords",
            "energy",
            "max_tempo_change",
            "markers",
        ] {
            assert!(shown.contains(needle), "the schema should carry {needle}");
        }

        let balance = tool("get_band_balance");
        for field in ["asset_id", "clip_id", "range", "sections"] {
            assert!(
                balance.input_schema["properties"].get(field).is_some(),
                "get_band_balance takes {field}"
            );
        }
        assert!(
            balance
                .input_schema
                .get("required")
                .is_none_or(|r| r.as_array().is_some_and(Vec::is_empty)),
            "the target is one of three, so none is required by the schema"
        );
        let balance = text("get_band_balance");
        for needle in ["sub", "high_mid", "share_pct", "LUFS", "windows_from", "notes"] {
            assert!(balance.contains(needle), "get_band_balance should mention {needle}");
        }
        assert!(tool("music_status").input_schema["properties"]
            .as_object()
            .is_none_or(|p| p.is_empty()));
    }

    /// What a model sends for a score parses, and a bad one is the caller's mistake.
    #[test]
    fn a_compose_music_call_parses_and_a_bad_score_is_the_callers() {
        let p: super::ComposeMusicParams = serde_json::from_value(serde_json::json!({
            "score": {
                "bpm": 110, "key": "Am", "style": "tropical_house",
                "sections": [
                    {"kind": "intro", "bars": 4, "chords": ["i", "VI", "III", "VII"]},
                    {"kind": "drop", "bars": 8, "chords": ["Am", "F", "C", "G"], "energy": 0.9}
                ],
                "hits": [{"at_s": 8.7, "kind": "impact"}]
            },
            "align": {"to": "cuts", "max_tempo_change": 0.05},
            "at": 2.0
        }))
        .unwrap();
        p.score.validate().unwrap();
        assert_eq!(p.align.unwrap().to, kerf_core::AlignTo::Cuts);

        let mut bad = p.score;
        bad.seed = kerf_core::MAX_SEED + 1;
        let e = super::core_err(bad.validate().unwrap_err());
        assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(e.message.contains("seed must be 0..=9007199254740991"), "{}", e.message);
        bad.seed = 0;
        bad.sections[1].chords = vec!["Hm".to_string()];
        let e = super::core_err(bad.validate().unwrap_err());
        assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(e.message.contains("sections[1]") && e.message.contains("Hm"), "{}", e.message);
    }

    /// A misspelled field is an error, not a default: in the score, in its sections and hits, and in
    /// the align block.
    #[test]
    fn a_misspelled_compose_music_field_is_refused() {
        let call = |score: serde_json::Value, align: serde_json::Value| {
            serde_json::from_value::<super::ComposeMusicParams>(serde_json::json!({ "score": score, "align": align }))
                .map(|_| ())
                .map_err(|e| e.to_string())
        };
        let score = |section: serde_json::Value, extra: serde_json::Value| {
            let mut s = serde_json::json!({"bpm": 100, "key": "C", "style": "pop", "sections": [section]});
            if let (Some(s), Some(extra)) = (s.as_object_mut(), extra.as_object()) {
                s.extend(extra.clone());
            }
            s
        };
        let section = serde_json::json!({"kind": "drop", "bars": 4, "chords": ["C"]});
        let ok_align = serde_json::json!({"to": "cuts"});
        call(score(section.clone(), serde_json::json!({})), ok_align.clone()).unwrap();
        let typo = serde_json::json!({"kind": "drop", "bars": 4, "chords": ["C"], "enrgy": 0.9});
        assert!(call(score(typo, serde_json::json!({})), ok_align.clone())
            .unwrap_err()
            .contains("unknown field `enrgy`"));
        assert!(call(score(section.clone(), serde_json::json!({"tempo": 90})), ok_align)
            .unwrap_err()
            .contains("unknown field `tempo`"));
        let e = call(
            score(section, serde_json::json!({})),
            serde_json::json!({"to": "cuts", "max_tempo": 0.1}),
        )
        .unwrap_err();
        assert!(e.contains("unknown field `max_tempo`"), "{e}");
    }

    /// A music request routes compose (aligned to the cuts) → fit → duck → measure.
    #[test]
    fn the_server_instructions_route_a_music_request_through_compose_music() {
        let text = super::INSTRUCTIONS;
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("the instructions should mention {needle}"))
        };
        let compose = at("compose_music takes a score");
        let fit = text[compose..]
            .find("fit_music")
            .map(|i| i + compose)
            .expect("fit_music after compose_music");
        let duck = text[compose..]
            .find("set_master_duck")
            .map(|i| i + compose)
            .expect("set_master_duck after compose_music");
        let levels = text[compose..]
            .find("get_levels")
            .map(|i| i + compose)
            .expect("get_levels after compose_music");
        let bands = text[compose..]
            .find("get_band_balance")
            .map(|i| i + compose)
            .expect("get_band_balance after compose_music");
        assert!(
            compose < fit && fit < duck && duck < levels && levels < bands,
            "the order of the music flow"
        );
        assert!(
            text.contains("align {to: \"cuts\"}") && text.contains("replace_clip_id"),
            "{text}"
        );
        let long = text
            .split("Long tools (")
            .nth(1)
            .and_then(|t| t.split(')').next())
            .unwrap_or_default();
        assert!(long.contains("compose_music"), "it is a long tool: {long}");
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

    /// The window is the whole point of `get_waveform_range`, so all four
    /// arguments are required and the schema has to say what they are in terms a
    /// model that has never seen the timeline can act on (source seconds, not
    /// timeline time).
    #[test]
    fn the_waveform_range_tool_takes_a_source_window() {
        let tools = router().list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == "get_waveform_range")
            .expect("get_waveform_range is registered");
        let mut required: Vec<&str> = tool
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        required.sort_unstable();
        assert_eq!(required, ["asset_id", "buckets", "end", "start"]);

        let properties = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("input schema has properties");
        for name in ["start", "end"] {
            let described = properties[name]
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or_default();
            assert!(described.contains("SOURCE seconds"), "{name}: {described}");
        }
    }

    /// A swapped or empty window is a mistake the model can fix, not a silent
    /// stretch of audio — the engine's answer to one is a row of zeros.
    #[test]
    fn a_waveform_window_must_run_forwards() {
        use rmcp::model::ErrorCode;
        assert!(require_window(0.0, 1.0).is_ok());
        assert!(require_window(-2.0, 0.5).is_ok(), "a window may begin before the media");
        for (start, end) in [(5.0, 1.0), (3.0, 3.0), (0.0, f64::NAN), (f64::INFINITY, 1.0)] {
            let e = require_window(start, end).expect_err("not a forward window");
            assert_eq!(e.code, ErrorCode::INVALID_PARAMS, "{start}..{end}");
        }
    }

    #[test]
    fn the_server_instructions_describe_the_svg_subset() {
        let text = super::INSTRUCTIONS;
        for needle in [
            "add_svg_overlay",
            "list_svg_presets",
            "SMIL",
            "animateTransform",
            "viewBox",
            "relative to the overlay's start",
            "animateMotion",
            "update_svg_overlay",
            "remove_svg_overlay",
            "validate_svg",
            "preview_svg_overlay",
        ] {
            assert!(text.contains(needle), "the instructions should mention `{needle}`: {text}");
        }
        let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("`{needle}` in the instructions"));
        assert!(
            at("validate_svg") < at("preview_svg_overlay")
                && at("preview_svg_overlay") < at("then add_svg_overlay, then")
                && at("then add_svg_overlay, then") < at("preview_timeline at a few moments between its start"),
            "the loop runs validate, preview, add, then look at the cut: {text}"
        );
        let tools = router().list_all();
        for name in [
            "add_svg_overlay",
            "update_svg_overlay",
            "remove_svg_overlay",
            "list_svg_presets",
            "validate_svg",
            "preview_svg_overlay",
        ] {
            assert!(tools.iter().any(|t| t.name == name), "`{name}` is registered");
        }
    }

    #[test]
    fn the_server_instructions_send_a_reusable_title_through_a_template() {
        let text = super::INSTRUCTIONS;
        let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("`{needle}` in the instructions"));
        assert!(
            at("preview_timeline at a few moments between its start") < at("To reuse a title")
                && at("To reuse a title") < at("save_template with `controls`")
                && at("save_template with `controls`") < at("apply_template puts one on the cut")
                && at("apply_template puts one on the cut") < at("set_template_values changes them later")
                && at("set_template_values changes them later") < at("To narrate a cut"),
            "the template paragraph follows the SVG one: author with ids, validate, add, save, apply, set: {text}"
        );
        for needle in [
            "`id` attributes",
            "list_templates",
            "ends the template",
            "delete_template is refused while the user reviews",
        ] {
            assert!(text.contains(needle), "the instructions should mention `{needle}`: {text}");
        }
    }

    #[test]
    fn the_template_tools_ask_for_little_and_say_what_they_take() {
        let tools = router().list_all();
        let tool = |name: &str| {
            tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is registered"))
        };
        let required = |name: &str| -> Vec<String> {
            let mut r: Vec<String> = tool(name)
                .input_schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            r.sort();
            r
        };
        assert!(required("list_templates").is_empty());
        assert_eq!(required("save_template"), ["name", "overlay_id"]);
        assert_eq!(required("apply_template"), ["start", "template_id"]);
        assert_eq!(required("set_template_values"), ["overlay_id"]);
        assert_eq!(required("import_template"), ["path"]);
        assert_eq!(required("export_template"), ["output_path", "template_id"]);
        assert_eq!(required("delete_template"), ["template_id"]);
        let description = |name: &str| tool(name).description.as_deref().unwrap_or_default().to_string();
        for (name, needles) in [
            (
                "save_template",
                &[
                    "`id` attributes",
                    "controls",
                    "validate_svg",
                    "not part of your staged proposal",
                ][..],
            ),
            (
                "apply_template",
                &["list_templates", "image_files", "staged edit", "set_template_values"][..],
            ),
            (
                "set_template_values",
                &["control", "timeline_summary", "update_svg_overlay"][..],
            ),
            ("import_template", &["already_present", "added_as_copy", "network share"][..]),
            ("export_template", &[".kerftemplate", "overwrite"][..]),
            ("delete_template", &["cannot be undone", "review"][..]),
        ] {
            for needle in needles {
                assert!(
                    description(name).contains(needle),
                    "{name} should say `{needle}`: {}",
                    description(name)
                );
            }
        }
    }

    #[test]
    fn a_control_in_save_template_refuses_a_key_it_does_not_name() {
        let parse = |controls: serde_json::Value| {
            serde_json::from_value::<super::templates::SaveTemplateParams>(serde_json::json!({
                "overlay_id": "x", "name": "n", "controls": controls
            }))
        };
        let ok = parse(serde_json::json!([
            {"id": "tagline", "label": "Tagline", "kind": "text", "targets": [{"element": "tagline"}]},
            {"id": "size", "label": "Size", "kind": "number", "min": 1, "max": 9,
             "targets": [{"element": "t", "attribute": "font-size"}]}
        ]));
        assert!(ok.is_ok(), "{ok:?}");
        for bad in [
            serde_json::json!([{"id": "a", "label": "A", "kind": "text", "target": [{"element": "t"}], "targets": []}]),
            serde_json::json!([{"id": "a", "label": "A", "kind": "text", "targets": [{"element": "t", "attr": "fill"}]}]),
            serde_json::json!([{"id": "a", "label": "A", "kind": "paragraph", "targets": [{"element": "t"}]}]),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_agent_reads_a_template_title_with_its_controls_and_values_but_not_its_source() {
        use kerf_core::{SvgOverlay, TemplateControl, TemplateInstance, TemplateKind, TemplateTarget};
        let o = SvgOverlay {
            id: uuid::Uuid::new_v4(),
            name: "End card".to_string(),
            svg: "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"/>".to_string(),
            start: 0.0,
            end: 4.0,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            opacity: 1.0,
            offset: 0.0,
            preset: None,
            template: Some(TemplateInstance {
                template_id: "0b8c2a52-6d7c-4a52-9d3c-1d3a6f3b6d11".to_string(),
                name: "Kitefun end card".to_string(),
                controls: vec![TemplateControl {
                    id: "tagline".to_string(),
                    label: "Tagline".to_string(),
                    kind: TemplateKind::Text,
                    targets: vec![TemplateTarget {
                        element: "tagline".to_string(),
                        attribute: None,
                    }],
                    min: None,
                    max: None,
                }],
                values: [("tagline".to_string(), "See you".to_string())].into(),
            }),
        };
        let v = serde_json::to_value(SvgOverlaySummary::from(&o)).unwrap();
        assert_eq!(v["kind"], "custom");
        assert_eq!(v["template"]["template_id"], "0b8c2a52-6d7c-4a52-9d3c-1d3a6f3b6d11");
        assert_eq!(v["template"]["controls"][0]["id"], "tagline");
        assert_eq!(v["template"]["controls"][0]["kind"], "text");
        assert_eq!(v["template"]["values"]["tagline"], "See you");
        assert!(v["template"]["controls"][0].get("targets").is_none(), "{v}");
        assert!(v.get("svg").is_none(), "{v}");
    }

    #[test]
    fn get_scopes_asks_only_for_a_time() {
        let tools = router().list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == "get_scopes")
            .expect("`get_scopes` is registered");
        let required = tool
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(required, [serde_json::json!("time_secs")]);
        let description = tool.description.as_deref().unwrap_or_default();
        assert!(
            description.contains("clipped_high") && description.contains("hue_deg"),
            "{description}"
        );
        let p: crate::mcp::ScopesParams =
            serde_json::from_value(serde_json::json!({ "time_secs": 1.0, "grid_size": { "vector": 4096 } })).unwrap();
        let size = p.grid_size.unwrap().resolve();
        assert_eq!((size.columns, size.rows), (64, 64));
        assert_eq!(
            size.vector,
            kerf_core::scopes::GridSize { vector: 4096, ..size }.clamped().vector
        );
    }

    #[test]
    fn the_svg_tools_ask_for_little_and_say_what_the_svg_may_do() {
        let tools = router().list_all();
        let tool = |name: &str| {
            tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is registered"))
        };
        let required = |name: &str| -> Vec<String> {
            let mut r: Vec<String> = tool(name)
                .input_schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            r.sort();
            r
        };
        assert_eq!(required("add_svg_overlay"), ["start"]);
        assert_eq!(required("update_svg_overlay"), ["overlay_id"]);
        assert_eq!(required("remove_svg_overlay"), ["overlay_id"]);
        let description = tool("add_svg_overlay").description.as_deref().unwrap_or_default().to_string();
        for needle in [
            "SMIL",
            "viewBox",
            "RELATIVE TO THE OVERLAY'S START",
            "animateMotion",
            "preview_timeline",
        ] {
            assert!(
                description.contains(needle),
                "add_svg_overlay should say `{needle}`: {description}"
            );
        }
    }

    #[test]
    fn a_title_comes_from_svg_or_from_a_preset_never_both_or_neither() {
        use kerf_core::{SvgPresetKind, SvgPresetParams, SvgSource};
        use rmcp::model::ErrorCode;
        let params = || Some(SvgPresetParams::new("Hello"));
        let kind = Some(SvgPresetKind::FadeUp);
        assert_eq!(
            svg_source(Some("<svg/>".to_string()), None, None).unwrap(),
            SvgSource::Custom("<svg/>".to_string())
        );
        assert!(matches!(
            svg_source(None, kind, params()).unwrap(),
            SvgSource::Preset(p) if p.kind == SvgPresetKind::FadeUp && p.params.text == "Hello"
        ));
        for (svg, kind, params) in [
            (Some("<svg/>".to_string()), kind, params()),
            (Some("<svg/>".to_string()), kind, None),
            (Some("<svg/>".to_string()), None, params()),
            (None, kind, None),
            (None, None, params()),
            (None, None, None),
        ] {
            let e = svg_source(svg, kind, params).expect_err("not one source");
            assert_eq!(e.code, ErrorCode::INVALID_PARAMS);
        }
    }

    /// The source of a title can be 128 KiB: the agent reads timing and placement, not markup.
    #[test]
    fn an_agent_reads_a_title_without_its_source() {
        use kerf_core::{SvgOverlay, SvgPreset, SvgPresetKind, SvgPresetParams};
        let mut o = SvgOverlay {
            id: uuid::Uuid::new_v4(),
            name: String::new(),
            svg: "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"/>".to_string(),
            start: 1.0,
            end: 4.0,
            x: 0.1,
            y: 0.2,
            width: 0.5,
            height: 0.25,
            opacity: 0.8,
            offset: 0.0,
            preset: None,
            template: None,
        };
        let custom = serde_json::to_value(SvgOverlaySummary::from(&o)).unwrap();
        assert_eq!(custom["kind"], "custom");
        assert_eq!(custom["name"], "SVG");
        assert_eq!(custom["svg_bytes"], o.svg.len());
        assert!(custom.get("svg").is_none() && custom.get("params").is_none(), "{custom}");

        o.preset = Some(SvgPreset {
            kind: SvgPresetKind::LowerThird,
            params: SvgPresetParams::new("Ada Lovelace"),
        });
        let preset = serde_json::to_value(SvgOverlaySummary::from(&o)).unwrap();
        assert_eq!(preset["kind"], "lower_third");
        assert_eq!(preset["name"], "Ada Lovelace");
        assert_eq!(preset["params"]["text"], "Ada Lovelace");
        assert!(preset.get("svg").is_none(), "{preset}");
    }

    fn svg_text(extra: &str) -> String {
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 160 90\"><rect width=\"20\" height=\"20\" fill=\"red\"><animate attributeName=\"x\" from=\"0\" to=\"100\" dur=\"3s\" fill=\"freeze\"/></rect>{extra}</svg>")
    }

    #[test]
    fn png_results_serialize_as_caption_plus_a_png_block() {
        let value = serde_json::to_value(png_result("cells".to_string(), vec![0x89, b'P', b'N', b'G'])).unwrap();
        let content = value["content"].as_array().unwrap();
        assert_eq!(content.len(), 2, "{content:?}");
        assert_eq!(
            (content[0]["type"].as_str(), content[0]["text"].as_str()),
            (Some("text"), Some("cells"))
        );
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["mimeType"], "image/png");
        assert_eq!(content[1]["data"], "iVBORw==");
    }

    #[test]
    fn a_preview_asks_for_moments_by_times_or_by_count_never_both() {
        use rmcp::model::ErrorCode;
        assert_eq!(svg_moments(None, None).unwrap(), SvgMoments::Spread(6), "six by default");
        assert_eq!(svg_moments(None, Some(0)).unwrap(), SvgMoments::Spread(1));
        assert_eq!(
            svg_moments(None, Some(500)).unwrap(),
            SvgMoments::Spread(36),
            "a count is clamped"
        );
        assert_eq!(
            svg_moments(Some(vec![0.0, 1.5]), None).unwrap(),
            SvgMoments::At(vec![0.0, 1.5])
        );
        for (times, count) in [
            (Some(vec![1.0]), Some(3)),
            (Some(vec![]), None),
            (Some(vec![0.5; 37]), None),
            (Some(vec![f64::NAN]), None),
            (Some(vec![-1.0]), None),
        ] {
            let e = svg_moments(times.clone(), count).expect_err("a mistake the caller can fix");
            assert_eq!(e.code, ErrorCode::INVALID_PARAMS, "{times:?} {count:?}");
        }
    }

    #[test]
    fn a_spread_runs_from_the_first_frame_to_the_end_of_the_title() {
        assert_eq!(svg_sheet_times(&SvgMoments::Spread(5), 4.0), [0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!(svg_sheet_times(&SvgMoments::Spread(2), 3.0), [0.0, 3.0]);
        assert_eq!(
            svg_sheet_times(&SvgMoments::Spread(1), 3.0),
            [1.5],
            "a single cell is the middle"
        );
        assert_eq!(
            svg_sheet_times(&SvgMoments::At(vec![0.5, 9.0]), 3.0),
            [0.5, 9.0],
            "asked-for times are kept"
        );
    }

    #[test]
    fn a_sheet_caption_indexes_the_cells_and_flags_a_slice_and_warnings() {
        let plain = svg_sheet_caption("draft", &[0.0, 1.5], 0.0, &[]);
        assert_eq!(
            plain,
            "draft: 2 moments (row-major), cell -> seconds after it appears:\n  cell 1: 0.00s\n  cell 2: 1.50s"
        );
        let full = svg_sheet_caption("t", &[2.0], 1.0, &["scripts were removed".to_string()]);
        assert!(full.starts_with("t: 1 moment (row-major)"), "{full}");
        assert!(full.contains("1.00s further on"), "{full}");
        assert!(full.ends_with("warnings: scripts were removed"), "{full}");
    }

    #[test]
    fn a_preview_draws_a_png_sheet_and_offsets_a_sliced_title() {
        let title = SvgPreviewTitle {
            svg: svg_text("<script>1</script>"),
            label: "draft".to_string(),
            span: None,
            offset: 0.0,
            report_warnings: true,
        };
        let (png, caption) = svg_preview(&title, &SvgMoments::Spread(3), 96, (160, 90)).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(
            caption.contains("cell 3: 3.00s"),
            "a draft runs for its animation's length: {caption}"
        );
        assert!(caption.contains("warnings:") && caption.contains("script"), "{caption}");

        let on_timeline = SvgPreviewTitle {
            span: Some(2.0),
            offset: 1.0,
            report_warnings: false,
            label: "SVG title".to_string(),
            ..title
        };
        let (png, caption) = svg_preview(&on_timeline, &SvgMoments::At(vec![0.0, 2.0]), 96, (160, 90)).unwrap();
        assert_eq!(&png[..4], b"\x89PNG");
        assert!(
            caption.contains("cell 2: 2.00s") && caption.contains("1.00s further on"),
            "{caption}"
        );
        assert!(!caption.contains("warnings"), "{caption}");

        let broken = SvgPreviewTitle {
            svg: "<nope/>".to_string(),
            label: "draft".to_string(),
            span: None,
            offset: 0.0,
            report_warnings: true,
        };
        let e = svg_preview(&broken, &SvgMoments::Spread(1), 96, (160, 90)).expect_err("not an svg");
        assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn validate_svg_answers_size_warnings_and_duration_without_the_source() {
        let check = kerf_core::check_svg(&svg_text("<script>1</script>")).unwrap();
        let sanitized = check.svg.len();
        let value = serde_json::to_value(SvgValidation::from(check)).unwrap();
        assert_eq!(value["svg_bytes"], sanitized);
        assert_eq!(value["natural_duration"], 3.0);
        assert!(value["warnings"][0].as_str().unwrap().contains("script"), "{value}");
        assert!(value.get("svg").is_none(), "{value}");
        let still = kerf_core::check_svg("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1 1\"/>").unwrap();
        assert!(serde_json::to_value(SvgValidation::from(still)).unwrap()["natural_duration"].is_null());
    }

    #[test]
    fn a_timeline_read_leaves_out_large_svg_sources_and_says_how_to_get_them() {
        use kerf_core::{SvgOverlay, Timeline};
        let title = |bytes: usize| SvgOverlay {
            id: uuid::Uuid::new_v4(),
            name: String::new(),
            svg: "x".repeat(bytes),
            start: 0.0,
            end: 1.0,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            opacity: 1.0,
            offset: 0.0,
            preset: None,
            template: None,
        };
        let mut timeline = Timeline::new();
        timeline.svg_overlays = vec![title(4096), title(4097), title(120_000)];
        elide_large_svg(&mut timeline);
        assert_eq!(timeline.svg_overlays[0].svg.len(), 4096, "up to 4 KiB is kept");
        for (o, bytes) in timeline.svg_overlays[1..].iter().zip([4097, 120_000]) {
            assert!(
                o.svg.contains(&format!("{bytes} bytes")) && o.svg.contains("full_svg=true"),
                "{}",
                o.svg
            );
            assert!(o.svg.len() < 200);
        }
    }

    #[test]
    fn the_svg_authoring_tools_are_described_for_a_model_that_has_not_seen_the_gui() {
        let tools = router().list_all();
        let tool = |name: &str| {
            tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("`{name}` is registered"))
        };
        let description = |name: &str| tool(name).description.as_deref().unwrap_or_default().to_string();
        let required = |name: &str| -> Vec<String> {
            tool(name)
                .input_schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let property = |name: &str, field: &str| tool(name).input_schema["properties"].get(field).cloned();
        assert_eq!(required("validate_svg"), ["svg"]);
        assert!(
            required("preview_svg_overlay").is_empty(),
            "everything is optional but one source"
        );
        assert!(required("get_timeline_state").is_empty());
        for field in ["overlay_id", "svg", "times", "count", "max_width"] {
            assert!(property("preview_svg_overlay", field).is_some(), "{field}");
        }
        assert!(property("update_svg_overlay", "preset_kind").is_some());
        assert!(property("get_timeline_state", "full_svg").is_some());
        assert!(description("update_svg_overlay").contains("preset_kind"));
        assert!(
            description("preview_svg_overlay").contains("PNG") && description("preview_svg_overlay").contains("preview_timeline")
        );
        assert!(description("validate_svg").contains("natural_duration"));
        assert!(description("get_timeline_state").contains("full_svg"));

        let presets = description("list_svg_presets");
        assert!(
            presets.contains("bar wipes in above a name"),
            "the lower_third bar sits above the name: {presets}"
        );
        let add = description("add_svg_overlay");
        for needle in ["data:image/png", "data:image/jpeg", "data:image/gif", "data:image/webp"] {
            assert!(add.contains(needle), "only these embedded images are kept: {add}");
        }
        assert!(!add.contains("`data:` images are fine"), "{add}");
    }

    // ---- review, ownership, jobs, paging: the surface this adapter adds ----------------------

    fn tool_names() -> Vec<String> {
        router().list_all().iter().map(|t| t.name.to_string()).collect()
    }

    /// The numbers and lists the README and the site print are the surface this build serves: a
    /// tool added without them fails here, not in a reader's head.
    #[test]
    fn the_readme_and_the_site_describe_the_tools_the_server_has() {
        const README: &str = include_str!("../../../README.md");
        const SITE: &str = include_str!("../../../site/hugo.toml");
        let names = tool_names();
        let count = names.len();
        let heading = README
            .lines()
            .find_map(|line| line.strip_prefix("### The tools (")?.strip_suffix(')'))
            .expect("the README has a `### The tools (N)` heading");
        assert_eq!(heading.parse::<usize>().ok(), Some(count), "README heading: {heading}");
        let shown = SITE
            .lines()
            .find_map(|line| line.trim().strip_prefix("toolCount = "))
            .expect("site/hugo.toml has a toolCount");
        assert_eq!(shown.trim().parse::<usize>().ok(), Some(count), "site toolCount: {shown}");
        let section = README
            .split("### The tools")
            .nth(1)
            .and_then(|rest| rest.split("\n## ").next())
            .expect("the tools section");
        let unlisted: Vec<&String> = names.iter().filter(|name| !section.contains(&format!("`{name}`"))).collect();
        assert!(unlisted.is_empty(), "tools the README's lists leave out: {unlisted:?}");
    }

    #[test]
    fn the_new_tools_are_on_the_surface() {
        let names = tool_names();
        for tool in [
            "project_info",
            "save_project",
            "save_project_as",
            "open_project",
            "new_project",
            "get_settings",
            "set_settings",
            "job_status",
            "cancel_job",
            "list_jobs",
            "get_transcript",
            "get_analysis_detail",
            "preview_svg_preset",
            "undo_staged_edit",
            "restore_discarded_edits",
            "apply_staged_subset",
            "list_assets",
            "get_asset_metadata",
            "missing_assets",
        ] {
            assert!(names.iter().any(|n| n == tool), "`{tool}` is not registered");
        }
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "a tool is registered twice");
    }

    #[test]
    fn exactly_the_long_tools_take_background() {
        let tools = router().list_all();
        let mut with: Vec<String> = tools
            .iter()
            .filter(|t| t.input_schema["properties"].get("background").is_some())
            .map(|t| t.name.to_string())
            .collect();
        with.sort();
        let mut long = vec![
            "analyze_asset",
            "compose_music",
            "download_speech_model",
            "export",
            "export_variants",
            "generate_voiceover",
            "import_asset",
            "refine_transcript",
            "separate_stems",
            "smart_crop",
            "sync_clips_by_audio",
        ];
        long.sort_unstable();
        assert_eq!(with, long);
        for name in &with {
            let tool = tools.iter().find(|t| t.name == name.as_str()).unwrap();
            let required: Vec<&str> = tool
                .input_schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            assert!(!required.contains(&"background"), "`background` is optional on {name}");
            let description = tool.description.as_deref().unwrap_or_default();
            assert!(
                description.contains("background") && description.contains("job"),
                "{name} should say a background job is possible: {description}"
            );
        }
        let job = |name: &str| {
            tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("{name}"))
                .input_schema
                .clone()
        };
        for name in ["job_status", "cancel_job"] {
            assert_eq!(job(name)["required"], serde_json::json!(["job_id"]), "{name}");
        }
        assert!(job("list_jobs")["properties"].as_object().is_none_or(|p| p.is_empty()));
    }

    #[test]
    fn paging_arguments_are_optional_on_history_and_the_task_list() {
        let tools = router().list_all();
        // The listings and diffs that used to answer everything page the same way.
        for name in ["list_assets", "staged_diff"] {
            let tool = tools.iter().find(|t| t.name == name).unwrap();
            for field in ["offset", "limit"] {
                assert!(tool.input_schema["properties"].get(field).is_some(), "{name} takes {field}");
            }
            assert!(tool
                .input_schema
                .get("required")
                .is_none_or(|r| r.as_array().is_some_and(Vec::is_empty)));
        }
        let revision = tools.iter().find(|t| t.name == "revision_diff").unwrap();
        assert_eq!(revision.input_schema["required"], serde_json::json!(["seq"]));
        for field in ["offset", "limit"] {
            assert!(
                revision.input_schema["properties"].get(field).is_some(),
                "revision_diff takes {field}"
            );
        }
        let state = tools.iter().find(|t| t.name == "get_timeline_state").unwrap();
        for field in ["clips_offset", "clips_limit", "captions_offset", "captions_limit"] {
            assert!(
                state.input_schema["properties"].get(field).is_some(),
                "get_timeline_state takes {field}"
            );
        }
        for name in ["history", "list_tasks"] {
            let tool = tools.iter().find(|t| t.name == name).unwrap();
            for field in ["offset", "limit"] {
                assert!(tool.input_schema["properties"].get(field).is_some(), "{name} takes {field}");
            }
            assert!(tool
                .input_schema
                .get("required")
                .is_none_or(|r| r.as_array().is_some_and(Vec::is_empty)));
        }
        let transcript = tools.iter().find(|t| t.name == "get_transcript").unwrap();
        for field in ["asset_id", "from", "to", "offset", "limit"] {
            assert!(
                transcript.input_schema["properties"].get(field).is_some(),
                "get_transcript takes {field}"
            );
        }
        assert_eq!(transcript.input_schema["required"], serde_json::json!(["asset_id"]));
        let detail = tools.iter().find(|t| t.name == "get_analysis_detail").unwrap();
        assert_eq!(detail.input_schema["required"], serde_json::json!(["asset_id", "kind"]));
    }

    #[test]
    fn the_review_tools_say_what_the_user_alone_can_do() {
        let tools = router().list_all();
        let description = |name: &str| -> String {
            tools
                .iter()
                .find(|t| t.name == name)
                .and_then(|t| t.description.as_deref())
                .unwrap_or_default()
                .to_string()
        };
        for name in ["apply_staged_edits", "apply_staged_subset"] {
            let text = description(name);
            assert!(text.contains("review") && text.contains("refused"), "{name}: {text}");
        }
        for name in ["undo", "redo", "revert_to"] {
            let text = description(name);
            assert!(text.contains("LIVE") && text.contains("review"), "{name}: {text}");
        }
        assert!(description("undo").contains("undo_staged_edit"));
        assert!(description("discard_staged_edits").contains("restore_discarded_edits"));
        assert!(description("resolve_task").contains("complete_task"));
        assert!(description("claim_next_task").contains("proposal"));
    }

    #[test]
    fn the_descriptions_the_review_called_thin_now_say_what_they_do() {
        let tools = router().list_all();
        let description = |name: &str| -> String {
            tools
                .iter()
                .find(|t| t.name == name)
                .and_then(|t| t.description.as_deref())
                .unwrap_or_default()
                .to_string()
        };
        let silence = description("remove_silence");
        assert!(
            silence.contains("analyze_asset") && silence.contains("silence") && silence.contains("first video track"),
            "{silence}"
        );
        let cut = description("cut_clip");
        assert!(
            cut.contains("SOURCE") && cut.contains("add_clip_to_timeline") && cut.contains("first"),
            "{cut}"
        );
        let concat = description("concatenate");
        assert!(
            concat.contains("FULL") && concat.contains("in order") && concat.contains("one revision"),
            "{concat}"
        );
        let wave = description("get_waveform");
        assert!(wave.contains("WHOLE") && wave.contains("get_waveform_range"), "{wave}");
        assert!(description("analyze_asset").contains("SUMMARY"));
        assert!(description("list_assets").contains("proxy") && description("list_assets").contains("source_missing"));
        assert!(description("timeline_summary").contains("media_notes"));
        assert!(description("platform_check").contains("width"));
        assert!(description("generate_voiceover").contains("warning"));
    }

    #[test]
    fn the_server_instructions_cover_review_jobs_paging_and_proxies() {
        let text = super::INSTRUCTIONS;
        for needle in [
            "STAGED",
            "agent_review",
            "undo_staged_edit",
            "restore_discarded_edits",
            "apply_staged_subset",
            "background: true",
            "job_status",
            "get_transcript",
            "get_analysis_detail",
            "captions_offset",
            "media_notes",
            "discard_unsaved",
        ] {
            assert!(text.contains(needle), "the instructions should mention {needle}");
        }
        assert!(text.find("complete_task").is_some());
    }

    #[test]
    fn a_mistyped_id_names_the_argument() {
        let e = super::parse_arg("clip_id", "not-a-uuid").unwrap_err();
        assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(
            e.message.contains("`clip_id`") && e.message.contains("not-a-uuid"),
            "{}",
            e.message
        );
        assert!(super::parse_arg("clip_id", &uuid::Uuid::new_v4().to_string()).is_ok());
        // Through the helpers that parse several ids, it is the field of the item that is named.
        let item = |id: &str| ClipMoveParams {
            clip_id: id.to_string(),
            timeline_start: 0.0,
            track_id: Some("nope".to_string()),
        };
        let e = clip_moves(&[item(&uuid::Uuid::new_v4().to_string())]).unwrap_err();
        assert!(e.message.contains("track_id"), "{}", e.message);
    }

    #[test]
    fn the_new_core_refusals_are_the_callers_to_fix() {
        use rmcp::model::ErrorCode;
        for e in [
            kerf_core::Error::StagedEditOwnedByOther,
            kerf_core::Error::AgentReviewRequired,
            kerf_core::Error::NoDiscardedEdit,
            kerf_core::Error::AssetInUse {
                asset: uuid::Uuid::new_v4(),
                clips: 2,
            },
        ] {
            let rendered = e.to_string();
            let mapped = core_err(e);
            assert_eq!(mapped.code, ErrorCode::INVALID_PARAMS, "{rendered}");
            assert_eq!(mapped.message, rendered, "the message reaches the model as the core wrote it");
        }
        assert!(core_err(kerf_core::Error::AgentReviewRequired)
            .message
            .contains("ask them to review"));
    }

    #[test]
    fn an_import_path_is_refused_on_the_call_not_in_a_job() {
        for bad in [
            "clip.mp4",
            "https://example.com/a.mp4",
            "concat:a|b",
            "/definitely/not/here.mp4",
        ] {
            let e = core_err(kerf_core::paths::validate_import_path(std::path::Path::new(bad)).unwrap_err());
            assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{bad}");
        }
    }

    #[test]
    fn a_network_or_device_path_is_refused_before_any_filesystem_call() {
        for bad in [
            "\\\\server\\share\\clip.mp4",
            "\\\\?\\UNC\\server\\share\\clip.mp4",
            "\\\\.\\pipe\\kerf",
            "\\/server/share/clip.mp4",
        ] {
            let why = crate::require_local_input_path(bad).expect_err(bad);
            assert!(why.contains("network"), "{bad}: {why}");
            let e = caption_source(Some(bad), None).unwrap_err();
            assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{bad}");
            assert!(e.message.contains("network"), "{bad}: {}", e.message);
        }
        for bad in ["//server/share/clip.mp4", "//./pipe/kerf"] {
            assert!(crate::check_input_path(bad, true).is_err(), "{bad}");
        }
        for ok in [
            "/home/user/clip.mp4",
            "//tmp/clip.mp4",
            "C:\\Users\\me\\clip.mp4",
            "\\\\?\\C:\\Users\\me\\clip.mp4",
        ] {
            assert!(crate::check_input_path(ok, false).is_ok(), "{ok}");
        }
        assert!(
            crate::check_input_path("//tmp/clip.mp4", true).is_err(),
            "//host/share is a share on Windows"
        );
    }

    #[test]
    fn a_platform_check_frame_is_both_sides_or_neither() {
        use super::check_frame;
        assert_eq!(check_frame(None, None).unwrap(), None);
        assert_eq!(check_frame(Some(1080), Some(1920)).unwrap(), Some((1080, 1920)));
        for (w, h) in [
            (Some(1080), None),
            (None, Some(1920)),
            (Some(0), Some(1920)),
            (Some(1080), Some(99_999)),
        ] {
            assert_eq!(
                check_frame(w, h).unwrap_err().code,
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "{w:?} {h:?}"
            );
        }
    }

    #[test]
    fn the_session_is_the_header_rmcp_stamps_on_the_request() {
        use super::session_from_parts;
        let parts = |header: Option<&str>| {
            let mut request = axum::http::Request::builder().uri("/mcp");
            if let Some(h) = header {
                request = request.header("mcp-session-id", h);
            }
            request.body(()).unwrap().into_parts().0
        };
        assert_eq!(session_from_parts(&parts(Some("abc-123"))), Some("abc-123".to_string()));
        assert_eq!(
            session_from_parts(&parts(None)),
            None,
            "a client with no session has no owner"
        );
        assert_eq!(session_from_parts(&parts(Some(""))), None);
    }

    #[test]
    fn a_project_swapped_mid_job_is_noticed() {
        let mut project = kerf_core::Project::open_in_memory().unwrap();
        let before = project.generation();
        assert!(super::require_generation(&project, before, "it ran").is_ok());
        project.replace_with(kerf_core::Project::open_in_memory().unwrap()).close();
        let e = super::require_generation(&project, before, "it ran").unwrap_err();
        assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(
            e.message.contains("replaced") && e.message.contains("Run it again"),
            "{}",
            e.message
        );
    }

    /// One agent's proposal is closed to another connected at the same time, whichever door it
    /// comes through — here the one every edit tool takes.
    #[test]
    fn a_second_session_is_refused_while_the_first_owns_the_proposal() {
        let shared = std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap());
        {
            let first = super::lock_agent_edit(&shared, Some("session-a")).expect("the first edit opens the proposal");
            first
                .add_overlay("Title".to_string(), 0.0, 2.0)
                .expect("it goes into the proposal");
            assert!(
                first.staged().unwrap().is_some(),
                "review is on by default, so the edit was staged"
            );
            assert!(first.timeline().unwrap().overlays.is_empty(), "the user's cut is untouched");
        }
        // The same session carries on.
        assert!(super::lock_agent_edit(&shared, Some("session-a")).is_ok());
        // Another is turned away, with words for the model.
        let refused = super::lock_agent_edit(&shared, Some("session-b")).err().expect("refused");
        assert_eq!(refused.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(refused.message.contains("another agent"), "{}", refused.message);
        // So is a client that names no session at all.
        assert!(super::lock_agent_edit(&shared, None).is_err());
        // It cannot throw the proposal away either: a session that merely declares itself is
        // checked against the owner.
        let stranger = super::lock_agent_as(&shared, Some("session-b"));
        let e = stranger.discard_staged().map(|_| ()).unwrap_err();
        assert!(matches!(e, kerf_core::Error::StagedEditOwnedByOther), "{e}");
        let e = stranger.undo_staged().map(|_| ()).unwrap_err();
        assert!(matches!(e, kerf_core::Error::StagedEditOwnedByOther), "{e}");
    }

    /// A task removed by an agent that does not own the proposal under it stays in the queue; the
    /// owner may remove it, and the user always may.
    #[test]
    fn a_task_is_not_removed_by_an_agent_that_does_not_own_its_proposal() {
        let shared = std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap());
        let task = super::lock_agent_as(&shared, Some("session-a")).add_task("tighten").unwrap();
        {
            let owner = super::lock_agent_edit(&shared, Some("session-a")).unwrap();
            owner.claim_next_task(Some("session-a")).unwrap();
            owner.add_overlay("Title".to_string(), 0.0, 2.0).unwrap();
        }
        let stranger = super::lock_agent_as(&shared, Some("session-b"));
        let e = stranger.remove_task(task.id).unwrap_err();
        assert!(matches!(e, kerf_core::Error::StagedEditOwnedByOther), "{e}");
        assert!(
            stranger.get_task(task.id).unwrap().is_some(),
            "the refusal left the task in the queue"
        );
        drop(stranger);
        let owner = super::lock_agent_as(&shared, Some("session-a"));
        owner.remove_task(task.id).unwrap();
        assert!(owner.get_task(task.id).unwrap().is_none());
    }

    /// What the user discarded, an agent does not bring back; what it discarded itself it may.
    #[test]
    fn an_agent_cannot_restore_a_proposal_the_user_discarded() {
        let shared = std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap());
        {
            let guard = super::lock_agent_edit(&shared, Some("session-a")).unwrap();
            guard.add_overlay("Title".to_string(), 0.0, 2.0).unwrap();
        }
        crate::lock_user(&shared).discard_staged().unwrap();
        let agent = super::lock_agent_as(&shared, Some("session-a"));
        let e = agent.restore_discarded().unwrap_err();
        assert!(e.to_string().contains("only they can bring it back"), "{e}");
        assert!(agent.staged().unwrap().is_none());
        drop(agent);
        assert!(crate::lock_user(&shared).restore_discarded().is_ok(), "the user can");
    }

    /// With review on, the agent leaves the user's ripple switch alone.
    #[test]
    fn an_agent_does_not_flip_the_ripple_switch_while_review_is_on() {
        let shared = std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap());
        let agent = super::lock_agent_as(&shared, Some("session-a"));
        assert!(agent.agent_review());
        let e = agent.set_ripple_mode(true).unwrap_err();
        assert_eq!(super::core_err(e).code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(!agent.ripple_mode().unwrap());
    }

    /// With review on, an agent cannot accept its own proposal — `force` or not — nor step the
    /// user's live history.
    #[test]
    fn an_agent_cannot_approve_its_own_work_while_review_is_on() {
        let shared = std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap());
        {
            let guard = super::lock_agent_edit(&shared, Some("session-a")).unwrap();
            guard.add_overlay("Title".to_string(), 0.0, 2.0).unwrap();
        }
        let agent = super::lock_agent_as(&shared, Some("session-a"));
        assert!(agent.agent_review(), "on by default");
        for force in [false, true] {
            let e = agent.apply_staged(force).map(|_| ()).unwrap_err();
            assert!(matches!(e, kerf_core::Error::AgentReviewRequired), "force={force}: {e}");
        }
        let e = agent.apply_staged_subset(&["x".to_string()], true).map(|_| ()).unwrap_err();
        assert!(matches!(e, kerf_core::Error::AgentReviewRequired), "{e}");
        assert!(agent.staged().unwrap().is_some(), "nothing was applied");
        // Its own steps it may take back, and bring a discarded proposal back.
        let (label, _) = agent.undo_staged().unwrap();
        assert!(!label.is_empty());
        agent.discard_staged().unwrap();
    }

    /// `mcp.md` lists every tool by name; a tool added without a line there fails here, so the
    /// index cannot fall behind the surface again.
    #[test]
    fn the_docs_name_every_tool() {
        let docs = include_str!("../../../.claude/docs/mcp.md");
        let missing: Vec<String> = tool_names()
            .into_iter()
            .filter(|name| !docs.contains(&format!("`{name}`")))
            .collect();
        assert!(
            missing.is_empty(),
            "add these to the tool index in .claude/docs/mcp.md: {missing:?}"
        );
    }

    /// An export, cover or subtitle file is never written over something the project still needs.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_output_onto_a_source_file_is_refused_before_anything_renders() {
        let dir = std::env::temp_dir().join(format!("kerf-mcp-out-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("clip.mp4");
        std::fs::write(&source, b"not really a video").unwrap();
        let project = kerf_core::Project::open_in_memory().unwrap();
        let asset: kerf_core::Asset = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(),
            "path": source.to_string_lossy(),
            "name": "clip.mp4",
            "duration": 2.0,
            "streams": [],
            "imported_at": "2025-01-01T00:00:00Z",
        }))
        .unwrap();
        project.insert_or_get_asset(&asset).unwrap();
        let shared = std::sync::Arc::new(std::sync::Mutex::new(project));

        let refused = super::check_output_paths(shared.clone(), None, vec![dir.join("out.mp4"), source.clone()])
            .await
            .expect_err("the source is not a place to render to");
        assert_eq!(refused.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(refused.message.contains("clip.mp4"), "{}", refused.message);
        // The same file by another way in.
        let detour = dir.join("..").join(dir.file_name().unwrap()).join("clip.mp4");
        assert!(super::check_output_paths(shared.clone(), None, vec![detour]).await.is_err());
        // Anywhere else is fine.
        super::check_output_paths(shared, None, vec![dir.join("out.mp4")])
            .await
            .expect("a fresh path");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A call that fails leaves no empty proposal owned by the agent behind.
    #[test]
    fn a_failed_first_edit_opens_no_proposal() {
        let shared = std::sync::Mutex::new(kerf_core::Project::open_in_memory().unwrap());
        {
            let guard = super::lock_agent_edit(&shared, Some("session-a")).unwrap();
            assert!(guard.remove_overlay(uuid::Uuid::new_v4()).is_err(), "no such title");
            assert!(guard.staged().unwrap().is_none(), "nothing was staged, so nothing is open");
        }
        // And so a second session is not locked out by a mistake of the first.
        let second = super::lock_agent_edit(&shared, Some("session-b")).unwrap();
        second.add_overlay("Title".to_string(), 0.0, 2.0).unwrap();
        assert!(second.staged().unwrap().is_some());
    }
}
