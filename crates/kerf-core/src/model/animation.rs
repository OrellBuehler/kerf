//! Easing and keyframes, and the interpolation of a keyed curve.

use super::*;

/// How a keyframe's value travels to the next one: the shape of the **outgoing** segment.
///
/// Every renderer reads the same curve because the curve *is* a polyline: a non-linear
/// segment is [`EASE_STEPS`] straight pieces through points of the true curve
/// ([`eased_points`]), which is what the still / preview path interpolates
/// ([`Clip::transform_at`]), what the export writes into its per-frame expressions
/// (`keyframe_expr`, which only knows straight lines) and what a GPU pass will sample — so
/// they agree exactly rather than approximately, and slicing an eased segment
/// ([`Timeline::slice`], a trimmed head) is exact too: its pieces become plain keys.
/// `Hold` keeps the value until the next key and jumps there. `Linear` is the default and is
/// omitted from the JSON, so every existing project and graph is byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    #[default]
    Linear,
    Hold,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// A CSS-style cubic bezier from (0, 0) to (1, 1). Both control points are held to the unit
    /// square: no overshoot, so an eased value never leaves the range of its two keys (and the
    /// engine's tiny-scale and opacity guards, which look at the keys, stay true).
    Bezier {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    },
}

/// Straight pieces an eased segment is drawn with (see [`Easing`]).
pub const EASE_STEPS: usize = 12;

impl Easing {
    pub fn is_linear(&self) -> bool {
        matches!(self, Easing::Linear)
    }

    /// The control points of a curved easing (`None` for `Linear` and `Hold`).
    fn bezier(&self) -> Option<(f64, f64, f64, f64)> {
        let unit = |v: f64| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        match *self {
            Easing::Linear | Easing::Hold => None,
            Easing::EaseIn => Some((0.42, 0.0, 1.0, 1.0)),
            Easing::EaseOut => Some((0.0, 0.0, 0.58, 1.0)),
            Easing::EaseInOut => Some((0.42, 0.0, 0.58, 1.0)),
            Easing::Bezier { x1, y1, x2, y2 } => Some((unit(x1), unit(y1), unit(x2), unit(y2))),
        }
    }

    /// The true curve's progress at `u` in `[0, 1]` (`Hold` is 0 until the end).
    pub fn curve(&self, u: f64) -> f64 {
        let u = u.clamp(0.0, 1.0);
        let Some((x1, y1, x2, y2)) = self.bezier() else {
            return if matches!(self, Easing::Hold) && u < 1.0 { 0.0 } else { u };
        };
        bezier_axis(bezier_param(x1, x2, u), y1, y2)
    }

    /// The two easings a segment becomes when a key is put `u` of the way along it
    /// (`0 < u < 1`): the first for the part before the new key, the second for the part
    /// after, each normalized to its own unit square, so the curve is the same curve drawn in
    /// two pieces (de Casteljau at the parameter where the curve is `u` of the way across).
    /// A hold stays a hold on both sides and a linear segment linear; the preset curves come
    /// back as the beziers they are. A half whose value does not change (the new key sits
    /// on a plateau) has no shape and is linear. Exact for every preset and for any bezier
    /// whose control points rise (`x1 <= x2`, `y1 <= y2`); a curve whose half would need a
    /// control point outside the unit square (an S whose `y` turns back) has it clamped there —
    /// the same ends and a smooth curve between them, re-fitted: a few hundredths off.
    pub fn split(&self, u: f64) -> (Easing, Easing) {
        let Some((x1, y1, x2, y2)) = self.bezier() else {
            return (*self, *self);
        };
        let lerp = |a: (f64, f64), b: (f64, f64), s: f64| (a.0 + (b.0 - a.0) * s, a.1 + (b.1 - a.1) * s);
        let (p0, p1, p2, p3) = ((0.0, 0.0), (x1, y1), (x2, y2), (1.0, 1.0));
        let s = bezier_param(x1, x2, u.clamp(0.0, 1.0));
        let (a, b, c) = (lerp(p0, p1, s), lerp(p1, p2, s), lerp(p2, p3, s));
        let (d, e) = (lerp(a, b, s), lerp(b, c, s));
        let at = lerp(d, e, s);
        let unit = |v: f64| v.clamp(0.0, 1.0);
        let half = |from: (f64, f64), h1: (f64, f64), h2: (f64, f64), to: (f64, f64)| {
            let (w, h) = (to.0 - from.0, to.1 - from.1);
            if w < 1e-9 || h < 1e-9 {
                return Easing::Linear;
            }
            let norm = |p: (f64, f64)| (unit((p.0 - from.0) / w), unit((p.1 - from.1) / h));
            let (n1, n2) = (norm(h1), norm(h2));
            Easing::Bezier {
                x1: n1.0,
                y1: n1.1,
                x2: n2.0,
                y2: n2.1,
            }
        };
        (half(p0, a, d, at), half(at, e, c, p3))
    }
}

/// One axis of the cubic bezier from 0 to 1 with inner control values `p1` and `p2`, at `s`.
fn bezier_axis(s: f64, p1: f64, p2: f64) -> f64 {
    let r = 1.0 - s;
    3.0 * r * r * s * p1 + 3.0 * r * s * s * p2 + s * s * s
}

