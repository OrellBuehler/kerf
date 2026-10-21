//! The path a text animator's property takes between the text as drawn and its value at an amount of 1: a
//! curve for each property (`Curves`, on a [`TextAnimation`](super::TextAnimation)).
//!
//! A curve is written for a unit **settling into its place**, as every preset does (the amount falls from 1,
//! the property at its value, to 0, the text as drawn). Its `progress(a)` says how far along that path the
//! property is when it is `a` of the way (the amount `1 - a`): 0 at the start, 1 at rest, and free to go
//! beyond either in between. A value past 1 is an **overshoot**: the property goes through its resting value
//! and comes back. A unit leaving its place (the amount rising) walks the same path backwards, and a curve
//! has nothing to shape where the amount jumps (a `square` window): the jump stays one.
//!
//! * **Speed** is After Effects' temporal ease, normalised to a segment of length 1: `out_speed` is how fast
//!   the property leaves its start and `in_speed` how fast it reaches rest, in multiples of the average speed
//!   (1 is the straight line, 0 stands still, more than 1 outruns it, below 0 backs the wrong way), and each
//!   speed holds for an `influence` (the share of the segment its handle reaches). Both handles make a cubic
//!   Bezier from `(0, 0)` to `(1, 1)`: `(out_influence, out_speed * out_influence)` and
//!   `(1 - in_influence, 1 - in_speed * in_influence)`. An influence stays in `0.01..=1`, so time only moves
//!   forward along it, and a speed that is large enough carries the curve past 1 or below 0.
//! * **Spring** is a damped oscillator that settles: `bounce` is how much it keeps (`1 - bounce` is the damping
//!   ratio). It is stretched so that it has all but stopped (a thousandth of its swing) when the property
//!   arrives, and the little that is left is spread along the way, so it ends exactly at rest.
//!
//! Everything here is plain `f64` arithmetic and `exp` / `sin` / `cos`, so a curve is the same on every
//! machine to the rounding the text animation's output keeps.

use serde::{Deserialize, Serialize};

use super::invalid;
use crate::error::Result;

/// The fastest a speed handle may be, in multiples of the average speed (either way).
pub const MAX_SPEED: f64 = 5.0;
/// The shortest an influence may be, and the longest.
pub const MIN_INFLUENCE: f64 = 0.01;
pub const MAX_INFLUENCE: f64 = 1.0;
/// How springy a spring may be: 0.05 settles almost without overshoot, 0.8 swings five times.
pub const MIN_BOUNCE: f64 = 0.05;
pub const MAX_BOUNCE: f64 = 0.8;
/// What is left of a spring's swing when it arrives.
const SPRING_LEFT: f64 = 1e-3;

/// A property's path to its resting value. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Curve {
    /// An After Effects speed / influence ease.
    Speed {
        /// How fast it leaves its start, in multiples of the average speed (-5 to 5).
        out_speed: f64,
        /// How much of the way that speed holds for (0.01 to 1).
        out_influence: f64,
        /// How fast it reaches rest, in multiples of the average speed (-5 to 5).
        in_speed: f64,
        /// How much of the way that speed holds for (0.01 to 1).
        in_influence: f64,
    },
    /// A damped spring that settles on rest.
    Spring {
        /// 0.05 (barely overshoots) to 0.8 (swings five times before it settles).
        bounce: f64,
    },
}

fn within(what: &str, name: &str, v: f64, lo: f64, hi: f64) -> Result<()> {
    if v.is_finite() && (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(invalid(format!("{what}: {name} is between {lo} and {hi}")))
    }
}

impl Curve {
    /// Every number finite and within its bounds. `what` names the property in the refusal.
    pub fn check(&self, what: &str) -> Result<()> {
        match *self {
            Curve::Speed {
                out_speed,
                out_influence,
                in_speed,
                in_influence,
            } => {
                within(what, "out speed", out_speed, -MAX_SPEED, MAX_SPEED)?;
                within(what, "out influence", out_influence, MIN_INFLUENCE, MAX_INFLUENCE)?;
                within(what, "in speed", in_speed, -MAX_SPEED, MAX_SPEED)?;
                within(what, "in influence", in_influence, MIN_INFLUENCE, MAX_INFLUENCE)
            }
            Curve::Spring { bounce } => within(what, "bounce", bounce, MIN_BOUNCE, MAX_BOUNCE),
        }
    }

