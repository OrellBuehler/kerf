//! Domain model for a Kerf project: assets, cached analysis metadata, and the
//! non-destructive timeline (edit-decision-list).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::error::{Error, Result};

mod analysis;
mod animation;
mod asset;
mod captions;
mod clip;
mod delivery;
mod diff;
mod edit_modes;
mod look;
mod overlays;
mod range_edit;
mod reframe;
mod ripple;
mod track;

pub use analysis::*;
pub use animation::*;
pub use asset::*;
pub use captions::*;
pub use clip::*;
pub use delivery::*;
pub use diff::*;
pub use edit_modes::*;
pub use look::*;
pub use overlays::*;
pub use range_edit::*;
pub use reframe::*;
pub use ripple::*;
pub use track::*;

fn one() -> f64 {
    1.0
}

fn half() -> f64 {
    0.5
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

/// The last stage of the mix: what every track has been summed into, before
/// the delivery's loudness normalisation.
///
/// A fader and a safety limiter, nothing more — the point is that the *finished*
/// mix can be pulled down or kept under a ceiling without touching a single
/// track, clip or effect. In the export graph it sits after the final sum (and
/// the duck bus) and **before** `loudnorm`, so a normalised delivery is
/// normalised from the level the master was left at.
///
/// Every field is defaulted, and [`MasterBus::is_default`] keeps a project that
/// never touched the master from writing a `master` key at all, so such a
/// project reads and renders exactly as it did.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MasterBus {
    /// The master fader: a linear gain on the finished mix (`1.0` is unity).
    /// [`set_master_volume`](crate::Project::set_master_volume) clamps it to
    /// `0..=`[`MASTER_MAX_VOLUME`].
    #[serde(default = "unity_master_volume")]
    pub volume: f64,
    /// A lookahead limiter holding the finished mix under `ceiling_db`.
    #[serde(default)]
    pub limiter: bool,
    /// Where the limiter stops the signal, in dBFS (so `-1.0` keeps the mix a
    /// decibel under full scale). Only read while `limiter` is on, but kept
    /// while it is off, so switching the limiter back on returns to the
    /// ceiling that was chosen.
    ///
    /// A **sample-peak** ceiling unless `true_peak` is on: the plain limiter does not
    /// oversample, so the *true* peak of the result can sit a fraction of a dB higher
    /// ([`Project::levels`](crate::Project::levels) measures it). With `true_peak` the
    /// limiter runs oversampled and holds the true peak at the ceiling too.
    #[serde(default = "default_master_ceiling")]
    pub ceiling_db: f64,
    /// How tracks flagged `duck` dip under the rest of the mix. `None` (the default) is
    /// the sidechain compressor, which dips by how loud the rest is; `Some(depth)` is a
    /// **speech gate**: the ducked bus drops by exactly `depth` dB (negative, clamped to
    /// [`DUCK_MIN_DEPTH_DB`]`..=-1`) while the rest is above the gate's threshold, ramping
    /// down in ~50 ms and back up ~300 ms after it falls quiet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duck_depth_db: Option<f64>,
    /// The limiter holds the **true** peak: it runs on the mix upsampled four times, so
    /// the peaks between samples stay under `ceiling_db` too. Only read while `limiter`
    /// is on; off (the default) is the sample-peak limiter and its graph.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub true_peak: bool,
}

/// The speech gate's depth when it is switched on without one.
pub const DUCK_DEFAULT_DEPTH_DB: f64 = -12.0;
/// The deepest the speech gate ducks: a bed 40 dB down is as good as gone.
pub const DUCK_MIN_DEPTH_DB: f64 = -40.0;

