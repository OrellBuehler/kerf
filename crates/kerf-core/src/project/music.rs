//! Composing music and measuring its balance: the project half of [`crate::engine::compose`] and
//! [`crate::engine::bands`].
//!
//! The usual heavy-op split, mirroring the voiceover:
//!
//! * **Compose.** [`Project::compose_music_inputs`] (under the lock: checks the request, with
//!   `align` fits the score to the working timeline's cuts or markers, and rehearses the placement
//!   so a piece that would land on another clip is refused before the render) →
//!   [`Project::synthesize_music`] (static, lock released: downloads the SoundFont on first use,
//!   renders, probes) → [`Project::place_music`] (under the lock, one transaction: stores the
//!   asset, writes its tempo / bar analysis **straight from the score**, and puts one `Add music`
//!   edit on the `M` track — or, with `replace_clip_id`, replaces the whole music bed with the
//!   new render).
//! * **Band balance.** [`Project::band_balance_inputs`] (under the lock: resolves the target and the
//!   windows) → [`Project::measure_band_balance`] (static, lock released: decodes and measures,
//!   ungated but capped and niced) — or [`Project::band_balance`] for a caller holding `&self`.

use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Project, MUSIC_TRACK};
use crate::engine;
use crate::engine::bands::{self, BandMeter, BandWindow, WindowBalance, MIN_WINDOW_S};
use crate::error::{Error, Result};
use crate::model::{
    plan_score_to_picture, AlignOptions, AlignReport, Asset, AssetAnalysis, Clip, MusicScore, SectionKind, StreamKind, TimeRange,
    Timeline, Track, DEFAULT_MAX_TEMPO_CHANGE,
};

fn invalid(msg: impl Into<String>) -> Error {
    Error::InvalidArgument(msg.into())
}

// ---- compose -------------------------------------------------------------------------------

/// What to fit a score to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlignTo {
    /// The boundaries between the clips of a video track.
    Cuts,
    /// The timeline's markers.
    Markers,
}

/// Fit the score to the picture ([`plan_score_to_picture`]): nudge the tempo and add the hits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScoreAlign {
    pub to: AlignTo,
    /// Which video track's cuts; the first video track when absent. Ignored for markers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub track_id: Option<Uuid>,
    /// The most the tempo may change, as a fraction (default 0.06; 0 keeps the tempo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tempo_change: Option<f64>,
}

/// Everything `compose_music` is asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComposeRequest {
    pub score: MusicScore,
    /// The audio track to put the music on (default: the track named `M`, made on first use).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub track_id: Option<Uuid>,
    /// Where on the timeline it starts, seconds (default: after what the track already holds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<ScoreAlign>,
    /// Replace this music with the new render instead of adding a clip — how a score is edited
    /// and composed again. **The whole bed goes**: every clip of this clip's asset on its track
    /// (what `fit_music` made of one piece) becomes one clip of the new render, at the earliest of
    /// their starts, with this clip's id, the first one's gain, fades-in and effects and the last
    /// one's fade-out; its length becomes the render's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub replace_clip_id: Option<Uuid>,
}

/// A checked request with the score fitted to the picture: what [`Project::synthesize_music`]
/// renders and [`Project::place_music`] lands.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ComposePlan {
    /// The score to render (the request's, or the aligned one).
    pub score: MusicScore,
    /// What aligning did, when it was asked for.
    pub align: Option<AlignReport>,
    /// Pass these to [`Project::place_music`]: the request's own, as given. A `None` start is
    /// resolved when the music is placed (after what the track holds *then*).
    pub track_id: Option<Uuid>,
    pub at: Option<f64>,
    pub replace_clip_id: Option<Uuid>,
    /// The clips the placement will replace — the whole bed of `replace_clip_id`, in time order;
    /// empty when a clip is added.
    pub replaces: Vec<Uuid>,
    /// Where on the timeline the music started when the plan was made (what `align` fitted the cuts
    /// to), seconds: informational.
    pub music_start: f64,
}

/// What `compose_music` did, for the caller to hand back: the asset and the clip
/// [`Project::place_music`] returned, the score that was rendered (aligned, when it was asked for) and
/// what aligning did.
#[derive(Debug, Clone, Serialize)]
pub struct Composed {
    pub asset: Asset,
    pub clip: Clip,
    pub score: MusicScore,
    pub align: Option<AlignReport>,
    /// The clips that were replaced by `clip` (`ComposePlan::replaces`); empty when a clip was
    /// added.
    pub replaced: Vec<Uuid>,
}

