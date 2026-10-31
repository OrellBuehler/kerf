//! Motion presets (B8a): an entrance, an exit or an emphasis as **pure data** that expands into
//! eased per-property keys ([`Clip::channels`]) on a clip.
//!
//! A preset adds no render path and no stored field: it only writes [`PropertyKey`]s into the tracks
//! of the five transform numbers (`scale`, `pos_x`, `pos_y`, `rotation`, `opacity`), so the export,
//! the still and the GPU plan draw it exactly as they draw keys a user typed in.
//!
//! **Each preset is a table row**, not code: a kind, a default span, a [`Base`] and, per property,
//! keys given as `(at, x, easing)`: `at` is the fraction (0..=1) of the preset's window the key sits
//! at, `x` how far the value departs from its reference, `easing` the shape of the segment leaving
//! it. For `scale` and `opacity` `x` is a factor of the reference (1 = no change), for `pos_x`,
//! `pos_y` (fractions of the frame) and `rotation` (degrees, clockwise) an offset added to it (0 = no
//! change); [`Property::neutral`] is the number that changes nothing. Every track runs from `at` 0 to
//! `at` 1, so the window is whole for every property the preset touches. A bezier easing is held to the
//! unit square (no overshoot), so an overshoot is its own key.
//!
//! **Relative to the pose.** The reference is what the clip does now ([`Clip::property_at`], sampled
//! before anything is written): an entrance ends on the pose at the window's end ([`Base::End`]), an
//! exit starts from the pose at its start ([`Base::Start`]), and a pulse, wiggle or bounce rides the
//! straight line between the two edge poses ([`Base::Trend`]: the clip's own motion goes on underneath).
//! A slide in onto a scaled, positioned clip lands on that clip's own framing. Anchoring the edge a
//! preset leaves neutral keeps it **idempotent**: applying the same preset again gives the same keys.
//! A [`MotionMode::Replace`] is relative to the number's *static* value instead, the framing the clip
//! rests at once the animation it throws away is gone: an entrance replacing a half-finished slide must
//! not park the clip where that slide happened to be at the window's end.
//!
//! **The window.** `at` is where it starts, in seconds from the clip's start. An entrance starts at 0 by
//! default, an exit ends at the clip's end (it starts `duration` before it), an emphasis starts at 0; a
//! preset with no fixed span (`push_in`, `pull_out`) fills the rest of the clip from `at`. `duration`
//! defaults to the preset's, and **is clamped to the clip** (to what is left after `at`): a half-second
//! fade on a 0.3 s clip is a 0.3 s fade, not an error. A window under [`MIN_MOTION_SECONDS`] is refused.
//!
//! **Replace or Add.** [`MotionMode::Replace`] gives the touched properties exactly the preset's keys
//! (their tracks are replaced; other properties are untouched). [`MotionMode::Add`] merges: the clip's
//! keys of those properties inside the window are replaced by the preset's, the ones outside are kept,
//! and a window edge that falls inside an eased segment splits it ([`Easing::split`]) so what is left
//! outside is the curve it was. So an entrance, an exit and a key in the middle coexist; where two
//! windows overlap the preset applied last wins inside its own. A property the legacy bundle drove is
//! detached from it ([`Clip::set_property_keys`]); the bundle's other numbers keep their keys.

use super::channels::upsert;
use super::*;
use crate::layer_geometry::TINY_SCALE;

/// The shortest window a preset is written over (seconds): under a frame at any rate Kerf cuts for.
pub const MIN_MOTION_SECONDS: f64 = 0.01;

/// The weakest and the strongest `intensity`: it scales how far every key departs from its reference.
pub const MIN_MOTION_INTENSITY: f64 = 0.25;
pub const MAX_MOTION_INTENSITY: f64 = 2.0;

/// What a preset does in a clip's life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MotionKind {
    Entrance,
    Exit,
    Emphasis,
}

/// How a preset meets the keys the clip already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MotionMode {
    /// The touched properties get exactly the preset's keys.
    Replace,
    /// The preset's keys replace the clip's inside the window; the ones outside stay.
    #[default]
    Add,
}

/// The presets. The wire name (snake_case) is what the surfaces share; a direction names where the
/// picture comes from (`slide_in_from_left`) or goes to (`slide_out_to_left`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MotionPreset {
    FadeIn,
    SlideInFromLeft,
    SlideInFromRight,
    SlideInFromTop,
    SlideInFromBottom,
    ZoomIn,
    PopIn,
    SpinIn,
    DropIn,
    FadeOut,
    SlideOutToLeft,
    SlideOutToRight,
    SlideOutToTop,
    SlideOutToBottom,
    ZoomOut,
    PopOut,
    SpinOut,
    DropOut,
    Pulse,
    Wiggle,
    Bounce,
    PushIn,
    PullOut,
}

/// How long a window is when `duration` is not given.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Span {
    Seconds(f64),
    /// The rest of the clip, from where the window starts.
    Clip,
}

/// Which pose the keys of a preset are relative to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Base {
    /// The pose at the window's start (an exit; a push in).
    Start,
    /// The pose at the window's end (an entrance; a pull out).
    End,
    /// The straight line between the poses at the two edges (a pulse, a wiggle, a bounce).
    Trend,
}

#[derive(Debug, Clone, Copy)]
struct KeySpec {
    at: f64,
    x: f64,
    easing: Easing,
}

#[derive(Debug, Clone, Copy)]
struct TrackSpec {
    prop: Property,
    keys: &'static [KeySpec],
}

#[derive(Debug)]
struct MotionSpec {
    name: &'static str,
    label: &'static str,
    hint: &'static str,
    kind: MotionKind,
    span: Span,
    base: Base,
    tracks: &'static [TrackSpec],
}

const LIN: Easing = Easing::Linear;
const IN: Easing = Easing::EaseIn;
const OUT: Easing = Easing::EaseOut;
const IO: Easing = Easing::EaseInOut;

const fn k(at: f64, x: f64, easing: Easing) -> KeySpec {
    KeySpec { at, x, easing }
}

const fn track(prop: Property, keys: &'static [KeySpec]) -> TrackSpec {
    TrackSpec { prop, keys }
}

const fn spec(
    name: &'static str,
    label: &'static str,
    hint: &'static str,
    kind: MotionKind,
    span: Span,
    base: Base,
    tracks: &'static [TrackSpec],
) -> MotionSpec {
    MotionSpec {
        name,
        label,
        hint,
        kind,
        span,
        base,
        tracks,
    }
}

use MotionKind::{Emphasis, Entrance, Exit};
use Property::{Opacity, PosX, PosY, Rotation, Scale};

