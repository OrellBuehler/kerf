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

// ---- the library listing, in pages that fit ------------------------------------

/// How much of the asset list one `list_assets` page may hold, in characters of JSON. A client
/// refuses (or saves to a file) a tool result past its limit, and a full entry is ~1 KB — every
/// stream the probe found, plus a music score — so a page is cut here whatever `limit` says.
const ASSET_PAGE_CHARS: usize = 20_000;
/// A full page past this size gets the pointer to the compact view.
const ASSET_HINT_CHARS: usize = 8_000;
/// Assets whose proxy is looked up (a cached ffprobe each) at a time while a page fills, so a
/// page cut short does not pay for the assets it leaves out.
const ASSET_CHUNK: usize = 16;

/// How much of each asset `list_assets` shows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum AssetFields {
    /// The whole asset: every probed stream, a voiceover's script, a music score.
    #[default]
    Full,
    /// One short line: id, name, duration, picture format, proxy and analysis state.
    Compact,
}

/// An asset as `compact` lists it: what is needed to pick one, and nothing to read twice.
#[derive(Debug, Serialize)]
pub(super) struct AssetLine {
    id: uuid::Uuid,
    name: String,
    /// Seconds, to the millisecond.
    duration: f64,
    /// `3840x2160@59.94` for video, `4000x3000 still` for an image, `audio`, or `data`.
    media: String,
    imported_at: chrono::DateTime<chrono::Utc>,
    /// The preview proxy's state (`ready`, `building`, `queued`, `missing`, `failed`, `off`,
    /// `not_needed`); `proxy_status` has the rest.
    proxy: ProxyPhase,
    /// The kinds that are done, in pass order, as `silence scenes loudness`; `all` when every one is,
    /// `none` when none is.
    analysis: String,
    /// The kinds whose step is running now, in the same words. Absent when none is.
    #[serde(skip_serializing_if = "String::is_empty")]
    analysis_running: String,
    /// Why a kind failed, by kind. Absent when none did.
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    analysis_failed: serde_json::Map<String, serde_json::Value>,
    /// The audio was composed from a score (`compose_music`); the score is in the full listing.
    /// Absent otherwise.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    music_score: bool,
    /// The source file is gone from disk. Absent otherwise.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    source_missing: bool,
}

/// `59.94005994` as `59.94`, `30.0` as `30`.
fn fps_label(fps: f64) -> String {
    let text = format!("{fps:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The picture (or the lack of one) in a few characters.
fn media_label(asset: &Asset) -> String {
    use kerf_core::StreamKind;
    match asset.primary_kind() {
        StreamKind::Audio => "audio".to_string(),
        StreamKind::Video => {
            let video = asset.streams.iter().find(|s| s.kind == StreamKind::Video);
            let size = video.and_then(|s| Some((s.width?, s.height?)));
            match (size, video.and_then(|s| s.fps)) {
                (Some((w, h)), _) if asset.is_image() => format!("{w}x{h} still"),
                (Some((w, h)), Some(fps)) if fps > 0.0 => format!("{w}x{h}@{}", fps_label(fps)),
                (Some((w, h)), _) => format!("{w}x{h}"),
                (None, _) => "video".to_string(),
            }
        }
        _ => "data".to_string(),
    }
}

/// The analysis kinds in `state`, in pass order.
fn kinds_in(status: &kerf_core::AnalysisStatus, state: kerf_core::AnalysisState) -> Vec<&'static str> {
    status
        .kinds
        .iter()
        .filter(|k| k.state == state)
        .map(|k| k.kind.name())
        .collect()
}

impl AssetLine {
    pub(super) fn new(asset: &Asset, proxy: &ProxyStatus, status: &kerf_core::AnalysisStatus) -> Self {
        let (_, analysis_failed) = AnalysisBrief::of(status);
        let done = kinds_in(status, kerf_core::AnalysisState::Done);
        let analysis = match done.len() {
            0 => "none".to_string(),
            n if n == status.kinds.len() => "all".to_string(),
            _ => done.join(" "),
        };
        Self {
            id: asset.id,
            name: asset.name.clone(),
            duration: (asset.duration * 1000.0).round() / 1000.0,
            media: media_label(asset),
            imported_at: asset.imported_at,
            proxy: proxy.state,
            analysis,
            analysis_running: kinds_in(status, kerf_core::AnalysisState::Running).join(" "),
            analysis_failed,
            music_score: asset.music_score.is_some(),
            source_missing: !std::path::Path::new(&asset.path).exists(),
        }
    }
}

/// One asset of a listing, in the view that was asked for.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(super) enum AssetRow {
    Full(Box<AssetEntry>),
    Compact(Box<AssetLine>),
}

/// What `list_assets` answers.
#[derive(Debug, Serialize)]
pub(super) struct AssetsPage {
    /// The assets that match (all of them unless `imported_after` narrowed it).
    total: usize,
    offset: usize,
    returned: usize,
    /// Where the next page starts; null on the last.
    next_offset: Option<usize>,
    /// When the newest asset of the whole library was imported, whatever the filter: the
    /// `imported_after` that finds what is imported from here on. Absent for an empty library.
    #[serde(skip_serializing_if = "Option::is_none")]
    newest_imported_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    assets: Vec<AssetRow>,
}

/// The page a `list_assets` call asked for.
#[derive(Debug, Clone, Copy)]
pub(super) struct AssetsQuery {
    fields: AssetFields,
    imported_after: Option<chrono::DateTime<chrono::Utc>>,
    offset: usize,
    limit: usize,
}

impl ListAssetsParams {
    /// The query, with the defaults and the cap applied. A time that is not RFC 3339 is the
    /// caller's mistake, said as one.
    pub(super) fn query(&self) -> Result<AssetsQuery, McpError> {
        let imported_after = match self.imported_after.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(text) => Some(
                chrono::DateTime::parse_from_rfc3339(text)
                    .map_err(|e| {
                        McpError::invalid_params(
                            format!(
                                "`imported_after` must be an RFC 3339 time such as 2026-10-22T09:30:00Z (the \
                                 `newest_imported_at` of an earlier list_assets answer works): {e}"
                            ),
                            None,
                        )
                    })?
                    .with_timezone(&chrono::Utc),
            ),
        };
        let (offset, limit) = page_of(self.offset, self.limit, DEFAULT_PAGE, MAX_PAGE);
        Ok(AssetsQuery {
            fields: self.fields.unwrap_or_default(),
            imported_after,
            offset,
            limit,
        })
    }
}