impl Project {
    /// Check a compose request and resolve it, under the lock: the score is valid, a replaced
    /// clip is a music clip, the music's start is known and, with `align`, the score is fitted to
    /// the working timeline's cuts or markers (an agent's staged cut included). Last, the
    /// placement is rehearsed on a copy of the cut ([`Project::check_music_fits`]).
    pub fn compose_music_inputs(&self, req: &ComposeRequest) -> Result<ComposePlan> {
        req.score.validate()?;
        let timeline = self.working_timeline()?;
        if let Some(at) = req.at {
            if !at.is_finite() || at < 0.0 {
                return Err(invalid("at must be a time in seconds, 0 or later"));
            }
        }
        let (start, replaces) = match req.replace_clip_id {
            Some(clip_id) => {
                let bed = self.music_bed(&timeline, clip_id)?;
                let start = bed.iter().map(|c| c.timeline_start).fold(f64::INFINITY, f64::min);
                (start, bed.iter().map(|c| c.id).collect())
            }
            None => (music_start(&timeline, req.track_id, req.at)?, Vec::new()),
        };
        let (score, align) = match &req.align {
            None => (req.score.clone(), None),
            Some(a) => {
                let max = a.max_tempo_change.unwrap_or(DEFAULT_MAX_TEMPO_CHANGE);
                if !max.is_finite() || !(0.0..=0.25).contains(&max) {
                    return Err(invalid("max_tempo_change is a fraction from 0 to 0.25 (0.06 is 6 %)"));
                }
                let cuts = cut_times(&timeline, a)?;
                let opts = AlignOptions {
                    max_tempo_change: max,
                    ..AlignOptions::default()
                };
                let (score, report) = plan_score_to_picture(&req.score, &cuts, start, &opts)?;
                (score, Some(report))
            }
        };
        // The placement, played out on a copy of the cut with the render's length: a piece that
        // would land on another clip of its track is refused now, not after the render.
        self.check_music_fits(&timeline, req, &score)?;
        Ok(ComposePlan {
            score,
            align,
            track_id: req.track_id,
            at: req.at,
            replace_clip_id: req.replace_clip_id,
            replaces,
            music_start: start,
        })
    }

    /// The music bed a clip belongs to, in time order: every clip of its asset on its track —
    /// what `fit_music` made of one piece. Refused unless it can be replaced: the clip is on an
    /// unlocked audio track, its asset was composed, and none of the bed is linked to a picture.
    fn music_bed(&self, timeline: &Timeline, clip_id: Uuid) -> Result<Vec<Clip>> {
        let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let track = &timeline.tracks[ti];
        if track.kind != StreamKind::Audio {
            return Err(invalid("only music on an audio track can be replaced"));
        }
        if track.locked {
            return Err(invalid(format!("{} is locked", track.name)));
        }
        let asset_id = track.clips[ci].asset_id;
        if self.require_asset(asset_id)?.music_score.is_none() {
            return Err(invalid(
                "that clip is not composed music: only a clip of a composed asset can be replaced",
            ));
        }
        let mut bed: Vec<Clip> = track.clips.iter().filter(|c| c.asset_id == asset_id).cloned().collect();
        if bed.iter().any(|c| c.link_id.is_some()) {
            return Err(invalid("unlink the music from its picture before composing it again"));
        }
        bed.sort_by(|a, b| a.timeline_start.total_cmp(&b.timeline_start));
        Ok(bed)
    }

    /// Render `score` and describe the result as an importable [`Asset`], *without* `&self`:
    /// composing runs for seconds to a minute (and may first download the SoundFont), so like
    /// [`Project::synthesize_voiceover`] it happens with the project lock released;
    /// [`Project::place_music`] takes it only to land the result. The asset carries its score.
    pub fn synthesize_music(score: &MusicScore, progress: engine::tts::ProgressFn, cancel: &dyn Fn() -> bool) -> Result<Asset> {
        let rendered = engine::compose::render(score, progress, cancel)?;
        tracing::info!(path = %rendered.path.display(), seconds = rendered.duration_s, cached = rendered.cached, "composed music");
        let mut asset = Self::probe_asset(&rendered.path)?;
        asset.name = format!("Music: {}", score.title());
        asset.music_score = Some(score.clone());
        Ok(asset)
    }

    /// Rehearse putting `score`'s render on `timeline` the way [`Project::place_music`] will, with
    /// the ripple the edit will have (the project's flag, or a [`Project::with_ripple`] override
    /// around the call): a stand-in asset of the render's length goes through the same placement
    /// and the same overlap check `edit_timeline` makes, so what it refuses is refused with the
    /// same words — the track is locked, the start lands on another clip, a longer recomposed
    /// piece would run into the next one — and with ripple on a piece that only *pushes* what is
    /// there passes. The render's real length is never shorter than the score's, so this can
    /// pass what placing then refuses by a frame; it never refuses what placing would take.
    fn check_music_fits(&self, timeline: &Timeline, req: &ComposeRequest, score: &MusicScore) -> Result<()> {
        let stand_in = Asset {
            id: Uuid::new_v4(),
            path: String::new(),
            name: String::new(),
            duration: score.duration_s(),
            streams: Vec::new(),
            imported_at: Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
            music_score: None,
            sound_effect: None,
            tags: Vec::new(),
        };
        let clip_id = req.replace_clip_id.unwrap_or_else(Uuid::new_v4);
        let mut after = timeline.clone();
        self.put_music(&mut after, &stand_in, req.track_id, req.at, req.replace_clip_id, clip_id)?;
        if self.ripple_active()? {
            after = after.ripple_lanes(timeline);
        }
        after.check_own_lanes(timeline, &[clip_id])
    }