/// The table, in [`MotionPreset`]'s order (a test holds the two together).
static SPECS: [MotionSpec; 23] = [
    spec(
        "fade_in",
        "Fade in",
        "Opacity rises from 0 to the clip's own.",
        Entrance,
        Span::Seconds(0.5),
        Base::End,
        &[track(Opacity, &[k(0.0, 0.0, OUT), k(1.0, 1.0, LIN)])],
    ),
    spec(
        "slide_in_from_left",
        "Slide in from left",
        "Slides in from beyond the left edge of the frame.",
        Entrance,
        Span::Seconds(0.6),
        Base::End,
        &[track(PosX, &[k(0.0, -1.0, OUT), k(1.0, 0.0, LIN)])],
    ),
    spec(
        "slide_in_from_right",
        "Slide in from right",
        "Slides in from beyond the right edge of the frame.",
        Entrance,
        Span::Seconds(0.6),
        Base::End,
        &[track(PosX, &[k(0.0, 1.0, OUT), k(1.0, 0.0, LIN)])],
    ),
    spec(
        "slide_in_from_top",
        "Slide in from top",
        "Slides in from above the frame.",
        Entrance,
        Span::Seconds(0.6),
        Base::End,
        &[track(PosY, &[k(0.0, -1.0, OUT), k(1.0, 0.0, LIN)])],
    ),
    spec(
        "slide_in_from_bottom",
        "Slide in from bottom",
        "Slides in from below the frame.",
        Entrance,
        Span::Seconds(0.6),
        Base::End,
        &[track(PosY, &[k(0.0, 1.0, OUT), k(1.0, 0.0, LIN)])],
    ),
    spec(
        "zoom_in",
        "Zoom in",
        "Grows from 70% of its size while fading in.",
        Entrance,
        Span::Seconds(0.6),
        Base::End,
        &[
            track(Scale, &[k(0.0, 0.7, OUT), k(1.0, 1.0, LIN)]),
            track(Opacity, &[k(0.0, 0.0, OUT), k(1.0, 1.0, LIN)]),
        ],
    ),
    spec(
        "pop_in",
        "Pop in",
        "Springs up from nothing, overshoots a little and settles.",
        Entrance,
        Span::Seconds(0.5),
        Base::End,
        &[
            track(Scale, &[k(0.0, 0.0, OUT), k(0.65, 1.12, IO), k(1.0, 1.0, LIN)]),
            track(Opacity, &[k(0.0, 0.0, LIN), k(0.3, 1.0, LIN), k(1.0, 1.0, LIN)]),
        ],
    ),
    spec(
        "spin_in",
        "Spin in",
        "Turns half a revolution while growing into place.",
        Entrance,
        Span::Seconds(0.7),
        Base::End,
        &[
            track(Rotation, &[k(0.0, -180.0, OUT), k(1.0, 0.0, LIN)]),
            track(Scale, &[k(0.0, 0.2, OUT), k(1.0, 1.0, LIN)]),
        ],
    ),
    spec(
        "drop_in",
        "Drop in",
        "Falls in from above the frame and settles with a small bounce.",
        Entrance,
        Span::Seconds(0.7),
        Base::End,
        &[track(
            PosY,
            &[k(0.0, -1.0, IN), k(0.62, 0.05, OUT), k(0.82, -0.015, IO), k(1.0, 0.0, LIN)],
        )],
    ),
    spec(
        "fade_out",
        "Fade out",
        "Opacity falls from the clip's own to 0.",
        Exit,
        Span::Seconds(0.5),
        Base::Start,
        &[track(Opacity, &[k(0.0, 1.0, IN), k(1.0, 0.0, LIN)])],
    ),
    spec(
        "slide_out_to_left",
        "Slide out to left",
        "Slides out beyond the left edge of the frame.",
        Exit,
        Span::Seconds(0.6),
        Base::Start,
        &[track(PosX, &[k(0.0, 0.0, IN), k(1.0, -1.0, LIN)])],
    ),
    spec(
        "slide_out_to_right",
        "Slide out to right",
        "Slides out beyond the right edge of the frame.",
        Exit,
        Span::Seconds(0.6),
        Base::Start,
        &[track(PosX, &[k(0.0, 0.0, IN), k(1.0, 1.0, LIN)])],
    ),
    spec(
        "slide_out_to_top",
        "Slide out to top",
        "Slides out above the frame.",
        Exit,
        Span::Seconds(0.6),
        Base::Start,
        &[track(PosY, &[k(0.0, 0.0, IN), k(1.0, -1.0, LIN)])],
    ),
    spec(
        "slide_out_to_bottom",
        "Slide out to bottom",
        "Slides out below the frame.",
        Exit,
        Span::Seconds(0.6),
        Base::Start,
        &[track(PosY, &[k(0.0, 0.0, IN), k(1.0, 1.0, LIN)])],
    ),
    spec(
        "zoom_out",
        "Zoom out",
        "Shrinks to 70% of its size while fading out.",
        Exit,
        Span::Seconds(0.6),
        Base::Start,
        &[
            track(Scale, &[k(0.0, 1.0, IN), k(1.0, 0.7, LIN)]),
            track(Opacity, &[k(0.0, 1.0, IN), k(1.0, 0.0, LIN)]),
        ],
    ),
    spec(
        "pop_out",
        "Pop out",
        "Swells a little, then shrinks to nothing.",
        Exit,
        Span::Seconds(0.5),
        Base::Start,
        &[
            track(Scale, &[k(0.0, 1.0, IO), k(0.35, 1.12, IN), k(1.0, 0.0, LIN)]),
            track(Opacity, &[k(0.0, 1.0, LIN), k(0.7, 1.0, LIN), k(1.0, 0.0, LIN)]),
        ],
    ),
    spec(
        "spin_out",
        "Spin out",
        "Turns half a revolution while shrinking away.",
        Exit,
        Span::Seconds(0.7),
        Base::Start,
        &[
            track(Rotation, &[k(0.0, 0.0, IN), k(1.0, 180.0, LIN)]),
            track(Scale, &[k(0.0, 1.0, IN), k(1.0, 0.2, LIN)]),
        ],
    ),
    spec(
        "drop_out",
        "Drop out",
        "Rises a touch, then falls out of the bottom of the frame.",
        Exit,
        Span::Seconds(0.7),
        Base::Start,
        &[track(PosY, &[k(0.0, 0.0, OUT), k(0.2, -0.04, IN), k(1.0, 1.0, LIN)])],
    ),
    spec(
        "pulse",
        "Pulse",
        "A quick beat: grows 8% and returns.",
        Emphasis,
        Span::Seconds(0.6),
        Base::Trend,
        &[track(Scale, &[k(0.0, 1.0, IO), k(0.5, 1.08, IO), k(1.0, 1.0, LIN)])],
    ),
    spec(
        "wiggle",
        "Wiggle",
        "Rocks left and right, settling to still.",
        Emphasis,
        Span::Seconds(0.8),
        Base::Trend,
        &[track(
            Rotation,
            &[
                k(0.0, 0.0, IO),
                k(0.14, 5.0, IO),
                k(0.36, -5.0, IO),
                k(0.57, 3.5, IO),
                k(0.78, -2.0, IO),
                k(1.0, 0.0, LIN),
            ],
        )],
    ),
    spec(
        "bounce",
        "Bounce",
        "Hops in place, each hop lower than the last.",
        Emphasis,
        Span::Seconds(0.8),
        Base::Trend,
        &[track(
            PosY,
            &[
                k(0.0, 0.0, OUT),
                k(0.22, -0.1, IN),
                k(0.45, 0.0, OUT),
                k(0.62, -0.04, IN),
                k(0.8, 0.0, OUT),
                k(0.9, -0.012, IN),
                k(1.0, 0.0, LIN),
            ],
        )],
    ),
    spec(
        "push_in",
        "Push in",
        "A slow zoom in, 15%, over the whole clip (Ken Burns).",
        Emphasis,
        Span::Clip,
        Base::Start,
        &[track(Scale, &[k(0.0, 1.0, IO), k(1.0, 1.15, LIN)])],
    ),
    spec(
        "pull_out",
        "Pull out",
        "A slow zoom out, 15%, over the whole clip.",
        Emphasis,
        Span::Clip,
        Base::End,
        &[track(Scale, &[k(0.0, 1.15, IO), k(1.0, 1.0, LIN)])],
    ),
];

impl MotionPreset {
    /// Every preset, entrances first, then exits, then emphases.
    pub const ALL: [MotionPreset; 23] = [
        MotionPreset::FadeIn,
        MotionPreset::SlideInFromLeft,
        MotionPreset::SlideInFromRight,
        MotionPreset::SlideInFromTop,
        MotionPreset::SlideInFromBottom,
        MotionPreset::ZoomIn,
        MotionPreset::PopIn,
        MotionPreset::SpinIn,
        MotionPreset::DropIn,
        MotionPreset::FadeOut,
        MotionPreset::SlideOutToLeft,
        MotionPreset::SlideOutToRight,
        MotionPreset::SlideOutToTop,
        MotionPreset::SlideOutToBottom,
        MotionPreset::ZoomOut,
        MotionPreset::PopOut,
        MotionPreset::SpinOut,
        MotionPreset::DropOut,
        MotionPreset::Pulse,
        MotionPreset::Wiggle,
        MotionPreset::Bounce,
        MotionPreset::PushIn,
        MotionPreset::PullOut,
    ];

