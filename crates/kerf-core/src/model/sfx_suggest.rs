//! **Suggesting sound effects**: where a cut would take a whoosh, a title a pop, a drop a riser and an
//! impact — found from the cut itself and the analysis Kerf already has. Pure and unit-tested.
//!
//! [`suggest`] reads a working timeline, its assets and their cached analysis
//! ([`Project::suggest_sound_effects_inputs`](crate::Project::suggest_sound_effects_inputs)) and
//! returns [`SfxSuggestion`]s: a [`SfxPlacement`] exactly as `add_sound_effects` takes it, the picture
//! event that asked for it, a short sentence saying why, and a score. Each *moment* in the picture
//! (a cut, a title appearing, a drop) is found from one source and scored; moments closer together than
//! the density allows are thinned, **the higher score winning**, and what is left gets a kind, a seed
//! and a gain. Nothing is random: the same cut gives the same suggestions.
//!
//! The thresholds are constants here so they can be tuned in one place, and every rule that leaves a
//! moment out (a cut that continues a shot, a moment an effect already covers, a riser that would start
//! before the timeline) is one an editor would also have left out.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    fmt_time, sfx_anchor_s, Asset, AssetAnalysis, AudioClass, Clip, HitKind, MusicAnalysis, SectionKind, SfxCategory, SfxKind,
    SfxSpec, StreamKind, SvgOverlay, SvgPresetKind, TimeRange, Timeline, Transition, MIN_SFX_CLIP_S,
};
use crate::error::{Error, Result};
use crate::project::{SfxPlacement, MAX_SFX_PER_CALL};
use crate::review::{head_source, pace_gap, scale_jump, tail_source, PUNCH_IN_SCALE};
use crate::transcript::words_of;

/// A moment this close (seconds) to an effect already on the timeline — before its start, after its
/// end, or inside it — is skipped.
pub const COVER_S: f64 = 0.25;
/// The gain of a suggestion in the clear, dB.
pub const CLEAR_GAIN_DB: f64 = -3.0;
/// The gain of one over speech, dB: the effect sits under the voice.
pub const SPEECH_GAIN_DB: f64 = -8.0;
/// Words this far apart (seconds) or less are one stretch of speech.
const SPEECH_GAP_S: f64 = 0.35;
/// A moment this far (seconds) outside a stretch of speech is still over it.
const SPEECH_SLACK_S: f64 = 0.1;
/// A title shown for less than this (seconds) is a flicker, not a title.
const MIN_TITLE_S: f64 = 0.3;
/// Centred text of at most this many characters pops; longer text swipes in.
const SHORT_TITLE_CHARS: usize = 32;
/// Text or artwork centred at or below this fraction of the frame height is a lower third.
const LOWER_THIRD_Y: f64 = 0.6;
/// A riser needs this much music (seconds) before the drop; with less there is no lead-in.
const MIN_LEAD_IN_S: f64 = 1.0;
/// A scene change this close (seconds) to the edge of its clip is the clip's own cut.
const SCENE_MARGIN_S: f64 = 0.3;
/// A score hit this close (seconds) to a drop already marks it.
const HIT_NEAR_S: f64 = 0.35;
/// Analysed music: the bars either side of a possible section change that are compared.
const SECTION_BARS: usize = 4;
/// Analysed music: how different (cosine distance of the mean chroma) two stretches must be to be
/// two sections.
const SECTION_NOVELTY: f64 = 0.25;
/// Analysed music is only trusted to have sections when it was classified as music this surely.
const MUSIC_CONFIDENCE: f64 = 0.5;
/// Analysed music: a section change this close (seconds) to the edge of its clip is the clip's own cut.
const MUSIC_EDGE_S: f64 = 0.5;

/// How many suggestions: the fewest and furthest apart, or the most.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SfxDensity {
    /// Transitions, titles, drops and markers, at least 4 s apart.
    Subtle,
    /// Adds the cuts between shots of 1.5 s and longer (and the punch-ins among them); at least 2 s apart.
    #[default]
    Balanced,
    /// Everything — every cut, the opening hit — at least 1 s apart.
    Punchy,
}

impl SfxDensity {
    pub const ALL: [SfxDensity; 3] = [SfxDensity::Subtle, SfxDensity::Balanced, SfxDensity::Punchy];

    /// The snake_case name used in JSON.
    pub fn name(self) -> &'static str {
        match self {
            SfxDensity::Subtle => "subtle",
            SfxDensity::Balanced => "balanced",
            SfxDensity::Punchy => "punchy",
        }
    }

    /// The closest two suggested moments may be, seconds.
    pub fn spacing_s(self) -> f64 {
        match self {
            SfxDensity::Subtle => 4.0,
            SfxDensity::Balanced => 2.0,
            SfxDensity::Punchy => 1.0,
        }
    }

    /// The shortest shot (on either side of a cut) a cut is suggested on, or `None` when cuts are not.
    pub fn min_shot_s(self) -> Option<f64> {
        match self {
            SfxDensity::Subtle => None,
            SfxDensity::Balanced => Some(1.5),
            SfxDensity::Punchy => Some(0.0),
        }
    }
}

/// The picture event a suggestion is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SfxSource {
    /// A hard cut between two pictures on a video track (or a scene change inside a clip): a whoosh or swoosh.
    Cut,
    /// A dissolve, dip, slide or push: a swoosh lasting the transition, anchored at its middle.
    Transition,
    /// A title, lower third or animated SVG title appearing: a pop (short text), a swipe (lower third) or a typewriter.
    Title,
    /// The same shot cut with a jump in scale (`PUNCH_IN_SCALE` or more): a punch.
    PunchIn,
    /// A drop in composed music, or a section change in analysed music: a riser ending on it and an impact on it.
    MusicDrop,
    /// A marker named `sfx:<kind>` (or `sfx`): that kind, at the marker.
    Marker,
    /// Punchy only: an impact on the first picture, the hook.
    Opening,
}

impl SfxSource {
    pub const ALL: [SfxSource; 7] = [
        SfxSource::Cut,
        SfxSource::Transition,
        SfxSource::Title,
        SfxSource::PunchIn,
        SfxSource::MusicDrop,
        SfxSource::Marker,
        SfxSource::Opening,
    ];

    /// The snake_case name used in JSON.
    pub fn name(self) -> &'static str {
        match self {
            SfxSource::Cut => "cut",
            SfxSource::Transition => "transition",
            SfxSource::Title => "title",
            SfxSource::PunchIn => "punch_in",
            SfxSource::MusicDrop => "music_drop",
            SfxSource::Marker => "marker",
            SfxSource::Opening => "opening",
        }
    }
}

/// What to suggest. `{}` is the whole cut at balanced density.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SfxSuggestRequest {
    /// Only suggestions whose hit falls in `[start, end)`, timeline seconds. The whole cut is read
    /// either way, so a range holds exactly what the same call over the whole cut puts there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<TimeRange>,
    /// How many: `subtle`, `balanced` (default) or `punchy`.
    #[serde(default)]
    pub density: SfxDensity,
    /// Only effects of these categories (`transition`, `impact`, `build`, `ui`, `texture`). With
    /// `kinds`, an effect of either list is kept. Both empty is every effect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<SfxCategory>,
    /// Only these kinds (`whoosh`, `pop`, …); see `categories`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<SfxKind>,
}

impl SfxSuggestRequest {
    /// Check the request: a range is finite, starts at 0 or later and ends after it starts.
    pub fn check(&self) -> Result<()> {
        if let Some(r) = self.range {
            if !r.start.is_finite() || !r.end.is_finite() || r.start < 0.0 || r.end <= r.start {
                return Err(Error::InvalidArgument("range needs 0 <= start < end, in seconds".to_string()));
            }
        }
        Ok(())
    }

    fn allows(&self, kind: SfxKind) -> bool {
        (self.categories.is_empty() && self.kinds.is_empty())
            || self.kinds.contains(&kind)
            || self.categories.contains(&kind.category())
    }
}

/// One effect worth adding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SfxSuggestion {
    /// Exactly what `add_sound_effects` takes: the kind, its seed, where its hit lands and the gain
    /// (lowered over speech). Suggestions never name a track; the effects lanes allocate.
    pub placement: SfxPlacement,
    /// The picture event it is for.
    pub source: SfxSource,
    /// Why, as a sentence: "Cut from interview.mp4 to broll.mp4 at 0:04.2".
    pub reason: String,
    /// 0..=1: how much the moment wants a sound. The higher score wins when two are closer than the density allows.
    pub score: f64,
}

/// What suggesting reads, resolved under the project lock: the working timeline, the assets and the
/// cached analysis of the assets the timeline uses.
#[derive(Debug, Clone)]
pub struct SfxSuggestInputs {
    pub timeline: Timeline,
    pub assets: Vec<Asset>,
    pub analyses: HashMap<Uuid, AssetAnalysis>,
}

// ---- moments -------------------------------------------------------------------------------

type Choices = (&'static [SfxKind], &'static [SfxKind]);

/// A cut: alternating whoosh and swoosh; a swipe only when the others are filtered out.
const CUT: Choices = (&[SfxKind::Whoosh, SfxKind::Swoosh], &[SfxKind::Swipe]);
const TRANSITION: Choices = (&[SfxKind::Swoosh], &[SfxKind::Whoosh, SfxKind::Swipe]);
const POP: Choices = (&[SfxKind::Pop], &[SfxKind::Bubble, SfxKind::Click, SfxKind::Ding]);
const SWIPE: Choices = (&[SfxKind::Swipe], &[SfxKind::Swoosh]);
const TYPEWRITER: Choices = (&[SfxKind::Typewriter], &[SfxKind::Click, SfxKind::Pop]);
const PUNCH: Choices = (&[SfxKind::Punch], &[SfxKind::Thud, SfxKind::Impact]);
const RISER: Choices = (&[SfxKind::Riser], &[SfxKind::ReverseSwell]);
const HIT: Choices = (&[SfxKind::Impact], &[SfxKind::Boom, SfxKind::Thud, SfxKind::Punch]);

/// The kinds that can serve a part: the preferred ones, else the fallbacks.
#[derive(Debug, Clone)]
struct Pool {
    preferred: Vec<SfxKind>,
    fallback: Vec<SfxKind>,
}

impl Pool {
    fn of(choices: Choices) -> Self {
        Pool {
            preferred: choices.0.to_vec(),
            fallback: choices.1.to_vec(),
        }
    }

    fn exact(kind: SfxKind) -> Self {
        Pool {
            preferred: vec![kind],
            fallback: Vec::new(),
        }
    }
}

/// One effect of a moment (a riser into a drop is one part, the impact on it another).
#[derive(Debug, Clone)]
struct Part {
    pool: Pool,
    /// Which part of its moment it is, so alternating kinds counts like with like.
    slot: usize,
    length_s: Option<f64>,
    reason: String,
    /// The kinds that survived the request's filter and can land at the moment, in order of preference.
    usable: Vec<SfxKind>,
}

impl Part {
    fn new(choices: Choices, slot: usize, length_s: Option<f64>, reason: String) -> Self {
        Part {
            pool: Pool::of(choices),
            slot,
            length_s,
            reason,
            usable: Vec::new(),
        }
    }

    /// A part for the one kind a marker names.
    fn exact(kind: SfxKind, reason: String) -> Self {
        Part {
            pool: Pool::exact(kind),
            ..Part::new(HIT, 0, None, reason)
        }
    }

