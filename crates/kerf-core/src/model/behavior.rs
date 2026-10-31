//! Live behaviours (B8b): procedural motion layered on a clip's pose.
//!
//! A [`Behavior`] is a closed-form function of clip-local time that is *added to* (or, for the
//! zoom, *multiplied into*) one or more of a clip's transform numbers on top of whatever pose it
//! has — its static transform, its keys, a motion preset. A sway, a breath, a spin, a drift and a
//! shake cost a short expression in the export (a sampled two-minute shake would put thousands of
//! keys in the file and blow the command line, which Windows caps at 32 K characters) and are
//! evaluated by one formula everywhere: [`Clip::property_at`] (hence [`Clip::transform_at`], the
//! scrubbed still, the plan's sampled pose), the export's `rotate` / `perspective` / `overlay`
//! expressions (`engine::cli::behavior` writes the same terms as text) and the playback stream.
//!
//! **Only the four transform numbers move** — scale, position and rotation; never opacity (that is
//! a per-pixel `geq`) and never a colour, a mask or the crop.
//!
//! | kind      | drives       | the term, with `u = local + phase`                                  |
//! | --------- | ------------ | -------------------------------------------------------------------- |
//! | `sway`    | rotation     | `+ amount * sin(2 pi speed u)` degrees, `speed` in Hz                |
//! | `breathe` | scale        | `* (1 + amount * sin(2 pi speed u))`, `amount <= MAX_BREATHE`        |
//! | `spin`    | rotation     | `+ speed * 360 * u` degrees, `speed` in turns a second (signed)      |
//! | `drift`   | pos x, y     | `+ speed * u * (cos angle, sin angle)`, frame widths / heights a second |
//! | `shake`   | pos x, y     | `+ amount * (0.5 sin + 0.3 sin + 0.2 sin)` per axis, three fixed multiples of `speed`, phases from `seed` |
//!
//! `amount` of a shake is the peak, in frame widths (x) and heights (y): the three weights sum to 1,
//! so it never leaves `+- amount`. A breath's `amount` is held under 1 so a scale can never reach
//! (or cross) zero. **`phase` is clip-local seconds** added to the clock, and a head move shifts it
//! by the cut (`rebase_behaviors`), so a split, a trim, a roll or a range export carries on from
//! the pose it left: the motion is a property of the footage, not of the clip's first frame.
//!
//! **One resolver.** `base_at` / `base_transform_at` are the pose *without* behaviours (keys or the
//! static value) — what an edit that writes keys (a motion preset, a new keyframe, a rebase) must
//! sample, or the behaviour would be written into the keys and then added a second time.
//! `property_at` / `transform_at` are the picture. Values are read **held to their ranges**
//! ([`Behavior::sanitized`]; a hand-edited file cannot hand a filter a number it refuses), a
//! behaviour that cannot move anything (amount or speed 0) is not live ([`Behavior::is_neutral`])
//! and changes nothing, flags included, so such a clip renders the graph it always did.

use super::*;
use std::f64::consts::{PI, TAU};

/// The most behaviours one clip carries.
pub const MAX_BEHAVIORS: usize = 8;
/// The fastest an oscillation runs (Hz): past half a frame rate it only aliases.
pub const MAX_BEHAVIOR_HZ: f64 = 30.0;
/// The widest a sway turns the picture each way (degrees).
pub const MAX_SWAY_DEGREES: f64 = 180.0;
/// The deepest a breath goes: the scale stays within `1 +- 0.9` of the pose, so never at or below 0.
pub const MAX_BREATHE: f64 = 0.9;
/// The fastest a spin turns (turns a second, either way).
pub const MAX_SPIN_TURNS: f64 = 10.0;
/// The fastest a drift moves (frame widths / heights a second, either way).
pub const MAX_DRIFT: f64 = 5.0;
/// The widest a shake throws the picture (frame widths / heights, each way).
pub const MAX_SHAKE: f64 = 0.25;
/// The most `phase` is read as (seconds either way).
pub const MAX_BEHAVIOR_PHASE: f64 = 1_000_000.0;
/// The most a drift's `angle` is read as (degrees either way).
const MAX_ANGLE: f64 = 36_000.0;

/// The frequency multiples and weights of a shake's three sines, per axis. Fixed, and no
/// multiple of another, so the motion does not repeat on the order of a clip.
const SHAKE_X: [(f64, f64); 3] = [(1.0, 0.5), (2.3, 0.3), (3.7, 0.2)];
const SHAKE_Y: [(f64, f64); 3] = [(1.3, 0.5), (2.9, 0.3), (4.1, 0.2)];

/// What a behaviour does ([`Behavior::kind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorKind {
    Sway,
    Breathe,
    Spin,
    Drift,
    Shake,
}

impl BehaviorKind {
    pub const ALL: [BehaviorKind; 5] = [
        BehaviorKind::Sway,
        BehaviorKind::Breathe,
        BehaviorKind::Spin,
        BehaviorKind::Drift,
        BehaviorKind::Shake,
    ];

    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            BehaviorKind::Sway => "sway",
            BehaviorKind::Breathe => "breathe",
            BehaviorKind::Spin => "spin",
            BehaviorKind::Drift => "drift",
            BehaviorKind::Shake => "shake",
        }
    }

    /// The kind a wire name spells.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Every wire name, for an "expected one of" message.
    pub fn wire_names() -> Vec<&'static str> {
        Self::ALL.iter().map(|k| k.as_str()).collect()
    }

    /// The transform numbers this kind moves.
    pub fn drives(self) -> &'static [Property] {
        match self {
            BehaviorKind::Sway | BehaviorKind::Spin => &[Property::Rotation],
            BehaviorKind::Breathe => &[Property::Scale],
            BehaviorKind::Drift | BehaviorKind::Shake => &[Property::PosX, Property::PosY],
        }
    }
}

/// One live behaviour of a clip ([`Clip::behaviors`]). JSON is tagged by `kind` (snake_case);
/// `phase` (seconds, default 0), `seed` and `angle` default to 0 when omitted.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Behavior {
    /// Rock the picture: rotation `+= amount * sin(2 pi speed (t + phase))`. `amount` is the peak,
    /// in degrees (`0..=MAX_SWAY_DEGREES`); `speed` the cycles a second (`0..=MAX_BEHAVIOR_HZ`).
    Sway {
        amount: f64,
        speed: f64,
        #[serde(default)]
        phase: f64,
    },
    /// Pulse the zoom: scale `*= 1 + amount * sin(2 pi speed (t + phase))`. `amount` is the depth
    /// as a fraction of the pose's scale (`0..=MAX_BREATHE`); `speed` the breaths a second.
    Breathe {
        amount: f64,
        speed: f64,
        #[serde(default)]
        phase: f64,
    },
    /// Turn steadily: rotation `+= speed * 360 * (t + phase)`. `speed` is turns a second, signed
    /// (`-MAX_SPIN_TURNS..=MAX_SPIN_TURNS`; positive is the direction a positive rotation turns).
    Spin {
        speed: f64,
        #[serde(default)]
        phase: f64,
    },
    /// Slide steadily: position `+= speed * (t + phase) * (cos angle, sin angle)`. `speed` is frame
    /// widths (x) and heights (y) a second, signed (`-MAX_DRIFT..=MAX_DRIFT`); `angle` is degrees
    /// from the right, turning towards the bottom of the frame (90 is straight down).
    Drift {
        speed: f64,
        #[serde(default)]
        angle: f64,
        #[serde(default)]
        phase: f64,
    },
    /// Handheld shake: position `+=` three sines per axis at fixed multiples of `speed` Hz, their
    /// phases drawn from `seed`. `amount` is the peak in frame widths / heights
    /// (`0..=MAX_SHAKE`); the same seed is the same shake on every machine.
    Shake {
        amount: f64,
        speed: f64,
        #[serde(default)]
        seed: u32,
        #[serde(default)]
        phase: f64,
    },
}