    fn spec(self) -> &'static MotionSpec {
        &SPECS[self as usize]
    }

    /// The wire name (the JSON the model, the Tauri commands and the MCP tools share).
    pub fn as_str(self) -> &'static str {
        self.spec().name
    }

    /// Every wire name, for an "expected one of" message.
    pub fn wire_names() -> Vec<&'static str> {
        Self::ALL.iter().map(|p| p.as_str()).collect()
    }

    /// The preset a wire name spells.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        self.spec().label
    }

    pub fn hint(self) -> &'static str {
        self.spec().hint
    }

    pub fn kind(self) -> MotionKind {
        self.spec().kind
    }

    /// How long its window is when no duration is given; `None` is the rest of the clip.
    pub fn default_duration(self) -> Option<f64> {
        match self.spec().span {
            Span::Seconds(s) => Some(s),
            Span::Clip => None,
        }
    }

    /// The properties it writes keys for.
    pub fn properties(self) -> Vec<Property> {
        self.spec().tracks.iter().map(|t| t.prop).collect()
    }
}

/// One preset as a picker or an agent lists it.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct MotionPresetInfo {
    pub id: MotionPreset,
    pub label: &'static str,
    pub kind: MotionKind,
    /// Seconds; absent for a preset that fills the rest of the clip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_duration: Option<f64>,
    pub hint: &'static str,
    /// The numbers it keys.
    pub properties: Vec<Property>,
}

/// Every preset, in [`MotionPreset::ALL`]'s order.
pub fn motion_presets() -> Vec<MotionPresetInfo> {
    MotionPreset::ALL
        .into_iter()
        .map(|id| MotionPresetInfo {
            id,
            label: id.label(),
            kind: id.kind(),
            default_duration: id.default_duration(),
            hint: id.hint(),
            properties: id.properties(),
        })
        .collect()
}

/// `(start, end)` of a preset's window on a clip `len` seconds long (see the module docs).
fn motion_window(spec: &MotionSpec, len: f64, at: Option<f64>, duration: Option<f64>) -> Result<(f64, f64)> {
    if !len.is_finite() || len < MIN_MOTION_SECONDS {
        return Err(Error::InvalidArgument(format!(
            "the clip is {len:.3} s long: a motion needs at least {MIN_MOTION_SECONDS} s"
        )));
    }
    if duration.is_some_and(|d| !d.is_finite() || d < MIN_MOTION_SECONDS) {
        return Err(Error::InvalidArgument(format!(
            "duration must be a number of at least {MIN_MOTION_SECONDS} seconds"
        )));
    }
    if at.is_some_and(|a| !a.is_finite() || a < 0.0) {
        return Err(Error::InvalidArgument("at must be a number of seconds >= 0".to_string()));
    }
    let from = at.unwrap_or(0.0);
    let room = len - from;
    if room < MIN_MOTION_SECONDS {
        return Err(Error::InvalidArgument(format!(
            "at {from:.3} s leaves no room in a clip {len:.3} s long (a motion needs at least {MIN_MOTION_SECONDS} s)"
        )));
    }
    let wanted = duration.unwrap_or(match spec.span {
        Span::Seconds(s) => s,
        Span::Clip => room,
    });
    let d = wanted.min(room);
    if at.is_none() && spec.kind == MotionKind::Exit {
        return Ok((len - d, len));
    }
    Ok((from, if d >= room { len } else { from + d }))
}

/// `reference` moved by `x` (a factor for scale and opacity, an offset for the rest) at `intensity`,
/// held to the range of the number; a scale never goes below [`TINY_SCALE`] (or below its own
/// reference, when that is smaller: a key that changes nothing is exactly the pose).
fn shaped(prop: Property, reference: f64, x: f64, intensity: f64) -> Result<f64> {
    let multiplies = matches!(prop, Property::Scale | Property::Opacity);
    let raw = if multiplies {
        reference * (1.0 + (x - 1.0) * intensity)
    } else {
        reference + x * intensity
    };
    if !raw.is_finite() {
        return Err(Error::InvalidArgument(format!(
            "the motion would put the {} at {raw}",
            prop.label()
        )));
    }
    let raw = if prop == Property::Scale {
        raw.max(TINY_SCALE.min(reference))
    } else {
        raw
    };
    let value = prop.clamp(raw);
    prop.check(value)?;
    Ok(value)
}

/// The preset's keys for one property over `window`. Added, they are relative to what `clip` does now;
/// replacing, to the number's static value: the framing the clip rests at once the animation that is
/// being thrown away is gone (an entrance replacing a half-finished slide must not park the clip where
/// that slide happened to be at the window's end).
fn preset_keys(
    clip: &Clip,
    spec: &MotionSpec,
    t: &TrackSpec,
    window: (f64, f64),
    intensity: f64,
    mode: MotionMode,
) -> Result<Vec<PropertyKey>> {
    let (start, end) = window;
    let pose = |at: f64| match mode {
        MotionMode::Add => clip.property_at(t.prop, at),
        MotionMode::Replace => clip.static_value(t.prop),
    };
    let (pose_start, pose_end) = (pose(start), pose(end));
    let last = t.keys.len() - 1;
    t.keys
        .iter()
        .enumerate()
        .map(|(i, key)| {
            let time = if key.at >= 1.0 { end } else { start + key.at * (end - start) };
            let reference = match spec.base {
                Base::Start => pose_start,
                Base::End => pose_end,
                Base::Trend => pose_start * (1.0 - key.at) + pose_end * key.at,
            };
            Ok(PropertyKey {
                time,
                value: shaped(t.prop, reference, key.x, intensity)?,
                easing: if i == last { Easing::Linear } else { key.easing },
            })
        })
        .collect()
}

/// `fresh` (the preset's keys, sorted, spanning the window) merged into the clip's keys of `prop`
/// ([`MotionMode::Add`]): its own inside the window go, the ones outside stay, and the segment each
/// window edge falls in is split first so that what stays is the curve it was.
fn merged(clip: &Clip, prop: Property, mut fresh: Vec<PropertyKey>) -> Vec<PropertyKey> {
    const SAME: f64 = 1e-6;
    let existing = clip.property_keys(prop);
    if existing.is_empty() {
        return fresh;
    }
    let (start, end) = (fresh[0].time, fresh[fresh.len() - 1].time);
    let mut keys = existing;
    for edge in [start, end] {
        upsert(&mut keys, PropertyKey::new(edge, clip.property_at(prop, edge)));
    }
    let leaving = keys
        .iter()
        .find(|k| (k.time - end).abs() <= SAME)
        .map_or(Easing::Linear, |k| k.easing);
    if let Some(last) = fresh.last_mut() {
        last.easing = leaving;
    }
    let mut out: Vec<PropertyKey> = keys.iter().filter(|k| k.time < start - SAME).copied().collect();
    out.append(&mut fresh);
    out.extend(keys.iter().filter(|k| k.time > end + SAME).copied());
    out
}

impl Clip {
    /// The window `preset` would be written over on this clip: `(start, end)` in seconds from the
    /// clip's start. Refuses a duration under [`MIN_MOTION_SECONDS`], a negative or non-finite `at`
    /// and a clip with no room.
    pub fn motion_window(&self, preset: MotionPreset, at: Option<f64>, duration: Option<f64>) -> Result<(f64, f64)> {
        motion_window(preset.spec(), self.duration(), at, duration)
    }