    fn spec(&self, kind: SfxKind, seed: u64) -> SfxSpec {
        SfxSpec {
            duration_s: self.length_s,
            seed,
            ..SfxSpec::new(kind)
        }
    }
}

/// A moment in the picture that wants a sound: one or two effects at one time.
#[derive(Debug, Clone)]
struct Moment {
    at: f64,
    source: SfxSource,
    score: f64,
    /// A marker is the user's own word: it is never thinned away, and it thins what is near it.
    pinned: bool,
    parts: Vec<Part>,
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn moment(at: f64, source: SfxSource, score: f64, parts: Vec<Part>) -> Moment {
    Moment {
        at: round3(at),
        source,
        score: round3(score.clamp(0.0, 1.0)),
        pinned: source == SfxSource::Marker,
        parts,
    }
}

/// Whether an effect of `spec` can be put with its hit at `at` without `add_sound_effects` refusing it:
/// the same rule as the placement's (a head cut off at 0 must leave [`MIN_SFX_CLIP_S`] of sound).
fn lands(spec: &SfxSpec, at: f64) -> bool {
    let len = spec.resolved_duration_s();
    let start = at - sfx_anchor_s(spec, len);
    start >= 0.0 || len + start >= MIN_SFX_CLIP_S
}

/// A name for a person from an asset or title name: whitespace collapsed, control characters gone,
/// cut to a sentence-sized length.
fn short(text: &str) -> String {
    const MAX: usize = 28;
    let flat = text
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() <= MAX {
        flat
    } else {
        let mut cut: String = flat.chars().take(MAX - 1).collect();
        cut.push('…');
        cut
    }
}

/// Whether a title from `start` to `end` is on screen long enough to be read.
fn shown(start: f64, end: f64) -> bool {
    start.is_finite() && end.is_finite() && end - start >= MIN_TITLE_S
}

/// What a marker called `name` asks for: `None` when it is not a sound-effect marker, `Some(None)` for
/// a bare `sfx`, `Some(Some(kind))` for `sfx:<kind>` (case, spaces and dashes forgiven).
fn marker_kind(name: &str) -> Option<Option<SfxKind>> {
    let name = name.trim().to_lowercase();
    if name == "sfx" {
        return Some(None);
    }
    let kind = name.strip_prefix("sfx:")?.trim().replace([' ', '-'], "_");
    SfxKind::parse(&kind).map(Some)
}

/// Stretches of speech on the timeline, sorted and disjoint.
struct Speech(Vec<(f64, f64)>);

impl Speech {
    fn new(mut words: Vec<(f64, f64)>) -> Self {
        words.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for (start, end) in words {
            match merged.last_mut() {
                Some(last) if start - last.1 <= SPEECH_GAP_S => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        Speech(merged)
    }

    fn over(&self, t: f64) -> bool {
        let n = self.0.partition_point(|s| s.0 <= t + SPEECH_SLACK_S);
        n > 0 && self.0[n - 1].1 >= t - SPEECH_SLACK_S
    }
}

/// The bars of analysed music where the harmony changes for good: `(source seconds, novelty)` at each
/// 4-bar boundary whose preceding and following four bars' mean chroma are [`SECTION_NOVELTY`] or more apart
/// (cosine distance). A silent stretch has no chroma and is no section.
fn section_changes(music: &MusicAnalysis) -> Vec<(f64, f64)> {
    let mean = |bars: &[[f32; 12]]| -> Option<[f64; 12]> {
        let mut sum = [0.0f64; 12];
        for bar in bars {
            for (s, v) in sum.iter_mut().zip(bar) {
                *s += f64::from(*v);
            }
        }
        let norm = sum.iter().map(|v| v * v).sum::<f64>().sqrt();
        (norm > 1e-9 && norm.is_finite()).then(|| sum.map(|v| v / norm))
    };
    let chroma = &music.bar_chroma;
    let mut out = Vec::new();
    let mut k = SECTION_BARS;
    while k + SECTION_BARS <= chroma.len() {
        if let (Some(before), Some(after)) = (mean(&chroma[k - SECTION_BARS..k]), mean(&chroma[k..k + SECTION_BARS])) {
            let novelty = 1.0 - before.iter().zip(&after).map(|(a, b)| a * b).sum::<f64>();
            if novelty >= SECTION_NOVELTY {
                out.push((music.grid.bar_start(k), novelty.min(1.0)));
            }
        }
        k += SECTION_BARS;
    }
    out
}

/// The cut and what it needs to be read, resolved once.
struct Reading<'a> {
    inputs: &'a SfxSuggestInputs,
    /// The cut as it renders: muted tracks and disabled clips are gone.
    rendered: Timeline,
    assets: HashMap<Uuid, &'a Asset>,
    /// One frame at the delivery rate, seconds.
    frame: f64,
}

/// A drop (or section change) to put a riser into and an impact on.
struct DropAt<'a> {
    at: f64,
    /// One bar of the music on the timeline, seconds.
    bar_s: f64,
    lead_in: bool,
    hit: bool,
    score: f64,
    /// "drop" or "section change".
    what: &'a str,
    name: &'a str,
}

impl<'a> Reading<'a> {
    fn new(inputs: &'a SfxSuggestInputs) -> Self {
        let (fps, _) = crate::delivery_rate(&inputs.timeline, &inputs.assets);
        Reading {
            rendered: inputs.timeline.for_render(),
            assets: inputs.assets.iter().map(|a| (a.id, a)).collect(),
            frame: 1.0 / fps.max(1.0),
            inputs,
        }
    }

    fn analysis(&self, asset: Uuid) -> Option<&'a AssetAnalysis> {
        self.inputs.analyses.get(&asset)
    }

    fn name_of(&self, clip: &Clip) -> String {
        match clip.title_clip() {
            Some(t) => short(&t.display_name()),
            None => self
                .assets
                .get(&clip.asset_id)
                .map_or_else(|| "a clip".to_string(), |a| short(&a.name)),
        }
    }

    /// Whether the clip draws a picture: footage, a still or a generated title.
    fn picture(&self, clip: &Clip) -> bool {
        clip.is_generated()
            || self
                .assets
                .get(&clip.asset_id)
                .is_some_and(|a| a.streams.iter().any(|s| s.kind == StreamKind::Video))
    }

    /// Whether the clip is moving footage (not a still, not a title).
    fn footage(&self, clip: &Clip) -> bool {
        !clip.is_generated()
            && self
                .assets
                .get(&clip.asset_id)
                .is_some_and(|a| !a.is_image() && a.streams.iter().any(|s| s.kind == StreamKind::Video))
    }

    /// Whether `b` plays on from where `a` stopped: a split nobody took anything out of, which is no
    /// cut to the eye.
    fn continues(&self, a: &Clip, b: &Clip) -> bool {
        let tolerance = self.frame / 2.0 * a.speed_at(a.duration()).abs();
        a.asset_id == b.asset_id && pace_gap(a, b) < 1e-9 && (head_source(b) - tail_source(a)).abs() <= tolerance
    }

    /// Whether a clip on a higher video track covers the whole frame at `t`, so a cut on track `track` is not seen.
    fn hidden(&self, track: usize, t: f64) -> bool {
        const EPS: f64 = 1e-6;
        self.rendered.tracks[track + 1..]
            .iter()
            .filter(|u| u.kind == StreamKind::Video)
            .flat_map(|u| u.clips.iter())
            .any(|c| c.timeline_start < t - EPS && c.timeline_end() > t + EPS && self.opaque(c))
    }

    fn opaque(&self, c: &Clip) -> bool {
        let t = &c.transform;
        self.picture(c)
            && !c.is_generated()
            && !self.assets.get(&c.asset_id).is_some_and(|a| a.has_alpha())
            && c.mask.is_none()
            && c.blend_mode.is_normal()
            && !c.is_animated()
            && t.opacity >= 0.99
            && t.scale >= 0.99
            && t.pos_x.abs() < 0.01
            && t.pos_y.abs() < 0.01
            && t.rotation.abs() < 0.01
            && [t.crop_left, t.crop_right, t.crop_top, t.crop_bottom]
                .iter()
                .all(|v| v.abs() < 0.01)
    }

    // ---- the picture: cuts, transitions, punch-ins ----

