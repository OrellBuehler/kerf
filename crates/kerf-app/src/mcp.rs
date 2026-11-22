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
    AudioEffect, BlendMode, CaptionFormat, CaptionImportRequest, CaptionOptions, CaptionStyle, CaptionTimeBase, ClipCut, ClipLut,
    ClipMove, CurvePoint, Curves, Delivery, EditSource, ExportOptions, Fit, Keyframe, Mask, MaskShape, Project, Projection,
    Property, PropertyKey, PropertyTrack, ReframeKeyframe, Region, SpeedKey, SplitSide, Stabilize, StabilizeStatus, StreamKind,
    SvgCheck, SvgDoc, SvgLayout, SvgOverlay, SvgOverlayPatch, SvgOverlayResult, SvgPreset, SvgPresetKind, SvgPresetParams,
    SvgSource, Tag, TagEdit, TextKeyframe, TimeRange, Timeline, TitleClipPatch, TitleClipPlacement, TitleClipResult, Transition,
    TransitionKind, VideoEffect, Wheel, Wheels,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use serde::Serialize;
use uuid::Uuid;

use crate::shell::Shell;

mod activity;
mod analysis;
mod args;
pub(crate) mod batch;
mod captions;
mod clip_look;
mod delivery;
mod editing;
mod fetch;
mod files;
mod hints;
mod jobs;
mod keyframes;
mod library;
mod linked;
mod looking;
mod mix;
mod music;
mod project_tools;
mod prompts;
mod reads;
mod review;
mod server;
mod sound;
mod tasks;
mod templates;
mod thumbnails;
mod titles;
mod toolsets;
mod workspace;

pub(crate) use activity::{project_changes, Event as ActivityEvent};
pub(crate) use files::{check_public_url, sweep_stale_parts, MediaIo, DEFAULT_MAX_UPLOAD};
use jobs::Run;
pub(crate) use server::{
    bind_problem, check_tool_sets, client_url, current_token, finish_jobs, set_overrides, start, start_stdio, stop,
    wait_listening, Overrides, Stdio,
};
pub use server::{
    check_addr, check_token, connection_info, endpoint_url, generate_token, server_error, settings_changed, ConnectionInfo,
    DEFAULT_ADDR,
};
pub use toolsets::ToolSetInfo;
pub(crate) use workspace::Workspace;

#[cfg(test)]
use delivery::*;
#[cfg(test)]
use editing::*;
#[cfg(test)]
use keyframes::*;
#[cfg(test)]
use library::*;
#[cfg(test)]
use looking::*;
#[cfg(test)]
use music::*;
#[cfg(test)]
use sound::*;
#[cfg(test)]
use titles::*;

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
    app: Shell,
    /// The rmcp session this server instance belongs to (the `Mcp-Session-Id` the client sends).
    /// rmcp makes one instance per session, so this is the agent's identity: it owns the
    /// proposal its edits open, and no other session may touch it. `None` for a client that
    /// keeps no session.
    session: Arc<Mutex<Option<String>>>,
    /// The resources this session subscribed to, and the task that tells it when they change.
    listener: activity::Listener,
    /// The one session of a `--stdio` run, which no HTTP request names. `None` over HTTP.
    stdio: Option<Arc<StdioSession>>,
    /// The directory every path the client names must lie in (`--workspace`); `None` leaves paths alone.
    workspace: Option<Arc<Workspace>>,
}

/// What an HTTP request says about its session, said once for the stdio one: its id (stable for
/// the life of the pipe, which is the process's) and the tool sets `--tools` picked.
struct StdioSession {
    id: String,
    tools: toolsets::Selection,
}