/// One sine of a shake: `weight * sin(2 pi freq u + phase)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wave {
    pub freq: f64,
    pub weight: f64,
    pub phase: f64,
}

/// A number held to `lo..=hi`, a non-number read as 0.
fn held(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_finite() {
        v.clamp(lo, hi)
    } else {
        0.0
    }
}

/// An integer hash (two multiply-xorshift rounds, all `u32` wrapping arithmetic, so a mirror in any
/// language has the same bits) of a seed and a wave's index.
fn mix(seed: u32, k: u32) -> u32 {
    let mut x = seed.wrapping_add(k.wrapping_mul(0x9E37_79B9)).wrapping_add(0x7F4A_7C15);
    x ^= x >> 16;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    x = x.wrapping_mul(0xC2B2_AE35);
    x ^ (x >> 16)
}

/// The phase (radians, `0..2 pi`) a seed gives wave `k`: one IEEE division and one multiplication,
/// so the text of an expression is the same on every machine (nothing here is libm).
pub fn shake_phase(seed: u32, k: u32) -> f64 {
    f64::from(mix(seed, k)) / 4_294_967_296.0 * TAU
}

fn check_within(what: &str, v: f64, lo: f64, hi: f64, unit: &str) -> Result<()> {
    if v.is_finite() && v >= lo && v <= hi {
        Ok(())
    } else {
        Err(Error::InvalidArgument(format!(
            "{what} must be a number within {lo}..={hi}{unit} (got {v})"
        )))
    }
}

impl Behavior {
    pub fn kind(&self) -> BehaviorKind {
        match self {
            Behavior::Sway { .. } => BehaviorKind::Sway,
            Behavior::Breathe { .. } => BehaviorKind::Breathe,
            Behavior::Spin { .. } => BehaviorKind::Spin,
            Behavior::Drift { .. } => BehaviorKind::Drift,
            Behavior::Shake { .. } => BehaviorKind::Shake,
        }
    }

    /// The clip-local seconds added to the clock.
    pub fn phase(&self) -> f64 {
        match *self {
            Behavior::Sway { phase, .. }
            | Behavior::Breathe { phase, .. }
            | Behavior::Spin { phase, .. }
            | Behavior::Drift { phase, .. }
            | Behavior::Shake { phase, .. } => phase,
        }
    }

    /// Refuse numbers outside the ranges (or not numbers): what a write checks. A stored behaviour
    /// is read through [`Behavior::sanitized`] instead and never fails.
    pub fn check(&self) -> Result<()> {
        match *self {
            Behavior::Sway { amount, speed, phase } => {
                check_within("sway amount", amount, 0.0, MAX_SWAY_DEGREES, " degrees")?;
                check_within("sway speed", speed, 0.0, MAX_BEHAVIOR_HZ, " Hz")?;
                check_within("sway phase", phase, -MAX_BEHAVIOR_PHASE, MAX_BEHAVIOR_PHASE, " seconds")
            }
            Behavior::Breathe { amount, speed, phase } => {
                check_within("breathe amount", amount, 0.0, MAX_BREATHE, "")?;
                check_within("breathe speed", speed, 0.0, MAX_BEHAVIOR_HZ, " Hz")?;
                check_within("breathe phase", phase, -MAX_BEHAVIOR_PHASE, MAX_BEHAVIOR_PHASE, " seconds")
            }
            Behavior::Spin { speed, phase } => {
                check_within("spin speed", speed, -MAX_SPIN_TURNS, MAX_SPIN_TURNS, " turns a second")?;
                check_within("spin phase", phase, -MAX_BEHAVIOR_PHASE, MAX_BEHAVIOR_PHASE, " seconds")
            }
            Behavior::Drift { speed, angle, phase } => {
                check_within("drift speed", speed, -MAX_DRIFT, MAX_DRIFT, " frames a second")?;
                check_within("drift angle", angle, -MAX_ANGLE, MAX_ANGLE, " degrees")?;
                check_within("drift phase", phase, -MAX_BEHAVIOR_PHASE, MAX_BEHAVIOR_PHASE, " seconds")
            }
            Behavior::Shake {
                amount, speed, phase, ..
            } => {
                check_within("shake amount", amount, 0.0, MAX_SHAKE, " of the frame")?;
                check_within("shake speed", speed, 0.0, MAX_BEHAVIOR_HZ, " Hz")?;
                check_within("shake phase", phase, -MAX_BEHAVIOR_PHASE, MAX_BEHAVIOR_PHASE, " seconds")
            }
        }
    }

    /// The behaviour held to its ranges (a number that is not one reads as 0): what every reader
    /// evaluates, so a hand-edited file cannot hand a filter a value it refuses.
    pub fn sanitized(&self) -> Self {
        let phase = |p: f64| held(p, -MAX_BEHAVIOR_PHASE, MAX_BEHAVIOR_PHASE);
        match *self {
            Behavior::Sway { amount, speed, phase: p } => Behavior::Sway {
                amount: held(amount, 0.0, MAX_SWAY_DEGREES),
                speed: held(speed, 0.0, MAX_BEHAVIOR_HZ),
                phase: phase(p),
            },
            Behavior::Breathe { amount, speed, phase: p } => Behavior::Breathe {
                amount: held(amount, 0.0, MAX_BREATHE),
                speed: held(speed, 0.0, MAX_BEHAVIOR_HZ),
                phase: phase(p),
            },
            Behavior::Spin { speed, phase: p } => Behavior::Spin {
                speed: held(speed, -MAX_SPIN_TURNS, MAX_SPIN_TURNS),
                phase: phase(p),
            },
            Behavior::Drift { speed, angle, phase: p } => Behavior::Drift {
                speed: held(speed, -MAX_DRIFT, MAX_DRIFT),
                angle: held(angle, -MAX_ANGLE, MAX_ANGLE),
                phase: phase(p),
            },
            Behavior::Shake {
                amount,
                speed,
                seed,
                phase: p,
            } => Behavior::Shake {
                amount: held(amount, 0.0, MAX_SHAKE),
                speed: held(speed, 0.0, MAX_BEHAVIOR_HZ),
                seed,
                phase: phase(p),
            },
        }
    }

    /// Whether the (sanitized) behaviour can move nothing: no amount, or no speed. Such a behaviour
    /// is kept in the list but is not live — no flag, no expression, no refusal.
    pub fn is_neutral(&self) -> bool {
        match *self {
            Behavior::Sway { amount, speed, .. }
            | Behavior::Breathe { amount, speed, .. }
            | Behavior::Shake { amount, speed, .. } => amount == 0.0 || speed == 0.0,
            Behavior::Spin { speed, .. } | Behavior::Drift { speed, .. } => speed == 0.0,
        }
    }