/// The bezier parameter at which its x is `u`: x(s) is monotonic on the unit square, so it is
/// found by bisection (exact enough in 40 halvings).
fn bezier_param(x1: f64, x2: f64, u: f64) -> f64 {
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if bezier_axis(mid, x1, x2) < u {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// The polyline a channel of `(time, value, outgoing easing)` keys is drawn as (sorted by time):
/// a linear segment is its two ends, a `Hold` a step (an equal-time pair at the next key), a
/// curve [`EASE_STEPS`] pieces through the true curve. All-linear keys come back as they went
/// in. [`interpolate`] over the result is the channel's value at any time.
pub fn eased_points(keys: &[(f64, f64, Easing)]) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(keys.len());
    for (i, &(t0, v0, easing)) in keys.iter().enumerate() {
        out.push((t0, v0));
        let Some(&(t1, v1, _)) = keys.get(i + 1) else { break };
        if t1 - t0 < 1e-9 {
            continue;
        }
        match easing {
            Easing::Linear => {}
            Easing::Hold => out.push((t1, v0)),
            curved => {
                for j in 1..EASE_STEPS {
                    let u = j as f64 / EASE_STEPS as f64;
                    out.push((t0 + (t1 - t0) * u, v0 + (v1 - v0) * curved.curve(u)));
                }
            }
        }
    }
    out
}

/// One keyframe of a clip's animated transform: the value of each animatable
/// channel at `time` (seconds from the clip's start). With two or more keyframes
/// the engine interpolates between them — along each key's [`Easing`], linearly unless
/// set — and renders the motion with per-frame ffmpeg expressions; crop and the rest of
/// the static [`Transform`] are unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Keyframe {
    /// Offset from the clip's `timeline_start`, in seconds.
    pub time: f64,
    #[serde(default = "one")]
    pub scale: f64,
    #[serde(default)]
    pub pos_x: f64,
    #[serde(default)]
    pub pos_y: f64,
    #[serde(default)]
    pub rotation: f64,
    #[serde(default = "one")]
    pub opacity: f64,
    /// The shape of the segment from this key to the next.
    #[serde(default, skip_serializing_if = "Easing::is_linear")]
    pub easing: Easing,
}

impl Keyframe {
    /// A keyframe at `time` carrying the values of `transform`'s animatable
    /// channels (the static defaults for a fresh keyframe), linear out.
    pub fn from_transform(time: f64, t: &Transform) -> Self {
        Self {
            time,
            scale: t.scale,
            pos_x: t.pos_x,
            pos_y: t.pos_y,
            rotation: t.rotation,
            opacity: t.opacity,
            easing: Easing::Linear,
        }
    }

    /// Every animatable channel `progress` of the way from `self` to `next`, at `time`.
    pub(super) fn toward(&self, next: &Keyframe, progress: f64, time: f64) -> Keyframe {
        let mix = |a: f64, b: f64| a + (b - a) * progress;
        Keyframe {
            time,
            scale: mix(self.scale, next.scale),
            pos_x: mix(self.pos_x, next.pos_x),
            pos_y: mix(self.pos_y, next.pos_y),
            rotation: mix(self.rotation, next.rotation),
            opacity: mix(self.opacity, next.opacity),
            easing: Easing::Linear,
        }
    }
}

/// Linearly interpolate a channel of `(time, value)` keyframes at `at`, holding
/// the end values flat beyond the first / last keyframe. Empty input → `None`.
pub fn interpolate(points: &[(f64, f64)], at: f64) -> Option<f64> {
    match points {
        [] => None,
        [single] => Some(single.1),
        _ => {
            if at <= points[0].0 {
                return Some(points[0].1);
            }
            for pair in points.windows(2) {
                let (t0, v0) = pair[0];
                let (t1, v1) = pair[1];
                if at < t1 {
                    if t1 <= t0 {
                        return Some(v0);
                    }
                    return Some(v0 + (v1 - v0) * (at - t0) / (t1 - t0));
                }
            }
            Some(points[points.len() - 1].1)
        }
    }
}

/// Wrap an angle in degrees into `[-180, 180)`.
pub fn wrap180(deg: f64) -> f64 {
    if !deg.is_finite() {
        return 0.0;
    }
    let mut d = (deg + 180.0) % 360.0;
    if d < 0.0 {
        d += 360.0;
    }
    d - 180.0
}

/// Linearly interpolate an *angular* channel (degrees) at `at`, taking the
/// shortest arc across the ±180 seam: a 170° → -170° pair travels +20°, not
/// -340°.
///
/// The whole sequence is unwrapped once onto a continuous path before
/// interpolating, rather than resolving each segment on its own. That matters
/// because two different callers walk this data — the still / preview path
/// samples it point by point, while the export emitter marches across it — and
/// per-segment unwrapping would let them disagree about which way the camera
/// turned right at the seam. The result is wrapped back into `[-180, 180)`,
/// since `v360` silently discards a command outside that range.
pub fn interpolate_angle(points: &[(f64, f64)], at: f64) -> Option<f64> {
    let (first, rest) = points.split_first()?;
    let mut unwrapped = Vec::with_capacity(points.len());
    let mut prev = wrap180(first.1);
    unwrapped.push((first.0, prev));
    for &(t, v) in rest {
        prev += wrap180(v - prev);
        unwrapped.push((t, prev));
    }
    interpolate(&unwrapped, at).map(wrap180)
}