    /// How far along its path a property is when it is `a` of the way there (0 to 1): 0 at the start and 1 at rest,
    /// possibly beyond either in between. Always finite.
    pub fn progress(&self, a: f64) -> f64 {
        let a = if a.is_nan() { 0.0 } else { a.clamp(0.0, 1.0) };
        if a == 0.0 || a == 1.0 {
            return a;
        }
        let p = match *self {
            Curve::Speed {
                out_speed,
                out_influence,
                in_speed,
                in_influence,
            } => bezier(
                out_influence,
                out_speed * out_influence,
                1.0 - in_influence,
                1.0 - in_speed * in_influence,
                a,
            ),
            Curve::Spring { bounce } => spring(bounce, a),
        };
        if p.is_finite() {
            p
        } else {
            a
        }
    }

    /// The factor a property's change is scaled by for a unit at `amount` (-1 to 1; 1 is the property at its value and
    /// 0 is the text as drawn): the amount itself for a straight line. A negative amount is the positive one mirrored.
    pub fn settle(curve: Option<&Curve>, amount: f64) -> f64 {
        let Some(curve) = curve else { return amount };
        let magnitude = amount.abs().min(1.0);
        if magnitude == 0.0 {
            return 0.0;
        }
        let factor = 1.0 - curve.progress(1.0 - magnitude);
        if amount < 0.0 {
            -factor
        } else {
            factor
        }
    }
}

/// `(a, b, c)` of `a t³ + b t² + c t` for a Bezier from 0 to 1 with inner control values `p1` and `p2`.
fn polynomial(p1: f64, p2: f64) -> (f64, f64, f64) {
    let c = 3.0 * p1;
    let b = 3.0 * (p2 - p1) - c;
    (1.0 - c - b, b, c)
}

fn at(p: (f64, f64, f64), t: f64) -> f64 {
    ((p.0 * t + p.1) * t + p.2) * t
}

fn slope(p: (f64, f64, f64), t: f64) -> f64 {
    (3.0 * p.0 * t + 2.0 * p.1) * t + p.2
}

/// The y of the cubic Bezier through `(0, 0)`, `(x1, y1)`, `(x2, y2)`, `(1, 1)` where its x is `x` (strictly inside
/// 0..1; `x1` and `x2` in 0..1, so x only ever grows): Newton's method from the straight line, bisection when it does
/// not close in.
fn bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    let (px, py) = (polynomial(x1, x2), polynomial(y1, y2));
    let mut t = x;
    for _ in 0..8 {
        let err = at(px, t) - x;
        if err.abs() < 1e-10 {
            return at(py, t);
        }
        let d = slope(px, t);
        if d.abs() < 1e-6 {
            break;
        }
        t = (t - err / d).clamp(0.0, 1.0);
    }
    if (at(px, t) - x).abs() >= 1e-10 {
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..60 {
            let mid = 0.5 * (lo + hi);
            if at(px, mid) < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        t = 0.5 * (lo + hi);
    }
    at(py, t)
}

/// The step response of a damped spring of ratio `1 - bounce` that is down to [`SPRING_LEFT`] of its swing at `a = 1`,
/// plus the little that is left, spread along the way.
fn spring(bounce: f64, a: f64) -> f64 {
    let zeta = 1.0 - bounce;
    let sigma = (1.0 / SPRING_LEFT).ln();
    let wd = sigma * (1.0 - zeta * zeta).sqrt() / zeta;
    let step = |t: f64| 1.0 - (-sigma * t).exp() * ((wd * t).cos() + sigma / wd * (wd * t).sin());
    step(a) + a * (1.0 - step(1.0))
}

/// A curve for each property of a text animation; a property without one travels in a straight line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Curves {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<Curve>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<Curve>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<Curve>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Curve>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<Curve>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blur: Option<Curve>,
}

impl Curves {
    /// No property has a curve (the animation as it was before curves).
    pub fn is_empty(&self) -> bool {
        *self == Curves::default()
    }

    /// Each property's curve, in the order the compiler samples them: x, y, rotation, scale, opacity, blur.
    pub(crate) fn by_channel(&self) -> [Option<Curve>; 6] {
        [self.x, self.y, self.rotation, self.scale, self.opacity, self.blur]
    }

    pub(crate) fn check(&self) -> Result<()> {
        for (name, curve) in [
            ("x", self.x),
            ("y", self.y),
            ("scale", self.scale),
            ("rotation", self.rotation),
            ("opacity", self.opacity),
            ("blur", self.blur),
        ] {
            if let Some(c) = curve {
                c.check(&format!("a text animation's {name} curve"))?;
            }
        }
        Ok(())
    }
}