    /// Whether this (sanitized, live) behaviour contributes to `prop`.
    pub fn drives(&self, prop: Property) -> bool {
        self.kind().drives().contains(&prop)
    }

    /// The behaviour continuing after the clip's start moved `by` seconds later (negative: earlier):
    /// the clock a cut piece reads is the old one, so the motion carries on from the pose it was in.
    pub fn shifted(&self, by: f64) -> Self {
        if !by.is_finite() {
            return *self;
        }
        let shift = |p: f64| (p + by).clamp(-MAX_BEHAVIOR_PHASE, MAX_BEHAVIOR_PHASE);
        let mut out = *self;
        match &mut out {
            Behavior::Sway { phase, .. }
            | Behavior::Breathe { phase, .. }
            | Behavior::Spin { phase, .. }
            | Behavior::Drift { phase, .. }
            | Behavior::Shake { phase, .. } => *phase = shift(*phase),
        }
        out
    }

    /// The three sines a shake throws `prop` (position x or y) by; `None` for any other pair. The
    /// export writes these same numbers into its expression.
    pub fn waves(&self, prop: Property) -> Option<[Wave; 3]> {
        let Behavior::Shake { speed, seed, .. } = *self else {
            return None;
        };
        let (table, base) = match prop {
            Property::PosX => (SHAKE_X, 0),
            Property::PosY => (SHAKE_Y, 3),
            _ => return None,
        };
        Some(std::array::from_fn(|k| Wave {
            freq: speed * table[k].0,
            weight: table[k].1,
            phase: shake_phase(seed, base + k as u32),
        }))
    }

    /// The term this (sanitized) behaviour adds to `prop` at `local` seconds from the clip's start:
    /// an offset (degrees, frame fractions), or for `breathe` the amount of the factor `1 + term`.
    /// 0 for a number it does not drive. **The export writes this exact arithmetic as text**
    /// (`engine::cli::behavior`), operation for operation.
    pub fn term(&self, prop: Property, local: f64) -> f64 {
        let u = local + self.phase();
        match *self {
            Behavior::Sway { amount, speed, .. } | Behavior::Breathe { amount, speed, .. } if self.drives(prop) => {
                amount * (TAU * speed * u).sin()
            }
            Behavior::Spin { speed, .. } if self.drives(prop) => speed * 360.0 * u,
            Behavior::Drift { speed, angle, .. } => match prop {
                Property::PosX => speed * (angle * PI / 180.0).cos() * u,
                Property::PosY => speed * (angle * PI / 180.0).sin() * u,
                _ => 0.0,
            },
            Behavior::Shake { amount, .. } => match self.waves(prop) {
                Some([a, b, c]) => {
                    let sine = |w: Wave| w.weight * (TAU * w.freq * u + w.phase).sin();
                    amount * (sine(a) + sine(b) + sine(c))
                }
                None => 0.0,
            },
            _ => 0.0,
        }
    }

    /// The farthest this (sanitized, live) behaviour moves `prop` from the pose over a clip of
    /// `horizon` seconds: an upper bound (a position, in frame fractions), for the cover crop that
    /// keeps what a frame can show.
    pub fn reach(&self, prop: Property, horizon: f64) -> f64 {
        match *self {
            Behavior::Drift { speed, angle, phase } => {
                let along = match prop {
                    Property::PosX => (angle * PI / 180.0).cos().abs(),
                    Property::PosY => (angle * PI / 180.0).sin().abs(),
                    _ => return 0.0,
                };
                speed.abs() * along * phase.abs().max((phase + horizon.max(0.0)).abs())
            }
            Behavior::Shake { amount, .. } if matches!(prop, Property::PosX | Property::PosY) => amount,
            _ => 0.0,
        }
    }

    /// A phrase for the Inspector and the diff: "sway 5° at 0.5 Hz".
    pub fn label(&self) -> String {
        match *self {
            Behavior::Sway { amount, speed, .. } => format!("sway {amount}° at {speed} Hz"),
            Behavior::Breathe { amount, speed, .. } => format!("breathe {:.0}% at {speed} Hz", amount * 100.0),
            Behavior::Spin { speed, .. } => format!("spin {speed} turns/s"),
            Behavior::Drift { speed, angle, .. } => format!("drift {speed} frames/s at {angle}°"),
            Behavior::Shake { amount, speed, seed, .. } => {
                format!("shake {:.1}% at {speed} Hz (seed {seed})", amount * 100.0)
            }
        }
    }
}

/// Refuse a list of behaviours a clip cannot carry: more than [`MAX_BEHAVIORS`], or one with a
/// number out of range.
pub fn check_behaviors(behaviors: &[Behavior]) -> Result<()> {
    if behaviors.len() > MAX_BEHAVIORS {
        return Err(Error::InvalidArgument(format!(
            "at most {MAX_BEHAVIORS} behaviours per clip (got {})",
            behaviors.len()
        )));
    }
    behaviors.iter().try_for_each(Behavior::check)
}

/// What [`Clip::bake_behaviors`] wrote.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Baked {
    /// The numbers that now carry keys of their own.
    pub properties: Vec<Property>,
    /// Keys written to each.
    pub keys: usize,
    /// The frame rate the keys were sampled at.
    pub fps: f64,
}

impl Clip {
    /// The behaviours that move something, as they are read (held to their ranges), in order.
    pub fn live_behaviors(&self) -> Vec<Behavior> {
        self.behaviors
            .iter()
            .map(Behavior::sanitized)
            .filter(|b| !b.is_neutral())
            .collect()
    }

    /// Whether any behaviour is live.
    pub fn has_behaviors(&self) -> bool {
        self.behaviors.iter().any(|b| !b.sanitized().is_neutral())
    }

    /// Whether a live behaviour moves `prop` (only a transform number can be).
    pub fn behaves(&self, prop: Property) -> bool {
        prop.is_transform()
            && self
                .behaviors
                .iter()
                .map(Behavior::sanitized)
                .any(|b| !b.is_neutral() && b.drives(prop))
    }

    /// Whether `prop` is an expression of time in the graph: it has keys, or a behaviour moves it.
    /// What decides how a clip's chain is built (a static number is a constant).
    pub fn is_driven(&self, prop: Property) -> bool {
        self.is_keyed(prop) || self.behaves(prop)
    }

    /// Whether the picture can be turned: a rotation key off zero, or a behaviour that rotates.
    pub fn rotation_turns(&self) -> bool {
        self.property_keys(Property::Rotation).iter().any(|k| k.value != 0.0) || self.behaves(Property::Rotation)
    }

    /// `prop` at `local` seconds **without** behaviours: its keys' curve, else its static value.
    /// What an edit that writes keys must sample.
    pub fn base_at(&self, prop: Property, local: f64) -> f64 {
        interpolate(&self.property_curve(prop), local).unwrap_or_else(|| self.static_value(prop))
    }

    /// `base` with every live behaviour that drives `prop` applied at `local` (the zoom
    /// multiplies, the rest add), in list order — the order the export's expression has.
    pub fn behaved(&self, prop: Property, base: f64, local: f64) -> f64 {
        let mut v = base;
        for b in self.behaviors.iter().map(Behavior::sanitized) {
            if b.is_neutral() || !b.drives(prop) {
                continue;
            }
            v = if prop == Property::Scale {
                v * (1.0 + b.term(prop, local))
            } else {
                v + b.term(prop, local)
            };
        }
        v
    }

