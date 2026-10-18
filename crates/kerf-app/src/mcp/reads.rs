//! Reads shaped for a model's context window.
//!
//! An asset's analysis can be megabytes (every word of a transcript with its time, a chroma vector
//! per bar, thousands of onsets), a timeline with captions hundreds of overlays. The tools that
//! used to hand all of it back now answer a **summary** — counts, durations, the head of the text —
//! and name the paging tools (`get_transcript`, `get_analysis_detail`, `get_timeline_state`'s
//! caption window) that read the rest a slice at a time.

use std::collections::HashMap;

use kerf_core::{AnalysisKind, Asset, AssetAnalysis, ProxyPhase, ProxyStatus, TimeRange, Timeline, TranscriptSegment};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{schemars, tool, tool_router, ErrorData as McpError};
use serde::Serialize;

use super::{blocking, core_err, json, lock_agent_as, parse_arg, KerfMcp};

/// How much of a transcript a summary quotes.
const TRANSCRIPT_PREVIEW_CHARS: usize = 600;
/// How much of a voiceover's script an asset listing quotes.
const SCRIPT_PREVIEW_CHARS: usize = 400;
/// Segments / items per page when the caller does not say, and the most it may ask for.
const DEFAULT_PAGE: usize = 100;
const MAX_PAGE: usize = 500;
/// Items of a detail list per page (these are bare numbers, so more fit).
const DEFAULT_DETAIL_PAGE: usize = 500;
const MAX_DETAIL_PAGE: usize = 2000;
/// Generated captions `get_timeline_state` shows inline unless asked for more.
pub(super) const DEFAULT_CAPTION_WINDOW: usize = 20;
const MAX_CAPTION_WINDOW: usize = 500;

/// `s` cut to `max` characters, with an ellipsis when it was cut.
pub(super) fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    let cut: String = s.chars().take(max).collect();
    (format!("{}…", cut.trim_end()), true)
}

/// An asset as the listing shows it: a voiceover's per-sentence timings are left to the
/// transcript tools and its script is quoted, not repeated whole.
pub(super) fn compact_asset(asset: &Asset) -> Asset {
    let mut asset = asset.clone();
    if let Some(voiceover) = &mut asset.voiceover {
        voiceover.segments.clear();
        voiceover.text = truncate_chars(&voiceover.text, SCRIPT_PREVIEW_CHARS).0;
    }
    asset
}

// ---- proxy and analysis state, briefly ----------------------------------------