/// The top of the master fader: +12 dB, as far as a fader has any business going
/// (the track faders stop at the same place).
pub const MASTER_MAX_VOLUME: f64 = 4.0;
/// The lowest ceiling the limiter can be given: `alimiter` takes a linear
/// `limit` of at least 0.0625, which is -24.08 dB.
pub const MASTER_MIN_CEILING_DB: f64 = -24.0;
/// The default limiter ceiling, in dBFS: `loudnorm`'s own true-peak target. The limiter
/// holds the *sample* peak, and the peak between samples runs above it (an 11 kHz tone
/// at a -1 dBFS ceiling read -0.2 dBTP, one at 15 kHz +0.1), so the default leaves a
/// half decibel under the -1 dBTP that platforms ask for.
pub const MASTER_DEFAULT_CEILING_DB: f64 = -1.5;

fn unity_master_volume() -> f64 {
    1.0
}

fn default_master_ceiling() -> f64 {
    MASTER_DEFAULT_CEILING_DB
}

impl Default for MasterBus {
    fn default() -> Self {
        Self {
            volume: 1.0,
            limiter: false,
            ceiling_db: MASTER_DEFAULT_CEILING_DB,
            duck_depth_db: None,
            true_peak: false,
        }
    }
}

impl MasterBus {
    /// Nothing set: what a project that never touched the master holds, and the
    /// only value that is not written to the `.kerf` file.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether the master leaves the mix alone — unity gain and no limiter. A
    /// neutral master adds nothing to the export graph, which is what keeps
    /// every graph from before the master existed byte-identical (a stored
    /// `ceiling_db` with the limiter off is not neutral by `is_default`, but is
    /// by this).
    pub fn is_neutral(&self) -> bool {
        !self.limiter && (self.safe_volume() - 1.0).abs() <= f64::EPSILON
    }

    /// The speech gate's depth as the graph will use it (finite, within
    /// [`DUCK_MIN_DEPTH_DB`]`..=-1`), or `None` for the compressor.
    pub fn safe_duck_depth_db(&self) -> Option<f64> {
        self.duck_depth_db.map(|d| {
            if d.is_finite() {
                d.clamp(DUCK_MIN_DEPTH_DB, -1.0)
            } else {
                DUCK_DEFAULT_DEPTH_DB
            }
        })
    }

    /// The fader as the graph will use it: finite, within `0..=`
    /// [`MASTER_MAX_VOLUME`]. A `.kerf` file never goes through the op that
    /// clamps, and `NaN` or a negative gain must not reach `volume=`.
    pub fn safe_volume(&self) -> f64 {
        if self.volume.is_finite() {
            self.volume.clamp(0.0, MASTER_MAX_VOLUME)
        } else {
            1.0
        }
    }

    /// The ceiling as the graph will use it: finite, within
    /// [`MASTER_MIN_CEILING_DB`]`..=0`.
    pub fn safe_ceiling_db(&self) -> f64 {
        if self.ceiling_db.is_finite() {
            self.ceiling_db.clamp(MASTER_MIN_CEILING_DB, 0.0)
        } else {
            MASTER_DEFAULT_CEILING_DB
        }
    }

    /// The limiter's ceiling as a linear amplitude (`alimiter`'s `limit`).
    pub fn limit_linear(&self) -> f64 {
        10f64.powf(self.safe_ceiling_db() / 20.0)
    }

    /// The master fader in dB, or `None` at zero gain (silence).
    pub fn volume_db(&self) -> Option<f64> {
        let v = self.safe_volume();
        (v > 0.0).then(|| 20.0 * v.log10())
    }
}

fn master_is_default(master: &MasterBus) -> bool {
    master.is_default()
}