    /// Land composed music: store the asset (reusing it when the same score was composed
    /// before), write its tempo and bar analysis **from the score** — the grid is exact, so
    /// `get_music_structure`, `fit_music` and beat snapping work at once, with no analysis pass —
    /// and put it on the timeline as one edit. All of it is one transaction: a placement that is
    /// refused (a locked track, a clip in the way, a score that does not resolve) stores neither
    /// the asset nor its analysis.
    ///
    /// With no `track_id` it goes on the audio track named [`MUSIC_TRACK`], made on first use;
    /// with no `timeline_start` after what that track holds *now*. A locked track is refused. With
    /// `replace_clip_id` the **whole music bed** is replaced: every clip of that clip's asset on its
    /// track (the segments `fit_music` made of one piece) becomes **one** clip of this asset, at the
    /// earliest of their starts and with the named clip's id, the first one's gain, fade-in and
    /// effects and the last one's fade-out; its length is the new render's and ripple applies to
    /// the clips after it as to any edit (without it the new clip must fit before the next one).
    pub fn place_music(
        &self,
        asset: &Asset,
        track_id: Option<Uuid>,
        timeline_start: Option<f64>,
        replace_clip_id: Option<Uuid>,
    ) -> Result<(Asset, Clip)> {
        let Some(score) = &asset.music_score else {
            return Err(invalid("that asset was not composed from a score"));
        };
        let rhythm = score.rhythm(asset.duration)?;
        let clip_id = replace_clip_id.unwrap_or_else(Uuid::new_v4);
        self.edit_lanes.borrow_mut().push(clip_id);
        let label = if replace_clip_id.is_some() {
            "Recompose music"
        } else {
            "Add music"
        };
        self.edit_timeline(label, |timeline| {
            // Inside the edit's transaction: if the placement is refused, so is the asset.
            let asset = self.insert_or_get_asset(asset)?;
            // The analysis the score implies; whatever an earlier pass measured for other kinds stays.
            let mut analysis = self.get_analysis(asset.id)?.unwrap_or_else(|| AssetAnalysis {
                asset_id: asset.id,
                ..AssetAnalysis::default()
            });
            analysis.merge(&AssetAnalysis {
                asset_id: asset.id,
                onsets: rhythm.onsets,
                tempo: rhythm.tempo,
                audio_class: rhythm.audio_class,
                music: rhythm.music,
                ran: vec![crate::model::AnalysisKind::Rhythm],
                ..AssetAnalysis::default()
            });
            self.set_analysis(&analysis)?;
            let clip = self.put_music(timeline, &asset, track_id, timeline_start, replace_clip_id, clip_id)?;
            Ok((asset, clip))
        })
    }

    /// Put `asset`'s music on `timeline` as clip `clip_id`: the one placement rule, shared by
    /// [`Project::place_music`] and the rehearsal before the render. Refuses a track that is
    /// missing, locked or not audio, and a replacement that is not an unlinked composed bed on
    /// an unlocked audio track.
    fn put_music(
        &self,
        timeline: &mut Timeline,
        asset: &Asset,
        track_id: Option<Uuid>,
        timeline_start: Option<f64>,
        replace_clip_id: Option<Uuid>,
        clip_id: Uuid,
    ) -> Result<Clip> {
        if let Some(replace) = replace_clip_id {
            self.music_bed(timeline, replace)?;
            return replace_bed(timeline, replace, asset);
        }
        let tid = match track_id {
            Some(t) => {
                let track = timeline.track(t).ok_or(Error::TrackNotFound(t))?;
                if track.kind != StreamKind::Audio {
                    return Err(invalid("music goes on an audio track"));
                }
                t
            }
            None => match timeline
                .tracks
                .iter()
                .find(|t| t.kind == StreamKind::Audio && t.name == MUSIC_TRACK)
            {
                Some(track) => track.id,
                None => {
                    let track = Track::new(StreamKind::Audio, MUSIC_TRACK.to_string());
                    let id = track.id;
                    timeline.tracks.push(track);
                    id
                }
            },
        };
        if let Some(track) = timeline.track(tid).filter(|t| t.locked) {
            return Err(invalid(format!("{} is locked", track.name)));
        }
        let start = timeline_start
            .map(|t| t.max(0.0))
            .unwrap_or_else(|| timeline.track(tid).map(Track::end).unwrap_or(0.0));
        let mut clip = Clip::for_asset(asset, 0.0, asset.duration, start);
        clip.id = clip_id;
        timeline
            .track_mut(tid)
            .expect("track resolved above")
            .clips
            .push(clip.clone());
        Ok(clip)
    }
}