    /// [`Clip::transform_at`] without behaviours: the pose the keys (or the static transform) give.
    pub fn base_transform_at(&self, local: f64) -> Transform {
        let mut t = self.transform;
        if !Property::TRANSFORM.iter().any(|p| self.is_keyed(*p)) {
            return t;
        }
        t.scale = self.base_at(Property::Scale, local);
        t.pos_x = self.base_at(Property::PosX, local);
        t.pos_y = self.base_at(Property::PosY, local);
        t.rotation = self.base_at(Property::Rotation, local);
        t.opacity = self.base_at(Property::Opacity, local);
        t
    }

    /// The furthest behaviours move the picture from the pose over `horizon` seconds of the clip, as
    /// `(x, y)` frame fractions (an upper bound).
    pub fn behavior_reach(&self, horizon: f64) -> (f64, f64) {
        let sum = |prop: Property| {
            self.behaviors
                .iter()
                .map(Behavior::sanitized)
                .filter(|b| !b.is_neutral())
                .map(|b| b.reach(prop, horizon))
                .sum::<f64>()
        };
        (sum(Property::PosX), sum(Property::PosY))
    }

    /// The behaviours' share of [`Clip::rebase_animation`]: the start moved `by` seconds later
    /// (the head cut) or earlier, so each behaviour's clock moves with the footage.
    pub(super) fn rebase_behaviors(&mut self, by: f64) {
        for b in &mut self.behaviors {
            *b = b.shifted(by);
        }
    }

    /// Replace the clip's behaviours (checked as [`check_behaviors`]).
    pub fn set_behaviors(&mut self, behaviors: Vec<Behavior>) -> Result<()> {
        check_behaviors(&behaviors)?;
        self.behaviors = behaviors;
        Ok(())
    }

    /// Sample the behaviours' combined result into keys of the numbers they move, at the output
    /// frames of a `fps` grid, and clear them. **Exact at those frames**: the keys sit on the
    /// grid's frame times (clip-local `k / fps - timeline_start`, plus the clip's first and last
    /// moment), linear between, so a frame the export draws at one of them reads the value the live
    /// behaviours gave — and the base pose (its keys, eased or held) is folded in the same way, so
    /// the numbers become plain keys. Anything between two frames is a straight line instead of the
    /// curve; an export at another rate reads values between keys.
    ///
    /// Refuses a clip without behaviours, an `fps` outside 1..=1000, and one that would need more
    /// than `max_keys` keys for a number (split the clip, or live with the behaviour).
    pub fn bake_behaviors(&mut self, fps: f64, max_keys: usize) -> Result<Baked> {
        if self.behaviors.is_empty() {
            return Err(Error::InvalidArgument("the clip has no behaviours to bake".to_string()));
        }
        if !fps.is_finite() || !(1.0..=1000.0).contains(&fps) {
            return Err(Error::InvalidArgument(format!(
                "bake at a frame rate within 1..=1000 (got {fps})"
            )));
        }
        let driven: Vec<Property> = Property::TRANSFORM.into_iter().filter(|p| self.behaves(*p)).collect();
        if driven.is_empty() {
            self.behaviors.clear();
            return Ok(Baked {
                properties: driven,
                keys: 0,
                fps,
            });
        }
        let duration = self.duration();
        let start = self.timeline_start;
        if !duration.is_finite() || !start.is_finite() || duration <= 0.0 {
            return Err(Error::InvalidArgument(
                "the clip has no length to bake behaviours over".to_string(),
            ));
        }
        let frames = (duration * fps).ceil();
        if frames + 2.0 > max_keys as f64 {
            return Err(Error::InvalidArgument(format!(
                "baking {duration:.1} s at {fps} fps needs about {} keys a number and a clip holds at most {max_keys}: \
                 split the clip into pieces under {:.1} s, or keep the behaviour live",
                frames + 2.0,
                (max_keys.saturating_sub(2)) as f64 / fps
            )));
        }
        let mut times = vec![0.0];
        let first = (start * fps - 1e-9).ceil() as i64;
        for k in first..=first.saturating_add(frames as i64 + 1) {
            let local = k as f64 / fps - start;
            if local >= duration - 1e-9 {
                break;
            }
            if local > 1e-9 {
                times.push(local);
            }
        }
        times.push(duration.max(1e-9));
        let tracks: Vec<(Property, Vec<PropertyKey>)> = driven
            .iter()
            .map(|p| {
                (
                    *p,
                    times.iter().map(|t| PropertyKey::new(*t, self.property_at(*p, *t))).collect(),
                )
            })
            .collect();
        let keys = times.len();
        for (prop, track) in tracks {
            self.set_property_keys(prop, track);
        }
        self.behaviors.clear();
        Ok(Baked {
            properties: driven,
            keys,
            fps,
        })
    }
}