/// An asset's proxy for a listing: the state, and the numbers that go with it.
#[derive(Debug, Serialize, PartialEq)]
pub(super) struct ProxyBrief {
    /// `ready`, `building`, `queued`, `missing` (wanted, nothing making it), `failed`, `off`
    /// (proxies are off, previews use the original, or this one was deleted) or `not_needed`.
    state: ProxyPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    fraction: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    eta_secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl From<&ProxyStatus> for ProxyBrief {
    fn from(s: &ProxyStatus) -> Self {
        Self {
            state: s.state,
            fraction: s.fraction,
            eta_secs: s.eta_secs,
            reason: s.reason.clone(),
        }
    }
}

/// Whether previews of an asset decode a proxy: a ready one, or none being needed. Anything else
/// — queued, building, failed, missing, off — means the preview reads the original.
pub(super) fn proxy_is_settled(state: ProxyPhase) -> bool {
    matches!(state, ProxyPhase::Ready | ProxyPhase::NotNeeded)
}

/// Where each analysis kind stands, as `{"silence": "done", …}`, plus the reasons of the kinds
/// that failed. Always every kind, in pass order.
#[derive(Debug, Serialize, PartialEq)]
pub(super) struct AnalysisBrief {
    #[serde(flatten)]
    states: serde_json::Map<String, serde_json::Value>,
}

impl AnalysisBrief {
    pub(super) fn of(status: &kerf_core::AnalysisStatus) -> (Self, serde_json::Map<String, serde_json::Value>) {
        let mut states = serde_json::Map::new();
        let mut failures = serde_json::Map::new();
        for kind in &status.kinds {
            let state = serde_json::to_value(kind.state).unwrap_or(serde_json::Value::Null);
            states.insert(kind.kind.name().to_string(), state);
            if let Some(reason) = &kind.reason {
                failures.insert(kind.kind.name().to_string(), serde_json::Value::String(reason.clone()));
            }
        }
        (Self { states }, failures)
    }
}

/// One line of `list_assets`.
#[derive(Debug, Serialize)]
pub(super) struct AssetEntry {
    #[serde(flatten)]
    asset: Asset,
    /// The source file is no longer on disk: previews and exports of it fail until it is back
    /// (the user can relink it in Kerf).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    source_missing: bool,
    proxy: ProxyBrief,
    analysis: AnalysisBrief,
    /// Why a kind failed, by kind. Absent when none did.
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    analysis_failed: serde_json::Map<String, serde_json::Value>,
}

impl AssetEntry {
    pub(super) fn new(asset: &Asset, proxy: &ProxyStatus, status: &kerf_core::AnalysisStatus) -> Self {
        let (analysis, analysis_failed) = AnalysisBrief::of(status);
        Self {
            asset: compact_asset(asset),
            source_missing: !std::path::Path::new(&asset.path).exists(),
            proxy: proxy.into(),
            analysis,
            analysis_failed,
        }
    }
}

// ---- an analysis, summarised --------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
struct SilenceSummary {
    segments: usize,
    total_secs: f64,
    longest_secs: f64,
}

#[derive(Debug, Serialize, PartialEq)]
struct TranscriptSummary {
    segments: usize,
    words: usize,
    /// The start of the first segment and the end of the last, in the asset's own time.
    from: f64,
    to: f64,
    /// The beginning of the text; `truncated` says whether there is more (`get_transcript`).
    text: String,
    truncated: bool,
}

#[derive(Debug, Serialize, PartialEq)]
struct RhythmSummary {
    onsets: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    bpm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    beats: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tempo_confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio_class: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bars: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phrases: Option<usize>,
}

/// What an analysis found, without the lists: counts, durations and the head of the transcript.
/// Only the kinds that are done appear.
#[derive(Debug, Serialize)]
pub(super) struct AnalysisSummary {
    asset_id: uuid::Uuid,
    /// The kinds that have run.
    done: Vec<AnalysisKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    silence: Option<SilenceSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scene_changes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    loudness: Option<kerf_core::model::Loudness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rhythm: Option<RhythmSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript: Option<TranscriptSummary>,
    /// Where the full lists are.
    read_more: &'static str,
}

const READ_MORE: &str = "get_transcript (a slice of the transcript), get_analysis_detail (silence / scenes / onsets / beats / bars / phrases), get_music_structure (bars and phrases for fit_music)";

impl AnalysisSummary {
    pub(super) fn of(analysis: &AssetAnalysis) -> Self {
        let silence = analysis.done(AnalysisKind::Silence).then(|| SilenceSummary {
            segments: analysis.silence_segments.len(),
            total_secs: analysis.silence_segments.iter().map(|s| (s.end - s.start).max(0.0)).sum(),
            longest_secs: analysis
                .silence_segments
                .iter()
                .map(|s| (s.end - s.start).max(0.0))
                .fold(0.0, f64::max),
        });
        let rhythm = analysis.done(AnalysisKind::Rhythm).then(|| RhythmSummary {
            onsets: analysis.onsets.len(),
            bpm: analysis.tempo.as_ref().map(|t| t.bpm),
            beats: analysis.tempo.as_ref().map(|t| t.beats.len()),
            tempo_confidence: analysis.tempo.as_ref().map(|t| t.confidence),
            audio_class: analysis.audio_class.and_then(|c| serde_json::to_value(c).ok()),
            bars: analysis.music.as_ref().map(|m| m.bar_chroma.len()),
            phrases: analysis.music.as_ref().map(|m| m.phrases.len()),
        });
        let transcript = analysis.done(AnalysisKind::Transcript).then(|| {
            let joined = analysis
                .transcript
                .iter()
                .map(|s| s.text.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let (text, truncated) = truncate_chars(&joined, TRANSCRIPT_PREVIEW_CHARS);
            TranscriptSummary {
                segments: analysis.transcript.len(),
                words: analysis.transcript.iter().map(|s| s.text.split_whitespace().count()).sum(),
                from: analysis.transcript.first().map_or(0.0, |s| s.start),
                to: analysis.transcript.last().map_or(0.0, |s| s.end),
                text,
                truncated,
            }
        });
        Self {
            asset_id: analysis.asset_id,
            done: analysis.done_kinds(),
            silence,
            scene_changes: analysis.done(AnalysisKind::Scenes).then_some(analysis.scene_changes.len()),
            loudness: analysis.done(AnalysisKind::Loudness).then_some(analysis.loudness).flatten(),
            rhythm,
            transcript,
            read_more: READ_MORE,
        }
    }
}

// ---- paging ------------------------------------------------------------------

/// The page a caller asked for, with the defaults and the cap applied.
pub(super) fn page_of(offset: Option<usize>, limit: Option<usize>, default: usize, max: usize) -> (usize, usize) {
    (offset.unwrap_or(0), limit.unwrap_or(default).clamp(1, max))
}

/// A window over an ordered list: how many matched in all, where this slice starts, and where the
/// next begins when there is more.
#[derive(Debug, Serialize, PartialEq)]
pub(super) struct Paged<T> {
    total: usize,
    offset: usize,
    returned: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_offset: Option<usize>,
    items: Vec<T>,
}

pub(super) fn paged<T: Clone>(matching: &[T], offset: usize, limit: usize) -> Paged<T> {
    let items: Vec<T> = matching.iter().skip(offset).take(limit).cloned().collect();
    let end = offset.saturating_add(items.len());
    Paged {
        total: matching.len(),
        offset,
        returned: items.len(),
        next_offset: (end < matching.len()).then_some(end),
        items,
    }
}

/// The transcript segments overlapping `[from, to)` (either end open).
pub(super) fn transcript_window(analysis: &AssetAnalysis, from: Option<f64>, to: Option<f64>) -> Vec<TranscriptSegment> {
    analysis
        .transcript
        .iter()
        .filter(|s| from.is_none_or(|f| s.end > f) && to.is_none_or(|t| s.start < t))
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Copy, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum DetailKind {
    /// Silent spans, `{start, end}`.
    Silence,
    /// Scene-change times.
    Scenes,
    /// Onset (transient) times.
    Onsets,
    /// Beat times.
    Beats,
    /// Bar-start times of the fitted bar grid, which is what a downbeat is.
    Bars,
    /// Repeating phrases, `{a, b, bars}` (bar indices).
    Phrases,
}

/// One page of an analysis list, windowed to `[from, to)` in the asset's own time.
pub(super) fn analysis_detail(
    analysis: &AssetAnalysis,
    kind: DetailKind,
    from: Option<f64>,
    to: Option<f64>,
    offset: usize,
    limit: usize,
) -> serde_json::Value {
    let inside = |t: f64| from.is_none_or(|f| t >= f) && to.is_none_or(|e| t < e);
    let times = |all: &[f64]| -> serde_json::Value {
        let matching: Vec<f64> = all.iter().copied().filter(|t| inside(*t)).collect();
        serde_json::to_value(paged(&matching, offset, limit)).unwrap_or_default()
    };
    match kind {
        DetailKind::Silence => {
            let matching: Vec<TimeRange> = analysis
                .silence_segments
                .iter()
                .filter(|s| from.is_none_or(|f| s.end > f) && to.is_none_or(|e| s.start < e))
                .copied()
                .collect();
            serde_json::to_value(paged(&matching, offset, limit)).unwrap_or_default()
        }
        DetailKind::Scenes => times(&analysis.scene_changes),
        DetailKind::Onsets => times(&analysis.onsets),
        DetailKind::Beats => times(analysis.tempo.as_ref().map_or(&[][..], |t| &t.beats)),
        DetailKind::Bars => {
            let bars: Vec<f64> = analysis
                .music
                .as_ref()
                .map(|m| (0..m.bar_chroma.len()).map(|k| m.grid.bar_start(k)).collect())
                .unwrap_or_default();
            times(&bars)
        }
        DetailKind::Phrases => {
            let phrases = analysis.music.as_ref().map_or(Vec::new(), |m| m.phrases.clone());
            serde_json::to_value(paged(&phrases, offset, limit)).unwrap_or_default()
        }
    }
}

// ---- timelines ---------------------------------------------------------------

/// A timeline in a few lines: the shape of the cut and how much is on it, for the tools that used
/// to answer with the whole thing after changing it.
pub(super) fn timeline_brief(timeline: &Timeline) -> serde_json::Value {
    let tracks: Vec<serde_json::Value> = timeline
        .tracks
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "name": t.name,
                "kind": format!("{:?}", t.kind).to_lowercase(),
                "clips": t.clips.len(),
                "duration_secs": t.end(),
            })
        })
        .collect();
    let mut brief = serde_json::json!({
        "total_duration_secs": timeline.duration(),
        "tracks": tracks,
        "total_clips": timeline.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
        "overlays": timeline.overlays.len(),
        "svg_overlays": timeline.svg_overlays.len(),
        "markers": timeline.markers.len(),
        "see": "get_timeline_state for every clip, timeline_summary for gaps and proxies",
    });
    if timeline.overlays.iter().any(|o| o.generated) {
        brief["captions"] = serde_json::json!(timeline.overlays.iter().filter(|o| o.generated).count());
    }
    brief
}