/// The non-destructive timeline (EDL): a set of multi-kind tracks, the text
/// overlays (titles / lower-thirds / captions) drawn over the composited
/// picture, and the user's markers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeline {
    pub tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<TextOverlay>,
    /// Animated SVG titles, drawn above the clips and below `overlays` (so captions stay on top), in
    /// list order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub svg_overlays: Vec<SvgOverlay>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
    /// The frame this cut is being made for. `None` (the default, and every
    /// timeline saved before this existed) keeps the historical behaviour:
    /// the shape follows the first video clip's footage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Delivery>,
    /// The master bus: the fader and limiter the finished mix passes through.
    /// Defaulted and not written while untouched (see [`MasterBus`]).
    #[serde(default, skip_serializing_if = "master_is_default")]
    pub master: MasterBus,
    /// Pictures designed beside the cut (see [`thumbnail`]): stored with it, never drawn in it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thumbnails: Vec<Thumbnail>,
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Timeline {
    /// A fresh timeline with one video and one audio track.
    pub fn new() -> Self {
        Self {
            tracks: vec![Track::new(StreamKind::Video, "V1"), Track::new(StreamKind::Audio, "A1")],
            overlays: Vec::new(),
            svg_overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: MasterBus::default(),
            thumbnails: Vec::new(),
        }
    }

    pub fn overlay(&self, id: Uuid) -> Option<&TextOverlay> {
        self.overlays.iter().find(|o| o.id == id)
    }

    /// Whether a clip of the timeline comes in through a dip to white. In an export the dip's `fade c=white` (RGB only) is a
    /// filter of every frame of such a clip and of the one before it, so a renderer that draws it asks what it needs for it
    /// (the conversions to and from RGB) before any frame. Only the clips the export has ([`Timeline::for_render`]: on video
    /// tracks that render, enabled) count: a muted or solo-shadowed track and a disabled clip are not in its graph.
    pub fn has_white_dip(&self) -> bool {
        self.tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video && self.track_renders(t))
            .flat_map(|t| &t.clips)
            .any(|c| c.enabled && c.transition_in.as_ref().is_some_and(|t| t.kind == TransitionKind::DipToWhite))
    }

    pub fn svg_overlay(&self, id: Uuid) -> Option<&SvgOverlay> {
        self.svg_overlays.iter().find(|o| o.id == id)
    }

    /// The frame preset artwork is drawn for: the delivery format, else full HD.
    pub fn preset_frame(&self) -> (u32, u32) {
        self.format.map_or(crate::svg::DEFAULT_FRAME, |f| (f.width, f.height))
    }

    /// Re-make every preset overlay's artwork for `frame` (the delivery frame changed) and the
    /// length it now has.
    pub fn regenerate_svg_presets(&mut self, frame: (u32, u32)) {
        for o in &mut self.svg_overlays {
            o.regenerate(frame);
        }
    }

    pub fn track(&self, id: Uuid) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: Uuid) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// The id of the first track of a given kind, if any.
    pub fn first_track_of(&self, kind: StreamKind) -> Option<Uuid> {
        self.tracks.iter().find(|t| t.kind == kind).map(|t| t.id)
    }

    /// The first track of `kind` that is not locked: where an edit that names no track puts a clip.
    pub fn first_unlocked_track_of(&self, kind: StreamKind) -> Option<Uuid> {
        self.tracks.iter().find(|t| t.kind == kind && !t.locked).map(|t| t.id)
    }

    /// Beat timestamps of the audio tracks mapped onto the timeline, ascending
    /// and de-duplicated. `tempos` supplies each asset's cached tempo; estimates
    /// below [`BEAT_MIN_CONFIDENCE`] are ignored, so a non-rhythmic source
    /// contributes nothing rather than a grid of noise.
    pub fn beat_grid(&self, tempos: &HashMap<Uuid, Tempo>) -> Vec<f64> {
        let mut times = Vec::new();
        for track in &self.tracks {
            if track.kind != StreamKind::Audio {
                continue;
            }
            for clip in &track.clips {
                let Some(tempo) = tempos.get(&clip.asset_id) else {
                    continue;
                };
                if tempo.confidence < BEAT_MIN_CONFIDENCE || tempo.bpm <= 0.0 {
                    continue;
                }
                for &beat in &tempo.beats {
                    if beat >= clip.source_in && beat <= clip.source_out {
                        times.push(clip.source_to_timeline(beat));
                    }
                }
            }
        }
        times.sort_by(f64::total_cmp);
        // Overlapping clips of one asset repeat the same beats; drop the copies.
        times.dedup_by(|a, b| (*a - *b).abs() <= 0.005);
        times
    }

    /// Bar starts of the audio tracks in timeline seconds: [`Self::beat_grid`] over the
    /// tempos' `downbeats`. Empty when no music has a fitted bar grid.
    pub fn bar_grid(&self, tempos: &HashMap<Uuid, Tempo>) -> Vec<f64> {
        let bars: HashMap<Uuid, Tempo> = tempos
            .iter()
            .map(|(id, t)| {
                (
                    *id,
                    Tempo {
                        beats: t.downbeats.clone(),
                        ..t.clone()
                    },
                )
            })
            .collect();
        self.beat_grid(&bars)
    }

    /// Find a clip by id, returning `(track_index, clip_index)`.
    pub fn locate(&self, clip_id: Uuid) -> Option<(usize, usize)> {
        for (ti, track) in self.tracks.iter().enumerate() {
            if let Some(ci) = track.clips.iter().position(|c| c.id == clip_id) {
                return Some((ti, ci));
            }
        }
        None
    }

    pub fn clip(&self, clip_id: Uuid) -> Option<&Clip> {
        self.locate(clip_id).map(|(ti, ci)| &self.tracks[ti].clips[ci])
    }

    /// Total timeline duration (seconds).
    pub fn duration(&self) -> f64 {
        self.tracks.iter().map(Track::end).fold(0.0, f64::max)
    }

    /// Whether any track of `kind` is soloed. While one is, the rest of that
    /// kind are silent/hidden — and the kinds solo independently, so soloing a
    /// music bed does not blank the picture.
    pub fn has_solo(&self, kind: StreamKind) -> bool {
        self.tracks.iter().any(|t| t.kind == kind && t.solo)
    }

    /// Whether this track's clips reach the render at all: muted tracks never
    /// do, and while any track of its kind is soloed, only the soloed ones do.
    pub fn track_renders(&self, track: &Track) -> bool {
        !track.muted && (!self.has_solo(track.kind) || track.solo)
    }

    /// The timeline as it should actually be rendered: muted (and solo-shadowed)
    /// tracks and disabled clips removed.
    ///
    /// Filtering here rather than inside the graph builders is deliberate. The
    /// export and still paths index clips by a flat position that `plan_inputs`,
    /// the `ClipFx` table and every `[v{n}]` label agree on, so dropping clips
    /// mid-graph would mean renumbering all of it. Handing those builders a
    /// timeline that simply does not contain the silenced clips keeps them — and
    /// their tests — untouched.
    ///
    /// Empty tracks are kept: a track carries `duck`, which the audio mix reads
    /// even when the track contributes nothing.
    pub fn for_render(&self) -> Timeline {
        Timeline {
            tracks: self
                .tracks
                .iter()
                .map(|track| Track {
                    clips: if self.track_renders(track) {
                        track.clips.iter().filter(|c| c.enabled).cloned().collect()
                    } else {
                        Vec::new()
                    },
                    ..track.clone()
                })
                .collect(),
            overlays: self.overlays.clone(),
            svg_overlays: self.svg_overlays.clone(),
            markers: self.markers.clone(),
            format: self.format,
            master: self.master,
            thumbnails: Vec::new(),
        }
    }

    /// The same cut delivered at another frame: a copy whose format is
    /// `delivery` and whose clips wear the crop they carry for *that* shape.
    ///
    /// This is the multi-format export's whole trick, and it is the same one
    /// `for_render` uses — change the timeline, not the graph. A clip framed for
    /// the shape ([`Clip::framing_for`]) swaps that crop in for its transform's;
    /// one that never was keeps whatever crop it has, since the alternative is
    /// throwing away a crop someone made by hand. Generated captions are re-fit
    /// to the new aspect the way [`Timeline::captions`] fit them to the old one
    /// (a 9:16 frame is half as wide, and `drawtext` draws off the edge rather
    /// than wrapping); a typed title is left alone. Delivering the shape the
    /// project is already cut for changes only the size. The copy keeps the
    /// project's frame rate unless `delivery` names its own.
    pub fn for_delivery(&self, delivery: Delivery) -> Timeline {
        let ratio = delivery.ratio();
        let same_shape = self.format.is_some_and(|f| f.ratio() == ratio);
        let aspect = delivery.aspect();
        Timeline {
            tracks: self
                .tracks
                .iter()
                .map(|track| Track {
                    clips: track
                        .clips
                        .iter()
                        .map(|clip| {
                            let mut clip = clip.clone();
                            if !same_shape {
                                if let Some(f) = clip.framing_for(ratio).copied() {
                                    let t = &mut clip.transform;
                                    (t.crop_left, t.crop_right, t.crop_top, t.crop_bottom) = f.crop();
                                }
                            }
                            clip
                        })
                        .collect(),
                    ..track.clone()
                })
                .collect(),
            overlays: self
                .overlays
                .iter()
                .map(|o| {
                    let mut o = o.clone();
                    if o.generated && !same_shape {
                        o.size = fit_size(&o.text, o.size, aspect);
                    }
                    o
                })
                .collect(),
            // Preset artwork is drawn for a frame (its viewBox is the frame), so it is re-made for
            // the new shape; custom artwork is the author's and is only ever scaled.
            svg_overlays: self
                .svg_overlays
                .iter()
                .map(|o| {
                    let mut o = o.clone();
                    if !same_shape {
                        o.regenerate((delivery.width, delivery.height));
                    }
                    o
                })
                .collect(),
            markers: self.markers.clone(),
            // A variant is a shape of the same cut: it keeps the project's frame rate unless
            // it names its own.
            format: Some(Delivery {
                fps: delivery.fps.or_else(|| self.format.and_then(|f| f.fps)),
                ..delivery
            }),
            // The same mix at every frame: a variant changes the picture only.
            master: self.master,
            thumbnails: self.thumbnails.clone(),
        }
    }

    /// The span a range export of `range` writes: `range` clamped to the cut, or `None` when there is
    /// none or it is empty after clamping — an inverted, empty or wholly-past-the-end range falls back
    /// to exporting everything. The one clamp the export and the platform check both read.
    pub fn export_span(&self, range: Option<(f64, f64)>) -> Option<(f64, f64)> {
        let (start, end) = range?;
        let start = start.max(0.0);
        let end = end.min(self.duration());
        (end - start > 1e-9).then_some((start, end))
    }

    /// A copy containing only `[start, end)`, shifted so `start` lands at 0 —
    /// the sub-timeline a range export renders. Clips overlapping the window
    /// edges are cut down (source window and keyframes adjusted, honoring speed
    /// and reverse); fades and transitions belonging to a removed edge are
    /// dropped; overlays are clipped and shifted the same way. A clip cut at
    /// the front keeps its animated pose by sampling a replacement keyframe at
    /// the new start.
    pub fn slice(&self, start: f64, end: f64) -> Timeline {
        let mut out = Timeline {
            tracks: Vec::with_capacity(self.tracks.len()),
            overlays: Vec::new(),
            svg_overlays: Vec::new(),
            // Markers inside the window come along, shifted like everything else;
            // without this a range export would desync every one of them.
            markers: self
                .markers
                .iter()
                .filter(|m| m.time >= start && m.time < end)
                .map(|m| Marker {
                    time: m.time - start,
                    duration: (m.end().min(end) - m.time).max(0.0),
                    ..m.clone()
                })
                .collect(),
            // A slice is still the same delivery: range export and playback both
            // build from one, and either would otherwise fall back to footage shape.
            format: self.format,
            // ...and the same mix: a range export of a limited master is limited.
            master: self.master,
            thumbnails: Vec::new(),
        };
        for track in &self.tracks {
            let mut t = Track {
                clips: Vec::new(),
                ..track.clone()
            };
            for clip in &track.clips {
                let (cs, ce) = (clip.timeline_start, clip.timeline_end());
                if ce <= start || cs >= end {
                    continue;
                }
                let mut c = clip.clone();
                let cut_front = (start - cs).max(0.0);
                let cut_back = (ce - end).max(0.0);
                // Both cuts are read off the ramp as the clip has it, before the front rebases it.
                let (front_src, back_src) = (clip.along(cut_front), clip.tail_shift(-cut_back));
                if cut_front > 0.0 {
                    if c.is_reversed() {
                        c.source_out -= front_src;
                    } else {
                        c.source_in += front_src;
                    }
                    c.fade_in = 0.0;
                    c.transition_in = None;
                    // Animation re-timed to the new start, the pose it lands on pinned
                    // as a key at 0 (transform and reframe camera alike).
                    c.rebase_animation(cut_front);
                }
                if cut_back > 0.0 {
                    if c.is_reversed() {
                        c.source_in -= back_src;
                    } else {
                        c.source_out += back_src;
                    }
                    c.fade_out = 0.0;
                }
                c.timeline_start = (cs - start).max(0.0);
                t.clips.push(c);
            }
            out.tracks.push(t);
        }
        for o in &self.overlays {
            if o.end <= start || o.start >= end {
                continue;
            }
            let mut ov = o.clone();
            let cut_front = (start - o.start).max(0.0);
            if cut_front > 0.0 && !ov.keyframes.is_empty() {
                let (pos_x, pos_y, opacity) = o.sample(start);
                let mut kfs = vec![TextKeyframe {
                    time: 0.0,
                    pos_x,
                    pos_y,
                    opacity,
                }];
                kfs.extend(ov.keyframes.iter().filter(|k| k.time > cut_front).map(|k| TextKeyframe {
                    time: k.time - cut_front,
                    ..*k
                }));
                ov.keyframes = kfs;
            }
            ov.start = (o.start - start).max(0.0);
            ov.end = (o.end.min(end) - start).max(ov.start);
            out.overlays.push(ov);
        }
        for o in &self.svg_overlays {
            if o.end <= start || o.start >= end {
                continue;
            }
            let mut ov = o.clone();
            // The animation is not replayed from its first frame: the part that was cut off the
            // front has already elapsed on the document's clock.
            ov.offset += (start - o.start).max(0.0);
            ov.start = (o.start - start).max(0.0);
            ov.end = (o.end.min(end) - start).max(ov.start);
            out.svg_overlays.push(ov);
        }
        out
    }
}