/// One page of the library: the assets imported after the query's time, from its offset, in the
/// view it chose, and no more than [`ASSET_PAGE_CHARS`] of them (at least one). The disk is asked
/// only about the assets the page holds, a chunk at a time; `proxies_of` answers a chunk's proxy
/// states (`kerf_core::proxy::statuses`).
pub(super) fn assets_page(
    mut inputs: Vec<kerf_core::proxy::ProxyInput>,
    statuses: &HashMap<uuid::Uuid, kerf_core::AnalysisStatus>,
    query: &AssetsQuery,
    proxies_of: impl Fn(&[kerf_core::proxy::ProxyInput]) -> Vec<ProxyStatus>,
) -> AssetsPage {
    let newest_imported_at = inputs.iter().map(|i| i.asset.imported_at).max();
    if let Some(after) = query.imported_after {
        inputs.retain(|i| i.asset.imported_at > after);
    }
    let total = inputs.len();
    let start = query.offset.min(total);
    let window = &inputs[start..];
    let window = &window[..window.len().min(query.limit)];

    let mut assets = Vec::new();
    let (mut used, mut consumed, mut cut) = (0, 0, false);
    'page: for chunk in window.chunks(ASSET_CHUNK) {
        let proxies: HashMap<_, _> = proxies_of(chunk).into_iter().map(|p| (p.asset_id, p)).collect();
        for input in chunk {
            let id = input.asset.id;
            if let (Some(proxy), Some(status)) = (proxies.get(&id), statuses.get(&id)) {
                let row = match query.fields {
                    AssetFields::Full => AssetRow::Full(Box::new(AssetEntry::new(&input.asset, proxy, status))),
                    AssetFields::Compact => AssetRow::Compact(Box::new(AssetLine::new(&input.asset, proxy, status))),
                };
                let size = serde_json::to_string(&row).map_or(0, |s| s.len() + 1);
                if !assets.is_empty() && used + size > ASSET_PAGE_CHARS {
                    cut = true;
                    break 'page;
                }
                used += size;
                assets.push(row);
            }
            consumed += 1;
        }
    }

    let end = start + consumed;
    let mut note = String::new();
    if query.fields == AssetFields::Full && (cut || used > ASSET_HINT_CHARS) {
        note.push_str(
            "Each asset here carries its whole probe (every stream) and any music score: pass fields=\"compact\" \
             for one short line per asset (id, name, duration, media, proxy, analysis) when you are looking for \
             one, and get_asset_metadata for one asset's detail. ",
        );
    }
    if cut {
        note.push_str(&format!(
            "This page stops at {} assets to stay under about {ASSET_PAGE_CHARS} characters; next_offset continues it.",
            assets.len()
        ));
    }
    AssetsPage {
        total,
        offset: query.offset,
        returned: assets.len(),
        next_offset: (end < total).then_some(end),
        newest_imported_at,
        note: (!note.is_empty()).then(|| note.trim_end().to_string()),
        assets,
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
    /// The speaker labels (S1, S2, …) when speakers were labelled.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    speakers: Vec<String>,
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
                speakers: speakers_of(&analysis.transcript),
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

/// The transcript segments overlapping `[from, to)` (either end open), of `speaker` when one is
/// named (case ignored). A line's words come only with `words` (its own, or estimated ones for a
/// line that has none): they are most of a transcript's weight.
pub(super) fn transcript_window(
    analysis: &AssetAnalysis,
    from: Option<f64>,
    to: Option<f64>,
    speaker: Option<&str>,
    words: bool,
) -> Vec<TranscriptSegment> {
    let want = speaker.map(str::trim).filter(|s| !s.is_empty());
    analysis
        .transcript
        .iter()
        .filter(|s| {
            from.is_none_or(|f| s.end > f)
                && to.is_none_or(|t| s.start < t)
                && want.is_none_or(|w| s.speaker.as_deref().is_some_and(|sp| sp.eq_ignore_ascii_case(w)))
        })
        .map(|s| TranscriptSegment {
            start: s.start,
            end: s.end,
            text: s.text.clone(),
            words: if words {
                kerf_core::transcript::words_of(s).into_owned()
            } else {
                Vec::new()
            },
            speaker: s.speaker.clone(),
        })
        .collect()
}

/// The speaker labels of a transcript, in order of first appearance.
pub(super) fn speakers_of(transcript: &[TranscriptSegment]) -> Vec<String> {
    let mut speakers: Vec<String> = Vec::new();
    for sp in transcript.iter().filter_map(|s| s.speaker.as_ref()) {
        if !speakers.contains(sp) {
            speakers.push(sp.clone());
        }
    }
    speakers
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

/// Whether the user has proxies switched off (size 0) or previews always on the original: no proxy is
/// wanted, so none being ready is not news.
pub(super) fn proxies_switched_off() -> bool {
    kerf_core::proxy::proxy_size() == kerf_core::ProxySize::Off
        || kerf_core::proxy::preview_source_setting() == kerf_core::PreviewSource::Original
}

/// What of the media `timeline` uses has no ready proxy, or no source file: a note per asset and
/// the clip count per track. Pure: `proxies` and `missing` are read off the disk by the caller.
///
/// With `proxies_off` (the user switched proxies off, or previews always use the original) a proxy
/// that is `off` is the user's choice and not worth a note on every clip. (One they deleted by hand
/// is `off` too, with proxies on: that asset is still noted.)
pub(super) fn media_notes(
    timeline: &Timeline,
    assets: &[Asset],
    proxies: &HashMap<uuid::Uuid, ProxyStatus>,
    missing: &std::collections::HashSet<uuid::Uuid>,
    proxies_off: bool,
) -> MediaNotes {
    let waiting = |p: &ProxyStatus| !(proxy_is_settled(p.state) || (proxies_off && p.state == ProxyPhase::Off));
    let unready = |id: &uuid::Uuid| proxies.get(id).is_some_and(waiting);
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
        let proxy = proxies.get(&asset.id).filter(|p| waiting(p));
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

/// A track as the tools that set one of its switches answer: the track's own fields (name, kind,
/// fader, pan, mute, solo, lock, duck…) and how many clips are on it, not the clips themselves —
/// `get_timeline_state` has those.
pub(super) fn track_brief(track: &kerf_core::Track) -> serde_json::Value {
    let mut value = serde_json::to_value(track).unwrap_or_default();
    if let Some(map) = value.as_object_mut() {
        map.remove("clips");
        map.insert("clip_count".to_string(), serde_json::json!(track.clips.len()));
    }
    value
}

/// Changes per page of a diff when the caller does not say, and the most it may ask for.
pub(super) const DEFAULT_DIFF_PAGE: usize = 100;
pub(super) const MAX_DIFF_PAGE: usize = 500;

/// A diff as the review tools answer it: the numbers and the headline always, the changes
/// themselves a page at a time (`next_offset` in `entries` is the next).
pub(super) fn diff_page(diff: &kerf_core::TimelineDiff, offset: Option<usize>, limit: Option<usize>) -> serde_json::Value {
    let (offset, limit) = page_of(offset, limit, DEFAULT_DIFF_PAGE, MAX_DIFF_PAGE);
    serde_json::json!({
        "headline": diff.headline(),
        "duration_before": diff.duration_before,
        "duration_after": diff.duration_after,
        "clips_before": diff.clips_before,
        "clips_after": diff.clips_after,
        "entries": paged(&diff.entries, offset, limit),
    })
}

/// How `get_timeline_state` is asked to show a timeline.
pub(super) struct StateView {
    /// Every SVG source whole, and every caption and clip inline.
    pub full: bool,
    pub captions_offset: Option<usize>,
    pub captions_limit: Option<usize>,
    pub clips_offset: Option<usize>,
    pub clips_limit: Option<usize>,
}

/// Clips `get_timeline_state` shows inline unless asked for more. An ordinary cut fits whole; it is
/// a long edit with hundreds of clips that is windowed.
pub(super) const DEFAULT_CLIP_WINDOW: usize = 200;
const MAX_CLIP_WINDOW: usize = 1000;

/// The timeline as `get_timeline_state` answers: SVG sources over the inline limit replaced by a
/// note (`super::elide_large_svg`, applied by the caller), and the **generated captions** — often
/// hundreds of overlays — moved out of `overlays` into a window of their own, unless the caller
/// asked for `full`. Typed titles stay in `overlays`.
pub(super) fn timeline_state(timeline: &Timeline, view: &StateView) -> Result<serde_json::Value, McpError> {
    let mut value = serde_json::to_value(timeline).map_err(|e| McpError::internal_error(e.to_string(), None))?;
    if view.full {
        return Ok(value);
    }
    window_clips(timeline, &mut value, view);
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

/// Keep only a window of the cut's clips in `value` (the timeline as JSON) when it has more than
/// fit: the window runs over the clips in track order, then start order, and `clips_window` says
/// where it sits and how to move it. A cut that fits is left whole and gets no `clips_window`.
fn window_clips(timeline: &Timeline, value: &mut serde_json::Value, view: &StateView) {
    let total: usize = timeline.tracks.iter().map(|t| t.clips.len()).sum();
    let (offset, limit) = page_of(view.clips_offset, view.clips_limit, DEFAULT_CLIP_WINDOW, MAX_CLIP_WINDOW);
    if total <= limit && offset == 0 {
        return;
    }
    let Some(tracks) = value.get_mut("tracks").and_then(|t| t.as_array_mut()) else {
        return;
    };
    let (mut index, mut returned) = (0usize, 0usize);
    for track in tracks.iter_mut() {
        let Some(clips) = track.get_mut("clips").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        let kept: Vec<serde_json::Value> = std::mem::take(clips)
            .into_iter()
            .filter(|_| {
                let inside = index >= offset && index < offset.saturating_add(limit);
                index += 1;
                returned += usize::from(inside);
                inside
            })
            .collect();
        *clips = kept;
    }
    let end = offset.saturating_add(returned);
    if let Some(map) = value.as_object_mut() {
        let mut window = serde_json::json!({
            "total": total,
            "offset": offset,
            "returned": returned,
            "note": "this cut has more clips than fit: the tracks list only the clips of this window, in track order. Pass clips_offset / clips_limit for another slice, or full=true for every clip (timeline_summary has the per-track counts and gaps)",
        });
        if end < total {
            window["next_offset"] = serde_json::json!(end);
        }
        map.insert("clips_window".to_string(), window);
    }
}

// ---- tools --------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct ListAssetsParams {
    #[schemars(
        description = "Skip this many of the matching assets (default 0), to page on; `next_offset` in the answer is the next page"
    )]
    offset: Option<usize>,
    #[schemars(
        description = "Most assets to return (default 100, at most 500). A page is also cut at about 20,000 characters, so \
                       a big library comes in several pages: follow `next_offset`"
    )]
    limit: Option<usize>,
    #[schemars(
        description = "`full` (default): each asset whole — every probed stream, a voiceover's script, a music score — about 1 KB \
                       apiece. `compact`: one short line each (id, name, duration, media = picture format / audio, imported_at, \
                       proxy state, analysis, and `music_score` / `source_missing` when true), about a quarter of the size: use it to \
                       find an asset in a big library"
    )]
    fields: Option<AssetFields>,
    #[schemars(
        description = "Only the assets imported strictly after this RFC 3339 time (e.g. 2026-10-22T09:30:00Z) — what a batch import \
                       just added. Take `newest_imported_at` from a list_assets answer before you import and pass it back after. \
                       Assets come oldest import first; `offset` counts within the matches"
    )]
    imported_after: Option<String>,
}

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
    #[schemars(description = "Only the lines of this speaker (e.g. \"S2\"), when speakers were labelled")]
    speaker: Option<String>,
    #[schemars(
        description = "Include each line's words with their own source times (default false: the lines alone, far fewer tokens). Ask for them over a from/to window."
    )]
    words: Option<bool>,
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
        description = "List the media assets in the project, oldest import first. The answer has `total` (the assets that \
                       match), `returned`, `next_offset` (null on the last page), `newest_imported_at` (when the newest asset of \
                       the whole library was imported) and `assets`. A page is at most `limit` assets (default 100, at most 500) \
                       and about 20,000 characters, so a big library comes in several pages: follow `next_offset`. By default \
                       each asset is shown whole, about 1 KB apiece: its probed streams, `source_missing` (the file is gone \
                       from disk — set only when true), `music_score` (set when the audio was composed with compose_music), \
                       `proxy` (the preview proxy: `state` is ready, building with `fraction` and `eta_secs`, queued, missing, \
                       failed with a `reason`, off, or not_needed; until it is ready previews decode the original and are \
                       slower — export is not affected) and `analysis` (per kind — silence, scenes, loudness, rhythm, \
                       transcript — done, not_run, running, failed or off; `analysis_failed` says why). For a big library pass \
                       `fields: \"compact\"`: one short line per asset — `id`, `name`, `duration`, `media` (\"3840x2160@59.94\", \
                       \"audio\", \"4000x3000 still\"), `imported_at`, `proxy` (just its state), `analysis` (the kinds that are \
                       done: \"all\", \"none\" or e.g. \"silence scenes\"; `analysis_running` and `analysis_failed` when any) and \
                       `music_score` (true: the score itself is in the whole entry and in get_asset_metadata) / `source_missing` when true. To find what an import added, take `newest_imported_at` from \
                       this tool before importing and pass it as `imported_after` afterwards: only the assets imported strictly \
                       after that time come back. proxy_status and analysis_status give the same per asset in more detail; \
                       get_asset_metadata summarises what the analysis found."
    )]
    async fn list_assets(&self, Parameters(p): Parameters<ListAssetsParams>) -> Result<String, McpError> {
        let query = p.query()?;
        let (project, owner) = (self.project.clone(), self.owner());
        let page = blocking(move || {
            // The transcription backend may need an ffmpeg start the first time it is asked: before
            // the lock, never inside it.
            crate::warm_transcription_backend();
            // Everything that is the project's, under the lock; the disk (the proxies' cached
            // ffprobe, a stat per source) with it released.
            let (inputs, statuses) = {
                let project = lock_agent_as(&project, owner.as_deref());
                (
                    project.proxy_inputs().map_err(core_err)?,
                    project.analysis_statuses().map_err(core_err)?,
                )
            };
            // Only the assets the page holds are looked at on the disk (a proxy's state is a cached
            // ffprobe, `source_missing` a stat, each): the listing of a library of thousands stays cheap.
            let statuses: HashMap<_, _> = statuses.into_iter().map(|s| (s.asset_id, s)).collect();
            Ok(assets_page(inputs, &statuses, &query, kerf_core::proxy::statuses))
        })
        .await?;
        json(&page)
    }

    #[tool(
        description = "List the assets whose source file is no longer on disk (moved, renamed, a drive that is not \
                       mounted): each with its name, the path it was imported from and how many clips of the cut use \
                       it. Empty means every file is there. Previews and exports that need an offline file fail until \
                       it is back. This is read-only: relinking or removing media is the user's, in Kerf's library — \
                       tell them which files and where they were."
    )]
    async fn missing_assets(&self) -> Result<String, McpError> {
        let (project, owner) = (self.project.clone(), self.owner());
        let missing = blocking(move || {
            // The project's files under the lock; asking the disk — a share that went away can take a
            // long time to say so — with it released.
            let files = lock_agent_as(&project, owner.as_deref())
                .missing_assets_inputs()
                .map_err(core_err)?;
            Ok(kerf_core::Project::find_missing_assets(files))
        })
        .await?;
        json(&missing)
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
        description = "Read an asset's transcript a slice at a time: the segments `{start, end, text, speaker}` (SOURCE \
                       seconds; `speaker` is S1, S2, … when speakers were labelled — a line's label is the speaker of \
                       most of its words) overlapping [from, to), at most `limit` (default 100) from `offset`, with \
                       `total`, `speakers` and `next_offset` for the next page. `speaker` keeps that speaker's lines \
                       only. words=true adds each line's `words` — each with its own source start/end, its speaker, a \
                       `confidence` when the recognizer gave one, and `estimated: true` when its times were spread over \
                       the line by character share and tightened to the speech energy rather than reported; they are \
                       most of the reply, so ask for them over a window. Pass a word's start/end to cut_clip_range to \
                       cut one word. The transcript must exist (analyze_asset with `transcript`; a generated \
                       voiceover's is its script). Use this instead of reading the whole transcript when you need a \
                       passage: get_asset_metadata shows how long it is."
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
        let window = transcript_window(&analysis, p.from, p.to, p.speaker.as_deref(), p.words.unwrap_or(false));
        let mut answer =
            serde_json::to_value(paged(&window, offset, limit)).map_err(|e| McpError::internal_error(e.to_string(), None))?;
        if let Some(map) = answer.as_object_mut() {
            map.insert("asset_id".to_string(), serde_json::json!(id));
            map.insert("speakers".to_string(), serde_json::json!(speakers_of(&analysis.transcript)));
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
            ..Default::default()
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
        assert_eq!(texts(transcript_window(&a, None, None, None, false)).len(), 3);
        assert_eq!(
            texts(transcript_window(&a, Some(2.0), Some(8.0), None, false)),
            vec!["this is a test"]
        );
        assert_eq!(
            texts(transcript_window(&a, Some(1.0), None, None, false)),
            vec!["Hello there", "this is a test", "the end"]
        );
        assert_eq!(
            texts(transcript_window(&a, None, Some(2.0), None, false)),
            vec!["Hello there"]
        );
        assert!(transcript_window(&a, Some(50.0), None, None, false).is_empty());
    }

    fn lines() -> AssetAnalysis {
        let mut transcript: Vec<TranscriptSegment> = (0..5)
            .map(|i| TranscriptSegment {
                start: i as f64 * 2.0,
                end: i as f64 * 2.0 + 1.5,
                text: "one two".into(),
                speaker: Some(if i % 2 == 0 { "S1" } else { "S2" }.into()),
                ..Default::default()
            })
            .collect();
        kerf_core::transcript::ensure_words(&mut transcript[..2]);
        AssetAnalysis {
            asset_id: Uuid::nil(),
            transcript,
            ran: vec![AnalysisKind::Transcript],
            ..AssetAnalysis::default()
        }
    }

    /// Words only when asked for (estimated ones for a line that has none); a window and a speaker
    /// narrow the lines, and a page of them says where the next starts.
    #[test]
    fn a_transcript_read_has_words_only_on_request_and_filters_by_speaker_and_window() {
        let a = lines();
        let plain = transcript_window(&a, None, None, None, false);
        assert_eq!(plain.len(), 5);
        assert!(plain.iter().all(|s| s.words.is_empty() && s.speaker.is_some()));
        assert_eq!(speakers_of(&a.transcript), ["S1", "S2"]);

        let words = transcript_window(&a, Some(3.0), Some(6.5), None, true);
        assert_eq!(words.iter().map(|s| s.start).collect::<Vec<_>>(), [2.0, 4.0, 6.0]);
        assert!(
            words.iter().all(|s| s.words.len() == 2),
            "a line without words gets estimated ones"
        );

        let s1 = transcript_window(&a, None, None, Some(" s1 "), false);
        assert_eq!(s1.iter().map(|s| s.start).collect::<Vec<_>>(), [0.0, 4.0, 8.0]);
        let page = paged(&s1, 0, 2);
        assert_eq!((page.total, page.returned, page.next_offset), (3, 2, Some(2)));
        assert!(transcript_window(&a, None, None, Some("S9"), false).is_empty());
    }

    #[test]
    fn the_summary_names_the_speakers_and_no_words() {
        let a = lines();
        let text = serde_json::to_string(&AnalysisSummary::of(&a)).unwrap();
        assert!(text.contains("\"speakers\":[\"S1\",\"S2\"]"), "{text}");
        assert!(!text.contains("\"words\":["), "{text}");
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
            clips_offset: None,
            clips_limit: None,
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
            clips_offset: None,
            clips_limit: None,
        };
        let state = timeline_state(&captioned(300), &full).unwrap();
        assert_eq!(state["overlays"].as_array().unwrap().len(), 301);
        assert!(state.get("captions").is_none());

        let plain = timeline_state(&Timeline::new(), &StateView { full: false, ..full }).unwrap();
        assert!(plain.get("captions").is_none() && plain.get("overlays").is_none());
    }

    /// A cut of `per_track` clips on each of two tracks.
    fn clipped(per_track: usize) -> Timeline {
        let mut t = Timeline::new();
        let asset = Uuid::nil();
        for track in &mut t.tracks {
            for i in 0..per_track {
                track.clips.push(kerf_core::Clip::new(asset, 0.0, 1.0, i as f64 * 2.0));
            }
        }
        t
    }

    fn clip_count(state: &serde_json::Value) -> usize {
        state["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["clips"].as_array().unwrap().len())
            .sum()
    }

    fn view(offset: Option<usize>, limit: Option<usize>, full: bool) -> StateView {
        StateView {
            full,
            captions_offset: None,
            captions_limit: None,
            clips_offset: offset,
            clips_limit: limit,
        }
    }

    #[test]
    fn a_cut_that_fits_is_shown_whole_and_a_long_one_in_a_window_of_clips() {
        let whole = timeline_state(&clipped(DEFAULT_CLIP_WINDOW / 2), &view(None, None, false)).unwrap();
        assert_eq!(clip_count(&whole), DEFAULT_CLIP_WINDOW, "two tracks of a hundred fit exactly");
        assert!(whole.get("clips_window").is_none(), "nothing to say about a cut that fits");

        let long = clipped(300);
        let first = timeline_state(&long, &view(None, None, false)).unwrap();
        assert_eq!(clip_count(&first), DEFAULT_CLIP_WINDOW);
        assert_eq!(first["clips_window"]["total"], 600);
        assert_eq!(first["clips_window"]["returned"], DEFAULT_CLIP_WINDOW);
        assert_eq!(first["clips_window"]["next_offset"], DEFAULT_CLIP_WINDOW);
        assert_eq!(first["tracks"].as_array().unwrap().len(), 2, "every track is still listed");
        assert!(first["tracks"][0]["clips"].as_array().unwrap().len() == DEFAULT_CLIP_WINDOW);
        assert!(
            first["tracks"][1]["clips"].as_array().unwrap().is_empty(),
            "the window ran out inside the first track"
        );

        // A page that crosses from one track into the next, to the end.
        let last = timeline_state(&long, &view(Some(250), Some(1000), false)).unwrap();
        assert_eq!(clip_count(&last), 350);
        assert_eq!(last["tracks"][0]["clips"].as_array().unwrap().len(), 50);
        assert_eq!(last["tracks"][1]["clips"].as_array().unwrap().len(), 300);
        assert!(last["clips_window"].get("next_offset").is_none(), "that was the end");

        // `full` is every clip, and no window.
        let all = timeline_state(&long, &view(None, None, true)).unwrap();
        assert_eq!(clip_count(&all), 600);
        assert!(all.get("clips_window").is_none());
    }

    #[test]
    fn a_track_answer_leaves_out_its_clips_and_a_diff_is_paged() {
        let track = &clipped(40).tracks[0];
        let brief = track_brief(track);
        assert!(brief.get("clips").is_none(), "{brief}");
        assert_eq!(brief["clip_count"], 40);
        assert_eq!(brief["id"], serde_json::json!(track.id));
        assert_eq!(brief["name"], "V1");

        let before = Timeline::new();
        let after = clipped(150);
        let diff = before.diff(&after);
        let total = diff.entries.len();
        assert!(total >= 300, "a change per clip added: {total}");
        let first = diff_page(&diff, None, None);
        assert_eq!(first["entries"]["returned"], DEFAULT_DIFF_PAGE);
        assert_eq!(first["entries"]["total"], diff.entries.len());
        assert_eq!(first["entries"]["next_offset"], DEFAULT_DIFF_PAGE);
        assert_eq!(first["clips_after"], 300);
        assert!(
            first["headline"].as_str().unwrap().contains(&format!("{total} changes")),
            "{first}"
        );
        let last = diff_page(&diff, Some(250), Some(9_999));
        assert_eq!(last["entries"]["returned"], total - 250);
        assert!(last["entries"].get("next_offset").is_none());
        let small = diff_page(&diff, Some(0), Some(0));
        assert_eq!(small["entries"]["returned"], 1, "a limit of nothing still returns one");
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
        let notes = media_notes(&timeline, &assets, &proxies, &missing, false);
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
        let notes = media_notes(&timeline, &[asset_named(id, "a.mp4")], &proxies, &Default::default(), false);
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
            let notes = media_notes(&timeline, &[asset_named(id, "a.mp4")], &proxies, &Default::default(), false);
            assert_eq!(notes.per_track[0], 1, "{state:?}");
            assert_eq!(notes.notes.len(), 1, "{state:?}");
        }
    }

    #[test]
    fn with_proxies_switched_off_no_clip_is_noted_for_having_none() {
        let (off, failed) = (Uuid::new_v4(), Uuid::new_v4());
        let mut timeline = Timeline::new();
        timeline.tracks[0].clips = vec![clip_of(off), clip_of(failed)];
        let assets = [asset_named(off, "off.mp4"), asset_named(failed, "failed.mp4")];
        let proxies: HashMap<_, _> = [
            (off, proxy(off, ProxyPhase::Off)),
            (failed, proxy(failed, ProxyPhase::Failed)),
        ]
        .into_iter()
        .collect();
        let notes = media_notes(&timeline, &assets, &proxies, &Default::default(), true);
        assert_eq!(notes.per_track, vec![1, 0], "only the failed one still waits");
        let names: Vec<&str> = notes.notes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["failed.mp4"], "off is the user's choice, a failure is not");
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

    // ---- list_assets: the compact view, what an import added, pages that fit ----------------

    /// When the `n`th asset of a test library was imported: a quarter of a second apart, as a
    /// batch import lands.
    fn imported(n: usize) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-10-20T08:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
            + chrono::TimeDelta::milliseconds(n as i64 * 250)
    }

    /// A score of `sections` sections of `bars` bars each, a chord per bar.
    fn long_score(sections: usize, bars: usize) -> serde_json::Value {
        serde_json::json!({
            "bpm": 96.0,
            "key": "Am",
            "style": "lofi",
            "sections": (0..sections).map(|i| serde_json::json!({
                "kind": if i % 2 == 0 { "verse" } else { "drop" },
                "bars": bars,
                "chords": (0..bars).map(|b| ["Am7", "Fmaj7", "C", "G/B", "Dm7", "E7"][b % 6]).collect::<Vec<_>>(),
                "energy": 0.6,
            })).collect::<Vec<_>>(),
            "hits": [{"at_s": 12.5, "kind": "impact"}],
            "seed": 7,
        })
    }

    /// The `n`th asset of a library like the one that overflowed: GoPro clips (a picture, a sound
    /// and three data tracks apiece), a music bed with its score every eleventh, and a still.
    fn library_asset(n: usize, score: (usize, usize)) -> Asset {
        let (name, streams, score) = match n % 11 {
            5 => (
                format!("lofi bed {n}.wav"),
                serde_json::json!([{"index": 0, "kind": "audio", "codec": "pcm_s16le", "sample_rate": 44100, "channels": 2}]),
                Some(long_score(score.0, score.1)),
            ),
            8 => (
                format!("title card {n}.jpg"),
                serde_json::json!([{"index": 0, "kind": "video", "codec": "mjpeg", "width": 4000, "height": 3000,
                                    "image": true, "pix_fmt": "yuvj444p", "color_range": "pc"}]),
                None,
            ),
            _ => (
                format!("GX01{n:04}.MP4"),
                serde_json::json!([
                    {"index": 0, "kind": "video", "codec": "hevc", "width": 3840, "height": 2160, "fps": 59.94005994005994,
                     "color_transfer": "bt709", "color_primaries": "bt709", "pix_fmt": "yuvj420p", "color_space": "bt709",
                     "color_range": "pc"},
                    {"index": 1, "kind": "audio", "codec": "aac", "sample_rate": 48000, "channels": 2},
                    {"index": 2, "kind": "data", "codec": "none"},
                    {"index": 3, "kind": "data", "codec": "none"},
                    {"index": 4, "kind": "data", "codec": "none"},
                ]),
                None,
            ),
        };
        let mut asset = serde_json::json!({
            "id": Uuid::from_u128(n as u128 + 1),
            // A file that exists, so no entry says `source_missing`.
            "path": concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
            "name": name,
            "duration": 23.123456 + n as f64,
            "streams": streams,
            "imported_at": imported(n),
        });
        if let Some(score) = score {
            asset["music_score"] = score;
        }
        serde_json::from_value(asset).unwrap()
    }

    /// An analysis status with the first `kinds` of the five kinds done and the rest not run.
    fn done(kinds: usize) -> kerf_core::AnalysisStatus {
        use kerf_core::AnalysisState::{Done, NotRun};
        kerf_core::AnalysisStatus {
            asset_id: Uuid::nil(),
            kinds: AnalysisKind::ALL
                .iter()
                .enumerate()
                .map(|(i, kind)| kerf_core::AnalysisKindStatus {
                    kind: *kind,
                    state: if i < kinds { Done } else { NotRun },
                    reason: None,
                })
                .collect(),
        }
    }

    type Library = (Vec<kerf_core::ProxyInput>, HashMap<Uuid, kerf_core::AnalysisStatus>);

    /// The first `count` assets of the library, with an analysis status each: all five kinds done,
    /// four done, or none; asset 3 is transcribing and asset 4's transcript failed.
    fn library(count: usize, score: (usize, usize)) -> Library {
        use kerf_core::AnalysisState::{Failed, Running};
        let mut inputs = Vec::new();
        let mut statuses = HashMap::new();
        for n in 0..count {
            let asset = library_asset(n, score);
            let mut status = done([5, 4, 0][n % 3]);
            status.asset_id = asset.id;
            if n == 3 || n == 4 {
                let transcript = status.kinds.last_mut().unwrap();
                (transcript.state, transcript.reason) = if n == 3 {
                    (Running, None)
                } else {
                    (Failed, Some("no speech model".to_string()))
                };
            }
            statuses.insert(asset.id, status);
            inputs.push(kerf_core::ProxyInput { asset, declined: false });
        }
        (inputs, statuses)
    }

    fn query(
        fields: Option<AssetFields>,
        imported_after: Option<&str>,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> AssetsQuery {
        ListAssetsParams {
            offset,
            limit,
            fields,
            imported_after: imported_after.map(str::to_string),
        }
        .query()
        .unwrap()
    }

    /// The page the tool would answer: as the client's JSON, as the text, and how many assets the
    /// disk was asked about for their proxies.
    fn listed(
        inputs: &[kerf_core::ProxyInput],
        statuses: &HashMap<Uuid, kerf_core::AnalysisStatus>,
        q: &AssetsQuery,
    ) -> (serde_json::Value, String, usize) {
        let looked_at = std::cell::Cell::new(0);
        let page = assets_page(inputs.to_vec(), statuses, q, |chunk| {
            looked_at.set(looked_at.get() + chunk.len());
            chunk.iter().map(|i| proxy(i.asset.id, ProxyPhase::Ready)).collect()
        });
        let text = serde_json::to_string(&page).unwrap();
        (serde_json::from_str(&text).unwrap(), text, looked_at.get())
    }

    /// The test-library numbers of a page's assets.
    fn ids(page: &serde_json::Value) -> Vec<u128> {
        page["assets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| Uuid::parse_str(a["id"].as_str().unwrap()).unwrap().as_u128() - 1)
            .collect()
    }

    /// What `list_assets` answered before this view existed: the first hundred assets, whole.
    fn the_old_answer(inputs: &[kerf_core::ProxyInput], statuses: &HashMap<Uuid, kerf_core::AnalysisStatus>) -> String {
        let items: Vec<_> = inputs
            .iter()
            .take(DEFAULT_PAGE)
            .map(|i| AssetEntry::new(&i.asset, &proxy(i.asset.id, ProxyPhase::Ready), &statuses[&i.asset.id]))
            .collect();
        serde_json::to_string(&serde_json::json!({
            "total": inputs.len(),
            "offset": 0,
            "returned": items.len(),
            "next_offset": serde_json::Value::Null,
            "assets": items,
        }))
        .unwrap()
    }

    #[test]
    fn compact_is_a_short_line_per_asset_with_the_fields_an_agent_picks_by() {
        let (inputs, statuses) = library(66, (6, 8));
        let (page, text, _) = listed(&inputs, &statuses, &query(Some(AssetFields::Compact), None, None, None));
        assert_eq!((page["total"].as_u64(), page["returned"].as_u64()), (Some(66), Some(66)));
        assert!(page["next_offset"].is_null() && page.get("note").is_none(), "{page}");

        // A GoPro clip: the id, the name, the length to the millisecond, the picture, when it came.
        let clip = &page["assets"][0];
        assert_eq!(
            clip,
            &serde_json::json!({
                "id": Uuid::from_u128(1),
                "name": "GX010000.MP4",
                "duration": 23.123,
                "media": "3840x2160@59.94",
                "imported_at": "2026-10-20T08:00:00Z",
                "proxy": "ready",
                "analysis": "all",
            })
        );
        // The analysis in a word: how far it got, what is running, why a kind failed.
        assert_eq!(page["assets"][1]["analysis"], "silence scenes loudness rhythm");
        assert_eq!(page["assets"][2]["analysis"], "none");
        assert_eq!(page["assets"][3]["analysis"], "silence scenes loudness rhythm");
        assert_eq!(page["assets"][3]["analysis_running"], "transcript");
        assert_eq!(page["assets"][4]["analysis_failed"]["transcript"], "no speech model");
        // Sound only, a score, a still: said in the same few words.
        let bed = &page["assets"][5];
        assert_eq!(
            (bed["media"].as_str(), bed["music_score"].as_bool()),
            (Some("audio"), Some(true))
        );
        assert!(bed.get("streams").is_none() && bed.get("sections").is_none(), "{bed}");
        assert_eq!(page["assets"][8]["media"], "4000x3000 still");
        assert!(clip.get("music_score").is_none(), "set only for a composed bed: {clip}");
        let scored = page["assets"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a.get("music_score").is_some());
        assert_eq!(scored.count(), 6);
        assert!(text.len() / 66 < 260, "{} characters a line", text.len() / 66);
    }

    #[test]
    fn a_missing_source_and_a_picture_without_a_rate_are_still_said() {
        let mut asset = library_asset(0, (1, 4));
        asset.path = "/nope/gone.mp4".to_string();
        asset.streams[0].fps = None;
        let line = AssetLine::new(&asset, &proxy(asset.id, ProxyPhase::Building), &done(2));
        let line = serde_json::to_value(line).unwrap();
        assert_eq!(line["source_missing"], true);
        assert_eq!(
            (line["media"].as_str(), line["proxy"].as_str()),
            (Some("3840x2160"), Some("building"))
        );
        assert_eq!(line["analysis"], "silence scenes");
        assert_eq!(fps_label(29.97002997), "29.97");
        assert_eq!(fps_label(30.0), "30");
        assert_eq!(fps_label(120.0), "120");
        assert_eq!(fps_label(23.976), "23.98");
        asset.streams.clear();
        assert_eq!(media_label(&asset), "data");
    }

    #[test]
    fn the_whole_listing_overflowed_and_the_compact_one_and_the_full_page_do_not() {
        let (inputs, statuses) = library(66, (6, 8));
        let before = the_old_answer(&inputs, &statuses).len();
        let (_, compact, _) = listed(&inputs, &statuses, &query(Some(AssetFields::Compact), None, None, None));
        let (full_page, full, _) = listed(&inputs, &statuses, &query(None, None, None, None));
        // Each asset's path is this checkout's own Cargo.toml, so the size moves with where the repo lives.
        assert!(before > 40_000, "the library is as big as the one that overflowed: {before}");
        assert!(compact.len() < 15_000, "all 66 in a line each: {}", compact.len());
        assert!(
            compact.len() * 3 < before,
            "the compact view is a third of it or less: {} of {before}",
            compact.len()
        );
        assert!(full.len() < 22_000, "a full page is cut well under 25k: {}", full.len());
        assert!(full_page["returned"].as_u64().unwrap() < 66);
    }

    #[test]
    fn a_full_page_is_cut_at_the_budget_and_points_at_compact_and_the_next_page() {
        let (inputs, statuses) = library(66, (6, 8));
        let (first, text, looked_at) = listed(&inputs, &statuses, &query(None, None, None, None));
        assert!(text.len() <= ASSET_PAGE_CHARS + 1_000, "{}", text.len());
        let returned = first["returned"].as_u64().unwrap() as usize;
        assert!((10..40).contains(&returned), "{returned}");
        assert_eq!(first["next_offset"], returned);
        let note = first["note"].as_str().unwrap();
        assert!(note.contains("fields=\"compact\"") && note.contains("next_offset"), "{note}");
        assert!(
            looked_at < returned + 2 * ASSET_CHUNK,
            "the disk was asked about {looked_at} assets for a page of {returned}"
        );
        // The entry itself is the one the listing always had.
        let first_entry = AssetEntry::new(
            &inputs[0].asset,
            &proxy(inputs[0].asset.id, ProxyPhase::Ready),
            &statuses[&inputs[0].asset.id],
        );
        assert_eq!(first["assets"][0], serde_json::to_value(first_entry).unwrap());

        // Following next_offset reads every asset once, in import order.
        let mut seen = Vec::new();
        let mut offset = Some(0);
        let mut pages = 0;
        while let Some(at) = offset {
            let (page, text, _) = listed(&inputs, &statuses, &query(None, None, Some(at), None));
            assert!(text.len() <= ASSET_PAGE_CHARS + 1_000, "{}", text.len());
            seen.extend(ids(&page));
            offset = page["next_offset"].as_u64().map(|n| n as usize);
            pages += 1;
        }
        assert_eq!(seen, (0..66).map(|n| n as u128).collect::<Vec<_>>());
        assert!(pages > 2, "{pages}");
    }

    #[test]
    fn a_few_assets_come_back_whole_without_a_note_as_before() {
        let (inputs, statuses) = library(3, (1, 4));
        let (page, text, _) = listed(&inputs, &statuses, &query(None, None, None, None));
        assert!(page.get("note").is_none() && page["next_offset"].is_null(), "{page}");
        assert!(page.as_object().unwrap().contains_key("next_offset"), "still there, as null");
        assert_eq!(page["returned"], 3);
        // What was there is there, in the same words.
        let old: serde_json::Value = serde_json::from_str(&the_old_answer(&inputs, &statuses)).unwrap();
        assert_eq!(page["assets"], old["assets"]);
        assert_eq!(page["total"], old["total"]);
        assert!(text.len() < ASSET_HINT_CHARS);
    }

    #[test]
    fn a_page_of_long_scores_still_fits_and_one_asset_over_the_budget_is_still_returned() {
        // Sixty-four sections of 64 bars apiece: a score bigger than the page budget on its own.
        let (inputs, statuses) = library(66, (64, 64));
        assert_eq!(inputs.iter().filter(|i| i.asset.music_score.is_some()).count(), 6);
        let (page, text, _) = listed(&inputs[..16], &statuses, &query(None, None, None, None));
        assert!(text.len() <= ASSET_PAGE_CHARS + 1_000, "{}", text.len());
        assert_eq!(ids(&page), (0..5).collect::<Vec<_>>(), "the page stops before the long score");
        assert_eq!(page["next_offset"], 5);
        // A page that starts at the long score is that one asset, not an empty page and no way on.
        let (page, text, _) = listed(&inputs, &statuses, &query(None, None, Some(5), Some(500)));
        assert!(text.len() > ASSET_PAGE_CHARS, "{}", text.len());
        assert_eq!(ids(&page), vec![5]);
        assert_eq!(page["next_offset"], 6, "it moves on");
        // Compact leaves the score out whatever its size.
        let (page, text, _) = listed(&inputs, &statuses, &query(Some(AssetFields::Compact), None, None, None));
        assert_eq!(page["returned"], 66);
        assert!(text.len() < 15_000, "{}", text.len());
    }

    #[test]
    fn imported_after_finds_what_a_batch_added() {
        let (all, statuses) = library(66, (2, 8));
        // The library as it stood before 16 GoPro clips were imported: take the newest time.
        let (before, _, _) = listed(&all[..50], &statuses, &query(None, None, None, Some(1)));
        let mark = before["newest_imported_at"].as_str().unwrap().to_string();
        assert_eq!(mark, "2026-10-20T08:00:12.250Z", "the 50th asset, to the millisecond");

        let compact = Some(AssetFields::Compact);
        let (batch, _, _) = listed(&all, &statuses, &query(compact, Some(&mark), None, None));
        assert_eq!(ids(&batch), (50..66).collect::<Vec<_>>(), "the 16 clips and nothing else");
        assert_eq!((batch["total"].as_u64(), batch["returned"].as_u64()), (Some(16), Some(16)));
        assert!(batch["next_offset"].is_null());
        assert_eq!(
            batch["newest_imported_at"], "2026-10-20T08:00:16.250Z",
            "the library's, not the match's"
        );

        // Strictly after: the asset stamped with the mark itself is not new.
        assert!(!ids(&batch).contains(&49));
        // Paging counts within the matches.
        let (second, _, _) = listed(&all, &statuses, &query(compact, Some(&mark), Some(10), Some(4)));
        assert_eq!(ids(&second), (60..64).collect::<Vec<_>>());
        assert_eq!(
            (second["total"].as_u64(), second["next_offset"].as_u64()),
            (Some(16), Some(14))
        );
        // Any offset of RFC 3339 does, and the full view filters the same way.
        let at = Some("2026-10-20T10:00:12.250+02:00");
        let (full, _, looked_at) = listed(&all, &statuses, &query(None, at, None, Some(5)));
        assert_eq!(ids(&full), (50..55).collect::<Vec<_>>());
        assert_eq!(full["total"], 16);
        assert!(
            looked_at <= ASSET_CHUNK,
            "only the page is looked at on the disk: {looked_at}"
        );
        // Nothing new is an empty page that still names the newest time.
        let (none, _, _) = listed(&all, &statuses, &query(compact, Some("2026-10-20T08:00:16.250Z"), None, None));
        assert_eq!((none["total"].as_u64(), none["returned"].as_u64()), (Some(0), Some(0)));
        assert_eq!(none["assets"], serde_json::json!([]));
        assert_eq!(none["newest_imported_at"], "2026-10-20T08:00:16.250Z");
        // No assets, no time.
        let (empty, _, _) = listed(&[], &statuses, &query(None, None, None, None));
        assert!(empty.get("newest_imported_at").is_none() && empty["total"] == 0);
    }

    #[test]
    fn the_listing_arguments_are_checked_and_capped() {
        let q = query(None, None, None, None);
        assert_eq!(
            (q.offset, q.limit, q.fields, q.imported_after),
            (0, DEFAULT_PAGE, AssetFields::Full, None)
        );
        assert_eq!(query(None, Some("  "), Some(3), Some(9_999)).limit, MAX_PAGE);
        assert_eq!(query(None, Some(""), None, None).imported_after, None, "blank is no filter");
        for bad in ["yesterday", "2026-10-22", "2026-10-22 09:30:00", "1700000000"] {
            let e = ListAssetsParams {
                imported_after: Some(bad.to_string()),
                ..Default::default()
            }
            .query()
            .unwrap_err();
            assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS, "{bad}");
            assert!(
                e.message.contains("RFC 3339") && e.message.contains("newest_imported_at"),
                "{}",
                e.message
            );
        }
        // A view that is not one of the two is refused at the schema, not read as the default.
        let typo = serde_json::from_value::<ListAssetsParams>(serde_json::json!({"fields": "short"}));
        assert!(typo.is_err());
        let ok: ListAssetsParams = serde_json::from_value(serde_json::json!({"fields": "compact"})).unwrap();
        assert_eq!(ok.fields, Some(AssetFields::Compact));
    }
}