    /// Write `preset` into the clip's animation (see the module docs) and answer the window it
    /// covers. All or nothing: a refusal leaves the clip as it was. `intensity` (default 1, within
    /// [`MIN_MOTION_INTENSITY`]..=[`MAX_MOTION_INTENSITY`]) scales how far each key departs from its
    /// reference.
    pub fn apply_motion(
        &mut self,
        preset: MotionPreset,
        mode: MotionMode,
        at: Option<f64>,
        duration: Option<f64>,
        intensity: Option<f64>,
    ) -> Result<(f64, f64)> {
        let intensity = match intensity {
            None => 1.0,
            Some(i) if i.is_finite() && (MIN_MOTION_INTENSITY..=MAX_MOTION_INTENSITY).contains(&i) => i,
            Some(i) => {
                return Err(Error::InvalidArgument(format!(
                    "intensity must be within {MIN_MOTION_INTENSITY}..={MAX_MOTION_INTENSITY} (got {i})"
                )))
            }
        };
        let spec = preset.spec();
        let window = self.motion_window(preset, at, duration)?;
        let mut written = Vec::with_capacity(spec.tracks.len());
        for t in spec.tracks {
            let fresh = preset_keys(self, spec, t, window, intensity, mode)?;
            let keys = match mode {
                MotionMode::Replace => fresh,
                MotionMode::Add => merged(self, t.prop, fresh),
            };
            written.push((t.prop, keys));
        }
        for (prop, keys) in written {
            self.set_property_keys(prop, keys);
        }
        Ok(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const CORPUS_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../frontend/src/lib/fixtures/motion-presets-corpus.json"
    );

    fn clip(len: f64) -> Clip {
        Clip::new(Uuid::from_u128(0xA1), 0.0, len, 0.0)
    }

    fn posed(len: f64) -> Clip {
        let mut c = clip(len);
        c.transform = Transform {
            scale: 0.5,
            pos_x: 0.1,
            pos_y: -0.2,
            rotation: 15.0,
            opacity: 0.8,
            ..Transform::default()
        };
        c
    }

    fn key(time: f64, value: f64, easing: Easing) -> PropertyKey {
        PropertyKey { time, value, easing }
    }

    fn keys_of(c: &Clip, prop: Property) -> Vec<(f64, f64)> {
        c.property_keys(prop).iter().map(|k| (k.time, k.value)).collect()
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn apply(c: &mut Clip, preset: MotionPreset, mode: MotionMode, at: Option<f64>, d: Option<f64>) -> (f64, f64) {
        c.apply_motion(preset, mode, at, d, None).unwrap()
    }

    #[test]
    fn the_table_is_whole_and_in_the_enum_s_order() {
        assert_eq!(SPECS.len(), MotionPreset::ALL.len());
        for (i, p) in MotionPreset::ALL.into_iter().enumerate() {
            assert_eq!(p as usize, i, "{p:?} is out of order");
            assert_eq!(serde_json::to_value(p).unwrap(), p.as_str(), "the wire name is the table's");
            assert_eq!(MotionPreset::parse(p.as_str()), Some(p));
            assert!(!p.label().is_empty() && !p.hint().is_empty());
            let s = p.spec();
            assert!(!s.tracks.is_empty());
            for t in s.tracks {
                assert!(t.prop.is_transform(), "{p:?}: only the transform numbers are keyed");
                assert!(t.keys.len() >= 2);
                assert_eq!((t.keys[0].at, t.keys[t.keys.len() - 1].at), (0.0, 1.0), "{p:?} {:?}", t.prop);
                for w in t.keys.windows(2) {
                    assert!(
                        w[1].at - w[0].at >= 0.05,
                        "{p:?}: keys {w:?} are closer than 5% of the window"
                    );
                }
                assert!(
                    t.keys
                        .iter()
                        .all(|k| !matches!(k.easing, Easing::Bezier { .. } | Easing::Hold)),
                    "a preset uses the named curves"
                );
                assert!(t.keys.iter().all(|k| k.x.is_finite()));
                // The edge a base anchors on is neutral, so a preset does not move its own pose.
                let neutral = t.prop.neutral();
                let (first, last) = (t.keys[0].x, t.keys[t.keys.len() - 1].x);
                match s.base {
                    Base::Start => assert_eq!(first, neutral, "{p:?} {:?}", t.prop),
                    Base::End => assert_eq!(last, neutral, "{p:?} {:?}", t.prop),
                    Base::Trend => assert_eq!((first, last), (neutral, neutral), "{p:?} {:?}", t.prop),
                }
                assert!(t.keys.iter().any(|k| k.x != neutral), "{p:?} {:?} moves nothing", t.prop);
            }
            let want = match s.kind {
                MotionKind::Entrance => Base::End,
                MotionKind::Exit => Base::Start,
                MotionKind::Emphasis => s.base,
            };
            assert_eq!(s.base, want, "{p:?}");
        }
        let names = MotionPreset::wire_names();
        assert_eq!(
            names.iter().collect::<HashSet<_>>().len(),
            names.len(),
            "wire names are unique"
        );
        assert_eq!(MotionPreset::parse("slide_in_left"), None);
        let kinds: Vec<MotionKind> = MotionPreset::ALL.iter().map(|p| p.kind()).collect();
        assert_eq!(kinds.iter().filter(|k| **k == MotionKind::Entrance).count(), 9);
        assert_eq!(kinds.iter().filter(|k| **k == MotionKind::Exit).count(), 9);
        assert_eq!(kinds.iter().filter(|k| **k == MotionKind::Emphasis).count(), 5);
    }

    #[test]
    fn the_listing_says_what_a_picker_and_an_agent_need() {
        let list = motion_presets();
        assert_eq!(list.len(), MotionPreset::ALL.len());
        let pop = serde_json::to_value(&list[6]).unwrap();
        assert_eq!(
            pop,
            json!({
                "id": "pop_in", "label": "Pop in", "kind": "entrance", "default_duration": 0.5,
                "hint": "Springs up from nothing, overshoots a little and settles.",
                "properties": ["scale", "opacity"]
            })
        );
        let push = serde_json::to_value(&list[21]).unwrap();
        assert_eq!(push["id"], "push_in");
        assert!(
            push.get("default_duration").is_none(),
            "the rest of the clip has no default length"
        );
        assert_eq!(serde_json::to_value(MotionMode::default()).unwrap(), "add");
        assert_eq!(serde_json::to_value(MotionMode::Replace).unwrap(), "replace");
    }

    #[test]
    fn an_entrance_ends_on_the_clip_s_own_pose_and_an_exit_starts_from_it() {
        let own = posed(4.0).transform;
        for p in MotionPreset::ALL {
            let mut c = posed(4.0);
            let (start, end) = apply(&mut c, p, MotionMode::Add, None, None);
            let touched = p.properties();
            match p.kind() {
                MotionKind::Entrance => assert_eq!((start, p.default_duration().map(|d| start + d)), (0.0, Some(end)), "{p:?}"),
                MotionKind::Exit => assert_eq!(end, 4.0, "{p:?}"),
                MotionKind::Emphasis => {}
            }
            for prop in Property::TRANSFORM {
                let pose = |t: &Transform| match prop {
                    Property::Scale => t.scale,
                    Property::PosX => t.pos_x,
                    Property::PosY => t.pos_y,
                    Property::Rotation => t.rotation,
                    _ => t.opacity,
                };
                if !touched.contains(&prop) {
                    assert!(c.channel(prop).is_none(), "{p:?} wrote an untouched {prop:?}");
                    continue;
                }
                let at = |t: f64| c.transform_at(t);
                match p.spec().base {
                    Base::End => {
                        assert!(
                            near(pose(&at(end)), pose(&own)),
                            "{p:?} {prop:?}: the entrance ends on the pose"
                        );
                        assert!(near(pose(&at(end + 1.0)), pose(&own)), "{p:?} {prop:?}: and stays there");
                    }
                    Base::Start => {
                        assert!(
                            near(pose(&at(start)), pose(&own)),
                            "{p:?} {prop:?}: the exit starts from the pose"
                        );
                        assert!(
                            near(pose(&at(start - 0.3)), pose(&own)),
                            "{p:?} {prop:?}: before it, the pose"
                        );
                    }
                    Base::Trend => {
                        assert!(
                            near(pose(&at(start)), pose(&own)) && near(pose(&at(end)), pose(&own)),
                            "{p:?}"
                        );
                    }
                }
                let keys = c.property_keys(prop);
                assert!(
                    keys.windows(2).all(|w| w[1].time - w[0].time > 1e-4),
                    "{p:?} {prop:?}: {keys:?}"
                );
                assert!(keys.iter().all(|k| prop.check(k.value).is_ok()), "{p:?} {prop:?}: {keys:?}");
                assert!(
                    keys.iter().all(|k| (start..=end).contains(&k.time)),
                    "{p:?} {prop:?}: {keys:?}"
                );
            }
        }
    }

    #[test]
    fn a_slide_in_lands_on_the_clip_s_own_framing() {
        let mut c = posed(5.0);
        let (s, e) = apply(&mut c, MotionPreset::SlideInFromLeft, MotionMode::Add, None, None);
        assert_eq!((s, e), (0.0, 0.6));
        assert_eq!(keys_of(&c, Property::PosX), [(0.0, -0.9), (0.6, 0.1)]);
        assert_eq!(c.transform_at(0.6).pos_x, 0.1);
        assert_eq!(c.transform_at(0.3).scale, 0.5, "the other numbers stay as they were");
        let mut d = posed(5.0);
        apply(&mut d, MotionPreset::SlideOutToBottom, MotionMode::Add, None, None);
        assert_eq!(keys_of(&d, Property::PosY), [(4.4, -0.2), (5.0, 0.8)]);
        let mut e = posed(5.0);
        apply(&mut e, MotionPreset::SlideInFromTop, MotionMode::Add, Some(1.0), Some(1.0));
        assert_eq!(keys_of(&e, Property::PosY), [(1.0, -1.2), (2.0, -0.2)]);
    }

    #[test]
    fn pop_in_overshoots_by_an_extra_key_and_never_goes_below_a_tiny_scale() {
        let mut c = clip(3.0);
        apply(&mut c, MotionPreset::PopIn, MotionMode::Add, None, None);
        let scale = c.property_keys(Property::Scale);
        assert_eq!(scale.len(), 3);
        assert_eq!(
            scale[0].value, TINY_SCALE,
            "from nothing, held at the smallest scale the export draws"
        );
        assert!(near(scale[1].value, 1.12) && scale[1].time > scale[0].time && near(scale[2].value, 1.0));
        assert!(c.property_curve(Property::Scale).iter().all(|p| p.1 >= TINY_SCALE));
        assert!(scale.iter().all(|k| !matches!(k.easing, Easing::Bezier { .. })));
        for p in [
            MotionPreset::PopIn,
            MotionPreset::PopOut,
            MotionPreset::SpinIn,
            MotionPreset::ZoomOut,
        ] {
            for intensity in [0.25, 1.0, 2.0] {
                for own in [1.0, 0.5, 0.02, 0.004] {
                    let mut c = clip(3.0);
                    c.transform.scale = own;
                    c.apply_motion(p, MotionMode::Replace, None, None, Some(intensity)).unwrap();
                    let floor = TINY_SCALE.min(own);
                    assert!(
                        c.property_keys(Property::Scale).iter().all(|k| k.value >= floor),
                        "{p:?} x{intensity} at {own}: {:?}",
                        c.property_keys(Property::Scale)
                    );
                }
            }
        }
        let mut huge = clip(3.0);
        huge.transform.scale = 2.0;
        huge.apply_motion(MotionPreset::PopIn, MotionMode::Add, None, None, Some(2.0))
            .unwrap();
        assert!(near(huge.property_at(Property::Scale, 0.5), 2.0));
    }

    #[test]
    fn the_amplitude_scales_how_far_a_key_departs_and_a_value_stays_in_its_range() {
        let mut c = clip(4.0);
        c.apply_motion(MotionPreset::SlideInFromLeft, MotionMode::Add, None, None, Some(0.5))
            .unwrap();
        assert_eq!(keys_of(&c, Property::PosX), [(0.0, -0.5), (0.6, 0.0)]);
        let mut f = clip(4.0);
        f.apply_motion(MotionPreset::FadeIn, MotionMode::Add, None, None, Some(0.5))
            .unwrap();
        assert_eq!(
            keys_of(&f, Property::Opacity),
            [(0.0, 0.5), (0.5, 1.0)],
            "a half-strength fade starts half there"
        );
        let mut g = clip(4.0);
        g.apply_motion(MotionPreset::FadeIn, MotionMode::Add, None, None, Some(2.0))
            .unwrap();
        assert_eq!(keys_of(&g, Property::Opacity)[0].1, 0.0, "an opacity below 0 is 0");
        let mut w = clip(4.0);
        w.apply_motion(MotionPreset::Wiggle, MotionMode::Add, None, None, Some(2.0))
            .unwrap();
        assert!(w.property_keys(Property::Rotation).iter().any(|k| near(k.value, 10.0)));
        for bad in [0.0, 0.2, 2.1, f64::NAN, f64::INFINITY, -1.0] {
            let mut c = clip(4.0);
            let err = c
                .apply_motion(MotionPreset::Pulse, MotionMode::Add, None, None, Some(bad))
                .unwrap_err();
            assert!(err.to_string().contains("intensity"), "{bad}: {err}");
            assert!(c.channels.is_empty());
        }
    }

    #[test]
    fn the_window_is_clamped_to_the_clip_and_a_bad_one_is_refused() {
        let w = |len: f64, p: MotionPreset, at: Option<f64>, d: Option<f64>| clip(len).motion_window(p, at, d);
        assert_eq!(w(10.0, MotionPreset::FadeIn, None, None).unwrap(), (0.0, 0.5));
        assert_eq!(w(10.0, MotionPreset::FadeIn, None, Some(2.0)).unwrap(), (0.0, 2.0));
        assert_eq!(w(10.0, MotionPreset::FadeOut, None, None).unwrap(), (9.5, 10.0));
        assert_eq!(w(10.0, MotionPreset::FadeOut, None, Some(2.0)).unwrap(), (8.0, 10.0));
        assert_eq!(w(10.0, MotionPreset::FadeOut, Some(3.0), Some(2.0)).unwrap(), (3.0, 5.0));
        assert_eq!(w(10.0, MotionPreset::Pulse, Some(4.0), None).unwrap(), (4.0, 4.6));
        assert_eq!(
            w(10.0, MotionPreset::PushIn, None, None).unwrap(),
            (0.0, 10.0),
            "the whole clip"
        );
        assert_eq!(
            w(10.0, MotionPreset::PushIn, Some(4.0), None).unwrap(),
            (4.0, 10.0),
            "the rest of it"
        );
        assert_eq!(w(10.0, MotionPreset::PullOut, None, Some(3.0)).unwrap(), (0.0, 3.0));
        // Clamped to the clip, not refused.
        assert_eq!(w(0.3, MotionPreset::FadeIn, None, None).unwrap(), (0.0, 0.3));
        assert_eq!(w(0.3, MotionPreset::FadeOut, None, None).unwrap(), (0.0, 0.3));
        assert_eq!(w(2.0, MotionPreset::FadeIn, None, Some(60.0)).unwrap(), (0.0, 2.0));
        assert_eq!(
            w(2.0, MotionPreset::Wiggle, Some(1.5), None).unwrap(),
            (1.5, 2.0),
            "cut at the clip's end"
        );
        for (len, at, d) in [
            (10.0, None, Some(0.0)),
            (10.0, None, Some(-1.0)),
            (10.0, None, Some(f64::NAN)),
            (10.0, None, Some(f64::INFINITY)),
            (10.0, None, Some(0.005)),
            (10.0, Some(-0.1), None),
            (10.0, Some(f64::NAN), None),
            (10.0, Some(10.0), None),
            (10.0, Some(99.0), None),
            (10.0, Some(9.995), None),
            (0.005, None, None),
        ] {
            assert!(w(len, MotionPreset::Pulse, at, d).is_err(), "{len} s at {at:?} for {d:?}");
        }
        let mut c = clip(10.0);
        assert!(c
            .apply_motion(MotionPreset::Pulse, MotionMode::Add, None, Some(-1.0), None)
            .is_err());
        assert!(c.channels.is_empty(), "a refusal writes nothing");
        // The clip's own length is the timeline's: a sped-up clip is shorter.
        let mut fast = Clip::new(Uuid::nil(), 0.0, 10.0, 0.0);
        fast.speed = 2.0;
        assert_eq!(
            fast.motion_window(MotionPreset::FadeOut, None, Some(1.0)).unwrap(),
            (4.0, 5.0)
        );
    }

    #[test]
    fn add_keeps_the_keys_outside_the_window_and_splits_the_segment_an_edge_falls_in() {
        let mut c = clip(10.0);
        c.set_property_keys(
            Property::Scale,
            vec![key(0.0, 1.0, Easing::EaseInOut), key(10.0, 2.0, Easing::Linear)],
        );
        let before = c.clone();
        let (s, e) = apply(&mut c, MotionPreset::Pulse, MotionMode::Add, Some(4.0), Some(1.0));
        assert_eq!((s, e), (4.0, 5.0));
        let keys = c.property_keys(Property::Scale);
        assert_eq!(keys.len(), 5);
        assert_eq!(
            (keys[0].time, keys[1].time, keys[2].time, keys[3].time, keys[4].time),
            (0.0, 4.0, 4.5, 5.0, 10.0)
        );
        // The edges are on the clip's own curve, the middle is the bump riding the line between them.
        assert!(near(keys[1].value, before.property_at(Property::Scale, 4.0)));
        assert!(near(keys[3].value, before.property_at(Property::Scale, 5.0)));
        let line = (keys[1].value + keys[3].value) / 2.0;
        assert!(near(keys[2].value, line * 1.08), "{keys:?}");
        // The curve outside is the curve it was, split in two (a few thousandths off at most).
        assert!(matches!(keys[0].easing, Easing::Bezier { .. }), "{:?}", keys[0].easing);
        assert!(matches!(keys[3].easing, Easing::Bezier { .. }), "{:?}", keys[3].easing);
        for i in 0..=100 {
            let t = f64::from(i) * 0.1;
            if (4.0..=5.0).contains(&t) {
                continue;
            }
            let (a, b) = (before.property_at(Property::Scale, t), c.property_at(Property::Scale, t));
            assert!((a - b).abs() < 1.5e-2, "at {t}: {a} became {b}");
        }
        // Replace is only the preset.
        let mut r = before;
        apply(&mut r, MotionPreset::Pulse, MotionMode::Replace, Some(4.0), Some(1.0));
        let kept = r.property_keys(Property::Scale);
        assert_eq!(kept.len(), 3);
        assert_eq!((kept[0].time, kept[2].time), (4.0, 5.0));
        assert_eq!(
            r.property_at(Property::Scale, 0.0),
            kept[0].value,
            "held flat before the first key"
        );
        assert_eq!(r.property_at(Property::Scale, 9.0), kept[2].value, "and after the last");
    }

    #[test]
    fn a_hold_stays_held_through_a_window_that_cuts_it() {
        let mut c = clip(10.0);
        c.set_property_keys(
            Property::Opacity,
            vec![key(0.0, 1.0, Easing::Hold), key(8.0, 0.4, Easing::Linear)],
        );
        apply(&mut c, MotionPreset::FadeOut, MotionMode::Add, Some(2.0), Some(1.0));
        let keys = c.property_keys(Property::Opacity);
        assert_eq!(keys.iter().map(|k| k.time).collect::<Vec<_>>(), [0.0, 2.0, 3.0, 8.0]);
        assert_eq!(keys[0].easing, Easing::Hold);
        assert_eq!(keys[2].easing, Easing::Hold, "the rest of the hold is still a hold");
        assert_eq!(c.property_at(Property::Opacity, 1.9), 1.0);
        assert_eq!(
            c.property_at(Property::Opacity, 3.5),
            0.0,
            "the exit ended at 0 and holds there"
        );
        assert_eq!(c.property_at(Property::Opacity, 8.0), 0.4);
    }

    #[test]
    fn an_entrance_an_exit_and_a_key_in_the_middle_coexist() {
        let mut c = clip(6.0);
        c.set_property_keys(
            Property::Scale,
            vec![key(0.0, 1.0, Easing::Linear), key(6.0, 1.5, Easing::Linear)],
        );
        apply(&mut c, MotionPreset::FadeIn, MotionMode::Add, None, None);
        apply(&mut c, MotionPreset::FadeOut, MotionMode::Add, None, None);
        c.insert_property_key(Property::Opacity, PropertyKey::new(3.0, 0.6));
        assert_eq!(
            keys_of(&c, Property::Opacity),
            [(0.0, 0.0), (0.5, 1.0), (3.0, 0.6), (5.5, 1.0), (6.0, 0.0)]
        );
        // The same entrance again changes nothing, and so does the exit.
        let once = c.clone();
        apply(&mut c, MotionPreset::FadeIn, MotionMode::Add, None, None);
        apply(&mut c, MotionPreset::FadeOut, MotionMode::Add, None, None);
        assert_eq!(c.channels, once.channels);
        // A zoom through the same window as the entrance wins inside it and leaves the exit alone.
        apply(&mut c, MotionPreset::ZoomIn, MotionMode::Add, None, Some(0.8));
        let o = keys_of(&c, Property::Opacity);
        assert_eq!(o.first(), Some(&(0.0, 0.0)));
        assert_eq!(o[1].0, 0.8);
        assert!(o.contains(&(3.0, 0.6)) && o.last() == Some(&(6.0, 0.0)));
    }

    #[test]
    fn two_presets_on_a_short_clip_leave_sorted_keys_and_the_last_one_wins_where_they_meet() {
        let mut c = clip(1.0);
        apply(&mut c, MotionPreset::PopIn, MotionMode::Add, None, Some(0.8));
        apply(&mut c, MotionPreset::PopOut, MotionMode::Add, None, Some(0.8));
        for prop in [Property::Scale, Property::Opacity] {
            let keys = c.property_keys(prop);
            assert!(keys.windows(2).all(|w| w[0].time + 1e-4 < w[1].time), "{prop:?}: {keys:?}");
            assert!(
                keys.iter()
                    .all(|k| (0.0..=1.0).contains(&k.time) && prop.check(k.value).is_ok()),
                "{keys:?}"
            );
            assert_eq!(keys.first().map(|k| k.time), Some(0.0));
            assert_eq!(keys.last().map(|k| k.time), Some(1.0));
        }
        let scale = c.property_keys(Property::Scale);
        assert!(near(c.property_at(Property::Scale, 1.0), scale.last().unwrap().value));
        assert_eq!(scale.last().unwrap().value, TINY_SCALE, "the exit finishes the clip small");
        assert!(scale.iter().all(|k| k.value >= TINY_SCALE));
    }

    #[test]
    fn applying_a_preset_twice_gives_the_same_keys() {
        for base in [clip(6.0), posed(6.0)] {
            for p in MotionPreset::ALL {
                for mode in [MotionMode::Add, MotionMode::Replace] {
                    let mut once = base.clone();
                    apply(&mut once, p, mode, None, None);
                    let mut twice = once.clone();
                    apply(&mut twice, p, mode, None, None);
                    assert_eq!(once.channels, twice.channels, "{p:?} {mode:?}");
                }
            }
        }
    }

    #[test]
    fn replace_gives_the_touched_numbers_exactly_the_preset_and_leaves_the_rest() {
        let mut c = clip(6.0);
        c.set_property_keys(
            Property::Opacity,
            vec![key(1.0, 0.2, Easing::Linear), key(2.0, 0.9, Easing::Linear)],
        );
        c.set_property_keys(
            Property::Scale,
            vec![key(0.0, 1.0, Easing::Linear), key(6.0, 1.3, Easing::Linear)],
        );
        c.set_property_keys(
            Property::Brightness,
            vec![key(0.0, 0.0, Easing::Linear), key(6.0, 0.4, Easing::Linear)],
        );
        let before = c.clone();
        apply(&mut c, MotionPreset::FadeIn, MotionMode::Replace, None, None);
        assert_eq!(keys_of(&c, Property::Opacity), [(0.0, 0.0), (0.5, 1.0)]);
        assert_eq!(c.channel(Property::Scale), before.channel(Property::Scale));
        assert_eq!(c.channel(Property::Brightness), before.channel(Property::Brightness));
        apply(&mut c, MotionPreset::FadeIn, MotionMode::Add, None, None);
        assert_eq!(
            keys_of(&c, Property::Opacity),
            [(0.0, 0.0), (0.5, 1.0)],
            "nothing left outside to keep"
        );
    }

    #[test]
    fn a_number_the_bundle_drives_detaches_alone() {
        let bundle = |time: f64, scale: f64, pos_x: f64, opacity: f64, easing: Easing| Keyframe {
            time,
            scale,
            pos_x,
            pos_y: 0.0,
            rotation: 0.0,
            opacity,
            easing,
        };
        let mut c = clip(6.0);
        c.keyframes = vec![
            bundle(0.0, 1.0, 0.0, 1.0, Easing::EaseIn),
            bundle(3.0, 1.5, 0.2, 0.6, Easing::Linear),
            bundle(6.0, 1.2, 0.0, 1.0, Easing::Linear),
        ];
        let before = c.clone();
        apply(&mut c, MotionPreset::FadeOut, MotionMode::Add, None, Some(1.0));
        assert!(c.channel(Property::Opacity).is_some(), "the opacity took a track");
        for p in [Property::Scale, Property::PosX, Property::PosY, Property::Rotation] {
            assert!(c.channel(p).is_none(), "{p:?} keeps the bundle");
        }
        assert_eq!(c.keyframes, before.keyframes, "the bundle itself is not touched");
        for i in 0..=120 {
            let t = f64::from(i) * 0.05;
            for p in [Property::Scale, Property::PosX] {
                assert_eq!(c.property_at(p, t), before.property_at(p, t), "{p:?} at {t}");
            }
            if t < 5.0 {
                assert!(
                    (c.property_at(Property::Opacity, t) - before.property_at(Property::Opacity, t)).abs() < 1.5e-2,
                    "opacity at {t}: the bundle's curve still plays before the exit"
                );
            }
        }
        assert_eq!(c.property_at(Property::Opacity, 6.0), 0.0);
        let o = c.property_keys(Property::Opacity);
        assert_eq!((o.first().unwrap().time, o.last().unwrap().time), (0.0, 6.0));
        // Replacing the bundle's opacity does the same, to that number only.
        let mut r = before;
        apply(&mut r, MotionPreset::FadeOut, MotionMode::Replace, None, Some(1.0));
        assert_eq!(keys_of(&r, Property::Opacity), [(5.0, 1.0), (6.0, 0.0)]);
        assert!(r.channel(Property::Scale).is_none());
        assert_eq!(r.property_at(Property::Scale, 3.0), 1.5);
    }

    #[test]
    fn a_pulse_rides_the_clip_s_own_motion_underneath() {
        let mut c = clip(10.0);
        c.set_property_keys(
            Property::Scale,
            vec![key(0.0, 1.0, Easing::Linear), key(10.0, 2.0, Easing::Linear)],
        );
        let before = c.clone();
        apply(&mut c, MotionPreset::Pulse, MotionMode::Add, Some(2.0), Some(2.0));
        for t in [0.0, 1.0, 2.0, 4.0, 5.0, 10.0] {
            assert!(
                near(c.property_at(Property::Scale, t), before.property_at(Property::Scale, t)),
                "at {t}"
            );
        }
        let mid = before.property_at(Property::Scale, 3.0) * 1.08;
        assert!(
            near(c.property_at(Property::Scale, 3.0), mid),
            "{}",
            c.property_at(Property::Scale, 3.0)
        );
    }

    #[test]
    fn a_preset_keeps_to_its_numbers_and_writes_no_other_field() {
        let mut c = posed(5.0);
        let before = c.clone();
        apply(&mut c, MotionPreset::SpinIn, MotionMode::Add, None, None);
        assert_eq!(c.transform, before.transform, "the static transform is not touched");
        assert!(c.keyframes.is_empty() && c.color == before.color);
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["channels"].as_array().unwrap().len(), 2);
        assert_eq!(json["channels"][0]["prop"], "scale");
        assert_eq!(json["channels"][1]["prop"], "rotation");
        let back: Clip = serde_json::from_value(json).unwrap();
        assert_eq!(back.channels, c.channels);
    }

    fn ease_name(e: Easing) -> Value {
        serde_json::to_value(e).unwrap()
    }

    fn table() -> Value {
        Value::Array(
            MotionPreset::ALL
                .into_iter()
                .map(|p| {
                    let s = p.spec();
                    json!({
                        "id": s.name,
                        "kind": s.kind,
                        "span": p.default_duration(),
                        "base": match s.base { Base::Start => "start", Base::End => "end", Base::Trend => "trend" },
                        "tracks": s.tracks.iter().map(|t| json!({
                            "prop": t.prop,
                            "keys": t.keys.iter().map(|k| json!({"at": k.at, "x": k.x, "easing": ease_name(k.easing)})).collect::<Vec<_>>(),
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect(),
        )
    }

    struct Case {
        name: String,
        clip: Clip,
        ops: Vec<Value>,
    }

    fn op(p: MotionPreset, mode: MotionMode, at: Option<f64>, duration: Option<f64>, intensity: Option<f64>) -> Value {
        json!({"preset": p, "mode": mode, "at": at, "duration": duration, "intensity": intensity})
    }

    fn bundled() -> Clip {
        let mut c = clip(6.0);
        let k = |time, scale, pos_x, opacity, easing| Keyframe {
            time,
            scale,
            pos_x,
            pos_y: 0.0,
            rotation: 0.0,
            opacity,
            easing,
        };
        c.keyframes = vec![
            k(0.0, 1.0, 0.0, 1.0, Easing::EaseIn),
            k(3.0, 1.5, 0.2, 0.6, Easing::Linear),
            k(6.0, 1.2, 0.0, 1.0, Easing::Linear),
        ];
        c
    }

    fn moving() -> Clip {
        let mut c = clip(8.0);
        c.set_property_keys(
            Property::Scale,
            vec![key(0.0, 1.0, Easing::EaseInOut), key(8.0, 1.6, Easing::Linear)],
        );
        c.set_property_keys(
            Property::PosX,
            vec![
                key(0.0, -0.1, Easing::Linear),
                key(4.0, 0.1, Easing::Hold),
                key(8.0, 0.3, Easing::Linear),
            ],
        );
        c.set_property_keys(
            Property::Opacity,
            vec![key(2.0, 0.5, Easing::EaseOut), key(6.0, 1.0, Easing::Linear)],
        );
        c
    }

    fn cases() -> Vec<Case> {
        use MotionMode::{Add, Replace};
        let mut out = Vec::new();
        for p in MotionPreset::ALL {
            out.push(Case {
                name: format!("{} on a plain clip", p.as_str()),
                clip: clip(10.0),
                ops: vec![op(p, Add, None, None, None)],
            });
            out.push(Case {
                name: format!("{} replaces on a posed clip", p.as_str()),
                clip: posed(4.0),
                ops: vec![op(p, Replace, None, None, None)],
            });
            out.push(Case {
                name: format!("{} adds into a moving clip, placed, at half strength", p.as_str()),
                clip: moving(),
                ops: vec![op(p, Add, Some(3.0), Some(1.5), Some(0.5))],
            });
        }
        let mut tiny = clip(3.0);
        tiny.transform.scale = 0.015;
        let mut short = clip(0.3);
        short.transform.opacity = 0.9;
        let by = |name: &str, clip: Clip, ops: Vec<Value>| Case {
            name: name.to_string(),
            clip,
            ops,
        };
        out.extend([
            by(
                "a bundle-driven number detaches alone (fade out, add)",
                bundled(),
                vec![op(MotionPreset::FadeOut, Add, None, Some(1.0), None)],
            ),
            by(
                "a bundle-driven number detaches alone (zoom in, replace)",
                bundled(),
                vec![op(MotionPreset::ZoomIn, Replace, None, None, None)],
            ),
            by(
                "a push in over a bundle-animated zoom",
                bundled(),
                vec![op(MotionPreset::PushIn, Add, None, None, None)],
            ),
            by(
                "a pulse on an eased segment splits it",
                moving(),
                vec![op(MotionPreset::Pulse, Add, Some(5.0), Some(2.0), None)],
            ),
            by(
                "a fade out inside a hold keeps it held",
                moving(),
                vec![op(MotionPreset::SlideOutToRight, Add, Some(3.0), Some(2.0), None)],
            ),
            by(
                "an entrance, an exit and a pulse in the middle",
                clip(6.0),
                vec![
                    op(MotionPreset::FadeIn, Add, None, None, None),
                    op(MotionPreset::FadeOut, Add, None, None, None),
                    op(MotionPreset::Pulse, Add, Some(2.0), None, None),
                ],
            ),
            by(
                "the same entrance twice",
                posed(5.0),
                vec![
                    op(MotionPreset::PopIn, Add, None, None, None),
                    op(MotionPreset::PopIn, Add, None, None, None),
                ],
            ),
            by(
                "an entrance and an exit on a short clip",
                clip(1.0),
                vec![
                    op(MotionPreset::PopIn, Add, None, Some(0.8), None),
                    op(MotionPreset::PopOut, Add, None, Some(0.8), None),
                ],
            ),
            by(
                "a fade on a clip shorter than the fade",
                short.clone(),
                vec![op(MotionPreset::FadeIn, Add, None, None, None)],
            ),
            by(
                "an exit on a clip shorter than the exit",
                short,
                vec![op(MotionPreset::SlideOutToLeft, Add, None, None, None)],
            ),
            by(
                "a duration longer than the clip",
                clip(2.0),
                vec![op(MotionPreset::PullOut, Add, None, Some(60.0), None)],
            ),
            by(
                "a pop in on a tiny scale at double strength",
                tiny.clone(),
                vec![op(MotionPreset::PopIn, Add, None, None, Some(2.0))],
            ),
            by(
                "a pop out on a tiny scale",
                tiny,
                vec![op(MotionPreset::PopOut, Replace, None, None, None)],
            ),
            by(
                "a zero duration is refused",
                clip(4.0),
                vec![op(MotionPreset::Pulse, Add, None, Some(0.0), None)],
            ),
            by(
                "a negative duration is refused",
                clip(4.0),
                vec![op(MotionPreset::Pulse, Add, None, Some(-1.0), None)],
            ),
            by(
                "a start past the end is refused",
                clip(4.0),
                vec![op(MotionPreset::Pulse, Add, Some(9.0), None, None)],
            ),
            by(
                "a negative start is refused",
                clip(4.0),
                vec![op(MotionPreset::Pulse, Add, Some(-1.0), None, None)],
            ),
            by(
                "an intensity over the top is refused",
                clip(4.0),
                vec![op(MotionPreset::Pulse, Add, None, None, Some(3.0))],
            ),
            by(
                "an intensity under the bottom is refused",
                clip(4.0),
                vec![op(MotionPreset::Pulse, Add, None, None, Some(0.1))],
            ),
            by(
                "a clip with no room is refused",
                clip(0.005),
                vec![op(MotionPreset::FadeIn, Add, None, None, None)],
            ),
        ]);
        out
    }

    fn slim(c: &Clip) -> Value {
        json!({
            "duration": c.duration(),
            "transform": {
                "scale": c.transform.scale, "pos_x": c.transform.pos_x, "pos_y": c.transform.pos_y,
                "rotation": c.transform.rotation, "opacity": c.transform.opacity,
            },
            "keyframes": c.keyframes,
            "channels": c.channels,
        })
    }

    fn run(case: &Case) -> Value {
        let mut c = case.clip.clone();
        let mut window = Value::Null;
        for o in &case.ops {
            let num = |k: &str| o[k].as_f64();
            let preset: MotionPreset = serde_json::from_value(o["preset"].clone()).unwrap();
            let mode: MotionMode = serde_json::from_value(o["mode"].clone()).unwrap();
            match c.apply_motion(preset, mode, num("at"), num("duration"), num("intensity")) {
                Ok((start, end)) => window = json!([start, end]),
                Err(e) => return json!({"err": e.to_string()}),
            }
        }
        json!({"ok": {"window": window, "keyframes": c.keyframes, "channels": c.channels}})
    }

    fn corpus() -> Value {
        let cases: Vec<Value> = cases()
            .iter()
            .map(|c| json!({"name": c.name, "clip": slim(&c.clip), "ops": c.ops, "expect": run(c)}))
            .collect();
        json!({
            "about": "Written by kerf-core (model/motion.rs); replayed by motion-presets.test.ts. Regenerate: KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- motion_corpus",
            "table": table(),
            "cases": cases,
        })
    }

    #[test]
    fn motion_corpus_is_what_this_engine_writes() {
        let fresh = format!("{}\n", serde_json::to_string(&corpus()).expect("serializes"));
        if std::env::var_os("KERF_BLESS_CORPUS").is_some() {
            std::fs::write(CORPUS_PATH, &fresh).expect("writes the corpus");
            return;
        }
        let on_disk = std::fs::read_to_string(CORPUS_PATH)
            .expect("frontend/src/lib/fixtures/motion-presets-corpus.json — generate it with KERF_BLESS_CORPUS=1")
            .replace("\r\n", "\n");
        assert!(
            on_disk == fresh,
            "the motion presets corpus is stale: regenerate with KERF_BLESS_CORPUS=1 cargo test -p kerf-core \
             --no-default-features -- motion_corpus, then run `bun test` — the replay says what motion-presets.ts has to follow"
        );
    }

    #[test]
    fn the_corpus_says_both_what_went_through_and_what_was_refused() {
        let c = corpus();
        let cases = c["cases"].as_array().unwrap();
        let refused = cases.iter().filter(|x| x["expect"].get("err").is_some()).count();
        assert_eq!(refused, 7, "the refusals are listed by hand");
        assert!(cases.len() - refused > 70);
        assert_eq!(c["table"].as_array().unwrap().len(), MotionPreset::ALL.len());
    }
}