/// Replace the whole music bed `clip_id` belongs to with one clip of `asset`, in `timeline`.
fn replace_bed(timeline: &mut Timeline, clip_id: Uuid, asset: &Asset) -> Result<Clip> {
    let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
    let track = &mut timeline.tracks[ti];
    if track.locked {
        return Err(invalid(format!("{} is locked", track.name)));
    }
    let asset_id = track.clips[ci].asset_id;
    let mut bed: Vec<Clip> = track.clips.iter().filter(|c| c.asset_id == asset_id).cloned().collect();
    bed.sort_by(|a, b| a.timeline_start.total_cmp(&b.timeline_start));
    let first = bed.first().expect("the named clip is in its bed").clone();
    let last = bed
        .iter()
        .max_by(|a, b| a.timeline_end().total_cmp(&b.timeline_end()))
        .expect("the named clip is in its bed");
    let mut clip = first.clone();
    clip.id = clip_id;
    clip.asset_id = asset.id;
    clip.source_in = 0.0;
    clip.source_out = asset.duration;
    clip.speed = 1.0;
    clip.speed_ramp.clear();
    clip.timeline_start = first.timeline_start;
    let duration = clip.duration();
    clip.fade_in = first.fade_in.min(duration);
    clip.fade_out = last.fade_out.min(duration);
    // The new clip takes the place of the first one of the bed in the track's order.
    let at = track
        .clips
        .iter()
        .position(|c| c.id == first.id)
        .expect("the first clip of the bed is on the track");
    track.clips.retain(|c| c.asset_id != asset_id);
    track.clips.insert(at.min(track.clips.len()), clip.clone());
    Ok(clip)
}

/// Where music placed with these arguments would start: the clip placement's own rule.
fn music_start(timeline: &Timeline, track_id: Option<Uuid>, at: Option<f64>) -> Result<f64> {
    let track = match track_id {
        Some(t) => {
            let track = timeline.track(t).ok_or(Error::TrackNotFound(t))?;
            if track.kind != StreamKind::Audio {
                return Err(invalid("music goes on an audio track"));
            }
            Some(track)
        }
        None => timeline
            .tracks
            .iter()
            .find(|t| t.kind == StreamKind::Audio && t.name == MUSIC_TRACK),
    };
    if let Some(track) = track.filter(|t| t.locked) {
        return Err(invalid(format!("{} is locked", track.name)));
    }
    Ok(at.map(|t| t.max(0.0)).unwrap_or_else(|| track.map(Track::end).unwrap_or(0.0)))
}

/// The times a score is fitted to: the boundaries *between* the clips of a video track (the very
/// first start and last end are the picture's edges, not cuts), or every marker.
fn cut_times(timeline: &Timeline, align: &ScoreAlign) -> Result<Vec<f64>> {
    match align.to {
        AlignTo::Markers => {
            if timeline.markers.is_empty() {
                return Err(invalid("there are no markers to align the music to"));
            }
            Ok(timeline.markers.iter().map(|m| m.time).collect())
        }
        AlignTo::Cuts => {
            let track = match align.track_id {
                Some(id) => {
                    let t = timeline.track(id).ok_or(Error::TrackNotFound(id))?;
                    if t.kind != StreamKind::Video {
                        return Err(invalid(
                            "cuts are the boundaries of a video track's clips: pick a video track",
                        ));
                    }
                    t
                }
                None => timeline
                    .tracks
                    .iter()
                    .find(|t| t.kind == StreamKind::Video)
                    .ok_or_else(|| invalid("there is no video track to align the music to"))?,
            };
            let mut edges: Vec<f64> = track
                .clips
                .iter()
                .flat_map(|c| [c.timeline_start, c.timeline_end()])
                .collect();
            edges.sort_by(f64::total_cmp);
            edges.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
            if edges.len() < 3 {
                return Err(invalid("the video track has no cuts: it holds a single clip"));
            }
            edges.remove(0);
            edges.pop();
            Ok(edges)
        }
    }
}

// ---- band balance --------------------------------------------------------------------------

/// What `get_band_balance` is asked: exactly one of `asset_id`, `clip_id` or `range`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BandBalanceRequest {
    /// An asset's whole sound. Times are the asset's own seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub asset_id: Option<Uuid>,
    /// A clip's source range, at the clip's gain. Times are source seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub clip_id: Option<Uuid>,
    /// The finished mix of the working timeline over this range (what `get_levels` measures).
    /// Times are timeline seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<TimeRange>,
    /// The stretches to compare, `{start, end, label?}` in the target's seconds. Without them: a
    /// composed asset's score sections, else the music's phrases (four bars) when it is analyzed,
    /// else eight equal windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<BandWindow>>,
}

/// Where the sound to measure comes from.
#[derive(Debug, Clone)]
pub enum BandSource {
    /// A file's audio from `start` for `secs`, times `gain`.
    File {
        path: PathBuf,
        start: f64,
        secs: f64,
        gain: f32,
    },
    /// The finished mix of a timeline.
    Mix {
        timeline: Box<Timeline>,
        assets: Vec<Asset>,
        range: Option<TimeRange>,
    },
}

/// A resolved band-balance request, ready to measure with the project lock released.
#[derive(Debug, Clone)]
pub struct BandPlan {
    pub source: BandSource,
    /// `asset`, `clip` or `mix`.
    pub target: &'static str,
    /// The asset's name, the clip's asset, or `mix`.
    pub name: String,
    /// The target's time at the start of the measured sound, and how long it lasts.
    pub origin: f64,
    pub span: f64,
    /// The windows, in the target's seconds, inside `origin..origin + span`.
    pub windows: Vec<BandWindow>,
    /// Where the windows came from: `sections`, `score`, `phrases` or `equal`.
    pub windows_from: &'static str,
}