// ---- tool parameter schemas ------------------------------------------------

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct PageParams {
    #[schemars(description = "Skip this many of the newest entries (default 0), to page back through older ones")]
    offset: Option<usize>,
    #[schemars(description = "Most entries to return (default 50, at most 500)")]
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct AssetIdParams {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
struct ProxyStatusParams {
    #[schemars(description = "UUID of the asset; omit for every asset in the project")]
    asset_id: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ClipIdParams {
    #[schemars(description = "UUID of the clip")]
    clip_id: String,
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
struct ClipCutParams {
    #[schemars(description = "UUID of the clip to cut")]
    clip_id: String,
    #[schemars(
        description = "Timeline time to cut it at (seconds); must lie inside the clip and leave at least 0.05s of the half you keep"
    )]
    at: f64,
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
    /// The size of the source as get_timeline_state shows it (its text form: outlines left out).
    svg_bytes: usize,
}

/// A title on a track as an agent reads it: the clip's place and its document's clock, never the source.
/// Its transitions, fades, transform and keys are the clip's, read like any clip's.
#[derive(Serialize)]
pub(crate) struct TitleClipSummary {
    id: Uuid,
    track_id: Uuid,
    name: String,
    start: f64,
    end: f64,
    /// Seconds of the title's own animation cut off its front (0 for one that plays from its start).
    source_in: f64,
    /// The template it came from, with the controls and their values; absent on custom artwork.
    #[serde(skip_serializing_if = "Option::is_none")]
    template: Option<TemplateSummary>,
    /// The size of the source as get_timeline_state shows it (its text form: outlines left out).
    svg_bytes: usize,
}

impl TitleClipSummary {
    fn of(clip: &kerf_core::Clip, track_id: Uuid) -> Option<Self> {
        let title = clip.title_clip()?;
        Some(Self {
            id: clip.id,
            track_id,
            name: title.display_name(),
            start: clip.timeline_start,
            end: clip.timeline_end(),
            source_in: clip.source_in,
            template: title.template.as_ref().map(TemplateSummary::from),
            svg_bytes: text_form_bytes(&title.svg),
        })
    }

    fn all(timeline: &Timeline) -> Vec<Self> {
        timeline
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().filter_map(|c| Self::of(c, t.id)))
            .collect()
    }
}

/// What the title-clip tools answer.
#[derive(Serialize)]
pub(crate) struct TitleClipOutcome {
    title_clip: Option<TitleClipSummary>,
    /// What the sanitiser, the SMIL reader or the machine could not honour.
    warnings: Vec<String>,
}

impl From<TitleClipResult> for TitleClipOutcome {
    fn from(r: TitleClipResult) -> Self {
        Self {
            title_clip: TitleClipSummary::of(&r.clip, r.track_id),
            warnings: r.warnings,
        }
    }
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
            svg_bytes: text_form_bytes(&o.svg),
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

/// `f` on the source of every animated title of `timeline`: the floating ones and the title clips.
fn each_title_svg(timeline: &mut Timeline, mut f: impl FnMut(&mut String)) {
    for o in &mut timeline.svg_overlays {
        f(&mut o.svg);
    }
    for clip in timeline.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
        if let Some(title) = clip.title_clip_mut() {
            f(&mut title.svg);
        }
    }
}

/// A title's text may be stored as glyph outlines, which are the engine's business: an agent reads the
/// text form (`svg::restore_text`), the `<text>` it can see and set (through the controls, a text box).
fn restore_title_text(timeline: &mut Timeline) {
    each_title_svg(timeline, |svg| {
        if let Ok(text) = kerf_core::svg::restore_text(svg) {
            *svg = text;
        }
    });
}

/// The size of a title's source in its text form, the one an agent reads. Only a source with outlines is
/// read to find it.
fn text_form_bytes(svg: &str) -> usize {
    kerf_core::svg::restore_text(svg).map_or(svg.len(), |text| text.len())
}

/// Leave the sources of large SVG titles out of a timeline read: a title can carry 128 KiB and a
/// timeline 50 of them. The note says how to get the source.
fn elide_large_svg(timeline: &mut Timeline) {
    each_title_svg(timeline, |svg| {
        if svg.len() > SVG_INLINE_LIMIT {
            *svg = format!(
                "[{} bytes of SVG left out; call get_timeline_state with full_svg=true to read it]",
                svg.len()
            );
        }
    });
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
    /// The animated titles that are clips on video tracks (counted in their tracks too), without their
    /// source. Absent when there are none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    title_clips: Vec<TitleClipSummary>,
    /// The thumbnails designed beside the cut (add_thumbnail), without their layers' sources. Absent
    /// when there are none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    thumbnails: Vec<serde_json::Value>,
    /// The frame this cut is being made for, e.g. "1080x1920 (cover, 50 fps)" (the rate only when
    /// the delivery states one). Absent when the project follows its footage's shape. Worth reading
    /// before framing anything: under a cover delivery the crop decides what survives.
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery_format: Option<String>,
    /// The frame rate an export writes when it is given no `fps` of its own, and where that comes from
    /// (`delivery`: the delivery frame's rate; `footage`: the first clip with a picture, which in
    /// footage of mixed rates is whichever comes first — set one with set_delivery_format). Absent
    /// with no picture and no delivery rate.
    #[serde(skip_serializing_if = "Option::is_none")]
    export_fps: Option<ExportFps>,
    /// Set while edits are staged: this summary describes the **proposal**, not
    /// the cut the user is looking at. `staged_diff` spells out the difference.
    #[serde(skip_serializing_if = "Option::is_none")]
    staged_changes: Option<usize>,
    /// The media this cut uses whose preview proxy is not ready, or whose source file is gone.
    /// Absent when everything is ready.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    media_notes: Vec<reads::MediaNote>,
}

/// A frame rate and where it comes from (`kerf_core::FpsSource`).
#[derive(Serialize)]
struct ExportFps {
    fps: f64,
    from: kerf_core::FpsSource,
}

/// The rate an export with no `fps` of its own writes for `timeline` (as it renders), or `None` when
/// nothing sets one: no clip with a picture and no delivery rate.
fn export_fps_of(timeline: &Timeline, assets: &[kerf_core::Asset]) -> Option<ExportFps> {
    let (fps, from) = kerf_core::delivery_rate(timeline, assets);
    (from != kerf_core::FpsSource::Default).then_some(ExportFps { fps, from })
}

// ---- tools -----------------------------------------------------------------

#[tool_router]
impl KerfMcp {
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
                       A template title's source is shown with its text as `<text>` (it is drawn as glyph outlines, \
                       which are left out); set its text with set_template_values. A clip with `speed_ramp` keys \
                       (set_speed_ramp) plays at a rate that changes over its length; its `speed` then only gives the \
                       direction. A clip with `behaviors` (set_behaviors) has live sway / breathe / spin / drift / shake on top of its pose. \
                       timeline_summary is much smaller (durations, gaps, proxies); preview_svg_overlay shows how a title looks."
    )]
    fn get_timeline_state(&self, Parameters(p): Parameters<TimelineStateParams>) -> Result<String, McpError> {
        let mut timeline = self.lock().working_timeline().map_err(core_err)?;
        restore_title_text(&mut timeline);
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
        match self.app.try_state::<crate::editor_context::EditorContextState>() {
            Some(state) => json(&state.read()),
            None => json(&crate::editor_context::EditorContextState::default().read()),
        }
    }

    #[tool(
        description = "The guide to editing with Kerf: how the tools fit together, in the order an edit uses \
                       them (inspect and analyse, look at the footage, cut, adjust, ripple mode, linked picture \
                       and sound, locked tracks, polish, effects, mixing, keyframes, 360 reframing, captions, \
                       SVG titles and templates, voiceover, delivery formats, staging and review, background \
                       jobs and subscriptions, paging, history, platform checks, loudness, music, export). \
                       Read it once before your first edit."
    )]
    fn get_editing_guide(&self, context: RequestContext<RoleServer>) -> Result<String, McpError> {
        let note = self.selection(&context)?.note().unwrap_or_default();
        Ok(format!("{EDITING_GUIDE}{note}"))
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
                title_clips: TitleClipSummary::all(&timeline),
                thumbnails: timeline.thumbnails.iter().map(thumbnails::thumbnail_view).collect(),
                delivery_format: timeline.format.map(|d| match d.rate() {
                    Some(fps) => format!(
                        "{}x{} ({}, {} fps)",
                        d.width,
                        d.height,
                        format!("{:?}", d.fit).to_lowercase(),
                        kerf_core::fmt_fps(fps)
                    ),
                    None => format!("{}x{} ({})", d.width, d.height, format!("{:?}", d.fit).to_lowercase()),
                }),
                export_fps: export_fps_of(&timeline, &assets),
                staged_changes,
                media_notes: media.notes,
            })
        })
        .await?;
        json(&summary)
    }
}