    fn pictures(&self, min_shot: Option<f64>, out: &mut Vec<Moment>) {
        for (ti, track) in self.rendered.tracks.iter().enumerate() {
            if track.kind != StreamKind::Video {
                continue;
            }
            let mut clips: Vec<&Clip> = track.clips.iter().filter(|c| self.picture(c)).collect();
            clips.sort_by(|a, b| a.timeline_start.total_cmp(&b.timeline_start));
            for pair in clips.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if (b.timeline_start - a.timeline_end()).abs() > self.frame / 2.0 || self.hidden(ti, b.timeline_start) {
                    continue;
                }
                if let Some(tr) = b.transition_in.filter(|t| t.duration.is_finite() && t.duration > 0.0) {
                    out.extend(self.transition(a, b, tr));
                    continue;
                }
                let Some(min_shot) = min_shot else { continue };
                if a.is_generated() || b.is_generated() || self.continues(a, b) {
                    continue;
                }
                let shot = a.duration().min(b.duration());
                if shot >= min_shot {
                    out.push(self.cut(a, b, shot));
                }
            }
            if let Some(min_shot) = min_shot {
                for clip in clips.iter().filter(|c| self.footage(c)) {
                    self.scene_changes(ti, clip, min_shot, out);
                }
            }
        }
    }

    /// The hard cut from `a` to `b`: a punch-in when it is the same shot at a new scale, else a cut.
    fn cut(&self, a: &Clip, b: &Clip, shot: f64) -> Moment {
        let at = b.timeline_start;
        let same = a.asset_id == b.asset_id;
        if same
            && self.footage(a)
            && self.footage(b)
            && scale_jump(a, b) >= PUNCH_IN_SCALE
            && !a.is_animated()
            && !b.is_animated()
        {
            let (from, to) = (a.transform.scale, b.transform.scale);
            let reason = format!(
                "{} on {} at {} ({:.0}% to {:.0}%)",
                if to > from { "Punch-in" } else { "Punch-out" },
                self.name_of(b),
                fmt_time(at),
                from * 100.0,
                to * 100.0
            );
            return moment(at, SfxSource::PunchIn, 0.65, vec![Part::new(PUNCH, 0, None, reason)]);
        }
        let new_shot = !same
            || self.analysis(a.asset_id).is_some_and(|x| {
                x.scene_changes
                    .iter()
                    .any(|&s| s >= a.source_out - self.frame && s <= b.source_in + self.frame)
            });
        let reason = if same {
            format!("Cut within {} at {}", self.name_of(b), fmt_time(at))
        } else {
            format!("Cut from {} to {} at {}", self.name_of(a), self.name_of(b), fmt_time(at))
        };
        let longer = ((shot - 1.0) / 4.0).clamp(0.0, 1.0);
        let score = 0.35 + 0.15 * longer + if new_shot { 0.10 } else { 0.0 };
        moment(at, SfxSource::Cut, score, vec![Part::new(CUT, 0, None, reason)])
    }

    /// The scene changes the footage of `clip` makes inside its window (a clip that is itself an edited
    /// piece of video): cuts of a lower score than the timeline's own.
    fn scene_changes(&self, track: usize, clip: &Clip, min_shot: f64, out: &mut Vec<Moment>) {
        let Some(analysis) = self.analysis(clip.asset_id) else {
            return;
        };
        let mut changes: Vec<f64> = analysis.scene_changes.iter().copied().filter(|s| s.is_finite()).collect();
        changes.sort_by(f64::total_cmp);
        let inside: Vec<f64> = changes
            .into_iter()
            .filter(|&s| s > clip.source_in + SCENE_MARGIN_S && s < clip.source_out - SCENE_MARGIN_S)
            .collect();
        for (i, &s) in inside.iter().enumerate() {
            let before = if i > 0 { inside[i - 1] } else { clip.source_in };
            let after = inside.get(i + 1).copied().unwrap_or(clip.source_out);
            let at = clip.source_to_timeline(s);
            let shot = (at - clip.source_to_timeline(before))
                .abs()
                .min((clip.source_to_timeline(after) - at).abs());
            if shot < min_shot || self.hidden(track, at) {
                continue;
            }
            let longer = ((shot - 1.0) / 4.0).clamp(0.0, 1.0);
            let reason = format!("Scene change in {} at {}", self.name_of(clip), fmt_time(at));
            out.push(moment(
                at,
                SfxSource::Cut,
                0.30 + 0.10 * longer,
                vec![Part::new(CUT, 0, None, reason)],
            ));
        }
    }

    /// A swoosh lasting the transition, anchored on its middle. A dissolve or a motion transition runs
    /// from the cut for its length (as far as both shots allow); a dip runs half either side.
    fn transition(&self, a: &Clip, b: &Clip, tr: Transition) -> Option<Moment> {
        let cut = b.timeline_start;
        let (start, end) = if tr.kind.dip_color().is_some() {
            (
                cut - (tr.duration / 2.0).min(a.duration()),
                cut + (tr.duration / 2.0).min(b.duration()),
            )
        } else {
            (cut, cut + tr.duration.min(a.duration()).min(b.duration()))
        };
        let length = end - start;
        if !length.is_finite() || length <= 0.0 {
            return None;
        }
        let reason = format!(
            "{} from {} to {} at {}",
            tr.kind.label(),
            self.name_of(a),
            self.name_of(b),
            fmt_time(cut)
        );
        Some(moment(
            start + length / 2.0,
            SfxSource::Transition,
            0.70,
            vec![Part::new(TRANSITION, 0, Some(length), reason)],
        ))
    }

    // ---- titles ----

    fn titles(&self, out: &mut Vec<Moment>) {
        let timeline = &self.inputs.timeline;
        for o in timeline.overlays.iter().filter(|o| !o.generated) {
            if !shown(o.start, o.end) {
                continue;
            }
            let lower = o.pos_y >= LOWER_THIRD_Y;
            let (choices, noun, verb) = if lower {
                (SWIPE, "Lower third", "slides in")
            } else if o.text.chars().count() <= SHORT_TITLE_CHARS {
                (POP, "Title", "appears")
            } else {
                (SWIPE, "Title", "slides in")
            };
            self.title(o.start, choices, noun, verb, &o.text, out);
        }
        for o in &timeline.svg_overlays {
            if !shown(o.start, o.end) {
                continue;
            }
            let (choices, noun, verb) = svg_title(o);
            self.title(o.start, choices, noun, verb, &o.display_name(), out);
        }
        for track in self.rendered.tracks.iter().filter(|t| t.kind == StreamKind::Video) {
            for clip in track.clips.iter().filter(|c| c.is_generated() && c.duration() >= MIN_TITLE_S) {
                let name = clip.title_clip().map(|t| t.display_name()).unwrap_or_default();
                let lowered = name.to_lowercase();
                let (choices, noun, verb) = if lowered.contains("lower third") || lowered.contains("lower-third") {
                    (SWIPE, "Lower third", "slides in")
                } else if lowered.contains("typewriter") {
                    (TYPEWRITER, "Title", "types on")
                } else {
                    (POP, "Title", "appears")
                };
                self.title(clip.timeline_start, choices, noun, verb, &name, out);
            }
        }
    }

    fn title(&self, at: f64, choices: Choices, noun: &str, verb: &str, text: &str, out: &mut Vec<Moment>) {
        let reason = format!("{noun} \"{}\" {verb} at {}", short(text), fmt_time(at));
        out.push(moment(at, SfxSource::Title, 0.75, vec![Part::new(choices, 0, None, reason)]));
    }

    // ---- music ----

    fn music(&self, out: &mut Vec<Moment>) {
        for clip in self.rendered.tracks.iter().flat_map(|t| t.clips.iter()) {
            if clip.is_generated() || !clip.source_audio || clip.is_reversed() {
                continue;
            }
            let Some(asset) = self.assets.get(&clip.asset_id) else {
                continue;
            };
            if !asset.has_audio() {
                continue;
            }
            // One bar of the music as it plays here (a sped-up clip has shorter bars).
            let pace = if clip.source_duration() > 0.0 {
                clip.duration() / clip.source_duration()
            } else {
                1.0
            };
            match &asset.music_score {
                Some(score) => self.composed(clip, asset, score, pace, out),
                None => self.analysed(clip, asset, pace, out),
            }
        }
    }

    /// The drops of a composed score, read from its sections: the score is exact. A drop after a build
    /// has the build's own riser, and a hit the score already has at the drop is not added again.
    fn composed(&self, clip: &Clip, asset: &Asset, score: &super::MusicScore, pace: f64, out: &mut Vec<Moment>) {
        let Ok(spans) = score.section_spans() else { return };
        let covers = |t: f64| t >= clip.source_in - 1e-6 && t < clip.source_out;
        let hits: Vec<(HitKind, f64)> = score
            .hits
            .iter()
            .filter(|h| covers(h.at_s))
            .map(|h| (h.kind, clip.source_to_timeline(h.at_s)))
            .collect();
        let name = short(&asset.name);
        for (i, span) in spans.iter().enumerate().filter(|(_, s)| s.kind == SectionKind::Drop) {
            if !covers(span.start_s) {
                continue;
            }
            let at = clip.source_to_timeline(span.start_s);
            let marked = |kinds: &[HitKind]| hits.iter().any(|(k, t)| kinds.contains(k) && (t - at).abs() <= HIT_NEAR_S);
            let built = i > 0 && spans[i - 1].kind == SectionKind::Build;
            self.drop_moment(
                DropAt {
                    at,
                    bar_s: score.bar_s() * pace,
                    lead_in: !built && !marked(&[HitKind::Riser, HitKind::Whoosh]),
                    hit: !marked(&[HitKind::Impact, HitKind::Dropout]),
                    score: 0.90 + 0.05 * span.energy.clamp(0.0, 1.0),
                    what: "drop",
                    name: &name,
                },
                out,
            );
        }
    }

    /// The section changes of analysed music, read from the harmony of its bars: only for audio
    /// classified as music, and only where the clip plays the bars on both sides.
    fn analysed(&self, clip: &Clip, asset: &Asset, pace: f64, out: &mut Vec<Moment>) {
        let Some(analysis) = self.analysis(clip.asset_id) else {
            return;
        };
        let Some(music) = &analysis.music else { return };
        if !analysis
            .audio_class
            .is_some_and(|k| k.class == AudioClass::Music && k.confidence >= MUSIC_CONFIDENCE)
        {
            return;
        }
        let bar_s = music.grid.bar_s();
        if !bar_s.is_finite() || bar_s <= 0.0 {
            return;
        }
        let name = short(&asset.name);
        for (t, novelty) in section_changes(music) {
            if t < clip.source_in + MUSIC_EDGE_S || t > clip.source_out - MUSIC_EDGE_S {
                continue;
            }
            let strength = ((novelty - SECTION_NOVELTY) / (1.0 - SECTION_NOVELTY)).clamp(0.0, 1.0);
            self.drop_moment(
                DropAt {
                    at: clip.source_to_timeline(t),
                    bar_s: bar_s * pace,
                    lead_in: true,
                    hit: true,
                    score: 0.55 + 0.30 * strength,
                    what: "section change",
                    name: &name,
                },
                out,
            );
        }
    }

    /// A riser that ends on the drop (two bars long, at most up to the start of the timeline) and an impact on it.
    fn drop_moment(&self, d: DropAt<'_>, out: &mut Vec<Moment>) {
        let when = fmt_time(d.at);
        let mut parts = Vec::new();
        if d.lead_in && d.at >= MIN_LEAD_IN_S {
            let length = (2.0 * d.bar_s).clamp(1.5, 4.0).min(d.at);
            parts.push(Part::new(
                RISER,
                0,
                Some(length),
                format!("Riser into the {} in {} at {when}", d.what, d.name),
            ));
        }
        if d.hit {
            parts.push(Part::new(
                HIT,
                1,
                None,
                format!("Impact on the {} in {} at {when}", d.what, d.name),
            ));
        }
        if !parts.is_empty() {
            out.push(moment(d.at, SfxSource::MusicDrop, d.score, parts));
        }
    }

    // ---- markers and the opening ----

    fn markers(&self, out: &mut Vec<Moment>) {
        for m in &self.inputs.timeline.markers {
            let Some(wanted) = marker_kind(&m.name) else { continue };
            let what = match wanted {
                Some(kind) => format!("a {}", kind.label().to_lowercase()),
                None => "a hit".to_string(),
            };
            let reason = format!("Marker \"{}\" at {} asks for {what}", short(&m.name), fmt_time(m.time));
            let part = match wanted {
                Some(kind) => Part::exact(kind, reason),
                None => Part::new(HIT, 0, None, reason),
            };
            out.push(moment(m.time, SfxSource::Marker, 1.0, vec![part]));
        }
    }

    /// An impact on the first picture, the hook.
    fn opening(&self, out: &mut Vec<Moment>) {
        let first = self
            .rendered
            .tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .flat_map(|t| t.clips.iter())
            .filter(|c| self.picture(c))
            .min_by(|a, b| a.timeline_start.total_cmp(&b.timeline_start));
        if let Some(clip) = first {
            let reason = format!(
                "Hook: a hit as {} opens at {}",
                self.name_of(clip),
                fmt_time(clip.timeline_start)
            );
            out.push(moment(
                clip.timeline_start,
                SfxSource::Opening,
                0.80,
                vec![Part::new(HIT, 0, None, reason)],
            ));
        }
    }

    // ---- what is already there ----

    /// The spans of the effects already on the timeline (every track, muted or not).
    fn existing_effects(&self) -> Vec<(f64, f64)> {
        self.inputs
            .timeline
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| self.assets.get(&c.asset_id).is_some_and(|a| a.sound_effect.is_some()))
            .map(|c| (c.timeline_start, c.timeline_end()))
            .collect()
    }

    /// The speech in the cut: every transcribed word of a clip that plays its own sound, where it plays.
    fn speech(&self) -> Speech {
        let mut words = Vec::new();
        for clip in self.rendered.tracks.iter().flat_map(|t| t.clips.iter()) {
            if clip.is_generated() || !clip.source_audio {
                continue;
            }
            let Some(asset) = self.assets.get(&clip.asset_id) else {
                continue;
            };
            if !asset.has_audio() || asset.sound_effect.is_some() || asset.music_score.is_some() {
                continue;
            }
            let Some(analysis) = self.analysis(clip.asset_id) else {
                continue;
            };
            for seg in analysis
                .transcript
                .iter()
                .filter(|s| s.end >= clip.source_in && s.start <= clip.source_out)
            {
                for w in words_of(seg).iter() {
                    let (s, e) = (w.start.max(clip.source_in), w.end.min(clip.source_out));
                    if e > s {
                        let (x, y) = (clip.source_to_timeline(s), clip.source_to_timeline(e));
                        words.push((x.min(y), x.max(y)));
                    }
                }
            }
        }
        Speech::new(words)
    }
}