/// What the balance of a target is.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BandBalance {
    pub target: &'static str,
    pub name: String,
    /// The measured stretch, in the target's seconds.
    pub start: f64,
    pub end: f64,
    pub windows: Vec<WindowBalance>,
    /// The whole stretch as one window.
    pub overall: WindowBalance,
    pub windows_from: &'static str,
    /// What stands out, in words.
    pub notes: Vec<String>,
}

/// Name a window for a section `kind`, numbered when the kind repeats.
fn section_labels(kinds: &[SectionKind]) -> Vec<String> {
    kinds
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let total = kinds.iter().filter(|o| *o == k).count();
            if total == 1 {
                k.name().to_string()
            } else {
                let n = kinds[..=i].iter().filter(|o| *o == k).count();
                format!("{} {n}", k.name())
            }
        })
        .collect()
}

/// Cut `windows` to `lo..hi`, dropping what is left under [`MIN_WINDOW_S`].
fn clip_windows(windows: Vec<BandWindow>, lo: f64, hi: f64) -> Vec<BandWindow> {
    windows
        .into_iter()
        .filter_map(|w| {
            let (start, end) = (w.start.max(lo), w.end.min(hi));
            (end - start >= MIN_WINDOW_S).then_some(BandWindow {
                start,
                end,
                label: w.label,
            })
        })
        .collect()
}

/// Groups of `bars` bars along a bar grid, up to `hi`: the phrases of analyzed music.
fn phrase_windows(first_downbeat: f64, bar_s: f64, lo: f64, hi: f64) -> Vec<BandWindow> {
    let mut bars = 4usize;
    let total = (((hi - first_downbeat) / bar_s).floor().max(0.0)) as usize;
    while total.div_ceil(bars) > 16 {
        bars *= 2;
    }
    let mut out = Vec::new();
    let mut k = 0;
    while k < total {
        let end_bar = (k + bars).min(total);
        out.push(BandWindow {
            start: first_downbeat + k as f64 * bar_s,
            end: first_downbeat + end_bar as f64 * bar_s,
            label: Some(if end_bar - k == 1 {
                format!("bar {}", k + 1)
            } else {
                format!("bars {}–{}", k + 1, end_bar)
            }),
        });
        k = end_bar;
    }
    clip_windows(out, lo, hi)
}

fn equal_windows(lo: f64, hi: f64) -> Vec<BandWindow> {
    let n = 8usize;
    let step = (hi - lo) / n as f64;
    if step < MIN_WINDOW_S {
        return vec![BandWindow {
            start: lo,
            end: hi,
            label: None,
        }];
    }
    (0..n)
        .map(|i| BandWindow {
            start: lo + i as f64 * step,
            end: lo + (i + 1) as f64 * step,
            label: Some(format!("{}/{n}", i + 1)),
        })
        .collect()
}

impl Project {
    /// Resolve a band-balance request under the lock: the target, the stretch of its sound and
    /// the windows. Nothing is decoded.
    pub fn band_balance_inputs(&self, req: &BandBalanceRequest) -> Result<BandPlan> {
        let named = [req.asset_id.is_some(), req.clip_id.is_some(), req.range.is_some()];
        if named.iter().filter(|n| **n).count() != 1 {
            return Err(invalid("name exactly one of asset_id, clip_id or range to measure"));
        }
        // The asset-shaped targets: the file, the stretch of it, the gain.
        let resolve_file = |asset: Asset,
                            start: f64,
                            secs: f64,
                            gain: f32,
                            target: &'static str|
         -> Result<(BandSource, &'static str, String, f64, f64, Asset)> {
            if !asset.has_audio() {
                return Err(invalid(format!("{} has no audio stream to measure", asset.name)));
            }
            if asset.is_image() {
                return Err(invalid(format!("{} is a still image", asset.name)));
            }
            Ok((
                BandSource::File {
                    path: PathBuf::from(&asset.path),
                    start,
                    secs,
                    gain,
                },
                target,
                asset.name.clone(),
                start,
                secs,
                asset,
            ))
        };
        let (source, target, name, origin, span, asset) = if let Some(id) = req.asset_id {
            let asset = self.require_asset(id)?;
            let secs = asset.duration;
            resolve_file(asset, 0.0, secs, 1.0, "asset")?
        } else if let Some(id) = req.clip_id {
            let timeline = self.working_timeline()?;
            let clip = timeline.clip(id).ok_or(Error::ClipNotFound(id))?;
            if clip.is_generated() {
                return Err(Error::InvalidArgument("that clip is a title, which has no sound".to_string()));
            }
            let asset = self.require_asset(clip.asset_id)?;
            let secs = (clip.source_out - clip.source_in).max(0.0);
            resolve_file(asset, clip.source_in, secs, clip.volume, "clip")?
        } else {
            let range = req.range.expect("one target was named");
            if !range.start.is_finite() || !range.end.is_finite() || range.start < 0.0 || range.end <= range.start {
                return Err(invalid("range needs 0 <= start < end, in seconds"));
            }
            let (timeline, assets) = self.levels_inputs()?;
            if let Some(why) = timeline.range_past_the_sound(range.start) {
                return Err(invalid(why));
            }
            let end = range.end.min(timeline.untitled_duration());
            let name = "mix".to_string();
            return self.band_plan_for_mix(timeline, assets, range, end, name, req);
        };
        if span < MIN_WINDOW_S {
            return Err(invalid(format!("the sound is {span:.2}s long: too short to measure")));
        }
        let (lo, hi) = (origin, origin + span);
        let (windows, windows_from) = match &req.sections {
            Some(given) => {
                let windows = check_sections(given, lo, hi)?;
                (windows, "sections")
            }
            None => {
                let from_score = asset.music_score.as_ref().and_then(|score| {
                    let spans = score.section_spans().ok()?;
                    let labels = section_labels(&spans.iter().map(|s| s.kind).collect::<Vec<_>>());
                    Some(clip_windows(
                        spans
                            .iter()
                            .zip(labels)
                            .map(|(s, label)| BandWindow {
                                start: s.start_s,
                                end: s.end_s,
                                label: Some(label),
                            })
                            .collect(),
                        lo,
                        hi,
                    ))
                });
                match from_score.filter(|w| !w.is_empty()) {
                    Some(w) => (w, "score"),
                    None => {
                        let grid = self
                            .get_analysis(asset.id)?
                            .and_then(|a| a.music)
                            .map(|m| (m.grid.first_downbeat(), m.grid.bar_s(), m.duration));
                        match grid
                            .map(|(first, bar, dur)| phrase_windows(first, bar, lo, hi.min(dur)))
                            .filter(|w| w.len() >= 2)
                        {
                            Some(w) => (w, "phrases"),
                            None => (equal_windows(lo, hi), "equal"),
                        }
                    }
                }
            }
        };
        Ok(BandPlan {
            source,
            target,
            name,
            origin,
            span,
            windows,
            windows_from,
        })
    }