impl KerfMcp {
    fn new(project: Arc<Mutex<Project>>, app: Shell) -> Self {
        Self {
            project,
            app,
            session: Arc::new(Mutex::new(None)),
            listener: activity::Listener::default(),
            stdio: None,
            workspace: server::workspace(),
        }
    }

    /// The server of the `--stdio` session `id`, listing the tool sets `tools`.
    fn over_stdio(project: Arc<Mutex<Project>>, app: Shell, id: String, tools: toolsets::Selection) -> Self {
        Self {
            listener: activity::Listener::lasting(),
            stdio: Some(Arc::new(StdioSession { id, tools })),
            ..Self::new(project, app)
        }
    }

    /// A path the client named, as the tool reads or writes it: as given, or resolved inside the
    /// workspace when the server has one (`workspace::confine`).
    fn path_arg(&self, path: &str) -> Result<String, McpError> {
        workspace::confine(self.workspace.as_deref(), path)
    }

    /// An import once its path is checked: probe (and, for a lens pair, stitch) with the lock
    /// released, insert into the project of `generation` as `owner`, queue the proxy. The GUI's Stop
    /// on an import ends it too.
    fn import_run(
        &self,
        path: String,
        run: Run,
        owner: Option<String>,
        generation: u64,
    ) -> impl std::future::Future<Output = Result<String, McpError>> + Send + 'static {
        let me = self.clone();
        let gui_stop = self.app.state::<crate::AppState>().begin_import();
        async move {
            let run = run.also_cancelled_by(gui_stop);
            let (project, app, workspace) = (me.project.clone(), me.app.clone(), me.workspace.clone());
            let asset = blocking(move || {
                // Checked again here, right before FFmpeg reads it: `import_url`'s file was not there
                // when its call was checked, and a file in the workspace may have been replaced since.
                crate::require_local_input_path(&path).map_err(|e| McpError::invalid_params(e, None))?;
                kerf_core::paths::validate_import_path(std::path::Path::new(&path)).map_err(core_err)?;
                if let Some(ws) = &workspace {
                    ws.check_media(&path).map_err(|e| McpError::invalid_params(e, None))?;
                }
                // The same shape as the GUI's import, so a multi-minute stitch never freezes the
                // user's editing.
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
        }
    }

    /// The files a tool names after a checked output (`cut-9x16.mp4` beside `cut.mp4`) held to the
    /// workspace too: one of them may be a link out of it.
    fn check_beside<'a>(&self, paths: impl IntoIterator<Item = &'a std::path::Path>) -> Result<(), McpError> {
        if self.workspace.is_some() {
            for path in paths {
                self.path_arg(&path.to_string_lossy())?;
            }
        }
        Ok(())
    }