/// One medium the cut uses that is not in a good state for previewing.
#[derive(Debug, Serialize, PartialEq)]
pub(super) struct MediaNote {
    asset_id: uuid::Uuid,
    name: String,
    /// How many clips of the cut are cut from it.
    clips: usize,
    /// Its proxy's state when that is not `ready` / `not_needed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    proxy: Option<ProxyBrief>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    source_missing: bool,
}

pub(super) struct MediaNotes {
    /// Clips per track (in track order) cut from media with no ready proxy.
    pub per_track: Vec<usize>,
    pub notes: Vec<MediaNote>,
}

/// What of the media `timeline` uses has no ready proxy, or no source file: a note per asset and
/// the clip count per track. Pure: `proxies` and `missing` are read off the disk by the caller.
pub(super) fn media_notes(
    timeline: &Timeline,
    assets: &[Asset],
    proxies: &HashMap<uuid::Uuid, ProxyStatus>,
    missing: &std::collections::HashSet<uuid::Uuid>,
) -> MediaNotes {
    let unready = |id: &uuid::Uuid| proxies.get(id).is_some_and(|p| !proxy_is_settled(p.state));
    let per_track = timeline
        .tracks
        .iter()
        .map(|t| t.clips.iter().filter(|c| unready(&c.asset_id)).count())
        .collect();
    let mut notes = Vec::new();
    for asset in assets {
        let clips = timeline
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.asset_id == asset.id)
            .count();
        let proxy = proxies.get(&asset.id).filter(|p| !proxy_is_settled(p.state));
        let source_missing = missing.contains(&asset.id);
        if clips > 0 && (proxy.is_some() || source_missing) {
            notes.push(MediaNote {
                asset_id: asset.id,
                name: asset.name.clone(),
                clips,
                proxy: proxy.map(ProxyBrief::from),
                source_missing,
            });
        }
    }
    MediaNotes { per_track, notes }
}

/// A proposal in a few lines: how big it is, what it was told to be, how far back undo reaches and
/// whether the cut moved under it. `staged_diff` has the entries.
pub(super) fn staged_brief(staged: &kerf_core::StagedEdit) -> serde_json::Value {
    serde_json::json!({
        "headline": staged.diff.headline(),
        "changes": staged.diff.entries.len(),
        "asset_changes": staged.asset_changes.len(),
        "note": staged.note,
        "task_id": staged.task_id,
        "edits": staged.edits,
        "undo_depth": staged.undo_depth,
        "stale": staged.stale,
    })
}

/// What `generate_captions` answers: how many captions it wrote, the first few whole (so the look
/// can be judged), and the ids of the first hundred.
pub(super) fn caption_result(written: &[kerf_core::TextOverlay]) -> serde_json::Value {
    const FIRST: usize = 5;
    const IDS: usize = 100;
    serde_json::json!({
        "count": written.len(),
        "first": written.iter().take(FIRST).collect::<Vec<_>>(),
        "ids": written.iter().take(IDS).map(|o| o.id).collect::<Vec<_>>(),
        "ids_omitted": written.len().saturating_sub(IDS),
    })
}