    fn band_plan_for_mix(
        &self,
        timeline: Timeline,
        assets: Vec<Asset>,
        range: TimeRange,
        end: f64,
        name: String,
        req: &BandBalanceRequest,
    ) -> Result<BandPlan> {
        let span = end - range.start;
        if span < MIN_WINDOW_S {
            return Err(invalid(format!("the range is {span:.2}s long: too short to measure")));
        }
        let (windows, windows_from) = match &req.sections {
            Some(given) => (check_sections(given, range.start, end)?, "sections"),
            None => (equal_windows(range.start, end), "equal"),
        };
        Ok(BandPlan {
            source: BandSource::Mix {
                timeline: Box::new(timeline),
                assets,
                range: Some(TimeRange { start: range.start, end }),
            },
            target: "mix",
            name,
            origin: range.start,
            span,
            windows,
            windows_from,
        })
    }

    /// Measure a [`BandPlan`], *without* `&self`: the decode happens with the project lock
    /// released. Ungated like every read an agent looks at (capped to a share of the cores and
    /// niced), and `cancel` ends it with [`Error::Cancelled`].
    pub fn measure_band_balance(plan: &BandPlan, cancel: &dyn Fn() -> bool) -> Result<BandBalance> {
        let to_stream = |w: &BandWindow| BandWindow {
            start: (w.start - plan.origin).max(0.0),
            end: (w.end - plan.origin).max(0.0),
            label: w.label.clone(),
        };
        let mut windows: Vec<BandWindow> = plan.windows.iter().map(to_stream).collect();
        windows.push(BandWindow {
            start: 0.0,
            end: plan.span,
            label: None,
        });
        let mut meter = BandMeter::new(bands::SAMPLE_RATE, 2, &windows)?;
        match &plan.source {
            BandSource::File { path, start, secs, gain } => {
                let mut scaled: Vec<f32> = Vec::new();
                bands::stream_file(path, *start, Some(*secs), cancel, &mut |frames| {
                    if (*gain - 1.0).abs() < 1e-6 {
                        meter.push(frames);
                    } else {
                        scaled.clear();
                        scaled.extend(frames.iter().map(|s| s * gain));
                        meter.push(&scaled);
                    }
                })?;
            }
            BandSource::Mix { timeline, assets, range } => {
                let heard = engine::stream_mix_pcm(timeline, assets, *range, bands::SAMPLE_RATE, 2, cancel, &mut |frames| {
                    meter.push(frames);
                })?;
                if heard <= 0.0 {
                    return Err(invalid("the cut has no audio to measure"));
                }
            }
        }
        let mut measured = meter.finish();
        let mut overall = measured.pop().expect("the whole-span window was added");
        // Back to the target's clock.
        let shift = |w: &mut WindowBalance| {
            w.start += plan.origin;
            w.end += plan.origin;
        };
        measured.iter_mut().for_each(shift);
        shift(&mut overall);
        let notes = balance_notes(&overall, &measured);
        Ok(BandBalance {
            target: plan.target,
            name: plan.name.clone(),
            start: plan.origin,
            end: plan.origin + plan.span,
            windows: measured,
            overall,
            windows_from: plan.windows_from,
            notes,
        })
    }