    /// Note which rmcp session a call arrived on (called for every tool call, before it runs).
    fn note_session(&self, context: &RequestContext<RoleServer>) {
        let session = session_id_of(context).or_else(|| self.stdio.as_ref().map(|s| s.id.clone()));
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

/// The rmcp session a request belongs to: the `Mcp-Session-Id` header of the HTTP request it
/// rode in on. `None` for a client that keeps no session, and for anything that did not come
/// over HTTP (the stdio session is [`KerfMcp::stdio`]'s).
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

/// A page of the task queue, as `list_tasks` answers it.
fn tasks_page(project: &Project, page: PageParams) -> Result<String, McpError> {
    let (offset, limit) = reads::page_of(page.offset, page.limit, 50, 500);
    let (tasks, total) = project
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

/// What `kerf://project` reads: which project is open (`generation` moves when another one is), the
/// cut the session works on in brief, and the proposal staged on it, if any.
fn project_brief(project: &Project) -> Result<String, McpError> {
    let timeline = project.working_timeline().map_err(core_err)?;
    let staged = project.staged().map_err(core_err)?;
    json(&serde_json::json!({
        "generation": project.generation(),
        "timeline": reads::timeline_brief(&timeline),
        "proposal": staged.map(|s| serde_json::json!({
            "owned_by_agent": s.owned_by_agent,
            "edits": s.edits.len(),
            "stale": s.stale,
        })),
    }))
}

/// The text of the resource `uri` (`activity`): the polling tools' answers — `kerf://jobs` is
/// `list_jobs`, a job its `job_status`, `kerf://tasks` the first page of `list_tasks`,
/// `kerf://project` [`project_brief`]. The project is read under its lock as the session `owner`;
/// the jobs need none.
fn resource_text(project: &Mutex<Project>, owner: Option<&str>, uri: &str) -> Result<String, McpError> {
    match uri {
        activity::PROJECT_URI => project_brief(&lock_agent_as(project, owner)),
        activity::TASKS_URI => tasks_page(&lock_agent_as(project, owner), PageParams::default()),
        activity::JOBS_URI => json(&jobs::jobs().list()),
        other => match activity::job_of(other).and_then(|id| jobs::jobs().view(id)) {
            Some(view) => json(&view),
            None => Err(activity::unknown_resource(other)),
        },
    }
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
            + KerfMcp::tool_router_templates()
            + KerfMcp::tool_router_batch()
            + KerfMcp::tool_router_thumbnails()
            + KerfMcp::tool_router_files()
            + KerfMcp::tool_router_analysis()
            + KerfMcp::tool_router_captions()
            + KerfMcp::tool_router_clip_look()
            + KerfMcp::tool_router_delivery()
            + KerfMcp::tool_router_editing()
            + KerfMcp::tool_router_keyframes()
            + KerfMcp::tool_router_library()
            + KerfMcp::tool_router_linked()
            + KerfMcp::tool_router_looking()
            + KerfMcp::tool_router_mix()
            + KerfMcp::tool_router_music()
            + KerfMcp::tool_router_review()
            + KerfMcp::tool_router_sound()
            + KerfMcp::tool_router_tasks()
            + KerfMcp::tool_router_titles();
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

impl KerfMcp {
    /// The tool sets this connection lists: `--tools` over stdio, else the URL's.
    fn selection(&self, context: &RequestContext<RoleServer>) -> Result<toolsets::Selection, McpError> {
        match &self.stdio {
            Some(stdio) => Ok(stdio.tools.clone()),
            None => tool_selection(context),
        }
    }
}

/// The tool sets the request's URL asks for (`?tools=core,titles`, `toolsets.rs`); every tool for a
/// request that came some other way.
fn tool_selection(context: &RequestContext<RoleServer>) -> Result<toolsets::Selection, McpError> {
    let query = context
        .extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.uri.query());
    toolsets::Selection::from_query(query).map_err(|e| McpError::invalid_params(e, None))
}

/// `initialize`, refused when the URL names a tool set there is not.
fn checked_initialize<S: ServerHandler>(
    server: &S,
    request: rmcp::model::InitializeRequestParams,
    context: &RequestContext<RoleServer>,
) -> Result<rmcp::model::InitializeResult, McpError> {
    tool_selection(context)?;
    context.peer.set_peer_info(request.clone());
    server.negotiate_initialize(&request)
}

/// What the server offers: tools, the workflow prompts and the resources with their subscriptions.
fn capabilities() -> ServerCapabilities {
    ServerCapabilities::builder()
        .enable_prompts()
        .enable_resources()
        .enable_resources_subscribe()
        .enable_tools()
        .build()
}

/// How the server introduces itself to clients. `ServerConfig::default()` fills
/// `server_info` in from *rmcp's* own build env, so an untouched default has
/// every client listing this server as "rmcp".
fn server_identity() -> Implementation {
    Implementation::new("kerf", env!("CARGO_PKG_VERSION"))
}

/// What the server tells a client about itself and how to work (the MCP
/// `instructions`): the workflow an agent cannot infer from the tool list.
/// Sent at `initialize`. Clients cut server instructions short (Claude Code at 4096 characters),
/// so this stays a short routing note and the whole workflow is `EDITING_GUIDE`.
const INSTRUCTIONS: &str = "Kerf is the video editor open on this computer, and the user watches what you do \
             in its window. Use these tools for any video or audio work (cutting, titles, captions, \
             music, sound effects, voiceover, reframing, export) instead of running ffmpeg or writing scripts: an edit \
             made here lands in the user's timeline, stays editable, is reviewed by them, and nothing is \
             re-encoded until export. Call get_editing_guide once before your first edit: it explains \
             how the tools fit together (ripple mode, linked picture and sound, captions, titles and \
             templates, music, sound effects, voiceover, delivery formats, loudness, jobs) and what a refusal means. \
             The loop: claim_next_task takes the user's oldest queued task (list_tasks shows the queue). \
             See what is there with list_assets / get_timeline_state (import_asset adds a file by its \
             absolute path) and analyze_asset (silence, scenes, transcript, loudness, beats); \
             search_transcripts finds where anything is said across the whole library. When the \
             user says \"this clip\", \"here\" or \"the selection\", get_editor_context says what they \
             have selected and where the playhead and marks are. Look at the footage with skim_asset \
             (a contact sheet), get_frame and preview_timeline (the cut you have built) before and after \
             you edit, and at a whole assembled cut with skim_timeline. Cut with split / trim / move_clip / remove / ripple_delete, then polish with \
             set_volume, set_fade, set_transition, set_transform, set_color, apply_motion_preset (an \
             entrance, exit or emphasis such as pop_in or slide_out_to_left, on top of the clip's own pose), \
             set_behaviors (a live sway, breath, spin, drift or shake that never needs keys), \
             add_overlay and generate_captions (caption last, once the cut is settled). A plan of several edits runs as \
             one apply_edits call: one step, all or nothing, and dry_run previews it. Before you call a cut done, run \
             platform_check and review_cut, and get_levels for anything going to social; export renders it to an \
             absolute path. \
             While the user's review is on (the default; get_settings shows `agent_review`) your edits \
             are STAGED: they go into a proposal the user accepts or dismisses in Kerf, your reads \
             (get_timeline_state, preview_timeline, timeline_summary) show the proposal, and staged_diff \
             shows what you are handing over; you cannot apply it yourself. With review off your edits, \
             task work included, land on the cut as you make them, each an undoable step (stage_edits \
             holds a change back for the user to look at first). Finish a task with complete_task (or \
             fail_task) either way, and say what you did. \
             A LOCKED track (`locked` in get_timeline_state) is the user's: nothing on it is moved, \
             trimmed, split, removed or added to, and the call fails with `track V1 is locked`. You may \
             lock a track but not unlock one, so ask the user; a clip added without a track goes to the \
             first unlocked one. \
             Long tools (analyze_asset, export, compose_music, generate_voiceover, smart_crop, ...) take \
             `background: true` and answer a job_id at once; follow it with job_status or subscribe to \
             kerf://jobs/{job_id}. \
             The prompts (rough_cut_from_transcript, podcast_cleanup, social_reel, music_bed, sound_design, deliver) are \
             these workflows as steps. A client may connect with ?tools=core,titles on the URL (--tools core,titles over \
             stdio) to list fewer tools; get_editing_guide then names the sets left out.";

/// What `get_editing_guide` answers: the workflow across every tool.
const EDITING_GUIDE: &str = "How to edit with Kerf. The user queues editing tasks in the desktop app; \
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
             have assembled at a given time so you can check it on screen. Once a \
             cut is assembled, check it with skim_timeline: one contact sheet of \
             the cut (or a span of it), each cell indexed to its timeline time and \
             the clips on screen, so the pacing, a black gap, a shot used twice and \
             the flow show in one call. Look, \
             then look closer: skim_asset and skim_timeline with `cell` open one sheet cell at \
             full detail, and get_frame / preview_timeline take a `region` \
             (fractions of the frame) that is cropped out and enlarged — the way \
             to read a face, on-screen text, a caption against the safe area or \
             a mask edge, since a whole frame at the same width shows a fraction \
             of that detail. Prefer a region to a larger max_width. \
             Then assemble a non-destructive edit with the \
             cut/split/trim/add/reorder/move_clip/remove/ripple_delete tools \
             (move_clip frees a clip to any position or same-kind track; \
             ripple_delete closes the gap; move_clips / remove_clips do the \
             same to several clips as one revision, all or nothing; apply_edits \
             { ops: [{tool, args}], dry_run?, label? } runs a whole plan of the quick \
             edit tools, each with its own arguments, as ONE revision or staged step: \
             an op that fails applies nothing and the error names it, dry_run answers \
             the diff and changes nothing, and \"$K.path\" in an op's args is a value \
             from op K's answer, e.g. \"$0.right.id\" after a split_at). To adjust \
             a cut rather than build one: roll_edit moves the cut between two \
             adjacent clips (both shift, the pair's length is unchanged), \
             slip_clip shows other footage in the same place and length, \
             slide_clip moves a clip between its neighbours (they give way), \
             and split_remove trims a clip's start or end to a time (split_remove_clips \
             does that to a picture and its sound as one revision) — the first \
             three clamp to the footage and report `applied`, and never ripple. \
             To act on a TIME or a RANGE across tracks rather than on one clip: \
             split_all_tracks cuts every unlocked track at one time (or the tracks in \
             `track_ids`); lift_range takes [from, to) off those tracks and LEAVES the gap, \
             extract_range takes it off and CLOSES the gap; ripple_delete_gap closes an \
             empty stretch of one track. With no `track_ids` the targeted tracks are \
             every unlocked one (locked ones are left alone and listed under `skipped`); \
             a clip linked to one that is cut is cut with it (and, on an extract, moved) \
             on its own track unless `link: false`. None of them follows ripple mode, so \
             none takes `ripple`: lift leaves the gap whatever the mode, extract and the \
             gap delete close it themselves. Text overlays, floating titles and markers do \
             not move on an extract or a gap delete, so re-place them after one. A sliver is \
             never left: a lift or an extract is refused, with the nearest range that works, \
             when it would leave a remainder shorter than one frame of the cut's rate, and a cut \
             that would leave such a half is not made (`not_cut`). A sound that only leads its \
             picture into an extract is carried with it, trimming the linked sound of the shot \
             it lands on (`lock_trimmed` says which). All of them batch in apply_edits. \
             To edit by what is SAID, read the cut with get_cut_transcript: its \
             lines are the words the cut plays, in timeline time, each with its \
             clip_id, and words=true gives every word its timeline start/end and \
             source_start/source_end. Cut words with cut_clip_range on that clip \
             and those source times (or split_at at the timeline bounds and \
             ripple_delete the piece), then read it again — later lines move. \
             The project may be in ripple mode, so check get_ripple_mode before you trim, \
             remove or retime: with it on, trim / set_speed / set_speed_ramp / remove / \
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
             over the interview (later video tracks composite on top). A LOCKED \
             track (`locked` in get_timeline_state; the user can lock one while \
             your edits are under review, and the lock holds for them too) is the \
             user's: a clip on it is not moved, trimmed, split, cut, removed, \
             retimed, disabled or reordered, and nothing is added or pasted onto \
             it — the call fails with `track V1 is locked` and changes nothing; \
             its clips' volume, fades, effects, grade, transform and keyframes can \
             still be set. You may lock a track but not unlock one (set_track_locked \
             false is refused) nor remove a locked one: ask the user to unlock it \
             rather than working around it; a clip added without a track goes to \
             the first unlocked one. Polish \
             with set_volume / set_fade (fade-in/out, e.g. to smooth hard cuts), \
             set_speed (one constant rate) or set_speed_ramp (slow motion or a speed-up that \
             eases in and out: keys of clip-local timeline seconds and speed, which retime the \
             clip like set_speed and so ripple the same), stabilize_clip (shaky hand-held footage made steady; \
             it measures the camera first and plays as shot until then), set_transform (scale / position / rotation / opacity / \
             crop — picture-in-picture; set_transform_at for an animated clip, which keys \
             what is animated at a moment), set_color and set_transition (a fade \
             between scenes, a slide or push between the shots of a montage). \
             Go further: set_video_effects (blur / sharpen / \
             grayscale / invert / vignette / chroma_key — green-screen so a lower \
             track shows through), set_mask (cut a clip to a rectangle or ellipse so \
             a lower track shows through — with a duplicated, blurred copy above, \
             that is how a face or a number plate is blurred), \
             set_blend_mode (screen a light leak or flare shot on black onto the shot \
             below, multiply a texture into it), \
             set_audio_effects (highpass / lowpass / EQ / \
             compressor / gate / denoise). Mix with set_track_volume — a music bed belongs \
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
             clip the same animation. For the usual moves skip the keys: apply_motion_preset gives \
             clips an entrance (fade_in, slide_in_from_left, zoom_in, pop_in, drop_in, ...), an exit \
             (fade_out, slide_out_to_right, zoom_out, ...) or an emphasis (pulse, wiggle, bounce, \
             push_in, a slow Ken Burns zoom) in one edit (list_motion_presets says what each does). \
             It is relative to the clip's own pose, so it suits a clip already scaled or positioned; \
             entrances start at the clip's start, exits end at its end, `at` and `duration` move and \
             size the window (clamped to the clip: read the window each clip got back), and mode add \
             (default) keeps the clip's other keys outside the window so a fade_in, a fade_out and a \
             mid-clip pulse coexist, while replace overwrites those numbers' keys. For motion that just \
             goes on (a gentle sway, a breathing zoom, a steady spin or drift, a handheld shake) use \
             set_behaviors instead of keys: sway, breathe, spin, drift and shake ride on top of the clip's \
             pose and keys, each a few numbers (amount, speed in Hz or turns a second, phase), up to 8 \
             a clip, export exactly and cost nothing however long the clip is; the list you send REPLACES \
             the clip's (the clip's `behaviors` in get_timeline_state), [] clears it. Key edits write the \
             pose without them. bake_behaviors turns them into keys (about 33 s of a clip at 30 fps at most) \
             when you want to edit the motion itself. The GPU preview draws a clip with them through FFmpeg. \
             360 footage (Insta360 \
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
             title with `values` for its controls (an edit like any other) \
             and set_template_values changes them later; a title made from a \
             template lists its controls and current values under `template` in \
             timeline_summary. update_svg_overlay with `svg` replaces the artwork \
             and ends the template. A title can also be a clip on a video track \
             (add_title_clip, or apply_template with on_track=true): it then takes \
             a transition from the shot before it (a dip to white into an end \
             card), fades and keyframes, and extends like any clip; \
             timeline_summary lists those under `title_clips`. For text of any \
             length (a name on a lower third), give its <text> a box with \
             set_title_text_box: it wraps, or with fit=\"shrink\" stays on one line \
             made smaller to fit, and animate its letters, words or lines with \
             set_title_text_animation (a `preset` from list_text_animation_presets: \
             rise, typewriter, blur_in, ..., and a `curve` from \
             list_text_animation_curves: an overshoot or a spring). save_template, import_template and \
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
             not. It also sets the frame RATE every export defaults to (`fps`): \
             without one an export takes the rate of the first clip with a \
             picture, which in footage of mixed rates is whichever comes first — \
             timeline_summary shows what it would be (`export_fps`), and an \
             export reports the rate it wrote. Look at the result with preview_timeline; the crop is an \
             ordinary transform the user can adjust. When the same cut is \
             going to several places, export_variants renders it at every \
             shape (9:16, 1:1, 4:5, 16:9) in one call and one pass, framing each shot for \
             each shape and reporting what each file is ready for — prefer it \
             to exporting three times by hand. \
             Whether your edits are STAGED depends on the user's switch (Settings › Agent; \
             get_settings shows `agent_review`, on by default). With review ON, claiming a \
             task — or simply making your first edit — opens a proposal that is yours \
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
             With review OFF nothing is staged for you: claim_next_task only marks the \
             task working, and every edit lands on the live cut as you make it — an \
             ordinary revision in history, attributed to you, that the user (or you, \
             with undo) can step back. Your reads are the live cut, so look at the \
             result with preview_timeline as you go. complete_task then just settles the \
             task (it shows as done; its summary is what the user reads), and fail_task \
             only marks it failed: what you already did stays on the cut, so undo it \
             yourself or say what is left. The user's locks hold directly, and the user \
             may edit between your calls — each edit meets the cut as it is then. Another \
             agent connected at the same time edits the same live cut, so read \
             get_timeline_state again after a pause rather than trusting an old read. \
             stage_edits is still yours to call: it opens a proposal even with review \
             off, for a change worth the user's look before it lands (everything above \
             about a proposal then applies to it, apply_staged_edits included — or leave \
             it for the user). A proposal that is already open when the user switches \
             review off stays open and keeps taking your edits until it is applied or \
             discarded; it is never applied for you, and claim_next_task is refused while \
             it is open. A proposal is closed to another agent connected at the same time \
             (it is yours again if your connection drops and you reconnect). \
             Long tools (analyze_asset, export, export_variants, \
             generate_voiceover, compose_music, separate_stems, download_speech_model, \
             import_asset, smart_crop, refine_transcript, sync_clips_by_audio) take \
             `background: true`: they answer a \
             `job_id` at once and keep working, and job_status reports progress \
             and, at the end, the result (cancel_job stops one, list_jobs lists \
             them). Without it the call stays open — send a progressToken for \
             progress. Rather than polling, subscribe to the resources: \
             kerf://jobs/{job_id} (one job), kerf://jobs, kerf://tasks and \
             kerf://project are announced with notifications/resources/updated when \
             they change, whoever changed them (the user editing the cut included); \
             read one for the details, and read it once right after subscribing — what \
             changed before is not replayed. A subscribed client that sends no request \
             (a ping is enough) for five minutes loses its subscriptions. Results are kept small: get_asset_metadata and analyze_asset \
             summarise an analysis (counts, durations, the head of the transcript), \
             and get_transcript (lines with their speakers; words=true adds each \
             line's timed words, best over a from/to window) / get_cut_transcript \
             (the same for the whole cut, in timeline time) / get_analysis_detail \
             page through the rest. To find material by what is said, search_transcripts \
             searches every transcript at once (a phrase, all_words within a line or any_word; \
             `speaker` narrows it): each hit is an asset with SOURCE start/end and `in_cut`, \
             the clips already using it (empty when unused) — cut_clip / \
             add_clip_to_timeline that source window to use it, and \
             `assets_without_transcript` names what still needs analyze_asset; \
             get_timeline_state shows generated captions as a window \
             (captions_offset / captions_limit) and, for a very long cut, its clips \
             (clips_offset / clips_limit); history, list_tasks, list_assets, \
             staged_diff and revision_diff page with offset / limit. list_assets shows each asset's preview proxy (ready, \
             queued, building with an ETA, failed, off) and which analyses have \
             run; proxy_status adds the whole proxy queue (the order, an ETA, whether the user paused it) and \
             rebuild_proxy with `next: true` puts one asset's proxy first. In a big library pass list_assets \
             `fields: \"compact\"` (one short line per asset) and, to find \
             what an import added, `imported_after` set to the `newest_imported_at` it gave before the import. \
             Assets carry TAGS the user (or you) put on them whole: `location: Cumbuco` is a keyed tag, `jump` a \
             plain one. list_assets shows them and takes `tags` terms to narrow the list; list_tags is the \
             vocabulary in use (each key with its values and counts) — read it before tagging, to keep the user's \
             spelling; edit_asset_tags adds and removes tags on one asset or many (held back with your proposal \
             like any edit). To cut a video by place, rider or any other tag, tag the assets and call \
             assemble_by_tag with that key: one section per value, A to Z or in your `order`, each titled with \
             its value, appended to the end of the cut as one edit. \
             timeline_summary's `media_notes` lists the cut's media whose \
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
             existing followers), and review_cut: it finds the editing mistakes \
             you would see on a watch (flash frames, cuts inside a word, sound \
             cut into sound with no fade, jump cuts, captions a later edit left \
             behind, black gaps) and names the tool that fixes each. For a cut \
             going to social, also run get_levels: \
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
             with replace_clip_id. For sound effects — whooshes on cuts, pops on \
             titles, a riser into a drop, an impact on a reveal — settle the cut, \
             then suggest_sound_effects reads it (cuts, transitions, titles, punch-ins, \
             drops in the music, markers named sfx:<kind>; density subtle, balanced or \
             punchy) and answers placements that add_sound_effects takes as they are: \
             drop the ones you do not want, change a kind or a volume_db, add the rest in \
             ONE call. list_sound_effects says what each of the 21 sounds is for. `at` is where \
             the effect's HIT lands, not where its clip starts: a whoosh peaks part-way \
             in, an impact starts on it, a riser ENDS on it, and Kerf starts the clip \
             earlier for you. Effects are synthesized (nothing to download; the same spec \
             is the same audio), go on FX, FX 2 … audio tracks (a new lane when they \
             overlap), and are staged like any edit. Then get_levels: suggested ones sit \
             about -3 dB, and -8 dB under speech. update_sound_effect re-renders one from a \
             whole new spec with its hit where it was. Call export to render, and export_cover to \
             write a plain frame of the video as its cover. A thumbnail with \
             graphics of its own (a headline, a template, a logo the video \
             never shows) comes once the cut is final: suggest_thumbnail_frames \
             for good frames (look at its contact sheet), add_thumbnail with \
             the frame, a size and its layers, preview_thumbnail to look, \
             update_thumbnail to adjust, export_thumbnail to write it. \
             export / export_srt / export_captions / export_interchange / \
             export_markers / export_cover / export_thumbnail / export_variants all take an \
             absolute local output path and refuse to overwrite a file that's \
             already there unless you pass overwrite=true. \
             The server's prompts — rough_cut_from_transcript, podcast_cleanup, social_reel, music_bed, sound_design and \
             deliver — are these workflows as numbered steps the user can start. A connection may list only \
             some tool sets (`?tools=core,titles` on the server URL, `--tools core,titles` for a stdio server; the sets are core, analysis, clips, \
             keyframes, titles, audio, music, delivery, tasks and project), and this guide then ends by \
             saying which.";

/// Added to the instructions when the server has a workspace (`--workspace`), the directory after it
/// when that fits the budget.
const WORKSPACE_NOTE: &str = "Files stay in the workspace: give paths relative to it, even where a tool says absolute.";

/// The words of [`INSTRUCTIONS`] a workspace makes wrong, and what they say instead.
const WORKSPACE_WORDING: [(&str, &str); 2] = [
    ("by its absolute path", "by its path"),
    ("to an absolute path.", "to a path."),
];

/// What a client reading [`INSTRUCTIONS`] would not exceed (Claude Code shows 4096 characters).
const INSTRUCTIONS_BUDGET: usize = 3500;

/// The server's instructions: [`INSTRUCTIONS`], and with a workspace its rule ([`WORKSPACE_NOTE`] and the
/// directory, left out rather than going past [`INSTRUCTIONS_BUDGET`]).
fn instructions(workspace: Option<&Workspace>) -> String {
    instructions_for(workspace.map(Workspace::root))
}

fn instructions_for(root: Option<&str>) -> String {
    let Some(root) = root else {
        return INSTRUCTIONS.to_string();
    };
    let mut text = WORKSPACE_WORDING
        .iter()
        .fold(INSTRUCTIONS.to_string(), |text, (from, to)| text.replace(from, to));
    text.push(' ');
    text.push_str(WORKSPACE_NOTE);
    let root = format!(" Workspace: {root}.");
    if text.len() + root.len() <= INSTRUCTIONS_BUDGET {
        text.push_str(&root);
    }
    text
}

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
        self.listener.touch();
        // A workspace tool is no tool at all on a server without its workspace (or its switch).
        let offered = if files::offered(self, &tool) {
            Ok(())
        } else {
            Err(McpError::invalid_params("tool not found", None))
        };
        let result = match offered.and_then(|()| reject_unknown_arguments(&request)) {
            Ok(()) => {
                let tcc = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
                router().call(tcc).await
            }
            Err(e) => Err(e),
        };
        if let Err(e) = &result {
            log_tool_error(&tool, e);
        }
        match (result, &self.workspace) {
            (Ok(rmcp::model::CallToolResponse::Complete(mut answer)), Some(ws)) => {
                for block in &mut answer.content {
                    if let ContentBlock::Text(text) = block {
                        text.text = ws.relative_in(std::mem::take(&mut text.text));
                    }
                }
                Ok(answer.into())
            }
            (result, _) => result,
        }
    }

    fn get_info(&self) -> ServerConfig {
        // `initialize` is the one moment we know an agent is on the other end
        // of the socket, so it counts as being seen even before it calls a tool.
        note_agent_activity();
        let mut info = ServerConfig::default();
        info.server_info = server_identity();
        info.capabilities = capabilities();
        info.instructions = Some(instructions(self.workspace.as_deref()));
        info
    }

    /// A URL naming a tool set there is not is refused here, where the client is connecting,
    /// rather than leaving it with a short tool list and no reason.
    async fn initialize(
        &self,
        request: rmcp::model::InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::InitializeResult, McpError> {
        checked_initialize(self, request, &context)
    }

    /// The tools of the sets the connection asked for (all of them by default). A call to one
    /// left out still runs: the selection trims the list, it is not a permission.
    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, McpError> {
        self.listener.touch();
        let selection = self.selection(&context)?;
        let mut tools = router().list_all();
        tools.retain(|tool| selection.lists(&tool.name) && files::offered(self, &tool.name));
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= rmcp::model::ProtocolVersion::V_2026_07_28);
        let mut result = rmcp::model::ListToolsResult::with_all_items(tools);
        result.ttl_ms = supports_cache_hints.then_some(0);
        result.cache_scope = supports_cache_hints.then_some(rmcp::model::CacheScope::Public);
        Ok(result)
    }

    async fn list_prompts(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListPromptsResult, McpError> {
        self.listener.touch();
        Ok(rmcp::model::ListPromptsResult::with_all_items(prompts::list()))
    }

    async fn get_prompt(
        &self,
        request: rmcp::model::GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::GetPromptResponse, McpError> {
        self.listener.touch();
        Ok(prompts::get(&request.name, request.arguments.as_ref())?.into())
    }

    async fn ping(&self, _context: RequestContext<RoleServer>) -> Result<(), McpError> {
        self.listener.touch();
        Ok(())
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, McpError> {
        self.listener.touch();
        Ok(rmcp::model::ListResourcesResult::with_all_items(activity::resources()))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListResourceTemplatesResult, McpError> {
        self.listener.touch();
        Ok(rmcp::model::ListResourceTemplatesResult::with_all_items(
            activity::resource_templates(),
        ))
    }

    /// The resources are the polling tools' answers: `kerf://jobs` is `list_jobs`, a job is its
    /// `job_status`, `kerf://tasks` the first page of `list_tasks`, `kerf://project` a brief of the
    /// open project.
    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, McpError> {
        self.listener.touch();
        let uri = request.uri;
        let text = resource_text(&self.project, self.owner().as_deref(), &uri)?;
        Ok(rmcp::model::ReadResourceResult::new(vec![
            rmcp::model::ResourceContents::text(text, uri).with_mime_type("application/json")
        ])
        .into())
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: rmcp::model::SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        // A job is subscribed to while the registry knows it: one that never was, or ended over an
        // hour ago, would never be told about.
        if let Some(id) = activity::job_of(&request.uri) {
            if jobs::jobs().view(id).is_none() {
                return Err(activity::unknown_resource(&request.uri));
            }
        }
        self.listener.subscribe(&request.uri, context.peer)
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        request: rmcp::model::UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        self.listener.unsubscribe(&request.uri);
        Ok(())
    }

    /// `subscriptions/listen`: the resources of this server a client asked for, nothing else.
    fn accepted_subscription_filter(
        &self,
        requested: &rmcp::model::SubscriptionFilter,
    ) -> Option<rmcp::model::SubscriptionFilter> {
        let mut accepted = rmcp::model::SubscriptionFilter::default();
        accepted.resource_subscriptions = requested
            .resource_subscriptions
            .as_ref()
            .map(|uris| uris.iter().filter(|uri| activity::known(uri)).cloned().collect());
        Some(accepted)
    }

    async fn listen(&self, context: rmcp::service::SubscriptionContext) -> Result<(), McpError> {
        let subscribed = context.accepted().resource_subscriptions.clone().unwrap_or_default();
        let ended = context.request_context().ct.clone();
        activity::listen(subscribed, context.sink().clone(), ended).await;
        Ok(())
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

/// The tracks a range edit was told to work on, with their ids parsed: `None` is every unlocked track,
/// and a mistyped uuid is the caller's, and says which entry.
fn track_ids(list: Option<&[String]>) -> Result<Option<Vec<Uuid>>, McpError> {
    list.map(|list| list.iter().map(|s| parse_arg("track_ids[]", s)).collect())
        .transpose()
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
/// A contact sheet's grid as asked for, clamped: `skim_asset` and `skim_timeline` share it.
fn sheet_grid(columns: Option<u32>, rows: Option<u32>, cell_width: Option<u32>) -> (u32, u32, u32) {
    (
        columns.unwrap_or(4).clamp(1, 8),
        rows.unwrap_or(4).clamp(1, 8),
        cell_width.unwrap_or(240).clamp(80, 640),
    )
}

/// The text index of a sheet of the cut: each cell's timeline time and the clips on screen there.
fn timeline_sheet_caption(columns: u32, rows: u32, (start, end): (f64, f64), cells: &[kerf_core::SheetCell]) -> String {
    let index = cells
        .iter()
        .enumerate()
        .map(|(i, c)| format!("  cell {}: {}  {}", i + 1, fmt_ts(c.time), on_screen_clips(&c.clips)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "contact sheet {columns}x{rows} (row-major) of the cut {} - {}, cell -> timeline time, clips on screen (bottom first):\n{index}",
        fmt_ts(start),
        fmt_ts(end)
    )
}

/// The clips a cell shows, bottom first; a gap says so.
fn on_screen_clips(clips: &[kerf_core::SheetClip]) -> String {
    if clips.is_empty() {
        return "no clip (gap)".to_string();
    }
    clips
        .iter()
        .map(|c| format!("clip {} {:?}", c.clip_id, c.name))
        .collect::<Vec<_>>()
        .join(", ")
}

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
pub(super) fn require_window(start: f64, end: f64) -> Result<(), McpError> {
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

/// The part of an `export_variants` job's progress its smart-crop framing pass takes, when it runs.
const FRAMING_SHARE: f64 = 0.1;

/// What a running export says when it is not (yet) rendering (pure): the queue it waits in, or the
/// stage it is in — metering the mix, starting ffmpeg, finishing the file, measuring what it wrote.
/// `None` for the render itself, which each tool words with its own detail. Without this the tools
/// told every tick "rendering": a queued export, a metering pass and the seconds ffmpeg spends
/// opening the footage all read as a render stuck at 0.
fn export_wait_line(waiting: Option<&str>, stage: Option<kerf_core::ExportStage>) -> Option<String> {
    match (waiting, stage) {
        (Some(why), _) => Some(why.to_string()),
        (None, Some(stage)) if stage != kerf_core::ExportStage::Rendering => Some(stage.label().to_string()),
        _ => None,
    }
}

/// The progress line of `export`.
fn export_message(progress: &kerf_core::ExportProgress) -> String {
    // An export rendered in parts says which, in place of "rendering": "rendering part 2 of 5", "encoding".
    let stage = progress.part.map_or_else(|| "rendering".to_string(), |part| part.to_string());
    match export_wait_line(progress.waiting, progress.stage) {
        Some(line) => match progress.part {
            Some(part) if progress.waiting.is_none() => format!("{line} ({part})"),
            _ => line,
        },
        None => match progress.eta_secs {
            Some(eta) => format!("{stage}, {} to go", fmt_ts(eta)),
            None => stage,
        },
    }
}

/// The progress line of `export_variants`: which file of how many, or all of them at once.
fn variants_message(progress: &kerf_core::VariantProgress, labels: &[String]) -> String {
    if let Some(line) = export_wait_line(progress.waiting, progress.stage) {
        return match progress.part {
            Some(part) if progress.waiting.is_none() => format!("{line} ({part})"),
            _ => line,
        };
    }
    let mut message = if progress.together {
        format!("rendering {} together ({})", progress.total, labels.join(", "))
    } else {
        format!(
            "rendering {} of {} ({})",
            progress.variant + 1,
            progress.total,
            labels.get(progress.variant).cloned().unwrap_or_default()
        )
    };
    if let Some(part) = progress.part {
        message.push_str(&format!(", {part}"));
    }
    if let Some(eta) = progress.eta_secs {
        message.push_str(&format!(", {} to go", fmt_ts(eta)));
    }
    message
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
mod tests;