/// How `get_timeline_state` is asked to show a timeline.
pub(super) struct StateView {
    /// Every SVG source whole, and every caption inline.
    pub full: bool,
    pub captions_offset: Option<usize>,
    pub captions_limit: Option<usize>,
}

/// The timeline as `get_timeline_state` answers: SVG sources over the inline limit replaced by a
/// note (`super::elide_large_svg`, applied by the caller), and the **generated captions** — often
/// hundreds of overlays — moved out of `overlays` into a window of their own, unless the caller
/// asked for `full`. Typed titles stay in `overlays`.
pub(super) fn timeline_state(timeline: &Timeline, view: &StateView) -> Result<serde_json::Value, McpError> {
    let mut value = serde_json::to_value(timeline).map_err(|e| McpError::internal_error(e.to_string(), None))?;
    if view.full {
        return Ok(value);
    }
    let generated: Vec<serde_json::Value> = timeline
        .overlays
        .iter()
        .filter(|o| o.generated)
        .map(|o| serde_json::to_value(o).unwrap_or_default())
        .collect();
    if generated.is_empty() {
        return Ok(value);
    }
    let (offset, limit) = page_of(
        view.captions_offset,
        view.captions_limit,
        DEFAULT_CAPTION_WINDOW,
        MAX_CAPTION_WINDOW,
    );
    let window = paged(&generated, offset, limit);
    let typed: Vec<serde_json::Value> = timeline
        .overlays
        .iter()
        .filter(|o| !o.generated)
        .map(|o| serde_json::to_value(o).unwrap_or_default())
        .collect();
    if let Some(map) = value.as_object_mut() {
        if typed.is_empty() {
            map.remove("overlays");
        } else {
            map.insert("overlays".to_string(), serde_json::Value::Array(typed));
        }
        let mut captions = serde_json::to_value(&window).unwrap_or_default();
        if let Some(c) = captions.as_object_mut() {
            c.insert(
                "note".to_string(),
                serde_json::Value::String(
                    "generated captions, windowed: pass captions_offset / captions_limit for another slice, or full=true for all"
                        .to_string(),
                ),
            );
        }
        map.insert("captions".to_string(), captions);
    }
    Ok(value)
}

// ---- tools --------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct AssetOnly {
    #[schemars(description = "UUID of the asset")]
    asset_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct TranscriptParams {
    #[schemars(description = "UUID of the asset (it must have been transcribed: analyze_asset with `transcript`)")]
    asset_id: String,
    #[schemars(
        description = "Only segments that end after this time, in SOURCE seconds (the asset's own clock). Omit for the start."
    )]
    from: Option<f64>,
    #[schemars(description = "Only segments that start before this time, in SOURCE seconds. Omit for the end.")]
    to: Option<f64>,
    #[schemars(
        description = "Skip this many matching segments (for paging; default 0). `next_offset` in the answer is the next page."
    )]
    offset: Option<usize>,
    #[schemars(description = "Most segments to return (default 100, at most 500)")]
    limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct AnalysisDetailParams {
    #[schemars(description = "UUID of the asset (analyzed: analyze_asset)")]
    asset_id: String,
    #[schemars(description = "Which list: silence, scenes, onsets, beats, bars or phrases")]
    kind: DetailKind,
    #[schemars(description = "Only items at or after this time, in SOURCE seconds (a span counts when it ends after it)")]
    from: Option<f64>,
    #[schemars(description = "Only items before this time, in SOURCE seconds (a span counts when it starts before it)")]
    to: Option<f64>,
    #[schemars(
        description = "Skip this many matching items (for paging; default 0). `next_offset` in the answer is the next page."
    )]
    offset: Option<usize>,
    #[schemars(description = "Most items to return (default 500, at most 2000)")]
    limit: Option<usize>,
}

/// Normalise a window the caller gave: `from` after `to` is a mistake worth saying so.
fn check_window(from: Option<f64>, to: Option<f64>) -> Result<(), McpError> {
    if from.is_some_and(|f| !f.is_finite()) || to.is_some_and(|t| !t.is_finite()) {
        return Err(McpError::invalid_params(
            "`from` and `to` must be finite numbers of seconds",
            None,
        ));
    }
    if let (Some(f), Some(t)) = (from, to) {
        if t <= f {
            return Err(McpError::invalid_params(
                format!("`to` must be greater than `from`, got from={f}, to={t}"),
                None,
            ));
        }
    }
    Ok(())
}