    /// [`Project::band_balance_inputs`] and [`Project::measure_band_balance`] in one, holding
    /// `&self` for the whole decode. A caller that shares the project behind a lock resolves the
    /// plan, releases the lock and measures.
    pub fn band_balance(&self, req: &BandBalanceRequest) -> Result<BandBalance> {
        let plan = self.band_balance_inputs(req)?;
        Self::measure_band_balance(&plan, &|| false)
    }
}

/// Explicit sections: finite, ordered, inside the stretch; cut to it, and the ones left with
/// nothing are an error that names them.
fn check_sections(given: &[BandWindow], lo: f64, hi: f64) -> Result<Vec<BandWindow>> {
    if given.is_empty() {
        return Err(invalid("sections is empty: give at least one {start, end}"));
    }
    if given.len() > 100 {
        return Err(invalid("at most 100 sections"));
    }
    let mut out = Vec::with_capacity(given.len());
    for (i, w) in given.iter().enumerate() {
        if !w.start.is_finite() || !w.end.is_finite() || w.end <= w.start {
            return Err(invalid(format!("sections[{i}]: needs start < end, in seconds")));
        }
        let cut = clip_windows(vec![w.clone()], lo, hi);
        match cut.into_iter().next() {
            Some(w) => out.push(w),
            None => {
                return Err(invalid(format!(
                    "sections[{i}] ({:.1}..{:.1}s) has less than {MIN_WINDOW_S}s inside the measured sound ({lo:.1}..{hi:.1}s)",
                    w.start, w.end
                )))
            }
        }
    }
    Ok(out)
}