/// A named curve a user can start from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CurvePreset {
    pub id: String,
    pub name: String,
    pub description: String,
    /// `None` for the straight line.
    pub curve: Option<Curve>,
}

fn preset(id: &str, name: &str, description: &str, curve: Option<Curve>) -> CurvePreset {
    CurvePreset {
        id: id.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        curve,
    }
}

/// A third of the way: the influence of After Effects' Easy Ease.
const THIRD: f64 = 1.0 / 3.0;

/// The curves a picker lists, in order. Choosing one copies it onto the animation's moving properties
/// (`TextAnimation::with_motion_curve`); nothing refers back to it.
pub fn curve_presets() -> Vec<CurvePreset> {
    vec![
        preset("linear", "Linear", "Every property moves at one pace.", None),
        preset(
            "ease_in",
            "Ease in",
            "Starts slowly and arrives at full speed.",
            Some(Curve::Speed {
                out_speed: 0.0,
                out_influence: THIRD,
                in_speed: 1.0,
                in_influence: THIRD,
            }),
        ),
        preset(
            "ease_out",
            "Ease out",
            "Starts at full speed and slows into place.",
            Some(Curve::Speed {
                out_speed: 1.0,
                out_influence: THIRD,
                in_speed: 0.0,
                in_influence: THIRD,
            }),
        ),
        preset(
            "ease_in_out",
            "Ease in and out",
            "Slow to start and slow to arrive (Easy Ease).",
            Some(Curve::Speed {
                out_speed: 0.0,
                out_influence: THIRD,
                in_speed: 0.0,
                in_influence: THIRD,
            }),
        ),
        preset(
            "overshoot",
            "Overshoot",
            "Goes a little past its place, then comes back.",
            Some(Curve::Speed {
                out_speed: 4.0,
                out_influence: 0.4,
                in_speed: 0.0,
                in_influence: 0.4,
            }),
        ),
        preset(
            "spring",
            "Spring",
            "Swings past its place and back, settling like a spring.",
            Some(Curve::Spring { bounce: 0.5 }),
        ),
    ]
}