#[tool_router(router = tool_router_reads, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "List the media assets in the project. Each carries its probed streams, `source_missing` (the \
                       file is gone from disk — set only when true), `proxy` (the preview proxy: `state` is ready, \
                       building with `fraction` and `eta_secs`, queued, missing, failed with a `reason`, off, or \
                       not_needed; until it is ready previews decode the original and are slower — export is not \
                       affected) and `analysis` (per kind — silence, scenes, loudness, rhythm, transcript — done, \
                       not_run, running, failed or off; `analysis_failed` says why). proxy_status and analysis_status \
                       give the same per asset in more detail; get_asset_metadata summarises what the analysis found."
    )]
    async fn list_assets(&self) -> Result<String, McpError> {
        let (project, owner) = (self.project.clone(), self.owner());
        let entries = blocking(move || {
            // Everything that is the project's, under the lock; the disk (the proxies' cached
            // ffprobe, a stat per source) with it released.
            let (inputs, statuses) = {
                let project = lock_agent_as(&project, owner.as_deref());
                (
                    project.proxy_inputs().map_err(core_err)?,
                    project.analysis_statuses().map_err(core_err)?,
                )
            };
            let proxies: HashMap<_, _> = kerf_core::proxy::statuses(&inputs)
                .into_iter()
                .map(|s| (s.asset_id, s))
                .collect();
            let statuses: HashMap<_, _> = statuses.into_iter().map(|s| (s.asset_id, s)).collect();
            Ok(inputs
                .iter()
                .filter_map(|input| {
                    let id = input.asset.id;
                    Some(AssetEntry::new(&input.asset, proxies.get(&id)?, statuses.get(&id)?))
                })
                .collect::<Vec<_>>())
        })
        .await?;
        json(&entries)
    }

    #[tool(
        description = "Get an asset's probed metadata and a SUMMARY of its analysis: for each kind that has run, the \
                       counts and totals (silent spans and how long, scene changes, loudness in LUFS, onsets, tempo, \
                       speech/music class, bars) and the first ~600 characters of the transcript. The lists themselves \
                       are not here — page them with get_transcript and get_analysis_detail (get_music_structure for \
                       bars and phrases). Also `analysis_status` (per kind: done / not_run / running / failed with a \
                       reason / off), `proxy` (ready, building with a fraction, queued, failed with a reason, off or \
                       not_needed) and `source_missing`."
    )]
    async fn get_asset_metadata(&self, Parameters(p): Parameters<AssetOnly>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        let (project, owner) = (self.project.clone(), self.owner());
        let meta = blocking(move || {
            let (input, analysis) = {
                let project = lock_agent_as(&project, owner.as_deref());
                (
                    project.proxy_input(id).map_err(core_err)?,
                    project.get_analysis(id).map_err(core_err)?,
                )
            };
            // The proxy's state touches the disk (and, once per file, a cached ffprobe): lock released.
            let proxy = kerf_core::proxy::status(&input);
            let source_missing = !std::path::Path::new(&input.asset.path).exists();
            Ok(serde_json::json!({
                "asset": compact_asset(&input.asset),
                "source_missing": source_missing,
                "analysis": analysis.as_ref().map(AnalysisSummary::of),
                "analysis_status": kerf_core::analysis_status(id, analysis.as_ref()),
                "proxy": ProxyBrief::from(&proxy),
            }))
        })
        .await?;
        json(&meta)
    }

    #[tool(
        description = "Read an asset's transcript a slice at a time: the segments `{start, end, text}` (SOURCE seconds) \
                       overlapping [from, to), at most `limit` (default 100) from `offset`, with `total` and \
                       `next_offset` for the next page. The transcript must exist (analyze_asset with `transcript`; \
                       a generated voiceover's is its script). Use this instead of reading the whole transcript when \
                       you need a passage: get_asset_metadata shows how long it is."
    )]
    fn get_transcript(&self, Parameters(p): Parameters<TranscriptParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        check_window(p.from, p.to)?;
        let (offset, limit) = page_of(p.offset, p.limit, DEFAULT_PAGE, MAX_PAGE);
        let analysis = self.lock().get_analysis(id).map_err(core_err)?;
        let Some(analysis) = analysis.filter(|a| a.done(AnalysisKind::Transcript)) else {
            return Err(McpError::invalid_params(
                "this asset has no transcript yet — call analyze_asset with steps [\"transcript\"] first",
                None,
            ));
        };
        let window = transcript_window(&analysis, p.from, p.to);
        let mut answer =
            serde_json::to_value(paged(&window, offset, limit)).map_err(|e| McpError::internal_error(e.to_string(), None))?;
        if let Some(map) = answer.as_object_mut() {
            map.insert("asset_id".to_string(), serde_json::json!(id));
            map.insert("segments".to_string(), map.get("items").cloned().unwrap_or_default());
            map.remove("items");
        }
        json(&answer)
    }

    #[tool(
        description = "Read one list of an asset's analysis a page at a time, in SOURCE seconds: `silence` (spans \
                       {start, end}), `scenes` (scene-change times), `onsets`, `beats`, `bars` (bar-start times of the \
                       fitted grid) or `phrases` (repeating bar ranges {a, b, bars}). Windowed to [from, to) and paged \
                       with offset / limit (default 500, at most 2000); the answer has `total`, `returned` and \
                       `next_offset`. Run analyze_asset for the kind first; get_asset_metadata says what has run and how \
                       many items there are."
    )]
    fn get_analysis_detail(&self, Parameters(p): Parameters<AnalysisDetailParams>) -> Result<String, McpError> {
        let id = parse_arg("asset_id", &p.asset_id)?;
        check_window(p.from, p.to)?;
        let (offset, limit) = page_of(p.offset, p.limit, DEFAULT_DETAIL_PAGE, MAX_DETAIL_PAGE);
        let analysis = self.lock().get_analysis(id).map_err(core_err)?;
        let Some(analysis) = analysis else {
            return Err(McpError::invalid_params(
                "this asset has not been analyzed yet — call analyze_asset first",
                None,
            ));
        };
        let mut answer = analysis_detail(&analysis, p.kind, p.from, p.to, offset, limit);
        if let Some(map) = answer.as_object_mut() {
            map.insert("asset_id".to_string(), serde_json::json!(id));
            map.insert("kind".to_string(), serde_json::json!(format!("{:?}", p.kind).to_lowercase()));
        }
        json(&answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerf_core::model::{AudioClass, AudioClassification, Loudness, Tempo};
    use uuid::Uuid;

    fn seg(start: f64, end: f64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            start,
            end,
            text: text.to_string(),
        }
    }

    fn analysis() -> AssetAnalysis {
        AssetAnalysis {
            asset_id: Uuid::nil(),
            silence_segments: vec![TimeRange { start: 1.0, end: 2.5 }, TimeRange { start: 10.0, end: 10.2 }],
            scene_changes: vec![3.0, 6.5, 9.0],
            transcript: vec![
                seg(0.0, 2.0, "Hello there"),
                seg(2.0, 5.0, "this is a test"),
                seg(8.0, 9.0, "the end"),
            ],
            loudness: Some(Loudness {
                integrated_lufs: -16.0,
                loudness_range: 5.0,
                true_peak_dbtp: -2.0,
                threshold_lufs: -26.0,
            }),
            onsets: vec![0.5, 1.0, 1.5, 2.0],
            tempo: Some(Tempo {
                bpm: 120.0,
                beats: vec![0.0, 0.5, 1.0],
                confidence: 0.9,
                downbeats: vec![],
            }),
            audio_class: Some(AudioClassification {
                class: AudioClass::Speech,
                confidence: 0.8,
            }),
            music: None,
            ran: AnalysisKind::ALL.to_vec(),
        }
    }

    #[test]
    fn a_summary_counts_instead_of_listing() {
        let s = AnalysisSummary::of(&analysis());
        let silence = s.silence.as_ref().unwrap();
        assert_eq!(silence.segments, 2);
        assert!((silence.total_secs - 1.7).abs() < 1e-9, "{}", silence.total_secs);
        assert_eq!(silence.longest_secs, 1.5);
        assert_eq!(s.scene_changes, Some(3));
        let t = s.transcript.as_ref().unwrap();
        assert_eq!((t.segments, t.words), (3, 8));
        assert_eq!(t.text, "Hello there this is a test the end");
        assert!(!t.truncated);
        assert_eq!((t.from, t.to), (0.0, 9.0));
        let r = s.rhythm.as_ref().unwrap();
        assert_eq!((r.onsets, r.beats), (4, Some(3)));
        assert_eq!(r.bpm, Some(120.0));
        let text = serde_json::to_string(&s).unwrap();
        assert!(
            !text.contains("scene_changes\":[") && !text.contains("onsets\":["),
            "no lists in {text}"
        );
        assert!(text.contains("get_transcript") && text.contains("get_analysis_detail"));
    }

    #[test]
    fn a_long_transcript_is_quoted_and_marked_cut() {
        let mut a = analysis();
        a.transcript = (0..400).map(|i| seg(i as f64, i as f64 + 1.0, "word word word")).collect();
        let s = AnalysisSummary::of(&a);
        let t = s.transcript.unwrap();
        assert!(t.truncated);
        assert!(
            t.text.chars().count() <= TRANSCRIPT_PREVIEW_CHARS + 1,
            "{}",
            t.text.chars().count()
        );
        assert!(t.text.ends_with('…'));
        assert_eq!((t.segments, t.words), (400, 1200));
    }

    #[test]
    fn only_the_kinds_that_ran_are_summarised() {
        let a = AssetAnalysis {
            asset_id: Uuid::nil(),
            ran: vec![AnalysisKind::Silence],
            ..AssetAnalysis::default()
        };
        let s = AnalysisSummary::of(&a);
        assert_eq!(s.done, vec![AnalysisKind::Silence]);
        assert!(s.silence.is_some());
        assert!(s.transcript.is_none() && s.rhythm.is_none() && s.loudness.is_none() && s.scene_changes.is_none());
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate_chars("héllo", 10), ("héllo".to_string(), false));
        assert_eq!(truncate_chars("héllo wörld", 5), ("héllo…".to_string(), true));
    }

    #[test]
    fn a_page_says_where_the_next_one_starts() {
        let all: Vec<u32> = (0..10).collect();
        let first = paged(&all, 0, 4);
        assert_eq!((first.total, first.returned, first.next_offset), (10, 4, Some(4)));
        assert_eq!(first.items, vec![0, 1, 2, 3]);
        let last = paged(&all, 8, 4);
        assert_eq!((last.returned, last.next_offset), (2, None));
        let past = paged(&all, 50, 4);
        assert_eq!((past.total, past.returned, past.next_offset), (10, 0, None));
        assert_eq!(page_of(None, None, 100, 500), (0, 100));
        assert_eq!(page_of(Some(3), Some(9999), 100, 500), (3, 500));
        assert_eq!(
            page_of(None, Some(0), 100, 500),
            (0, 1),
            "a limit of 0 still returns something"
        );
    }

    #[test]
    fn the_transcript_window_keeps_what_overlaps() {
        let a = analysis();
        let texts = |w: Vec<TranscriptSegment>| w.into_iter().map(|s| s.text).collect::<Vec<_>>();
        assert_eq!(texts(transcript_window(&a, None, None)).len(), 3);
        assert_eq!(texts(transcript_window(&a, Some(2.0), Some(8.0))), vec!["this is a test"]);
        assert_eq!(
            texts(transcript_window(&a, Some(1.0), None)),
            vec!["Hello there", "this is a test", "the end"]
        );
        assert_eq!(texts(transcript_window(&a, None, Some(2.0))), vec!["Hello there"]);
        assert!(transcript_window(&a, Some(50.0), None).is_empty());
    }

    #[test]
    fn detail_lists_are_windowed_and_paged() {
        let a = analysis();
        let v = analysis_detail(&a, DetailKind::Scenes, Some(3.0), Some(9.0), 0, 10);
        assert_eq!(v["items"], serde_json::json!([3.0, 6.5]));
        assert_eq!(v["total"], 2);
        let v = analysis_detail(&a, DetailKind::Onsets, None, None, 1, 2);
        assert_eq!(v["items"], serde_json::json!([1.0, 1.5]));
        assert_eq!(v["next_offset"], 3);
        let v = analysis_detail(&a, DetailKind::Silence, Some(2.0), None, 0, 10);
        assert_eq!(v["total"], 2, "a span counts when it ends after `from`");
        assert_eq!(v["items"][0], serde_json::json!({"start": 1.0, "end": 2.5}));
        let v = analysis_detail(&a, DetailKind::Beats, None, None, 0, 10);
        assert_eq!(v["items"], serde_json::json!([0.0, 0.5, 1.0]));
        let v = analysis_detail(&a, DetailKind::Bars, None, None, 0, 10);
        assert_eq!(v["total"], 0, "no bar grid is an empty list, not an error");
    }

    #[test]
    fn a_backwards_window_is_refused() {
        assert!(check_window(Some(5.0), Some(2.0)).is_err());
        assert!(check_window(Some(2.0), Some(2.0)).is_err());
        assert!(check_window(Some(f64::NAN), None).is_err());
        assert!(check_window(Some(1.0), None).is_ok());
        assert!(check_window(None, None).is_ok());
    }

    fn captioned(n: usize) -> Timeline {
        let mut t = Timeline::new();
        t.overlays.push(kerf_core::TextOverlay::new("Title", 0.0, 3.0));
        for i in 0..n {
            let mut o = kerf_core::TextOverlay::new(format!("caption {i}"), i as f64, i as f64 + 1.0);
            o.generated = true;
            t.overlays.push(o);
        }
        t
    }

    #[test]
    fn captions_leave_the_timeline_state_in_a_window_of_their_own() {
        let view = StateView {
            full: false,
            captions_offset: None,
            captions_limit: None,
        };
        let state = timeline_state(&captioned(300), &view).unwrap();
        assert_eq!(state["overlays"].as_array().unwrap().len(), 1, "typed titles stay");
        assert_eq!(state["overlays"][0]["text"], "Title");
        assert_eq!(state["captions"]["total"], 300);
        assert_eq!(state["captions"]["returned"], DEFAULT_CAPTION_WINDOW);
        assert_eq!(state["captions"]["next_offset"], DEFAULT_CAPTION_WINDOW);
        assert_eq!(state["captions"]["items"][0]["text"], "caption 0");

        let page = StateView {
            captions_offset: Some(295),
            captions_limit: Some(50),
            ..view
        };
        let state = timeline_state(&captioned(300), &page).unwrap();
        assert_eq!(state["captions"]["returned"], 5);
        assert!(state["captions"].get("next_offset").is_none());
        assert_eq!(state["captions"]["items"][0]["text"], "caption 295");
    }

    #[test]
    fn full_gives_back_every_overlay_inline_and_a_captionless_cut_is_untouched() {
        let full = StateView {
            full: true,
            captions_offset: None,
            captions_limit: None,
        };
        let state = timeline_state(&captioned(300), &full).unwrap();
        assert_eq!(state["overlays"].as_array().unwrap().len(), 301);
        assert!(state.get("captions").is_none());

        let plain = timeline_state(&Timeline::new(), &StateView { full: false, ..full }).unwrap();
        assert!(plain.get("captions").is_none() && plain.get("overlays").is_none());
    }

    #[test]
    fn a_brief_describes_the_shape_not_the_clips() {
        let brief = timeline_brief(&captioned(7));
        assert_eq!(brief["tracks"].as_array().unwrap().len(), 2);
        assert_eq!(brief["overlays"], 8);
        assert_eq!(brief["captions"], 7);
        assert_eq!(brief["total_clips"], 0);
        assert!(brief["see"].as_str().unwrap().contains("get_timeline_state"));
    }

    #[test]
    fn a_listing_quotes_a_voiceover_script_and_drops_its_timings() {
        let asset: Asset = serde_json::from_value(serde_json::json!({
            "id": Uuid::nil(),
            "path": "/nope",
            "name": "vo",
            "duration": 1.0,
            "streams": [],
            "imported_at": "2025-01-01T00:00:00Z",
            "voiceover": {
                "text": "word ".repeat(500),
                "voice": "af_heart",
                "speed": 1.0,
                "segments": vec![serde_json::json!({"start": 0.0, "end": 1.0, "text": "x"}); 50],
            },
        }))
        .unwrap();
        let compact = compact_asset(&asset);
        let vo = compact.voiceover.unwrap();
        assert!(vo.segments.is_empty());
        assert!(vo.text.chars().count() <= SCRIPT_PREVIEW_CHARS + 1);
        assert_eq!(asset.voiceover.unwrap().segments.len(), 50, "the original is untouched");
    }

    #[test]
    fn the_analysis_brief_lists_every_kind_and_the_failures() {
        use kerf_core::{AnalysisKindStatus, AnalysisState, AnalysisStatus};
        let status = AnalysisStatus {
            asset_id: Uuid::nil(),
            kinds: vec![
                AnalysisKindStatus {
                    kind: AnalysisKind::Silence,
                    state: AnalysisState::Done,
                    reason: None,
                },
                AnalysisKindStatus {
                    kind: AnalysisKind::Transcript,
                    state: AnalysisState::Failed,
                    reason: Some("no model".to_string()),
                },
            ],
        };
        let (brief, failed) = AnalysisBrief::of(&status);
        let value = serde_json::to_value(&brief).unwrap();
        assert_eq!(value["silence"], "done");
        assert_eq!(value["transcript"], "failed");
        assert_eq!(failed["transcript"], "no model");
        assert!(failed.get("silence").is_none());
    }

    #[test]
    fn a_proxy_is_settled_when_ready_or_not_needed() {
        assert!(proxy_is_settled(ProxyPhase::Ready));
        assert!(proxy_is_settled(ProxyPhase::NotNeeded));
        for pending in [
            ProxyPhase::Queued,
            ProxyPhase::Building,
            ProxyPhase::Missing,
            ProxyPhase::Failed,
            ProxyPhase::Off,
        ] {
            assert!(!proxy_is_settled(pending), "{pending:?}");
        }
    }

    fn clip_of(asset: Uuid) -> kerf_core::Clip {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4(),
            "asset_id": asset,
            "source_in": 0.0,
            "source_out": 2.0,
            "timeline_start": 0.0,
            "volume": 1.0,
            "fade_in": 0.0,
            "fade_out": 0.0,
        }))
        .unwrap()
    }

    fn asset_named(id: Uuid, name: &str) -> Asset {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "path": "/nope.mp4",
            "name": name,
            "duration": 2.0,
            "streams": [],
            "imported_at": "2025-01-01T00:00:00Z",
        }))
        .unwrap()
    }

    fn proxy(asset: Uuid, state: ProxyPhase) -> ProxyStatus {
        serde_json::from_value(serde_json::json!({"asset_id": asset, "state": state})).unwrap()
    }

    #[test]
    fn a_cut_names_the_media_whose_proxy_is_not_ready() {
        let (ready, building, missing_src) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut timeline = Timeline::new();
        timeline.tracks[0].clips = vec![clip_of(ready), clip_of(building), clip_of(building)];
        timeline.tracks[1].clips = vec![clip_of(missing_src)];
        let assets = vec![
            asset_named(ready, "ready.mp4"),
            asset_named(building, "building.mp4"),
            asset_named(missing_src, "gone.mp4"),
        ];
        let proxies: HashMap<_, _> = [
            (ready, proxy(ready, ProxyPhase::Ready)),
            (building, proxy(building, ProxyPhase::Building)),
            (missing_src, proxy(missing_src, ProxyPhase::NotNeeded)),
        ]
        .into_iter()
        .collect();
        let missing: std::collections::HashSet<Uuid> = [missing_src].into_iter().collect();
        let notes = media_notes(&timeline, &assets, &proxies, &missing);
        assert_eq!(notes.per_track, vec![2, 0], "two clips on V1 wait for a proxy");
        let names: Vec<&str> = notes.notes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["building.mp4", "gone.mp4"], "the ready one is not mentioned");
        let building_note = &notes.notes[0];
        assert_eq!(building_note.clips, 2);
        assert!(building_note.proxy.is_some() && !building_note.source_missing);
        let gone = &notes.notes[1];
        assert!(
            gone.source_missing && gone.proxy.is_none(),
            "a missing source is said even when no proxy is needed"
        );
        let text = serde_json::to_string(&notes.notes).unwrap();
        assert!(text.contains("\"building\"") && text.contains("source_missing"), "{text}");
    }

    #[test]
    fn a_cut_with_everything_ready_has_no_notes() {
        let id = Uuid::new_v4();
        let mut timeline = Timeline::new();
        timeline.tracks[0].clips = vec![clip_of(id)];
        let proxies: HashMap<_, _> = [(id, proxy(id, ProxyPhase::Ready))].into_iter().collect();
        let notes = media_notes(&timeline, &[asset_named(id, "a.mp4")], &proxies, &Default::default());
        assert!(notes.notes.is_empty());
        assert_eq!(notes.per_track, vec![0, 0]);
    }

    #[test]
    fn a_proxy_that_is_off_or_failed_counts_as_not_ready() {
        for state in [ProxyPhase::Off, ProxyPhase::Failed, ProxyPhase::Missing, ProxyPhase::Queued] {
            let id = Uuid::new_v4();
            let mut timeline = Timeline::new();
            timeline.tracks[0].clips = vec![clip_of(id)];
            let proxies: HashMap<_, _> = [(id, proxy(id, state))].into_iter().collect();
            let notes = media_notes(&timeline, &[asset_named(id, "a.mp4")], &proxies, &Default::default());
            assert_eq!(notes.per_track[0], 1, "{state:?}");
            assert_eq!(notes.notes.len(), 1, "{state:?}");
        }
    }

    #[test]
    fn a_listing_entry_says_proxy_analysis_and_missing_source() {
        let id = Uuid::new_v4();
        let asset = asset_named(id, "a.mp4");
        let mut status = proxy(id, ProxyPhase::Building);
        status.fraction = Some(0.4);
        status.eta_secs = Some(12.0);
        let analysis = kerf_core::AnalysisStatus {
            asset_id: id,
            kinds: vec![kerf_core::AnalysisKindStatus {
                kind: AnalysisKind::Silence,
                state: kerf_core::AnalysisState::Done,
                reason: None,
            }],
        };
        let entry = serde_json::to_value(AssetEntry::new(&asset, &status, &analysis)).unwrap();
        assert_eq!(entry["name"], "a.mp4", "the asset's own fields stay at the top");
        assert_eq!(entry["proxy"]["state"], "building");
        assert_eq!(entry["proxy"]["fraction"], 0.4);
        assert_eq!(entry["proxy"]["eta_secs"], 12.0);
        assert_eq!(entry["analysis"]["silence"], "done");
        assert_eq!(entry["source_missing"], true, "/nope.mp4 is not there");
        assert!(entry.get("analysis_failed").is_none());
    }
}