/// What stands out in a balance, in words: a dull or boomy whole, and the sections that depart.
fn balance_notes(overall: &WindowBalance, windows: &[WindowBalance]) -> Vec<String> {
    let mut notes = Vec::new();
    if overall.bands.is_empty() || overall.rms_dbfs.is_none() {
        notes.push("The sound is silent.".to_string());
        return notes;
    }
    let share = |w: &WindowBalance, name: &str| w.bands.iter().find(|b| b.name == name).map_or(0.0, |b| b.share_pct);
    let low = share(overall, "sub") + share(overall, "bass");
    if low > 60.0 {
        notes.push(format!(
            "The low end (sub + bass, under 250 Hz) holds {low:.0}% of the energy: heavy. A voice over it needs the music's low end \
             cut back (an EQ high-pass on the music) or the music turned down."
        ));
    }
    if share(overall, "sub") > 20.0 {
        notes.push(format!(
            "The sub band (20–60 Hz) holds {:.0}%: more than small speakers and phones play, and it eats headroom.",
            share(overall, "sub")
        ));
    }
    let mids = share(overall, "mid") + share(overall, "high_mid");
    if mids < 10.0 {
        notes.push(format!(
            "The mid range (500 Hz–6 kHz) holds only {mids:.0}%: thin or boomy, and a voice will have little room to be heard."
        ));
    }
    if share(overall, "air") < 1.0 && share(overall, "high_mid") < 6.0 {
        notes.push("Almost nothing above 2 kHz: the sound is dull.".to_string());
    }
    let loud: Vec<(&WindowBalance, f64)> = windows.iter().filter_map(|w| w.loudness_lufs.map(|l| (w, l))).collect();
    if loud.len() >= 2 {
        let name = |w: &WindowBalance| w.label.clone().unwrap_or_else(|| format!("{:.0}–{:.0}s", w.start, w.end));
        let hi = loud.iter().max_by(|a, b| a.1.total_cmp(&b.1)).expect("two or more");
        let lo = loud.iter().min_by(|a, b| a.1.total_cmp(&b.1)).expect("two or more");
        if hi.1 - lo.1 > 6.0 {
            notes.push(format!(
                "Loudness swings {:.0} LU between \"{}\" ({:.1} LUFS) and \"{}\" ({:.1} LUFS).",
                hi.1 - lo.1,
                name(hi.0),
                hi.1,
                name(lo.0),
                lo.1
            ));
        }
    }
    for w in windows {
        let d = share(w, "sub") + share(w, "bass") - low;
        if d > 15.0 {
            notes.push(format!(
                "\"{}\" is {d:.0} points bassier than the whole.",
                w.label.clone().unwrap_or_else(|| format!("{:.0}–{:.0}s", w.start, w.end))
            ));
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SectionKind;

    #[test]
    fn sections_are_named_and_numbered_when_a_kind_repeats() {
        let labels = section_labels(&[SectionKind::Intro, SectionKind::Drop, SectionKind::Verse, SectionKind::Drop]);
        assert_eq!(labels, vec!["intro", "drop 1", "verse", "drop 2"]);
    }

    #[test]
    fn phrases_come_four_bars_at_a_time_and_grow_for_a_long_piece() {
        // 2 s bars, 20 bars: five phrases of four.
        let w = phrase_windows(0.0, 2.0, 0.0, 40.0);
        assert_eq!(w.len(), 5);
        assert_eq!(w[0].label.as_deref(), Some("bars 1–4"));
        assert_eq!((w[1].start, w[1].end), (8.0, 16.0));
        // 200 bars would be 50 phrases: eight-bar groups still 25, so sixteen.
        let long = phrase_windows(0.0, 2.0, 0.0, 400.0);
        assert!(long.len() <= 16, "{}", long.len());
        assert_eq!(long[0].end - long[0].start, 32.0);
        // A trailing bar or two becomes a shorter last window.
        let w = phrase_windows(0.0, 2.0, 0.0, 18.0);
        assert_eq!(w.len(), 3);
        assert_eq!(w[2].label.as_deref(), Some("bar 9"));
    }

    #[test]
    fn equal_windows_split_the_stretch_in_eight() {
        let w = equal_windows(10.0, 18.0);
        assert_eq!(w.len(), 8);
        assert_eq!((w[0].start, w[7].end), (10.0, 18.0));
        assert!((w[3].end - w[4].start).abs() < 1e-12);
        // Too short to split: one window.
        assert_eq!(equal_windows(0.0, 1.0).len(), 1);
    }

    #[test]
    fn explicit_sections_are_cut_to_the_sound_and_bad_ones_named() {
        let given = vec![
            BandWindow {
                start: -5.0,
                end: 4.0,
                label: Some("a".into()),
            },
            BandWindow {
                start: 8.0,
                end: 99.0,
                label: None,
            },
        ];
        let w = check_sections(&given, 0.0, 10.0).unwrap();
        assert_eq!((w[0].start, w[0].end), (0.0, 4.0));
        assert_eq!((w[1].start, w[1].end), (8.0, 10.0));
        let e = check_sections(
            &[BandWindow {
                start: 20.0,
                end: 30.0,
                label: None,
            }],
            0.0,
            10.0,
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("sections[0]"), "{e}");
        assert!(check_sections(
            &[BandWindow {
                start: 3.0,
                end: 3.0,
                label: None
            }],
            0.0,
            10.0
        )
        .is_err());
        assert!(check_sections(&[], 0.0, 10.0).is_err());
    }

    #[test]
    fn the_notes_call_out_a_heavy_low_end_and_a_dull_top() {
        let band = |name: &'static str, share: f64| crate::engine::bands::BandLevel {
            name,
            low_hz: 0.0,
            high_hz: 0.0,
            energy_dbfs: Some(-20.0),
            share_pct: share,
        };
        let w = |label: &str, shares: [f64; 6], lufs: f64| WindowBalance {
            start: 0.0,
            end: 8.0,
            label: Some(label.to_string()),
            rms_dbfs: Some(-20.0),
            loudness_lufs: Some(lufs),
            bands: ["sub", "bass", "low_mid", "mid", "high_mid", "air"]
                .iter()
                .zip(shares)
                .map(|(n, s)| band(n, s))
                .collect(),
            dominant: Some("bass"),
        };
        let overall = w("all", [25.0, 50.0, 15.5, 5.0, 4.0, 0.5], -18.0);
        let notes = balance_notes(
            &overall,
            &[
                w("verse", [10.0, 40.0, 20.0, 15.0, 10.0, 5.0], -24.0),
                w("drop", [40.0, 52.0, 3.0, 2.0, 2.0, 1.0], -12.0),
            ],
        );
        let text = notes.join("\n");
        assert!(
            text.contains("low end") && text.contains("sub band") && text.contains("mid range") && text.contains("dull"),
            "{text}"
        );
        assert!(
            text.contains("Loudness swings 12 LU") && text.contains("\"drop\"") && text.contains("\"verse\""),
            "{text}"
        );
        assert!(text.contains("\"drop\" is"), "{text}");
        // A balanced one says nothing.
        let fine = w("all", [3.0, 22.0, 20.0, 30.0, 18.0, 7.0], -14.0);
        assert!(balance_notes(&fine, std::slice::from_ref(&fine)).is_empty());
    }

    #[test]
    fn a_score_with_cuts_on_the_beat_needs_no_tempo_change() {
        // A video track of three 4 s clips: cuts at 4 and 8 s, the first start and last end are edges.
        let asset = Uuid::new_v4();
        let mut timeline = crate::engine::test_support::timeline_of(vec![crate::engine::test_support::video_track(vec![
            Clip::new(asset, 0.0, 4.0, 0.0),
            Clip::new(asset, 0.0, 4.0, 4.0),
            Clip::new(asset, 0.0, 4.0, 8.0),
        ])]);
        let cuts = cut_times(
            &timeline,
            &ScoreAlign {
                to: AlignTo::Cuts,
                track_id: None,
                max_tempo_change: None,
            },
        )
        .unwrap();
        assert_eq!(cuts, vec![4.0, 8.0]);
        timeline.markers.push(crate::model::Marker {
            id: Uuid::new_v4(),
            time: 3.0,
            ..Default::default()
        });
        let m = cut_times(
            &timeline,
            &ScoreAlign {
                to: AlignTo::Markers,
                track_id: None,
                max_tempo_change: None,
            },
        )
        .unwrap();
        assert_eq!(m, vec![3.0]);
        let one = crate::engine::test_support::timeline_of(vec![crate::engine::test_support::video_track(vec![Clip::new(
            asset, 0.0, 4.0, 0.0,
        )])]);
        assert!(cut_times(
            &one,
            &ScoreAlign {
                to: AlignTo::Cuts,
                track_id: None,
                max_tempo_change: None
            }
        )
        .is_err());
    }
}