/// The curve preset with this id.
pub fn curve_preset(id: &str) -> Option<CurvePreset> {
    curve_presets().into_iter().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speed(os: f64, oi: f64, is: f64, ii: f64) -> Curve {
        Curve::Speed {
            out_speed: os,
            out_influence: oi,
            in_speed: is,
            in_influence: ii,
        }
    }

    fn named(id: &str) -> Curve {
        curve_preset(id).unwrap().curve.unwrap()
    }

    #[test]
    fn every_curve_starts_at_the_start_and_ends_at_rest() {
        for p in curve_presets().iter().filter_map(|p| p.curve) {
            assert_eq!((p.progress(0.0), p.progress(1.0)), (0.0, 1.0), "{p:?}");
            assert_eq!((p.progress(-3.0), p.progress(3.0)), (0.0, 1.0), "{p:?}: held to the path");
            assert_eq!(p.progress(f64::NAN), 0.0);
            let mut last = None;
            for i in 0..=1000 {
                let v = p.progress(f64::from(i) / 1000.0);
                assert!(v.is_finite() && (-0.5..=2.0).contains(&v), "{p:?} at {i}: {v}");
                last = Some(v);
            }
            assert_eq!(last, Some(1.0));
        }
    }

    #[test]
    fn the_straight_line_and_the_ease_presets_are_what_they_say() {
        // A speed of 1 over a third each way is the diagonal.
        let line = speed(1.0, THIRD, 1.0, THIRD);
        for i in 0..=20 {
            let a = f64::from(i) / 20.0;
            assert!((line.progress(a) - a).abs() < 1e-9, "{a}");
        }
        let (ein, eout, both) = (named("ease_in"), named("ease_out"), named("ease_in_out"));
        assert!(ein.progress(0.25) < 0.25 && ein.progress(0.5) < 0.5 && ein.progress(0.75) < 0.75);
        assert!(eout.progress(0.25) > 0.25 && eout.progress(0.5) > 0.5 && eout.progress(0.75) > 0.75);
        assert!(both.progress(0.25) < 0.25 && both.progress(0.75) > 0.75);
        assert!((both.progress(0.5) - 0.5).abs() < 1e-9, "symmetric");
        // None of the three leaves the path.
        for c in [ein, eout, both] {
            for i in 0..=200 {
                let v = c.progress(f64::from(i) / 200.0);
                assert!((0.0..=1.0).contains(&v), "{c:?}: {v}");
            }
        }
    }

    #[test]
    fn a_fast_start_and_a_backwards_arrival_overshoot() {
        // The overshoot preset is past 1 for a stretch, by about a tenth, and ends at 1.
        let c = named("overshoot");
        let peak = (1..1000).map(|i| c.progress(f64::from(i) / 1000.0)).fold(0.0f64, f64::max);
        assert!((1.08..1.15).contains(&peak), "{peak}");
        // A negative speed arriving: the path goes back out before it comes in (past 1 at the end).
        let late = speed(1.0, 0.3, -2.0, 0.5);
        let top = (1..1000).map(|i| late.progress(f64::from(i) / 1000.0)).fold(0.0f64, f64::max);
        assert!(top > 1.2, "{top}");
        // A negative out-speed dips below the start first.
        let wind_up = speed(-3.0, 0.3, 1.0, 0.3);
        let low = (1..1000)
            .map(|i| wind_up.progress(f64::from(i) / 1000.0))
            .fold(0.0f64, f64::min);
        assert!(low < -0.1, "{low}");
    }

    #[test]
    fn the_bezier_solver_agrees_with_a_slow_bisection_for_awkward_handles() {
        // x nearly flat at an end (handles at the extremes), y well outside 0..1.
        for (x1, x2) in [
            (0.01, 0.99),
            (1.0, 0.0),
            (0.01, 0.0),
            (1.0, 0.99),
            (0.5, 0.5),
            (0.2, 0.9),
            (0.9, 0.1),
        ] {
            for (y1, y2) in [(1.8, -0.6), (-3.0, 4.0), (0.0, 1.0), (5.0, -4.0)] {
                let (px, py) = (polynomial(x1, x2), polynomial(y1, y2));
                for i in 1..200 {
                    let x = f64::from(i) / 200.0;
                    let (mut lo, mut hi) = (0.0, 1.0);
                    for _ in 0..200 {
                        let mid = 0.5 * (lo + hi);
                        if at(px, mid) < x {
                            lo = mid;
                        } else {
                            hi = mid;
                        }
                    }
                    let want = at(py, 0.5 * (lo + hi));
                    let got = bezier(x1, y1, x2, y2, x);
                    assert!((got - want).abs() < 1e-4, "x {x1} {x2} y {y1} {y2} at {x}: {got} vs {want}");
                }
            }
        }
    }

    #[test]
    fn a_spring_overshoots_as_much_as_its_damping_says_and_settles() {
        for bounce in [0.1, 0.3, 0.5, 0.7, 0.8] {
            let c = Curve::Spring { bounce };
            let zeta = 1.0 - bounce;
            let first_peak = 1.0 + (-std::f64::consts::PI * zeta / (1.0 - zeta * zeta).sqrt()).exp();
            let values: Vec<f64> = (0..=4000).map(|i| c.progress(f64::from(i) / 4000.0)).collect();
            let peak = values.iter().copied().fold(0.0f64, f64::max);
            assert!((peak - first_peak).abs() < 0.004, "bounce {bounce}: {peak} vs {first_peak}");
            assert!(values.iter().all(|v| (0.0..2.0).contains(v)), "bounce {bounce}");
            // It has all but stopped at the end: the last tenth is within a few thousandths of 1.
            assert!(values[3600..].iter().all(|v| (v - 1.0).abs() < 0.01), "bounce {bounce}");
            assert_eq!(values[4000], 1.0);
        }
        // More bounce, more swings.
        let crossings = |bounce: f64| {
            let c = Curve::Spring { bounce };
            let mut n = 0;
            let mut above = false;
            for i in 1..4000 {
                let up = c.progress(f64::from(i) / 4000.0) > 1.0;
                if up != above {
                    n += 1;
                    above = up;
                }
            }
            n
        };
        assert!(
            crossings(0.3) < crossings(0.5) && crossings(0.5) < crossings(0.8),
            "{} {} {}",
            crossings(0.3),
            crossings(0.5),
            crossings(0.8)
        );
    }

    #[test]
    fn settling_is_the_amount_for_a_straight_line_and_odd_for_the_rest() {
        for m in [-1.0, -0.4, 0.0, 0.3, 1.0] {
            assert_eq!(Curve::settle(None, m), m);
        }
        let c = named("spring");
        for m in [0.1, 0.35, 0.8, 1.0] {
            let up = Curve::settle(Some(&c), m);
            assert_eq!(Curve::settle(Some(&c), -m), -up, "{m}");
            assert!(up.is_finite());
        }
        assert_eq!((Curve::settle(Some(&c), 0.0), Curve::settle(Some(&c), 1.0)), (0.0, 1.0));
        assert_eq!(Curve::settle(Some(&c), 7.0), 1.0, "an amount is held to 1");
        // On the way the spring has gone past rest: a negative factor.
        let near = (1..100)
            .map(|i| Curve::settle(Some(&c), f64::from(i) / 100.0))
            .fold(f64::INFINITY, f64::min);
        assert!(near < -0.1, "{near}");
    }

    #[test]
    fn numbers_out_of_bounds_are_refused_by_name() {
        let bad = |c: Curve| c.check("the curve").unwrap_err().to_string();
        assert!(bad(speed(6.0, 0.5, 0.0, 0.5)).contains("out speed"));
        assert!(bad(speed(0.0, 0.0, 0.0, 0.5)).contains("out influence"));
        assert!(bad(speed(0.0, 0.5, -5.5, 0.5)).contains("in speed"));
        assert!(bad(speed(0.0, 0.5, 0.0, 1.5)).contains("in influence"));
        assert!(bad(speed(f64::NAN, 0.5, 0.0, 0.5)).contains("out speed"));
        assert!(bad(Curve::Spring { bounce: 0.0 }).contains("bounce"));
        assert!(bad(Curve::Spring { bounce: 0.9 }).contains("bounce"));
        assert!(bad(Curve::Spring { bounce: f64::INFINITY }).contains("bounce"));
        for p in curve_presets().iter().filter_map(|p| p.curve) {
            p.check("preset").unwrap();
        }
        // The extremes are fine and stay finite.
        for c in [
            speed(5.0, 1.0, -5.0, 1.0),
            speed(-5.0, 0.01, 5.0, 0.01),
            speed(5.0, 0.01, 5.0, 1.0),
            Curve::Spring { bounce: MIN_BOUNCE },
            Curve::Spring { bounce: MAX_BOUNCE },
        ] {
            c.check("extreme").unwrap();
            for i in 0..=100 {
                assert!(c.progress(f64::from(i) / 100.0).is_finite(), "{c:?}");
            }
        }
    }

    #[test]
    fn curves_read_strictly_and_a_straight_animation_writes_none() {
        let read = |s: &str| serde_json::from_str::<Curve>(s);
        assert_eq!(
            read(r#"{"kind":"spring","bounce":0.5}"#).unwrap(),
            Curve::Spring { bounce: 0.5 }
        );
        assert!(read(r#"{"kind":"spring","bounce":0.5,"extra":1}"#).is_err());
        assert!(read(r#"{"kind":"spring"}"#).is_err());
        assert!(read(r#"{"kind":"bounce","bounce":0.5}"#).is_err());
        assert!(read(r#"{"bounce":0.5}"#).is_err());
        assert!(read(r#"{"kind":"speed","out_speed":0,"out_influence":0.3,"in_speed":0,"in_influence":0.3,"x":1}"#).is_err());
        assert!(read(r#"{"kind":"speed","out_speed":0,"out_influence":0.3,"in_speed":0,"in_influence":0.3}"#).is_ok());
        assert_eq!(serde_json::to_string(&Curves::default()).unwrap(), "{}");
        let one = Curves {
            scale: Some(Curve::Spring { bounce: 0.5 }),
            ..Curves::default()
        };
        assert_eq!(
            serde_json::to_string(&one).unwrap(),
            r#"{"scale":{"kind":"spring","bounce":0.5}}"#
        );
        assert!(serde_json::from_str::<Curves>(r#"{"z":null}"#).is_err());
        assert_eq!(serde_json::from_str::<Curves>(r#"{"x":null}"#).unwrap(), Curves::default());
    }

    #[test]
    fn the_presets_are_unique_valid_and_start_with_the_straight_line() {
        let presets = curve_presets();
        let mut ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids[0], "linear");
        assert_eq!(presets[0].curve, None);
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), presets.len());
        for p in &presets {
            if let Some(c) = p.curve {
                c.check(&p.id).unwrap();
                assert_eq!(serde_json::from_str::<Curve>(&serde_json::to_string(&c).unwrap()).unwrap(), c);
            }
        }
    }
}