/// The effect for an animated SVG title: by its preset, else by where it sits in the frame.
fn svg_title(o: &SvgOverlay) -> (Choices, &'static str, &'static str) {
    match o.preset.as_ref().map(|p| p.kind) {
        Some(SvgPresetKind::LowerThird) => (SWIPE, "Lower third", "slides in"),
        Some(SvgPresetKind::Typewriter) => (TYPEWRITER, "Title", "types on"),
        Some(SvgPresetKind::WordPop) => (POP, "Title", "pops up"),
        _ if o.y + o.height / 2.0 >= LOWER_THIRD_Y && o.height <= 0.5 => (SWIPE, "Lower third", "slides in"),
        _ => (POP, "Title", "appears"),
    }
}

// ---- suggesting ----------------------------------------------------------------------------

/// Suggest sound effects for the cut `inputs` describes.
///
/// The picture's moments are found (cuts, transitions, titles, punch-ins, drops, markers, the opening);
/// those an effect already covers ([`COVER_S`]), those the request's filter leaves no effect for and
/// those a placement would refuse are dropped; the rest are thinned to the density's spacing, the
/// highest [`SfxSuggestion::score`] winning (a marker always stays); and each survivor gets its kinds
/// (alternating down a run of the same source), a seed (the running count, so no two suggestions are
/// the same render) and its gain. At most [`MAX_SFX_PER_CALL`] come back, the best of them, in time order.
pub fn suggest(inputs: &SfxSuggestInputs, request: &SfxSuggestRequest) -> Result<Vec<SfxSuggestion>> {
    request.check()?;
    let density = request.density;
    let reading = Reading::new(inputs);

    let mut moments = Vec::new();
    reading.pictures(density.min_shot_s(), &mut moments);
    reading.titles(&mut moments);
    reading.music(&mut moments);
    reading.markers(&mut moments);
    if density == SfxDensity::Punchy {
        reading.opening(&mut moments);
    }

    // What cannot be suggested must not hold back what can, so all of it goes before the thinning.
    let existing = reading.existing_effects();
    moments.retain(|m| {
        m.at.is_finite()
            && m.at >= 0.0
            && !existing
                .iter()
                .any(|&(start, end)| m.at >= start - COVER_S && m.at <= end + COVER_S)
    });
    for m in &mut moments {
        for part in &mut m.parts {
            let ok = |k: &&SfxKind| request.allows(**k) && lands(&part.spec(**k, 0), m.at);
            let preferred: Vec<SfxKind> = part.pool.preferred.iter().filter(ok).copied().collect();
            part.usable = if preferred.is_empty() {
                part.pool.fallback.iter().filter(ok).copied().collect()
            } else {
                preferred
            };
        }
        m.parts.retain(|p| !p.usable.is_empty());
    }
    moments.retain(|m| !m.parts.is_empty());

    // Thin: markers first, then the best scores, each kept unless one already kept is too near.
    let mut order: Vec<usize> = (0..moments.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&moments[a], &moments[b]);
        b.pinned
            .cmp(&a.pinned)
            .then(b.score.total_cmp(&a.score))
            .then(a.at.total_cmp(&b.at))
            .then(a.source.cmp(&b.source))
    });
    let spacing = density.spacing_s();
    let mut kept: Vec<usize> = Vec::new();
    let mut parts = 0;
    for i in order {
        let m = &moments[i];
        if parts + m.parts.len() > MAX_SFX_PER_CALL {
            continue;
        }
        if !m.pinned && kept.iter().any(|&j| (moments[j].at - m.at).abs() < spacing - 1e-9) {
            continue;
        }
        parts += m.parts.len();
        kept.push(i);
    }
    kept.sort_by(|&a, &b| {
        moments[a]
            .at
            .total_cmp(&moments[b].at)
            .then(moments[a].source.cmp(&moments[b].source))
    });

    let speech = reading.speech();
    let mut seen: HashMap<(SfxSource, usize), usize> = HashMap::new();
    let mut seed = 0u64;
    let mut out = Vec::with_capacity(parts);
    for i in kept {
        let m = &moments[i];
        for part in &m.parts {
            let n = seen.entry((m.source, part.slot)).or_insert(0);
            let kind = part.usable[*n % part.usable.len()];
            *n += 1;
            out.push(SfxSuggestion {
                placement: SfxPlacement {
                    spec: part.spec(kind, seed),
                    at: m.at,
                    volume_db: Some(if speech.over(m.at) { SPEECH_GAIN_DB } else { CLEAR_GAIN_DB }),
                    track_id: None,
                },
                source: m.source,
                reason: part.reason.clone(),
                score: m.score,
            });
            seed += 1;
        }
    }
    if let Some(range) = request.range {
        out.retain(|s| s.placement.at >= range.start && s.placement.at < range.end);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{audio_stream, av_asset, test_asset, timeline_of, video_track};
    use crate::model::{
        AudioClassification, BeatGrid, Marker, MusicScore, MusicStyle, ScoreHit, ScoreSection, SvgOverlay, SvgPreset,
        SvgPresetParams, TextOverlay, TitleClip, Track, TranscriptSegment, TransitionKind, Word,
    };

    fn asset(name: &str) -> Asset {
        let mut a = av_asset(Uuid::new_v4(), 60.0);
        a.name = name.to_string();
        a
    }

    fn shot(asset: &Asset, source_in: f64, source_out: f64, start: f64) -> Clip {
        Clip::new(asset.id, source_in, source_out, start)
    }

    fn inputs(timeline: Timeline, assets: &[&Asset]) -> SfxSuggestInputs {
        SfxSuggestInputs {
            timeline,
            assets: assets.iter().map(|a| (*a).clone()).collect(),
            analyses: HashMap::new(),
        }
    }

    fn request(density: SfxDensity) -> SfxSuggestRequest {
        SfxSuggestRequest {
            density,
            ..SfxSuggestRequest::default()
        }
    }

    fn ask(i: &SfxSuggestInputs, density: SfxDensity) -> Vec<SfxSuggestion> {
        suggest(i, &request(density)).unwrap()
    }

    fn times(out: &[SfxSuggestion]) -> Vec<f64> {
        out.iter().map(|s| s.placement.at).collect()
    }

    fn kinds(out: &[SfxSuggestion]) -> Vec<SfxKind> {
        out.iter().map(|s| s.placement.spec.kind).collect()
    }

    fn sources(out: &[SfxSuggestion]) -> Vec<SfxSource> {
        out.iter().map(|s| s.source).collect()
    }

    /// `n` back-to-back shots of `len` seconds on V1, alternating between two assets.
    fn run_of_shots(n: usize, len: f64) -> SfxSuggestInputs {
        let (a, b) = (asset("interview.mp4"), asset("broll.mp4"));
        let clips = (0..n)
            .map(|i| shot(if i % 2 == 0 { &a } else { &b }, 0.0, len, i as f64 * len))
            .collect();
        inputs(timeline_of(vec![video_track(clips)]), &[&a, &b])
    }

    fn marker(time: f64, name: &str) -> Marker {
        Marker {
            id: Uuid::new_v4(),
            time,
            name: name.to_string(),
            ..Marker::default()
        }
    }

    #[test]
    fn a_hard_cut_between_two_shots_gets_a_whoosh_on_the_cut_and_says_why() {
        let i = run_of_shots(2, 4.0);
        let out = ask(&i, SfxDensity::Balanced);
        assert_eq!(out.len(), 1, "{out:?}");
        let s = &out[0];
        assert_eq!(
            (s.source, s.placement.spec.kind, s.placement.at),
            (SfxSource::Cut, SfxKind::Whoosh, 4.0)
        );
        assert_eq!(s.reason, "Cut from interview.mp4 to broll.mp4 at 0:04.0");
        assert_eq!(s.placement.volume_db, Some(CLEAR_GAIN_DB));
        assert_eq!(s.placement.track_id, None);
        assert!(s.score > 0.0 && s.score <= 1.0, "{}", s.score);
        assert!(ask(&i, SfxDensity::Subtle).is_empty(), "subtle does not whoosh on cuts");
        let punchy = ask(&i, SfxDensity::Punchy);
        assert_eq!(sources(&punchy), [SfxSource::Opening, SfxSource::Cut]);
        assert_eq!(
            ask(&i, SfxDensity::Balanced),
            suggest(&i, &SfxSuggestRequest::default()).unwrap(),
            "balanced is the default"
        );
    }

    #[test]
    fn density_sets_the_spacing_and_which_sources_count() {
        // Cuts every 1.6 s: balanced thins them to 2 s apart, punchy keeps every one (1 s apart or more).
        let i = run_of_shots(9, 1.6);
        let balanced = ask(&i, SfxDensity::Balanced);
        let punchy = ask(&i, SfxDensity::Punchy);
        assert!(balanced.len() >= 3 && balanced.len() < 8, "{:?}", times(&balanced));
        assert!(times(&balanced).windows(2).all(|w| w[1] - w[0] >= 2.0 - 1e-9));
        assert_eq!(punchy.iter().filter(|s| s.source == SfxSource::Cut).count(), 8);
        assert_eq!(punchy[0].source, SfxSource::Opening);
        assert!(ask(&i, SfxDensity::Subtle).is_empty());

        // Shots under 1.5 s are not whooshed on at balanced, only at punchy; 1 s apart is far enough.
        let quick = run_of_shots(6, 1.0);
        assert!(ask(&quick, SfxDensity::Balanced).is_empty());
        let punchy = ask(&quick, SfxDensity::Punchy);
        assert_eq!(punchy.iter().filter(|s| s.source == SfxSource::Cut).count(), 5);
        assert_eq!(SfxDensity::Subtle.spacing_s(), 4.0);
        assert_eq!(SfxDensity::default(), SfxDensity::Balanced);
    }

    #[test]
    fn the_higher_score_wins_a_spacing_conflict_even_when_it_is_later() {
        // A jump cut in the same footage at 4 s, a cut to other footage at 5.5 s: 1.5 s apart, so one goes.
        let (a, b) = (asset("a.mp4"), asset("b.mp4"));
        let t = timeline_of(vec![video_track(vec![
            shot(&a, 0.0, 4.0, 0.0),
            shot(&a, 9.0, 10.5, 4.0),
            shot(&b, 0.0, 4.0, 5.5),
        ])]);
        let i = inputs(t, &[&a, &b]);
        let out = ask(&i, SfxDensity::Balanced);
        assert_eq!(times(&out), [5.5]);
        assert_eq!(out[0].reason, "Cut from a.mp4 to b.mp4 at 0:05.5");
        // Both survive where there is room.
        assert_eq!(times(&ask(&i, SfxDensity::Punchy)), [0.0, 4.0, 5.5]);
        // A title (0.75) beats a cut at the same moment.
        let mut t = run_of_shots(2, 4.0);
        t.timeline.overlays.push(TextOverlay {
            pos_y: 0.5,
            ..TextOverlay::new("Chapter one", 4.5, 8.0)
        });
        let out = ask(&t, SfxDensity::Balanced);
        assert_eq!((sources(&out), times(&out)), (vec![SfxSource::Title], vec![4.5]));
    }

    #[test]
    fn a_split_that_continues_the_shot_is_not_a_cut_but_a_jump_in_the_footage_is() {
        let a = asset("a.mp4");
        let continuing = inputs(
            timeline_of(vec![video_track(vec![shot(&a, 0.0, 5.0, 0.0), shot(&a, 5.0, 10.0, 5.0)])]),
            &[&a],
        );
        assert!(ask(&continuing, SfxDensity::Punchy)
            .iter()
            .all(|s| s.source == SfxSource::Opening));
        let jumping = inputs(
            timeline_of(vec![video_track(vec![shot(&a, 0.0, 5.0, 0.0), shot(&a, 7.0, 12.0, 5.0)])]),
            &[&a],
        );
        let out = ask(&jumping, SfxDensity::Balanced);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reason, "Cut within a.mp4 at 0:05.0");
        // A jump over a detected scene change is a new shot: it scores higher than one inside a shot.
        let mut cutaway = jumping;
        cutaway.analyses.insert(
            a.id,
            AssetAnalysis {
                asset_id: a.id,
                scene_changes: vec![6.0],
                ..AssetAnalysis::default()
            },
        );
        assert!(ask(&cutaway, SfxDensity::Balanced)[0].score > out[0].score);
        // A gap between the pictures is not a cut between them.
        let gap = inputs(
            timeline_of(vec![video_track(vec![shot(&a, 0.0, 5.0, 0.0), shot(&a, 7.0, 12.0, 6.0)])]),
            &[&a],
        );
        assert!(ask(&gap, SfxDensity::Balanced).is_empty());
    }

    #[test]
    fn a_run_of_cuts_alternates_the_kind_and_never_repeats_a_seed() {
        let i = run_of_shots(7, 3.0);
        let out = ask(&i, SfxDensity::Balanced);
        assert_eq!(out.len(), 6);
        assert_eq!(
            kinds(&out),
            [
                SfxKind::Whoosh,
                SfxKind::Swoosh,
                SfxKind::Whoosh,
                SfxKind::Swoosh,
                SfxKind::Whoosh,
                SfxKind::Swoosh
            ]
        );
        let mut seeds: Vec<u64> = out.iter().map(|s| s.placement.spec.seed).collect();
        assert_eq!(seeds, [0, 1, 2, 3, 4, 5]);
        seeds.dedup();
        assert_eq!(seeds.len(), 6);
        for s in &out {
            s.placement.spec.validate().unwrap();
        }
    }

    #[test]
    fn a_transition_gets_a_swoosh_as_long_as_it_lasts_anchored_at_its_middle() {
        let timeline = |kind: TransitionKind, duration: f64| {
            let (a, b) = (asset("a.mp4"), asset("b.mp4"));
            let mut second = shot(&b, 0.0, 6.0, 6.0);
            second.transition_in = Some(Transition { kind, duration });
            inputs(
                timeline_of(vec![video_track(vec![shot(&a, 0.0, 6.0, 0.0), second])]),
                &[&a, &b],
            )
        };
        // A dissolve runs from the cut for its length.
        let out = ask(&timeline(TransitionKind::Crossfade, 1.0), SfxDensity::Subtle);
        assert_eq!(out.len(), 1, "one sound for the cut and its dissolve: {out:?}");
        let s = &out[0];
        assert_eq!(
            (s.source, s.placement.spec.kind, s.placement.at),
            (SfxSource::Transition, SfxKind::Swoosh, 6.5)
        );
        assert_eq!(s.placement.spec.duration_s, Some(1.0));
        assert_eq!(s.reason, "Crossfade from a.mp4 to b.mp4 at 0:06.0");
        // A dip runs half either side of the cut, so it is centred on it.
        let dip = ask(&timeline(TransitionKind::DipToBlack, 1.0), SfxDensity::Balanced);
        assert_eq!((dip[0].placement.at, dip[0].placement.spec.duration_s), (6.0, Some(1.0)));
        // A slide is a transition too, and a transition longer than a shot is as long as the shot allows.
        let push = ask(&timeline(TransitionKind::PushLeft, 0.5), SfxDensity::Punchy);
        assert_eq!(push.iter().filter(|s| s.source == SfxSource::Transition).count(), 1);
        let long = ask(&timeline(TransitionKind::Crossfade, 20.0), SfxDensity::Subtle);
        assert_eq!(long[0].placement.spec.duration_s, Some(6.0));
        assert_eq!(long[0].placement.at, 9.0);
        // Transitions, unlike cuts, count at every density.
        for d in SfxDensity::ALL {
            assert_eq!(
                ask(&timeline(TransitionKind::Crossfade, 1.0), d)
                    .iter()
                    .filter(|s| s.source == SfxSource::Transition)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn a_title_pops_a_lower_third_swipes_and_captions_and_flickers_are_left_alone() {
        let mut t = Timeline::new();
        t.overlays.push(TextOverlay {
            pos_y: 0.5,
            ..TextOverlay::new("Hello", 5.0, 8.0)
        });
        t.overlays.push(TextOverlay::new("Jane Doe, CEO", 12.0, 16.0));
        t.overlays.push(TextOverlay {
            generated: true,
            ..TextOverlay::new("a caption", 20.0, 22.0)
        });
        t.overlays.push(TextOverlay::new("flicker", 24.0, 24.1));
        t.overlays.push(TextOverlay {
            pos_y: 0.4,
            ..TextOverlay::new("A very long line of centred text that is a sentence", 28.0, 31.0)
        });
        let out = ask(&inputs(t, &[]), SfxDensity::Subtle);
        assert_eq!(times(&out), [5.0, 12.0, 28.0]);
        assert_eq!(kinds(&out), [SfxKind::Pop, SfxKind::Swipe, SfxKind::Swipe]);
        assert_eq!(sources(&out), [SfxSource::Title; 3]);
        assert_eq!(out[0].reason, "Title \"Hello\" appears at 0:05.0");
        assert_eq!(out[1].reason, "Lower third \"Jane Doe, CEO\" slides in at 0:12.0");
        assert!(out[2].reason.contains("A very long line of centred…"), "{}", out[2].reason);
    }

    #[test]
    fn animated_titles_floating_or_on_a_track_are_titles_too() {
        let svg = |name: &str, start: f64, preset: Option<SvgPresetKind>, y: f64, height: f64| SvgOverlay {
            id: Uuid::new_v4(),
            name: name.to_string(),
            svg: "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"/>".to_string(),
            start,
            end: start + 3.0,
            x: 0.0,
            y,
            width: 1.0,
            height,
            opacity: 1.0,
            offset: 0.0,
            preset: preset.map(|kind| SvgPreset {
                kind,
                params: SvgPresetParams::new("Hi"),
            }),
            template: None,
        };
        let mut t = Timeline::new();
        t.svg_overlays = vec![
            svg("Logo", 2.0, None, 0.0, 1.0),
            svg("Name", 8.0, Some(SvgPresetKind::LowerThird), 0.7, 0.2),
            svg("Banner", 14.0, None, 0.7, 0.2),
            svg("Typed", 20.0, Some(SvgPresetKind::Typewriter), 0.0, 1.0),
            svg("Word", 26.0, Some(SvgPresetKind::WordPop), 0.0, 1.0),
        ];
        // A title clip on a track, and one that is called a lower third.
        let mut v2 = Track::new(StreamKind::Video, "V2");
        let title = |name: &str| TitleClip {
            name: name.to_string(),
            svg: "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"/>".to_string(),
            template: None,
        };
        v2.clips.push(Clip::title(title("Intro card"), 32.0, 3.0));
        v2.clips.push(Clip::title(title("Lower third: Jane"), 38.0, 3.0));
        t.tracks.push(v2);
        let out = ask(&inputs(t, &[]), SfxDensity::Subtle);
        assert_eq!(times(&out), [2.0, 8.0, 14.0, 20.0, 26.0, 32.0, 38.0]);
        assert_eq!(
            kinds(&out),
            [
                SfxKind::Pop,
                SfxKind::Swipe,
                SfxKind::Swipe,
                SfxKind::Typewriter,
                SfxKind::Pop,
                SfxKind::Pop,
                SfxKind::Swipe
            ]
        );
        assert_eq!(out[5].reason, "Title \"Intro card\" appears at 0:32.0");
    }

    #[test]
    fn a_change_of_scale_in_the_same_shot_is_a_punch_in_and_a_small_one_is_not() {
        let a = asset("talk.mp4");
        let zoomed = |scale: f64, keyed: bool| {
            let mut second = shot(&a, 8.0, 14.0, 5.0);
            second.transform.scale = scale;
            if keyed {
                second.keyframes.push(crate::model::Keyframe::from_transform(
                    0.0,
                    &crate::model::Transform::default(),
                ));
            }
            inputs(timeline_of(vec![video_track(vec![shot(&a, 0.0, 5.0, 0.0), second])]), &[&a])
        };
        let out = ask(&zoomed(1.15, false), SfxDensity::Balanced);
        assert_eq!(out.len(), 1);
        assert_eq!(
            (out[0].source, out[0].placement.spec.kind, out[0].placement.at),
            (SfxSource::PunchIn, SfxKind::Punch, 5.0)
        );
        assert_eq!(out[0].reason, "Punch-in on talk.mp4 at 0:05.0 (100% to 115%)");
        // Zooming back out is a jump in scale too.
        let mut back = zoomed(1.15, false);
        back.timeline.tracks[0].clips.reverse();
        back.timeline.tracks[0].clips[0].timeline_start = 0.0;
        back.timeline.tracks[0].clips[1].timeline_start = 6.0;
        assert!(ask(&back, SfxDensity::Balanced)[0].reason.starts_with("Punch-out on"));
        // PUNCH_IN_SCALE is the threshold review uses: below it, it is an ordinary jump cut.
        let small = ask(&zoomed(1.0 + PUNCH_IN_SCALE / 2.0, false), SfxDensity::Balanced);
        assert_eq!(
            (small[0].source, small[0].placement.spec.kind),
            (SfxSource::Cut, SfxKind::Whoosh)
        );
        let keyed = ask(&zoomed(1.15, true), SfxDensity::Balanced);
        assert_eq!(keyed[0].source, SfxSource::Cut, "a keyed zoom is a move, not a punch-in");
        // Another asset at another scale is a different shot framed differently.
        let b = asset("other.mp4");
        let mut second = shot(&b, 0.0, 5.0, 5.0);
        second.transform.scale = 1.3;
        let other = inputs(
            timeline_of(vec![video_track(vec![shot(&a, 0.0, 5.0, 0.0), second])]),
            &[&a, &b],
        );
        assert_eq!(ask(&other, SfxDensity::Balanced)[0].source, SfxSource::Cut);
        assert!(ask(&zoomed(1.15, false), SfxDensity::Subtle).is_empty());
    }

    #[test]
    fn a_cut_under_a_full_frame_cutaway_is_not_seen_and_one_under_a_pip_is() {
        let (a, b, c) = (asset("a.mp4"), asset("b.mp4"), asset("c.mp4"));
        let cutaway = |edit: fn(&mut Clip)| {
            let mut over = shot(&c, 0.0, 6.0, 2.0);
            edit(&mut over);
            let mut v2 = Track::new(StreamKind::Video, "V2");
            v2.clips.push(over);
            inputs(
                timeline_of(vec![video_track(vec![shot(&a, 0.0, 4.0, 0.0), shot(&b, 0.0, 4.0, 4.0)]), v2]),
                &[&a, &b, &c],
            )
        };
        let cuts = |i: &SfxSuggestInputs| {
            ask(i, SfxDensity::Balanced)
                .iter()
                .filter(|s| s.source == SfxSource::Cut)
                .count()
        };
        assert_eq!(
            cuts(&cutaway(|_| {})),
            0,
            "V2 covers the cut at 4 s (its own edges meet no picture)"
        );
        assert_eq!(cuts(&cutaway(|c| c.transform.opacity = 0.5)), 1);
        assert_eq!(
            cuts(&cutaway(|c| c.transform.scale = 0.3)),
            1,
            "a picture-in-picture shows the cut"
        );
        assert_eq!(cuts(&cutaway(|c| c.enabled = false)), 1, "a disabled clip covers nothing");
        assert_eq!(
            cuts(&cutaway(|c| c.timeline_start = 4.0)),
            1,
            "it has to cover the cut, not start on it"
        );
    }

    #[test]
    fn a_scene_change_inside_one_clip_is_a_cut_of_a_lower_score() {
        let a = asset("montage.mp4");
        let mut i = inputs(timeline_of(vec![video_track(vec![shot(&a, 10.0, 30.0, 0.0)])]), &[&a]);
        i.analyses.insert(
            a.id,
            AssetAnalysis {
                asset_id: a.id,
                // 2 s is before the clip; 10.1 and 29.9 are its own edges; 18 and 24 are inside.
                scene_changes: vec![24.0, 2.0, 10.1, 18.0, 29.9],
                ..AssetAnalysis::default()
            },
        );
        let out = ask(&i, SfxDensity::Balanced);
        assert_eq!(times(&out), [8.0, 14.0]);
        assert_eq!(out[0].reason, "Scene change in montage.mp4 at 0:08.0");
        assert!(out.iter().all(|s| s.score < 0.5));
        // The shot either side has to be as long as the density wants.
        i.analyses.get_mut(&a.id).unwrap().scene_changes = vec![11.0, 12.0];
        assert!(ask(&i, SfxDensity::Balanced).is_empty());
        assert_eq!(times(&ask(&i, SfxDensity::Punchy)), [0.0, 1.0, 2.0]);
    }

    fn score(sections: &[(SectionKind, u32)], hits: Vec<ScoreHit>) -> MusicScore {
        MusicScore {
            bpm: 120.0,
            key: "Am".to_string(),
            style: MusicStyle::Pop,
            sections: sections
                .iter()
                .map(|&(kind, bars)| ScoreSection {
                    kind,
                    bars,
                    chords: vec!["Am".to_string()],
                    energy: None,
                })
                .collect(),
            hits,
            seed: 0,
            instruments: None,
        }
    }

    /// The composed piece `score` as an asset and its clip on a music track: `source_in..source_out` at `start`.
    fn composed(score: MusicScore, source_in: f64, source_out: f64, start: f64) -> SfxSuggestInputs {
        let mut m = test_asset(vec![audio_stream(48_000, 2)]);
        m.name = "Score".to_string();
        m.duration = score.duration_s();
        m.music_score = Some(score);
        let mut track = Track::new(StreamKind::Audio, "M");
        track.clips.push(shot(&m, source_in, source_out, start));
        inputs(timeline_of(vec![track]), &[&m])
    }

    #[test]
    fn a_drop_in_composed_music_gets_a_riser_into_it_and_an_impact_on_it() {
        use SectionKind::{Breakdown, Build, Drop, Verse};
        // Verse 0-8 s, drop 8-16, breakdown 16-20, build 20-24, drop 24-32 (a bar is 2 s), placed from 10 s.
        let piece = score(&[(Verse, 4), (Drop, 4), (Breakdown, 2), (Build, 2), (Drop, 4)], Vec::new());
        let i = composed(piece.clone(), 0.0, 32.0, 10.0);
        let out = ask(&i, SfxDensity::Subtle);
        assert_eq!(times(&out), [18.0, 18.0, 34.0]);
        assert_eq!(kinds(&out), [SfxKind::Riser, SfxKind::Impact, SfxKind::Impact]);
        assert!(out.iter().all(|s| s.source == SfxSource::MusicDrop && s.score == 0.95));
        assert_eq!(out[0].placement.spec.duration_s, Some(4.0), "two bars");
        assert_eq!(out[0].reason, "Riser into the drop in Score at 0:18.0");
        assert_eq!(out[1].reason, "Impact on the drop in Score at 0:18.0");
        // The second drop follows a build, which carries its own riser.
        assert_eq!(out[2].reason, "Impact on the drop in Score at 0:34.0");
        // The riser ends on the drop: its hit, not its start, is at 18 s.
        let spec = &out[0].placement.spec;
        assert_eq!(sfx_anchor_s(spec, spec.resolved_duration_s()), 4.0);

        // A trimmed clip shows only the drops it plays: from source 12 s the first drop is gone.
        let trimmed = composed(piece.clone(), 12.0, 32.0, 0.0);
        assert_eq!(times(&ask(&trimmed, SfxDensity::Subtle)), [12.0]);
        // A sped-up clip has shorter bars, and the drop lands where the clip plays it.
        let mut fast = composed(piece.clone(), 0.0, 32.0, 0.0);
        fast.timeline.tracks[0].clips[0].speed = 2.0;
        let out = ask(&fast, SfxDensity::Subtle);
        assert_eq!(times(&out), [4.0, 4.0, 12.0]);
        assert_eq!(out[0].placement.spec.duration_s, Some(2.0), "two bars of 1 s, not 2 s");
        // A drop with no music before it has no riser to put there; one with a second has a short one.
        let early = composed(score(&[(Drop, 4)], Vec::new()), 0.0, 8.0, 0.5);
        assert_eq!(kinds(&ask(&early, SfxDensity::Subtle)), [SfxKind::Impact]);
        let near = composed(score(&[(Drop, 4)], Vec::new()), 0.0, 8.0, 1.2);
        let out = ask(&near, SfxDensity::Subtle);
        assert_eq!(
            (kinds(&out), out[0].placement.spec.duration_s),
            (vec![SfxKind::Riser, SfxKind::Impact], Some(1.2))
        );
        // A muted music track is not heard, so it asks for nothing.
        let mut muted = composed(piece, 0.0, 32.0, 10.0);
        muted.timeline.tracks[0].muted = true;
        assert!(ask(&muted, SfxDensity::Subtle).is_empty());
    }

    #[test]
    fn a_hit_the_score_already_has_at_the_drop_is_not_added_twice() {
        use SectionKind::{Drop, Verse};
        let sections = [(Verse, 4), (Drop, 4)];
        let hit = |kind, at_s| ScoreHit { at_s, kind };
        let kinds_with = |hits: Vec<ScoreHit>| kinds(&ask(&composed(score(&sections, hits), 0.0, 16.0, 0.0), SfxDensity::Subtle));
        assert_eq!(kinds_with(Vec::new()), [SfxKind::Riser, SfxKind::Impact]);
        assert_eq!(kinds_with(vec![hit(HitKind::Impact, 8.0)]), [SfxKind::Riser]);
        assert_eq!(kinds_with(vec![hit(HitKind::Dropout, 8.1)]), [SfxKind::Riser]);
        assert_eq!(kinds_with(vec![hit(HitKind::Riser, 7.9)]), [SfxKind::Impact]);
        assert!(kinds_with(vec![hit(HitKind::Riser, 8.0), hit(HitKind::Impact, 8.0)]).is_empty());
        assert_eq!(
            kinds_with(vec![hit(HitKind::Impact, 12.0)]),
            [SfxKind::Riser, SfxKind::Impact],
            "a hit elsewhere"
        );
    }

    #[test]
    fn a_section_change_in_analysed_music_is_a_drop_but_only_for_music_the_analysis_trusts() {
        let mut m = test_asset(vec![audio_stream(48_000, 2)]);
        m.name = "Song".to_string();
        m.duration = 64.0;
        let chord = |notes: [usize; 3]| {
            let mut c = [0.0f32; 12];
            notes.iter().for_each(|n| c[*n] = 1.0);
            c
        };
        // 32 bars of 2 s: C major for 16, then F# major.
        let bars: Vec<[f32; 12]> = (0..32)
            .map(|b| if b < 16 { chord([0, 4, 7]) } else { chord([6, 10, 1]) })
            .collect();
        let analysis = |class: Option<AudioClassification>, chroma: Vec<[f32; 12]>| AssetAnalysis {
            asset_id: m.id,
            audio_class: class,
            music: Some(MusicAnalysis {
                grid: BeatGrid {
                    period_s: 0.5,
                    phase_s: 0.0,
                    downbeat_offset: 0,
                    beats_per_bar: 4,
                },
                duration: 64.0,
                bar_chroma: chroma,
                phrases: Vec::new(),
            }),
            ..AssetAnalysis::default()
        };
        let music = Some(AudioClassification {
            class: AudioClass::Music,
            confidence: 0.9,
        });
        let mut track = Track::new(StreamKind::Audio, "M");
        track.clips.push(shot(&m, 0.0, 64.0, 0.0));
        let mut i = inputs(timeline_of(vec![track]), &[&m]);

        i.analyses.insert(m.id, analysis(music, bars.clone()));
        let out = ask(&i, SfxDensity::Subtle);
        assert_eq!(times(&out), [32.0, 32.0]);
        assert_eq!(kinds(&out), [SfxKind::Riser, SfxKind::Impact]);
        assert_eq!(out[1].reason, "Impact on the section change in Song at 0:32.0");
        assert!((0.8..=0.9).contains(&out[0].score), "{}", out[0].score);

        // Not music, or not surely: no sections. Nor a piece that never changes.
        for class in [
            Some(AudioClassification {
                class: AudioClass::Speech,
                confidence: 0.9,
            }),
            Some(AudioClassification {
                class: AudioClass::Mixed,
                confidence: 0.9,
            }),
            Some(AudioClassification {
                class: AudioClass::Music,
                confidence: 0.3,
            }),
            None,
        ] {
            i.analyses.insert(m.id, analysis(class, bars.clone()));
            assert!(ask(&i, SfxDensity::Subtle).is_empty(), "{class:?}");
        }
        i.analyses.insert(m.id, analysis(music, vec![chord([0, 4, 7]); 32]));
        assert!(ask(&i, SfxDensity::Subtle).is_empty(), "the same chord throughout");
        i.analyses.insert(m.id, analysis(music, vec![[0.0; 12]; 32]));
        assert!(ask(&i, SfxDensity::Subtle).is_empty(), "silence has no sections");

        // A clip that stops just after the change does not play the bars after it.
        i.analyses.insert(m.id, analysis(music, bars));
        i.timeline.tracks[0].clips[0].source_out = 32.2;
        assert!(ask(&i, SfxDensity::Subtle).is_empty());
        i.timeline.tracks[0].clips[0].source_out = 40.0;
        assert_eq!(times(&ask(&i, SfxDensity::Subtle)), [32.0, 32.0]);
    }

    #[test]
    fn a_marker_names_the_effect_it_wants_and_always_stays() {
        let mut t = Timeline::new();
        t.markers = vec![
            marker(5.0, "sfx:whoosh"),
            marker(9.0, "SFX: Reverse Swell"),
            marker(14.0, "sfx"),
            marker(20.0, "sfx:nope"),
            marker(22.0, "Chapter 2"),
            marker(25.0, "  Sfx:Record-Scratch "),
            marker(30.0, "sfx:pop"),
            marker(30.3, "sfx:pop"),
        ];
        let out = ask(&inputs(t, &[]), SfxDensity::Subtle);
        assert_eq!(
            times(&out),
            [5.0, 9.0, 14.0, 25.0, 30.0, 30.3],
            "two markers 0.3 s apart both stay"
        );
        assert_eq!(
            kinds(&out),
            [
                SfxKind::Whoosh,
                SfxKind::ReverseSwell,
                SfxKind::Impact,
                SfxKind::RecordScratch,
                SfxKind::Pop,
                SfxKind::Pop
            ]
        );
        assert!(out.iter().all(|s| s.source == SfxSource::Marker && s.score == 1.0));
        assert_eq!(out[0].reason, "Marker \"sfx:whoosh\" at 0:05.0 asks for a whoosh");
        assert_eq!(out[2].reason, "Marker \"sfx\" at 0:14.0 asks for a hit");

        // A marker beats the cut it sits on, and the cut near it.
        let mut cut = run_of_shots(2, 4.0);
        cut.timeline.markers.push(marker(4.5, "sfx:thud"));
        let out = ask(&cut, SfxDensity::Balanced);
        assert_eq!((sources(&out), kinds(&out)), (vec![SfxSource::Marker], vec![SfxKind::Thud]));

        // The request's filter applies to the kind the marker names.
        let mut only = request(SfxDensity::Subtle);
        only.categories = vec![SfxCategory::Ui];
        let mut t = Timeline::new();
        t.markers = vec![marker(5.0, "sfx:whoosh"), marker(8.0, "sfx:pop")];
        assert_eq!(kinds(&suggest(&inputs(t, &[]), &only).unwrap()), [SfxKind::Pop]);
    }

    #[test]
    fn nothing_is_suggested_that_add_sound_effects_would_refuse() {
        // A riser's hit is its end: at 0.01 s nothing of it would be left, at 0.5 s half a second is.
        let mut t = Timeline::new();
        t.markers = vec![marker(0.01, "sfx:riser"), marker(0.5, "sfx:riser"), marker(0.0, "sfx:impact")];
        let out = ask(&inputs(t, &[]), SfxDensity::Subtle);
        assert_eq!(times(&out), [0.0, 0.5]);
        for s in &out {
            assert!(lands(&s.placement.spec, s.placement.at), "{s:?}");
        }
        // A riser is dropped for the moment but not the impact beside it.
        let mut only = request(SfxDensity::Subtle);
        only.kinds = vec![SfxKind::Riser];
        let early = composed(score(&[(SectionKind::Drop, 4)], Vec::new()), 0.0, 8.0, 0.5);
        assert!(suggest(&early, &only).unwrap().is_empty(), "the riser has no room before 1 s");
        // The same rule as the placement's: a cut head must leave 20 ms.
        let riser = SfxSpec::new(SfxKind::Riser);
        assert!(!lands(&riser, 0.01) && lands(&riser, 0.02) && lands(&riser, 3.0));
        let mut reversed = SfxSpec::new(SfxKind::Riser);
        reversed.reverse = true;
        assert!(lands(&reversed, 0.0));
    }

    #[test]
    fn the_opening_hit_is_for_punchy_and_goes_on_the_first_picture() {
        let a = asset("a.mp4");
        let i = inputs(
            timeline_of(vec![video_track(vec![shot(&a, 0.0, 6.0, 2.0), shot(&a, 20.0, 26.0, 8.0)])]),
            &[&a],
        );
        let punchy = ask(&i, SfxDensity::Punchy);
        assert_eq!(
            (times(&punchy)[0], kinds(&punchy)[0], punchy[0].source),
            (2.0, SfxKind::Impact, SfxSource::Opening)
        );
        assert_eq!(punchy[0].reason, "Hook: a hit as a.mp4 opens at 0:02.0");
        for d in [SfxDensity::Subtle, SfxDensity::Balanced] {
            assert!(ask(&i, d).iter().all(|s| s.source != SfxSource::Opening));
        }
        assert!(
            ask(&inputs(Timeline::new(), &[]), SfxDensity::Punchy).is_empty(),
            "no picture, no hook"
        );
    }

    #[test]
    fn a_moment_an_effect_already_covers_is_skipped_but_one_further_off_is_not() {
        // Cuts at 4 s and 8 s; an effect clip across the first, and one that ends 0.2 s / 0.3 s before the second.
        let mut i = run_of_shots(3, 4.0);
        let mut fx = Asset {
            name: "SFX: Whoosh".to_string(),
            duration: 1.0,
            ..test_asset(vec![audio_stream(48_000, 2)])
        };
        fx.sound_effect = Some(SfxSpec::new(SfxKind::Whoosh));
        let mut lane = Track::new(StreamKind::Audio, "FX");
        lane.clips.push(shot(&fx, 0.0, 1.0, 3.5));
        i.timeline.tracks.push(lane);
        i.assets.push(fx);
        assert_eq!(
            times(&ask(&i, SfxDensity::Balanced)),
            [8.0],
            "the effect across the cut at 4 s covers it"
        );
        i.timeline.tracks[1].clips[0].timeline_start = 6.8; // 6.8..7.8: 0.2 s before the cut at 8
        assert!(ask(&i, SfxDensity::Balanced).iter().all(|s| s.placement.at != 8.0));
        i.timeline.tracks[1].clips[0].timeline_start = 6.7; // ends 0.3 s before it
        assert!(times(&ask(&i, SfxDensity::Balanced)).contains(&8.0));
        // Footage that is not a sound effect covers nothing.
        let a = asset("a.mp4");
        let mut plain = run_of_shots(2, 4.0);
        let mut track = Track::new(StreamKind::Audio, "A1");
        track.clips.push(shot(&a, 0.0, 6.0, 2.0));
        plain.timeline.tracks.push(track);
        plain.assets.push(a);
        assert_eq!(times(&ask(&plain, SfxDensity::Balanced)), [4.0]);
    }

    fn words(segment: &[(&str, f64, f64)]) -> Vec<TranscriptSegment> {
        let w: Vec<Word> = segment
            .iter()
            .map(|(text, start, end)| Word {
                text: format!(" {text}"),
                start: *start,
                end: *end,
                ..Word::default()
            })
            .collect();
        vec![TranscriptSegment {
            start: w[0].start,
            end: w[w.len() - 1].end,
            text: segment.iter().map(|w| w.0).collect::<Vec<_>>().join(" "),
            words: w,
            speaker: None,
        }]
    }

    #[test]
    fn over_speech_the_effect_is_quieter() {
        // Cuts at 4 s and 10 s. Speech at 3.6-4.3 s (the first clip's source) and nothing at 10 s.
        let (a, b) = (asset("a.mp4"), asset("b.mp4"));
        let t = timeline_of(vec![video_track(vec![
            shot(&a, 0.0, 4.0, 0.0),
            shot(&b, 20.0, 26.0, 4.0),
            shot(&a, 30.0, 34.0, 10.0),
        ])]);
        let mut i = inputs(t, &[&a, &b]);
        i.analyses.insert(
            a.id,
            AssetAnalysis {
                asset_id: a.id,
                transcript: words(&[("so", 3.6, 3.8), ("then", 3.85, 4.3)]),
                ..AssetAnalysis::default()
            },
        );
        let out = ask(&i, SfxDensity::Balanced);
        assert_eq!(times(&out), [4.0, 10.0]);
        assert_eq!(out[0].placement.volume_db, Some(SPEECH_GAIN_DB));
        assert_eq!(out[1].placement.volume_db, Some(CLEAR_GAIN_DB));

        // Words are read through the clip: b's words at source 21-21.5 are at 5-5.5 s on the timeline,
        // and words outside the clip's window are not heard at all.
        i.analyses.insert(
            b.id,
            AssetAnalysis {
                asset_id: b.id,
                transcript: words(&[("hello", 21.0, 21.5), ("outside", 1.0, 1.5), ("late", 27.0, 27.5)]),
                ..AssetAnalysis::default()
            },
        );
        i.timeline.markers = vec![marker(5.2, "sfx:pop"), marker(1.2, "sfx:pop"), marker(8.0, "sfx:pop")];
        let out = ask(&i, SfxDensity::Subtle);
        let gain = |t: f64| out.iter().find(|s| s.placement.at == t).unwrap().placement.volume_db;
        assert_eq!(gain(5.2), Some(SPEECH_GAIN_DB));
        assert_eq!(gain(1.2), Some(CLEAR_GAIN_DB));
        assert_eq!(gain(8.0), Some(CLEAR_GAIN_DB));
        // A clip that does not play its own sound, or a muted track, has no speech to speak of.
        i.timeline.tracks[0].clips[1].source_audio = false;
        assert_eq!(
            ask(&i, SfxDensity::Subtle)
                .iter()
                .find(|s| s.placement.at == 5.2)
                .unwrap()
                .placement
                .volume_db,
            Some(CLEAR_GAIN_DB)
        );
        i.timeline.tracks[0].muted = true;
        assert!(ask(&i, SfxDensity::Subtle)
            .iter()
            .all(|s| s.placement.volume_db == Some(CLEAR_GAIN_DB)));
    }

    #[test]
    fn a_filter_picks_the_kinds_a_source_can_use_and_what_it_leaves_out_holds_nothing_back() {
        // A cut at 4 s, a pop for a title at 8 s, a lower third at 12 s.
        let mut i = run_of_shots(2, 8.0);
        i.timeline.overlays.push(TextOverlay {
            pos_y: 0.4,
            ..TextOverlay::new("Hi", 12.0, 14.0)
        });
        i.timeline.overlays.push(TextOverlay::new("Jane Doe", 20.0, 23.0));
        let with = |categories: Vec<SfxCategory>, kinds: Vec<SfxKind>| {
            suggest(
                &i,
                &SfxSuggestRequest {
                    density: SfxDensity::Balanced,
                    categories,
                    kinds,
                    ..SfxSuggestRequest::default()
                },
            )
            .unwrap()
        };
        let every = with(vec![], vec![]);
        assert_eq!(kinds(&every), [SfxKind::Whoosh, SfxKind::Pop, SfxKind::Swipe]);
        assert_eq!(times(&every), [8.0, 12.0, 20.0]);
        let ui = with(vec![SfxCategory::Ui], vec![]);
        assert_eq!(kinds(&ui), [SfxKind::Pop]);
        assert_eq!(times(&ui), [12.0]);
        let swipes = with(vec![], vec![SfxKind::Swipe]);
        assert_eq!(
            (kinds(&swipes), times(&swipes), sources(&swipes)),
            (
                vec![SfxKind::Swipe, SfxKind::Swipe],
                vec![8.0, 20.0],
                vec![SfxSource::Cut, SfxSource::Title]
            ),
            "the cut takes a swipe when it is the one allowed"
        );
        // A category and a kind are both allowed.
        let both = with(vec![SfxCategory::Ui], vec![SfxKind::Swipe]);
        assert_eq!(kinds(&both), [SfxKind::Swipe, SfxKind::Pop, SfxKind::Swipe]);
        // Nothing the sources can use: nothing.
        assert!(with(vec![SfxCategory::Texture], vec![]).is_empty());
        assert!(with(vec![], vec![SfxKind::Heartbeat]).is_empty());

        // A title that the filter leaves no effect for does not keep a cut 0.5 s from it away.
        let mut near = run_of_shots(2, 4.0);
        near.timeline.overlays.push(TextOverlay {
            pos_y: 0.4,
            ..TextOverlay::new("Hi", 4.5, 7.0)
        });
        let all = ask(&near, SfxDensity::Balanced);
        assert_eq!(sources(&all), [SfxSource::Title]);
        let transitions = suggest(
            &near,
            &SfxSuggestRequest {
                categories: vec![SfxCategory::Transition],
                ..SfxSuggestRequest::default()
            },
        )
        .unwrap();
        assert_eq!(sources(&transitions), [SfxSource::Cut]);

        // A drop keeps its impact when builds are filtered out, and loses its riser.
        let drop = composed(
            score(&[(SectionKind::Verse, 4), (SectionKind::Drop, 4)], Vec::new()),
            0.0,
            16.0,
            0.0,
        );
        let out = suggest(
            &drop,
            &SfxSuggestRequest {
                categories: vec![SfxCategory::Impact],
                ..SfxSuggestRequest::default()
            },
        )
        .unwrap();
        assert_eq!(kinds(&out), [SfxKind::Impact]);
        let out = suggest(
            &drop,
            &SfxSuggestRequest {
                kinds: vec![SfxKind::ReverseSwell, SfxKind::Boom],
                ..SfxSuggestRequest::default()
            },
        )
        .unwrap();
        assert_eq!(
            kinds(&out),
            [SfxKind::ReverseSwell, SfxKind::Boom],
            "fallbacks stand in when they are all that is allowed"
        );
    }

    #[test]
    fn a_range_holds_what_the_whole_cut_holds_there() {
        let i = run_of_shots(20, 2.5);
        let whole = ask(&i, SfxDensity::Punchy);
        let range = TimeRange { start: 10.0, end: 25.0 };
        let part = suggest(
            &i,
            &SfxSuggestRequest {
                range: Some(range),
                density: SfxDensity::Punchy,
                ..SfxSuggestRequest::default()
            },
        )
        .unwrap();
        let expected: Vec<&SfxSuggestion> = whole
            .iter()
            .filter(|s| s.placement.at >= 10.0 && s.placement.at < 25.0)
            .collect();
        assert!(!part.is_empty());
        assert_eq!(
            part.iter().collect::<Vec<_>>(),
            expected,
            "the kinds and seeds are the whole cut's too"
        );
        // Half-open: a hit at the end is not in the range.
        let end = suggest(
            &i,
            &SfxSuggestRequest {
                range: Some(TimeRange { start: 2.5, end: 5.0 }),
                density: SfxDensity::Punchy,
                ..SfxSuggestRequest::default()
            },
        )
        .unwrap();
        assert_eq!(times(&end), [2.5]);
        for bad in [(5.0, 5.0), (6.0, 2.0), (-1.0, 3.0), (f64::NAN, 3.0), (0.0, f64::INFINITY)] {
            let e = suggest(
                &i,
                &SfxSuggestRequest {
                    range: Some(TimeRange {
                        start: bad.0,
                        end: bad.1,
                    }),
                    ..SfxSuggestRequest::default()
                },
            )
            .unwrap_err()
            .to_string();
            assert!(e.contains("range needs 0 <= start < end"), "{bad:?}: {e}");
        }
    }

    #[test]
    fn at_most_one_call_of_effects_comes_back_and_they_are_the_best() {
        // 600 one-second shots: a cut each second at punchy.
        let i = run_of_shots(600, 1.0);
        let out = ask(&i, SfxDensity::Punchy);
        assert_eq!(out.len(), MAX_SFX_PER_CALL);
        assert!(times(&out).windows(2).all(|w| w[0] <= w[1]));
        // The opening (0.8) outscores a cut, so it is kept.
        assert_eq!(out[0].source, SfxSource::Opening);
    }

    #[test]
    fn the_same_cut_gives_the_same_suggestions_and_they_survive_json() {
        let mut i = run_of_shots(8, 3.0);
        i.timeline.markers.push(marker(7.0, "sfx:ding"));
        i.timeline.overlays.push(TextOverlay::new("Name", 3.5, 6.0));
        let first = ask(&i, SfxDensity::Punchy);
        assert_eq!(first, ask(&i, SfxDensity::Punchy));
        assert_eq!(first, ask(&i.clone(), SfxDensity::Punchy));
        let json = serde_json::to_string(&first).unwrap();
        let back: Vec<SfxSuggestion> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, first);

        // The wire shape the surfaces pass on: a flat placement, snake_case sources.
        let cut = first.iter().find(|s| s.source == SfxSource::Cut).unwrap();
        let v: serde_json::Value = serde_json::to_value(cut).unwrap();
        assert_eq!(v["source"], "cut");
        assert_eq!(v["placement"]["kind"], "whoosh");
        assert_eq!(v["placement"]["at"], 6.0);
        assert_eq!(v["placement"]["volume_db"], -3.0);
        assert!(v["placement"].get("track_id").is_none());
        assert!(v["reason"].as_str().unwrap().starts_with("Cut from"));
        for s in SfxSource::ALL {
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.name()));
        }
    }

    #[test]
    fn a_request_reads_flat_defaults_to_balanced_and_refuses_what_it_does_not_know() {
        let r: SfxSuggestRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r.density, SfxDensity::Balanced);
        assert!(r.range.is_none() && r.categories.is_empty() && r.kinds.is_empty());
        let r: SfxSuggestRequest = serde_json::from_str(
            r#"{"density":"punchy","range":{"start":1,"end":5},"categories":["ui","impact"],"kinds":["impact","whoosh"]}"#,
        )
        .unwrap();
        assert_eq!(r.density, SfxDensity::Punchy);
        assert_eq!(r.categories, [SfxCategory::Ui, SfxCategory::Impact]);
        assert_eq!(r.kinds, [SfxKind::Impact, SfxKind::Whoosh]);
        assert!(r.allows(SfxKind::Pop) && r.allows(SfxKind::Impact) && r.allows(SfxKind::Whoosh));
        assert!(!r.allows(SfxKind::Riser));
        let e = serde_json::from_str::<SfxSuggestRequest>(r#"{"densty":"punchy"}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown field `densty`"), "{e}");
        assert!(serde_json::from_str::<SfxSuggestRequest>(r#"{"density":"loud"}"#).is_err());
        // The defaults are not written out.
        assert_eq!(
            serde_json::to_string(&SfxSuggestRequest::default()).unwrap(),
            r#"{"density":"balanced"}"#
        );
    }

    /// A small deterministic generator for the property test.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 33
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }

        fn unit(&mut self) -> f64 {
            (self.next() % 10_000) as f64 / 10_000.0
        }
    }

    /// Whatever the cut, the suggestions are sorted, spaced, valid placements of allowed kinds that the
    /// placement would accept, the same on a second reading, and a range holds the whole cut's share.
    #[test]
    fn random_cuts_always_suggest_valid_spaced_and_placeable_effects() {
        let (a, b, c) = (asset("a.mp4"), asset("b.mp4"), asset("c.mp4"));
        let assets = [&a, &b, &c];
        let mut rng = Lcg(7);
        let mut seen = std::collections::HashSet::new();
        for round in 0..150 {
            let n = 1 + rng.below(25) as usize;
            let mut clips = Vec::new();
            let mut at = rng.unit() * 2.0;
            for _ in 0..n {
                let asset = assets[rng.below(3) as usize];
                let len = 0.2 + rng.unit() * 6.0;
                let source_in = rng.unit() * 30.0;
                let mut clip = shot(asset, source_in, source_in + len, at);
                match rng.below(5) {
                    0 => clip.transform.scale = 1.0 + rng.unit() * 0.4,
                    1 => {
                        clip.transition_in = Some(Transition {
                            kind: TransitionKind::ALL[rng.below(11) as usize],
                            duration: 0.05 + rng.unit() * 2.0,
                        });
                    }
                    _ => {}
                }
                at += len + if rng.below(8) == 0 { rng.unit() } else { 0.0 };
                clips.push(clip);
            }
            let mut timeline = timeline_of(vec![video_track(clips)]);
            for _ in 0..rng.below(5) {
                let start = rng.unit() * at;
                timeline.overlays.push(TextOverlay {
                    pos_y: rng.unit(),
                    ..TextOverlay::new("Some text", start, start + 0.1 + rng.unit() * 4.0)
                });
            }
            let names = ["sfx:riser", "sfx", "sfx:reverse_swell", "sfx:heartbeat", "note", "sfx:glitch"];
            for _ in 0..rng.below(5) {
                timeline.markers.push(marker(rng.unit() * at, names[rng.below(6) as usize]));
            }
            let mut i = inputs(timeline, &assets);
            let mut words_at = rng.unit() * at;
            let spoken: Vec<(&str, f64, f64)> = (0..10)
                .map(|_| {
                    let w = ("word", words_at, words_at + 0.3);
                    words_at += 0.4;
                    w
                })
                .collect();
            i.analyses.insert(
                a.id,
                AssetAnalysis {
                    asset_id: a.id,
                    transcript: words(&spoken),
                    scene_changes: vec![5.0, 12.0, 20.0],
                    ..AssetAnalysis::default()
                },
            );
            let density = SfxDensity::ALL[rng.below(3) as usize];
            let mut req = request(density);
            if rng.below(3) == 0 {
                req.categories = vec![SfxCategory::ALL[rng.below(5) as usize]];
            }
            if rng.below(3) == 0 {
                req.kinds = vec![SfxKind::ALL[rng.below(21) as usize]];
            }
            let out = suggest(&i, &req).unwrap();
            seen.extend(out.iter().map(|s| s.source));
            assert_eq!(out, suggest(&i, &req).unwrap(), "round {round}: deterministic");
            assert!(out.len() <= MAX_SFX_PER_CALL);
            assert!(times(&out).windows(2).all(|w| w[0] <= w[1]), "round {round}: sorted");
            for s in &out {
                let p = &s.placement;
                p.spec.validate().unwrap_or_else(|e| panic!("round {round}: {e}: {s:?}"));
                assert!(p.at.is_finite() && p.at >= 0.0, "round {round}: {s:?}");
                assert!(lands(&p.spec, p.at), "round {round}: {s:?}");
                assert!(matches!(p.volume_db, Some(v) if v == CLEAR_GAIN_DB || v == SPEECH_GAIN_DB));
                assert!(p.track_id.is_none() && (0.0..=1.0).contains(&s.score) && !s.reason.is_empty());
                assert!(req.allows(p.spec.kind), "round {round}: {s:?} against {req:?}");
                if density != SfxDensity::Punchy {
                    assert_ne!(s.source, SfxSource::Opening);
                }
                if density == SfxDensity::Subtle {
                    assert!(!matches!(s.source, SfxSource::Cut | SfxSource::PunchIn), "round {round}");
                }
            }
            let thinned: Vec<&SfxSuggestion> = out.iter().filter(|s| s.source != SfxSource::Marker).collect();
            for (x, y) in thinned.iter().zip(thinned.iter().skip(1)) {
                let apart = y.placement.at - x.placement.at;
                assert!(
                    apart >= density.spacing_s() - 1e-6 || (apart == 0.0 && x.source == y.source),
                    "round {round}: {x:?} {y:?}"
                );
            }
            let seeds: std::collections::HashSet<u64> = out.iter().map(|s| s.placement.spec.seed).collect();
            assert_eq!(seeds.len(), out.len(), "round {round}: every seed once");
            let range = TimeRange {
                start: at * 0.25,
                end: at * 0.75,
            };
            let ranged = suggest(
                &i,
                &SfxSuggestRequest {
                    range: Some(range),
                    ..req.clone()
                },
            )
            .unwrap();
            let share: Vec<&SfxSuggestion> = out
                .iter()
                .filter(|s| s.placement.at >= range.start && s.placement.at < range.end)
                .collect();
            assert_eq!(ranged.iter().collect::<Vec<_>>(), share, "round {round}");
        }
        for source in SfxSource::ALL.into_iter().filter(|s| *s != SfxSource::MusicDrop) {
            assert!(seen.contains(&source), "the random cuts never asked for {source:?}");
        }
    }
}