/// What a change of behaviours reads as in the review diff: "behaviours: sway added, shake
/// removed". A change of nothing but `phase` (a head move shifts it) is "retimed".
pub(super) fn behavior_changes(before: &Clip, after: &Clip) -> Option<String> {
    if before.behaviors == after.behaviors {
        return None;
    }
    let mut parts = Vec::new();
    for kind in BehaviorKind::ALL {
        let of = |c: &Clip| -> Vec<Behavior> { c.behaviors.iter().copied().filter(|b| b.kind() == kind).collect() };
        let (was, now) = (of(before), of(after));
        for _ in was.len()..now.len() {
            parts.push(format!("{} added", kind.as_str()));
        }
        for _ in now.len()..was.len() {
            parts.push(format!("{} removed", kind.as_str()));
        }
        let (mut changed, mut retimed) = (false, false);
        for (a, b) in was.iter().zip(&now) {
            if a != b {
                if a.shifted(-a.phase()) == b.shifted(-b.phase()) {
                    retimed = true;
                } else {
                    changed = true;
                }
            }
        }
        if changed {
            parts.push(format!("{} changed", kind.as_str()));
        }
        if retimed {
            parts.push(format!("{} retimed", kind.as_str()));
        }
    }
    if parts.is_empty() {
        parts.push("reordered".to_string());
    }
    Some(format!("behaviours: {}", parts.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(seconds: f64) -> Clip {
        Clip::new(Uuid::new_v4(), 0.0, seconds, 0.0)
    }

    fn sway() -> Behavior {
        Behavior::Sway {
            amount: 5.0,
            speed: 0.5,
            phase: 0.0,
        }
    }

    #[test]
    fn the_json_is_tagged_by_kind_and_defaults_what_it_can() {
        let json = serde_json::to_string(&sway()).unwrap();
        assert_eq!(json, r#"{"kind":"sway","amount":5.0,"speed":0.5,"phase":0.0}"#);
        let read: Behavior = serde_json::from_str(r#"{"kind":"shake","amount":0.01,"speed":3}"#).unwrap();
        assert_eq!(
            read,
            Behavior::Shake {
                amount: 0.01,
                speed: 3.0,
                seed: 0,
                phase: 0.0
            }
        );
        let drift: Behavior = serde_json::from_str(r#"{"kind":"drift","speed":0.1}"#).unwrap();
        assert_eq!(
            drift,
            Behavior::Drift {
                speed: 0.1,
                angle: 0.0,
                phase: 0.0
            }
        );
        assert!(serde_json::from_str::<Behavior>(r#"{"kind":"wobble","amount":1}"#).is_err());
        for kind in BehaviorKind::ALL {
            assert_eq!(BehaviorKind::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn a_clip_without_behaviours_serializes_exactly_as_before() {
        let c = clip(4.0);
        assert!(!serde_json::to_string(&c).unwrap().contains("behaviors"));
        let mut b = clip(4.0);
        b.behaviors.push(sway());
        assert!(serde_json::to_string(&b).unwrap().contains(r#""behaviors":[{"kind":"sway""#));
        let old: Clip = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert!(old.behaviors.is_empty());
    }

    #[test]
    fn each_kind_adds_its_closed_form_to_the_pose() {
        let mut c = clip(10.0);
        c.transform.rotation = 10.0;
        c.transform.scale = 2.0;
        c.transform.pos_x = 0.1;
        c.behaviors = vec![
            sway(),
            Behavior::Breathe {
                amount: 0.2,
                speed: 1.0,
                phase: 0.0,
            },
            Behavior::Drift {
                speed: 0.1,
                angle: 90.0,
                phase: 0.0,
            },
        ];
        let at = |t: f64| c.transform_at(t);
        let t = 0.3_f64;
        assert!((at(t).rotation - (10.0 + 5.0 * (TAU * 0.5 * t).sin())).abs() < 1e-12);
        assert!((at(t).scale - 2.0 * (1.0 + 0.2 * (TAU * t).sin())).abs() < 1e-12);
        assert!((at(t).pos_y - 0.1 * t).abs() < 1e-12, "down is positive y");
        assert!((at(t).pos_x - 0.1).abs() < 1e-12, "a straight-down drift leaves x alone");
        assert_eq!(
            c.base_transform_at(t),
            c.transform,
            "without behaviours it is the static pose"
        );
        c.behaviors = vec![Behavior::Spin {
            speed: -0.25,
            phase: 0.0,
        }];
        assert!((c.transform_at(2.0).rotation - (10.0 - 0.25 * 360.0 * 2.0)).abs() < 1e-12);
    }

    #[test]
    fn behaviours_ride_on_top_of_keys() {
        let mut c = clip(10.0);
        c.set_property_keys(
            Property::Rotation,
            vec![PropertyKey::new(0.0, 0.0), PropertyKey::new(4.0, 40.0)],
        );
        c.behaviors = vec![sway()];
        let t = 1.3_f64;
        assert!((c.base_at(Property::Rotation, t) - 13.0).abs() < 1e-12);
        assert!((c.property_at(Property::Rotation, t) - (13.0 + 5.0 * (TAU * 0.5 * t).sin())).abs() < 1e-12);
        assert!((c.transform_at(t).rotation - c.property_at(Property::Rotation, t)).abs() < 1e-15);
    }

    #[test]
    fn a_shake_stays_within_its_amount_and_the_seed_decides_it() {
        let shake = |seed| Behavior::Shake {
            amount: 0.02,
            speed: 4.0,
            seed,
            phase: 0.0,
        };
        let mut a = clip(30.0);
        a.behaviors = vec![shake(7)];
        let mut b = a.clone();
        b.behaviors = vec![shake(8)];
        let mut moved = 0.0_f64;
        let mut differ = 0.0_f64;
        for i in 0..3000 {
            let t = f64::from(i) / 100.0;
            let (p, q) = (a.transform_at(t), b.transform_at(t));
            assert!(p.pos_x.abs() <= 0.02 + 1e-12 && p.pos_y.abs() <= 0.02 + 1e-12, "{p:?}");
            moved = moved.max(p.pos_x.abs()).max(p.pos_y.abs());
            differ = differ.max((p.pos_x - q.pos_x).abs());
            assert_eq!(p, a.transform_at(t), "deterministic");
        }
        assert!(moved > 0.012, "it actually shakes: {moved}");
        assert!(differ > 0.005, "another seed is another shake: {differ}");
        assert_eq!(a.transform_at(1.0).scale, 1.0);
        assert_eq!(a.transform_at(1.0).rotation, 0.0);
        // The axes do not move together.
        let p = a.transform_at(0.37);
        assert!((p.pos_x - p.pos_y).abs() > 1e-6);
    }

    #[test]
    fn the_seed_phases_are_pinned() {
        // The text of an expression carries these, so they are the same on every machine (and in the TS mirror).
        let want = [
            (0, 0, 1.859279059967247_f64),
            (1, 0, 6.193488414738267),
            (7, 3, 1.4183163432086827),
            (0xDEAD_BEEF, 5, 2.0658319936003027),
            (4_000_000_001, 2, 0.8788333674401654),
        ];
        for (seed, k, phase) in want {
            let got = shake_phase(seed, k);
            assert!((got - phase).abs() < 1e-12, "seed {seed} wave {k}: {got}");
            assert!((0.0..TAU).contains(&got));
        }
    }

    #[test]
    fn a_behaviour_that_moves_nothing_is_not_live() {
        let mut c = clip(4.0);
        c.behaviors = vec![
            Behavior::Sway {
                amount: 0.0,
                speed: 1.0,
                phase: 0.0,
            },
            Behavior::Shake {
                amount: 0.1,
                speed: 0.0,
                seed: 1,
                phase: 0.0,
            },
            Behavior::Spin { speed: 0.0, phase: 3.0 },
            Behavior::Drift {
                speed: 0.0,
                angle: 30.0,
                phase: 1.0,
            },
            Behavior::Breathe {
                amount: f64::NAN,
                speed: 1.0,
                phase: 0.0,
            },
        ];
        assert!(!c.has_behaviors() && c.live_behaviors().is_empty());
        assert!(!c.is_animated() && !c.zoom_animated() && !c.placement_animated() && !c.picture_moves());
        assert_eq!(c.transform_at(1.0), c.transform);
        for p in Property::TRANSFORM {
            assert!(!c.behaves(p) && !c.is_driven(p));
        }
    }

    #[test]
    fn they_are_read_held_to_their_ranges_and_refused_when_written_out_of_them() {
        let wild = Behavior::Breathe {
            amount: 40.0,
            speed: f64::INFINITY,
            phase: 1e300,
        };
        let s = wild.sanitized();
        assert_eq!(
            s,
            Behavior::Breathe {
                amount: MAX_BREATHE,
                speed: 0.0,
                phase: MAX_BEHAVIOR_PHASE
            }
        );
        let mut c = clip(4.0);
        c.behaviors = vec![Behavior::Breathe {
            amount: 40.0,
            speed: 2.0,
            phase: f64::NAN,
        }];
        for i in 0..100 {
            let scale = c.transform_at(f64::from(i) / 10.0).scale;
            assert!(scale.is_finite() && scale >= 1.0 - MAX_BREATHE - 1e-12, "{scale}");
        }
        assert!(check_behaviors(&[wild]).is_err());
        let bad = [
            Behavior::Sway {
                amount: -1.0,
                speed: 1.0,
                phase: 0.0,
            },
            Behavior::Sway {
                amount: 1.0,
                speed: -1.0,
                phase: 0.0,
            },
            Behavior::Spin {
                speed: MAX_SPIN_TURNS + 1.0,
                phase: 0.0,
            },
            Behavior::Drift {
                speed: 0.1,
                angle: f64::NAN,
                phase: 0.0,
            },
            Behavior::Shake {
                amount: MAX_SHAKE + 0.01,
                speed: 1.0,
                seed: 0,
                phase: 0.0,
            },
            Behavior::Breathe {
                amount: 0.95,
                speed: 1.0,
                phase: 0.0,
            },
        ];
        for b in bad {
            assert!(b.check().is_err(), "{b:?}");
            assert!(b.sanitized().check().is_ok(), "{b:?} -> {:?}", b.sanitized());
        }
        assert!(check_behaviors(&vec![sway(); MAX_BEHAVIORS]).is_ok());
        assert!(check_behaviors(&vec![sway(); MAX_BEHAVIORS + 1]).is_err());
    }

    #[test]
    fn the_flags_follow_what_a_behaviour_moves() {
        let flags = |b: Behavior| {
            let mut c = clip(8.0);
            c.behaviors = vec![b];
            (
                c.is_animated(),
                c.zoom_animated(),
                c.placement_animated(),
                c.picture_moves(),
                c.rotation_turns(),
            )
        };
        assert_eq!(flags(sway()), (true, false, false, false, true));
        assert_eq!(
            flags(Behavior::Spin { speed: 0.2, phase: 0.0 }),
            (true, false, false, false, true)
        );
        assert_eq!(
            flags(Behavior::Breathe {
                amount: 0.1,
                speed: 1.0,
                phase: 0.0
            }),
            (true, true, true, true, false)
        );
        assert_eq!(
            flags(Behavior::Drift {
                speed: 0.1,
                angle: 0.0,
                phase: 0.0
            }),
            (true, false, true, true, false)
        );
        assert_eq!(
            flags(Behavior::Shake {
                amount: 0.01,
                speed: 2.0,
                seed: 3,
                phase: 0.0
            }),
            (true, false, true, true, false)
        );
        let mut c = clip(8.0);
        c.behaviors = vec![sway()];
        assert!(c.is_driven(Property::Rotation) && !c.is_driven(Property::Scale) && !c.is_driven(Property::Opacity));
        assert!(!c.is_keyed(Property::Rotation), "a behaviour is not a key");
    }

    #[test]
    fn a_head_cut_keeps_the_motion_where_it_was() {
        let mut original = clip(10.0);
        original.timeline_start = 2.0;
        original.set_property_keys(Property::PosX, vec![PropertyKey::new(0.0, -0.2), PropertyKey::new(10.0, 0.2)]);
        original.behaviors = vec![
            sway(),
            Behavior::Breathe {
                amount: 0.1,
                speed: 0.7,
                phase: 0.25,
            },
            Behavior::Spin { speed: 0.05, phase: 0.0 },
            Behavior::Drift {
                speed: -0.02,
                angle: 33.0,
                phase: -0.5,
            },
            Behavior::Shake {
                amount: 0.01,
                speed: 3.0,
                seed: 11,
                phase: 0.0,
            },
        ];
        for by in [0.4, 1.0, 3.3, 7.0, -1.5, -0.2] {
            let mut cut = original.clone();
            cut.rebase_animation(by);
            for i in 0..=40 {
                let local = f64::from(i) * 0.1;
                let (a, b) = (cut.transform_at(local), original.transform_at(local + by));
                for (x, y, what) in [
                    (a.scale, b.scale, "scale"),
                    (a.pos_x, b.pos_x, "x"),
                    (a.pos_y, b.pos_y, "y"),
                    (a.rotation, b.rotation, "rotation"),
                ] {
                    assert!((x - y).abs() < 1e-9, "by {by}, local {local}: {what} {x} vs {y}");
                }
            }
        }
    }

    #[test]
    fn baking_samples_the_frames_and_clears_the_behaviours() {
        let mut live = clip(3.0);
        live.timeline_start = 0.1;
        live.set_property_keys(
            Property::Scale,
            vec![
                PropertyKey::new(0.0, 1.0),
                PropertyKey {
                    time: 1.0,
                    value: 1.4,
                    easing: Easing::EaseInOut,
                },
                PropertyKey::new(3.0, 1.0),
            ],
        );
        live.behaviors = vec![
            Behavior::Breathe {
                amount: 0.1,
                speed: 1.3,
                phase: 0.2,
            },
            Behavior::Shake {
                amount: 0.02,
                speed: 5.0,
                seed: 4,
                phase: 0.0,
            },
            sway(),
        ];
        let mut baked = live.clone();
        let report = baked.bake_behaviors(30.0, 1000).unwrap();
        assert!(baked.behaviors.is_empty());
        assert_eq!(
            report.properties,
            vec![Property::Scale, Property::PosX, Property::PosY, Property::Rotation]
        );
        assert_eq!(report.keys, baked.property_keys(Property::PosX).len());
        assert!(report.keys > 85 && report.keys < 100, "{}", report.keys);
        let mut frames = 0;
        for k in 3..=93 {
            let local = f64::from(k) / 30.0 - live.timeline_start;
            if local < 0.0 || local > live.duration() {
                continue;
            }
            frames += 1;
            let (a, b) = (live.transform_at(local), baked.transform_at(local));
            for (x, y) in [
                (a.scale, b.scale),
                (a.pos_x, b.pos_x),
                (a.pos_y, b.pos_y),
                (a.rotation, b.rotation),
            ] {
                assert!((x - y).abs() < 1e-9, "frame {k}: {x} vs {y}");
            }
        }
        assert!(frames > 85);
        // Opacity and the colour are not touched.
        assert!(!baked.is_keyed(Property::Opacity));
        // Between two frames it is a line, not the curve.
        let mid = 0.5 / 30.0 + 1.0 / 30.0 - live.timeline_start + 1.0 / 30.0;
        assert!((live.transform_at(mid).rotation - baked.transform_at(mid).rotation).abs() > 1e-5);
        // Nothing to bake, a silly rate, or too many keys are refused.
        assert!(clip(3.0).bake_behaviors(30.0, 1000).is_err());
        assert!(live.clone().bake_behaviors(0.0, 1000).is_err());
        assert!(live.clone().bake_behaviors(f64::NAN, 1000).is_err());
        let err = live.clone().bake_behaviors(30.0, 50).unwrap_err().to_string();
        assert!(err.contains("split the clip"), "{err}");
        let mut idle = clip(3.0);
        idle.behaviors = vec![Behavior::Spin { speed: 0.0, phase: 0.0 }];
        assert_eq!(idle.bake_behaviors(30.0, 1000).unwrap().keys, 0);
        assert!(idle.behaviors.is_empty() && idle.channels.is_empty());
    }

    #[test]
    fn the_reach_bounds_where_a_drift_and_a_shake_can_put_the_picture() {
        let mut c = clip(10.0);
        c.behaviors = vec![
            Behavior::Drift {
                speed: -0.05,
                angle: 0.0,
                phase: 2.0,
            },
            Behavior::Shake {
                amount: 0.02,
                speed: 3.0,
                seed: 1,
                phase: 0.0,
            },
        ];
        let (rx, ry) = c.behavior_reach(10.0);
        assert!((rx - (0.05 * 12.0 + 0.02)).abs() < 1e-12, "{rx}");
        assert!((ry - 0.02).abs() < 1e-9, "{ry}");
        for i in 0..=100 {
            let t = f64::from(i) / 10.0;
            let p = c.transform_at(t);
            assert!(p.pos_x.abs() <= rx + 1e-12 && p.pos_y.abs() <= ry + 1e-9);
        }
        let (px, py) = c.pan_extent();
        assert!((px - rx).abs() < 1e-12 && (py - ry).abs() < 1e-12, "the pan includes it");
        // A clip kept on past its end under a transition is drawn longer, and a drift keeps going.
        assert!(c.pan_extent_over(20.0).0 > px + 0.4, "{:?}", c.pan_extent_over(20.0));
    }

    #[test]
    fn the_diff_names_what_changed() {
        let a = clip(4.0);
        let mut b = a.clone();
        assert_eq!(behavior_changes(&a, &b), None);
        b.behaviors = vec![sway()];
        assert_eq!(behavior_changes(&a, &b).as_deref(), Some("behaviours: sway added"));
        assert_eq!(behavior_changes(&b, &a).as_deref(), Some("behaviours: sway removed"));
        let mut c = b.clone();
        c.behaviors = vec![
            Behavior::Sway {
                amount: 9.0,
                speed: 0.5,
                phase: 0.0,
            },
            Behavior::Spin { speed: 0.1, phase: 0.0 },
        ];
        assert_eq!(
            behavior_changes(&b, &c).as_deref(),
            Some("behaviours: sway changed, spin added")
        );
        let mut d = b.clone();
        d.rebase_animation(0.5);
        assert_eq!(behavior_changes(&b, &d).as_deref(), Some("behaviours: sway retimed"));
    }

    // ---- the corpus the TS mirror replays ---------------------------------------------------

    const CORPUS_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../frontend/src/lib/fixtures/behaviors-corpus.json"
    );

    fn pose(t: &Transform) -> serde_json::Value {
        serde_json::json!({"scale": t.scale, "pos_x": t.pos_x, "pos_y": t.pos_y, "rotation": t.rotation})
    }

    /// The part of a clip the resolver reads.
    fn slim(c: &Clip) -> serde_json::Value {
        serde_json::json!({
            "duration": c.duration(),
            "timeline_start": c.timeline_start,
            "transform": {
                "scale": c.transform.scale, "pos_x": c.transform.pos_x, "pos_y": c.transform.pos_y,
                "rotation": c.transform.rotation, "opacity": c.transform.opacity,
            },
            "keyframes": c.keyframes,
            "channels": c.channels,
            "behaviors": c.behaviors,
        })
    }

    fn lively(seconds: f64, behaviors: Vec<Behavior>) -> Clip {
        let mut c = clip(seconds);
        c.behaviors = behaviors;
        c
    }

    fn keyed(mut c: Clip) -> Clip {
        c.transform.scale = 0.9;
        c.transform.pos_x = 0.05;
        c.set_property_keys(
            Property::Scale,
            vec![
                PropertyKey::new(0.0, 0.7),
                PropertyKey {
                    time: 2.0,
                    value: 1.3,
                    easing: Easing::EaseInOut,
                },
                PropertyKey::new(5.0, 0.8),
            ],
        );
        c.set_property_keys(
            Property::Rotation,
            vec![PropertyKey::new(0.0, -10.0), PropertyKey::new(4.0, 25.0)],
        );
        c.set_property_keys(
            Property::PosY,
            vec![
                PropertyKey {
                    time: 1.0,
                    value: -0.1,
                    easing: Easing::Hold,
                },
                PropertyKey::new(3.0, 0.15),
            ],
        );
        c
    }

    fn all_kinds() -> Vec<Behavior> {
        vec![
            sway(),
            Behavior::Breathe {
                amount: 0.15,
                speed: 0.7,
                phase: 0.25,
            },
            Behavior::Spin {
                speed: -0.05,
                phase: 0.0,
            },
            Behavior::Drift {
                speed: 0.02,
                angle: 135.0,
                phase: -0.5,
            },
            Behavior::Shake {
                amount: 0.012,
                speed: 2.5,
                seed: 4_000_000_001,
                phase: 0.125,
            },
        ]
    }

    fn corpus() -> serde_json::Value {
        use serde_json::json;
        let times = [0.0, 0.1, 0.37, 0.5, 1.0, 1.9, 2.0, 2.75, 3.3, 4.0, 4.6, 5.0, 6.25, 9.99];
        // Resolver vectors: the pose without behaviours, and the picture's, at times.
        let sample_cases: Vec<(&str, Clip)> = vec![
            ("sway on a static transform", lively(10.0, vec![sway()])),
            (
                "breathe on a static transform",
                lively(
                    10.0,
                    vec![Behavior::Breathe {
                        amount: 0.4,
                        speed: 1.1,
                        phase: 0.0,
                    }],
                ),
            ),
            ("spin", lively(10.0, vec![Behavior::Spin { speed: 0.2, phase: 1.5 }])),
            (
                "drift to the right",
                lively(
                    10.0,
                    vec![Behavior::Drift {
                        speed: 0.03,
                        angle: 0.0,
                        phase: 0.0,
                    }],
                ),
            ),
            (
                "drift up and left",
                lively(
                    10.0,
                    vec![Behavior::Drift {
                        speed: 0.03,
                        angle: -135.0,
                        phase: 0.0,
                    }],
                ),
            ),
            (
                "shake, seed 0",
                lively(
                    10.0,
                    vec![Behavior::Shake {
                        amount: 0.02,
                        speed: 3.0,
                        seed: 0,
                        phase: 0.0,
                    }],
                ),
            ),
            (
                "shake, seed 4000000001, phase",
                lively(
                    10.0,
                    vec![Behavior::Shake {
                        amount: 0.02,
                        speed: 3.0,
                        seed: 4_000_000_001,
                        phase: 0.7,
                    }],
                ),
            ),
            ("every kind", lively(10.0, all_kinds())),
            ("every kind on keys", keyed(lively(10.0, all_kinds()))),
            (
                "two sways and a breath on keys",
                keyed(lively(
                    10.0,
                    vec![
                        sway(),
                        sway(),
                        Behavior::Breathe {
                            amount: 0.3,
                            speed: 0.4,
                            phase: 0.0,
                        },
                    ],
                )),
            ),
            (
                "a behaviour that moves nothing",
                keyed(lively(
                    10.0,
                    vec![
                        Behavior::Sway {
                            amount: 0.0,
                            speed: 1.0,
                            phase: 0.0,
                        },
                        Behavior::Drift {
                            speed: 0.0,
                            angle: 20.0,
                            phase: 3.0,
                        },
                    ],
                )),
            ),
            (
                "numbers out of range are held to it",
                lively(
                    10.0,
                    vec![
                        Behavior::Sway {
                            amount: 900.0,
                            speed: 100.0,
                            phase: 0.0,
                        },
                        Behavior::Breathe {
                            amount: 5.0,
                            speed: 1.0,
                            phase: 0.0,
                        },
                        Behavior::Spin {
                            speed: -50.0,
                            phase: 1e9,
                        },
                        Behavior::Drift {
                            speed: 50.0,
                            angle: 1e9,
                            phase: -1e9,
                        },
                        Behavior::Shake {
                            amount: 3.0,
                            speed: -1.0,
                            seed: 1,
                            phase: 0.0,
                        },
                    ],
                ),
            ),
        ];
        let samples: Vec<serde_json::Value> = sample_cases
            .iter()
            .map(|(name, c)| {
                json!({
                    "name": name,
                    "clip": slim(c),
                    "times": times,
                    "base": times.iter().map(|t| pose(&c.base_transform_at(*t))).collect::<Vec<_>>(),
                    "picture": times.iter().map(|t| pose(&c.transform_at(*t))).collect::<Vec<_>>(),
                    "flags": {
                        "is_animated": c.is_animated(),
                        "zoom_animated": c.zoom_animated(),
                        "placement_animated": c.placement_animated(),
                        "picture_moves": c.picture_moves(),
                        "rotation_turns": c.rotation_turns(),
                        "has_behaviors": c.has_behaviors(),
                        "live": c.live_behaviors(),
                    },
                })
            })
            .collect();
        // The seed phases and the per-axis waves.
        let seeds = [0u32, 1, 7, 255, 65_536, 0xDEAD_BEEF, 4_000_000_001, u32::MAX];
        let phases: Vec<serde_json::Value> = seeds
            .iter()
            .map(|seed| json!({"seed": seed, "phases": (0..6).map(|k| shake_phase(*seed, k)).collect::<Vec<_>>()}))
            .collect();
        // Rebase vectors: a head cut or pull, and the picture before and after at the same moments.
        let rebased: Vec<serde_json::Value> = [
            ("a head cut inside the keys", keyed(lively(10.0, all_kinds())), 2.5),
            ("a head cut on a key", keyed(lively(10.0, all_kinds())), 2.0),
            ("a cut past the last key", keyed(lively(10.0, all_kinds())), 6.0),
            ("a head pulled earlier", keyed(lively(10.0, all_kinds())), -1.5),
            ("a small cut", lively(10.0, all_kinds()), 0.04),
            ("no cut", lively(10.0, all_kinds()), 0.0),
        ]
        .iter()
        .map(|(name, c, by)| {
            let mut cut = c.clone();
            cut.rebase_animation(*by);
            let local = [0.0, 0.3, 1.1, 2.4, 3.9];
            json!({
                "name": name,
                "clip": slim(c),
                "by": by,
                "behaviors": cut.behaviors,
                "keyframes": cut.keyframes,
                "channels": cut.channels,
                "times": local,
                "picture": local.iter().map(|t| pose(&cut.transform_at(*t))).collect::<Vec<_>>(),
            })
        })
        .collect();
        // Bake vectors.
        let bakes: Vec<serde_json::Value> = [
            ("a swaying clip at 24 fps", {
                let mut c = lively(1.0, vec![sway()]);
                c.timeline_start = 0.0;
                c
            }, 24.0, 1000),
            ("a clip that starts between frames", {
                let mut c = lively(0.5, vec![Behavior::Shake { amount: 0.02, speed: 4.0, seed: 3, phase: 0.1 }]);
                c.timeline_start = 0.123;
                c
            }, 30.0, 1000),
            ("keys folded in", {
                let mut c = keyed(lively(2.0, vec![Behavior::Breathe { amount: 0.2, speed: 1.0, phase: 0.0 }]));
                c.source_out = 2.0;
                c
            }, 25.0, 1000),
            ("nothing to bake", clip(2.0), 25.0, 1000),
            ("too many keys", lively(2.0, vec![sway()]), 25.0, 20),
            ("a rate out of range", lively(2.0, vec![sway()]), 0.5, 1000),
        ]
        .iter()
        .map(|(name, c, fps, max)| {
            let mut baked = c.clone();
            let expect = match baked.bake_behaviors(*fps, *max) {
                Ok(b) => json!({"ok": {"properties": b.properties, "keys": b.keys, "channels": baked.channels, "behaviors": baked.behaviors}}),
                Err(e) => json!({"err": e.to_string()}),
            };
            json!({"name": name, "clip": slim(c), "fps": fps, "max_keys": max, "expect": expect})
        })
        .collect();
        // What is held and what is refused.
        let wild = [
            sway(),
            Behavior::Sway {
                amount: 181.0,
                speed: 1.0,
                phase: 0.0,
            },
            Behavior::Sway {
                amount: 1.0,
                speed: -1.0,
                phase: 0.0,
            },
            Behavior::Breathe {
                amount: 0.9,
                speed: 30.0,
                phase: 0.0,
            },
            Behavior::Breathe {
                amount: 0.91,
                speed: 1.0,
                phase: 0.0,
            },
            Behavior::Spin {
                speed: -10.0,
                phase: 0.0,
            },
            Behavior::Spin { speed: 10.5, phase: 0.0 },
            Behavior::Drift {
                speed: 5.0,
                angle: 36_000.0,
                phase: 0.0,
            },
            Behavior::Drift {
                speed: 5.0,
                angle: 36_001.0,
                phase: 0.0,
            },
            Behavior::Shake {
                amount: 0.25,
                speed: 30.0,
                seed: 9,
                phase: 1_000_000.0,
            },
            Behavior::Shake {
                amount: 0.26,
                speed: 30.0,
                seed: 9,
                phase: 0.0,
            },
            Behavior::Shake {
                amount: 0.1,
                speed: 3.0,
                seed: 9,
                phase: 1_000_001.0,
            },
        ];
        let held: Vec<serde_json::Value> = wild
            .iter()
            .map(|b| json!({"behavior": b, "ok": b.check().is_ok(), "sanitized": b.sanitized(), "neutral": b.sanitized().is_neutral()}))
            .collect();
        json!({
            "about": "Written by kerf-core (model/behavior.rs); replayed by the TS mirror. Regenerate: KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- behaviors_corpus",
            "limits": {
                "max_behaviors": MAX_BEHAVIORS, "max_hz": MAX_BEHAVIOR_HZ, "max_sway_degrees": MAX_SWAY_DEGREES,
                "max_breathe": MAX_BREATHE, "max_spin_turns": MAX_SPIN_TURNS, "max_drift": MAX_DRIFT,
                "max_shake": MAX_SHAKE, "max_phase": MAX_BEHAVIOR_PHASE,
            },
            "shake_waves": {"x": SHAKE_X, "y": SHAKE_Y},
            "seed_phases": phases,
            "samples": samples,
            "held": held,
            "rebase": rebased,
            "bake": bakes,
        })
    }

    #[test]
    fn behaviors_corpus_is_what_this_engine_writes() {
        let fresh = format!("{}\n", serde_json::to_string(&corpus()).expect("serializes"));
        if std::env::var_os("KERF_BLESS_CORPUS").is_some() {
            std::fs::write(CORPUS_PATH, &fresh).expect("writes the corpus");
            return;
        }
        let on_disk = std::fs::read_to_string(CORPUS_PATH)
            .expect("frontend/src/lib/fixtures/behaviors-corpus.json — generate it with KERF_BLESS_CORPUS=1")
            .replace("\r\n", "\n");
        assert!(
            on_disk == fresh,
            "the behaviours corpus is stale: regenerate with KERF_BLESS_CORPUS=1 cargo test -p kerf-core \
             --no-default-features -- behaviors_corpus, then run `bun test` — the replay says what the mirror has to follow"
        );
    }

    #[test]
    fn the_corpus_says_what_went_through_and_what_was_refused() {
        let c = corpus();
        let bakes = c["bake"].as_array().unwrap();
        assert_eq!(bakes.iter().filter(|b| b["expect"].get("err").is_some()).count(), 3);
        let held = c["held"].as_array().unwrap();
        assert_eq!(held.iter().filter(|h| h["ok"] == false).count(), 7);
        assert!(c["samples"].as_array().unwrap().len() >= 12);
        assert_eq!(c["seed_phases"].as_array().unwrap().len(), 8);
    }
}