mod behavior;
mod channels;
mod motion;
mod ramp_edit;
#[cfg(test)]
mod ramp_link_tests;
#[cfg(test)]
mod ramp_tests;
mod speed_ramp;
mod stabilize;
#[cfg(test)]
pub(crate) use behavior::with_libm_ulps;
pub use behavior::{
    check_behaviors, shake_phase, Baked, Behavior, BehaviorKind, Wave, MAX_BEHAVIORS, MAX_BEHAVIOR_HZ, MAX_BEHAVIOR_PHASE,
    MAX_BREATHE, MAX_DRIFT, MAX_SHAKE, MAX_SPIN_TURNS, MAX_SWAY_DEGREES,
};
use channels::channel_changes;
pub use channels::{key_polyline, Property, PropertyKey, PropertyTrack, MAX_CHANNEL_VOLUME};
pub use motion::{
    motion_presets, MotionKind, MotionMode, MotionPreset, MotionPresetInfo, MAX_MOTION_INTENSITY, MIN_MOTION_INTENSITY,
    MIN_MOTION_SECONDS,
};
pub use ramp_edit::check_speed_ramp;
pub use speed_ramp::{ramp_is_valid, RampEasing, SpeedKey, MAX_RAMP_TIME, MAX_SPEED_KEYS};
pub use stabilize::{Stabilize, MAX_STABILIZE_ZOOM};

mod links;
pub use links::{Detached, DetachedMany, SkippedDetach};

mod cut_transcript;
pub use cut_transcript::{CutLine, CutWord};

mod subset;
pub use subset::{AutoIncluded, SkippedChange, SubsetMerge};
mod grade;
pub use grade::{
    frame_matrix, frame_matrix_tag, matrix_tag_is_unlisted, normalize_curve, ClipLut, CurvePoint, CurveSegment, CurveShape,
    Curves, Grade, Wheel, Wheels, MAX_CURVE_POINTS,
};
mod check;
mod generator;
pub mod thumbnail;
pub use thumbnail::{Thumbnail, ThumbnailLayer, VideoGraphics, MAX_THUMBNAILS, MAX_THUMBNAIL_LAYERS};
mod values;
pub(crate) use generator::too_many_titles;
pub use generator::{Generator, TitleClip, GENERATED_ASSET, MAX_TITLE_CLIPS};

mod tags;
pub use tags::{
    dedupe_tags, group_by_key, normalize_key, same_tags, tags_match, Grouping, KeyGroup, Tag, TagCount, TagEdit, TagFilter,
    TagKeyCount, TagSummary, MAX_ASSET_TAGS, MAX_TAG_KEY, MAX_TAG_VALUE,
};
mod score;
pub use score::{
    bar_seconds, duration_s as score_duration_s, expand as expand_score, plan_score_to_picture, AlignOptions, AlignReport,
    Arrangement, BeatReading, Chord, Colour, CutAlignment, HitKind, InstrumentOverrides, Key, Makeup, Mix, Mode, MusicScore,
    MusicStyle, NoteEvent, Quality, Resolved, Role, ScoreHit, ScoreSection, SectionKind, SectionSpan, Sidechain,
    DEFAULT_MAX_TEMPO_CHANGE, MAX_BPM, MAX_HITS, MAX_SCORE_BARS, MAX_SCORE_SECONDS, MAX_SECTIONS, MAX_SECTION_BARS, MAX_SEED,
    MIN_BPM, RELEASE_TAIL_S, ROLES,
};

#[cfg(test)]
pub(crate) use score::{sample_song, style_shares};

mod sfx;
pub(crate) use sfx::span as sfx_span;
pub use sfx::{
    anchor_s as sfx_anchor_s, catalogue as sfx_catalogue, SfxAnchor, SfxCategory, SfxKind, SfxPreset, SfxSpec,
    DEFAULT_SFX_INTENSITY, DEFAULT_SFX_TAIL, MAX_SFX_PITCH, MIN_SFX_CLIP_S, SFX_HEAD_SNAP_S,
};

mod sfx_suggest;
pub use sfx_suggest::{
    suggest as suggest_sound_effects, SfxDensity, SfxSource, SfxSuggestInputs, SfxSuggestRequest, SfxSuggestion,
};

mod music_fit;
pub use music_fit::{
    music_fit_clips, plan_music_fit, to_samples, MusicFit, MusicFitReport, MusicSegment, FIT_FADE_S, MAX_FIT_BARS,
    SPLICE_CROSSFADE_S,
};

/// Lifecycle of a task in the agent queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Waiting for an agent to claim it.
    Queued,
    /// Claimed by an agent and in progress.
    Working,
    /// The agent finished; the resulting edit is staged for the user to review.
    Ready,
    /// Accepted by the user — or finished by the agent with review off, when its edits already
    /// landed on the live cut as it made them and there is nothing left to accept.
    Done,
    /// The agent could not complete it.
    Failed,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Working => "working",
            TaskStatus::Ready => "ready",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => TaskStatus::Queued,
            "working" => TaskStatus::Working,
            "ready" => TaskStatus::Ready,
            "done" => TaskStatus::Done,
            "failed" => TaskStatus::Failed,
            _ => return None,
        })
    }
}

/// A unit of work in the agent queue. A human (or a planning agent) enqueues a
/// `prompt`; a connected LLM claims it over MCP, performs timeline edits through
/// the same engine the GUI uses, then marks it `ready` (or `done` with review off,
/// or `failed`). Kerf never edits on its own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub prompt: String,
    pub status: TaskStatus,
    /// The agent's summary on completion, or the error message on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests;
