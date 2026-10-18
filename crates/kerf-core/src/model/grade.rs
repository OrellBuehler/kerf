//! A clip's grade beyond the `eq` numbers of [`Color`]: tone curves, colour wheels and a 3D LUT.
//!
//! Each part is optional and `None` at neutral, so a clip without one renders the graph it always
//! did. Order in the chain (the export's and the still's): `eq` (the [`Color`] numbers), then the
//! **LUT** (a look, applied to the corrected picture), then the **curves**, then the **wheels** —
//! Lumetri's order (basic, creative, curves, wheels). Nothing here is keyable.
//!
//! **Curves** are a master curve and one per channel, each a list of points in `0..=1`, through
//! which a monotone cubic (Fritsch–Carlson: no overshoot, so a curve between two points never
//! leaves their range) is drawn; outside its first and last point a curve holds their values. A
//! channel's curve runs first, the master on its result (`master(red(x))`), as FFmpeg's `curves`
//! and Photoshop do. [`CurveSegment`] is the cubic as the export writes it, so the model and the
//! graph evaluate the same polynomial.
//!
//! **Wheels** are FFmpeg's `colorbalance` (shadows / midtones / highlights, weighted by each pixel's
//! lightness): a wheel's puck `(x, y)` in the unit disc picks a hue (red at 0°, green at 120°, blue
//! at 240°, counter-clockwise) and a strength, and its `lightness` adds to all three channels of
//! its range. [`Wheel::rgb`] is the filter's three numbers for that range, exactly.

use super::*;

/// The most points one curve may have.
pub const MAX_CURVE_POINTS: usize = 16;

/// A point of a tone curve: input to output, both in `0..=1`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CurvePoint {
    pub x: f64,
    pub y: f64,
}

/// Tone curves. An empty list is the identity for that curve.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Curves {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub master: Vec<CurvePoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub red: Vec<CurvePoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub green: Vec<CurvePoint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blue: Vec<CurvePoint>,
}

/// One cubic piece of a curve, over `[x0, x0 + 1 / k)`: `y = a + u * (b + u * (c + u * d))` with
/// `u = (x - x0) * k`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurveSegment {
    pub x0: f64,
    pub k: f64,
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
}

impl CurveSegment {
    pub fn eval(&self, x: f64) -> f64 {
        let u = (x - self.x0) * self.k;
        self.a + u * (self.b + u * (self.c + u * self.d))
    }
}

/// A curve ready to evaluate: its points (sorted, clamped, distinct in `x`) and the cubic of each
/// span between two of them.
#[derive(Debug, Clone, PartialEq)]
pub struct CurveShape {
    pub points: Vec<CurvePoint>,
    pub segments: Vec<CurveSegment>,
}

fn finite01(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// A curve's points as the model keeps them: clamped to `0..=1`, sorted by `x`, one per `x` (the
/// first of equal ones), at most [`MAX_CURVE_POINTS`], and none at all when they draw the identity
/// (fewer than two points, or every point on `y = x` from 0 to 1).
pub fn normalize_curve(points: &[CurvePoint]) -> Vec<CurvePoint> {
    let mut pts: Vec<CurvePoint> = points
        .iter()
        .map(|p| CurvePoint {
            x: finite01(p.x),
            y: finite01(p.y),
        })
        .collect();
    pts.sort_by(|a, b| a.x.total_cmp(&b.x));
    pts.dedup_by(|b, a| (b.x - a.x).abs() < 1e-6);
    pts.truncate(MAX_CURVE_POINTS);
    let identity =
        pts.len() < 2 || (pts[0].x == 0.0 && pts[pts.len() - 1].x == 1.0 && pts.iter().all(|p| (p.y - p.x).abs() < 1e-9));
    if identity {
        Vec::new()
    } else {
        pts
    }
}

impl CurveShape {
    /// The monotone cubic through normalized points (Fritsch–Carlson), or `None` for an identity
    /// curve.
    pub fn new(points: &[CurvePoint]) -> Option<Self> {
        let pts = normalize_curve(points);
        let m = pts.len();
        if m < 2 {
            return None;
        }
        let h: Vec<f64> = (0..m - 1).map(|i| pts[i + 1].x - pts[i].x).collect();
        let d: Vec<f64> = (0..m - 1).map(|i| (pts[i + 1].y - pts[i].y) / h[i]).collect();
        let mut t = vec![0.0; m];
        t[0] = d[0];
        t[m - 1] = d[m - 2];
        for i in 1..m - 1 {
            t[i] = if d[i - 1] * d[i] <= 0.0 {
                0.0
            } else {
                (d[i - 1] + d[i]) / 2.0
            };
        }
        for i in 0..m - 1 {
            if d[i] == 0.0 {
                t[i] = 0.0;
                t[i + 1] = 0.0;
                continue;
            }
            let a = t[i] / d[i];
            let b = t[i + 1] / d[i];
            let s = a * a + b * b;
            if s > 9.0 {
                let k = 3.0 / s.sqrt();
                t[i] = k * a * d[i];
                t[i + 1] = k * b * d[i];
            }
        }
        let segments = (0..m - 1)
            .map(|i| {
                let (y0, y1, hh) = (pts[i].y, pts[i + 1].y, h[i]);
                let (m0, m1) = (hh * t[i], hh * t[i + 1]);
                CurveSegment {
                    x0: pts[i].x,
                    k: 1.0 / hh,
                    a: y0,
                    b: m0,
                    c: 3.0 * (y1 - y0) - 2.0 * m0 - m1,
                    d: 2.0 * (y0 - y1) + m0 + m1,
                }
            })
            .collect();
        Some(Self { points: pts, segments })
    }

    /// The segment `x` falls in (`x` inside the points' span): the one whose start is the last at
    /// or before it — the same choice the export's tree of comparisons makes.
    pub fn segment(&self, x: f64) -> &CurveSegment {
        let mut lo = 0;
        let mut hi = self.segments.len();
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if x < self.points[mid].x {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        &self.segments[lo]
    }

    /// The curve at `x`: held flat outside its first and last point.
    pub fn eval(&self, x: f64) -> f64 {
        let (first, last) = (self.points[0], self.points[self.points.len() - 1]);
        if x < first.x {
            first.y
        } else if x >= last.x {
            last.y
        } else {
            self.segment(x).eval(x)
        }
    }
}

impl Curves {
    pub fn is_identity(&self) -> bool {
        [&self.master, &self.red, &self.green, &self.blue]
            .iter()
            .all(|c| normalize_curve(c).is_empty())
    }

    pub fn normalized(&self) -> Self {
        Self {
            master: normalize_curve(&self.master),
            red: normalize_curve(&self.red),
            green: normalize_curve(&self.green),
            blue: normalize_curve(&self.blue),
        }
    }

    /// The channel curves (red, green, blue) and the master, as shapes; `None` is the identity.
    pub fn shapes(&self) -> ([Option<CurveShape>; 3], Option<CurveShape>) {
        (
            [
                CurveShape::new(&self.red),
                CurveShape::new(&self.green),
                CurveShape::new(&self.blue),
            ],
            CurveShape::new(&self.master),
        )
    }

    /// What the curves do to one channel value `x` in `0..=1` (`channel` 0, 1, 2 for red, green,
    /// blue): the channel's curve, then the master, clamped to `0..=1`.
    pub fn apply(&self, channel: usize, x: f64) -> f64 {
        let (channels, master) = self.shapes();
        let y = channels[channel].as_ref().map_or(x, |s| s.eval(x));
        master.as_ref().map_or(y, |s| s.eval(y)).clamp(0.0, 1.0)
    }
}

/// One colour wheel: a puck in the unit disc and a lightness offset in `-1..=1`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Wheel {
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub lightness: f64,
}

/// Rounded to 1e-4, so the filter's text does not ride on the last digit of a product.
fn tenk(v: f64) -> f64 {
    let r = (v * 1e4).round() / 1e4;
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

impl Wheel {
    pub fn normalized(&self) -> Self {
        let f = |v: f64| if v.is_finite() { v } else { 0.0 };
        let (x, y) = (f(self.x), f(self.y));
        let len = x.hypot(y);
        let s = if len > 1.0 { 1.0 / len } else { 1.0 };
        Self {
            x: tenk(x * s),
            y: tenk(y * s),
            lightness: tenk(f(self.lightness).clamp(-1.0, 1.0)),
        }
    }

    pub fn is_neutral(&self) -> bool {
        self.rgb() == [0.0; 3]
    }

    /// `colorbalance`'s red, green and blue for this wheel's range: the puck's hue as a zero-sum
    /// offset (`(cos a, cos(a - 120°), cos(a + 120°))` times its length), plus the lightness on all
    /// three, each held to the filter's `-1..=1`.
    pub fn rgb(&self) -> [f64; 3] {
        let w = self.normalized();
        let half_root3 = 3f64.sqrt() / 2.0;
        [w.x, -w.x / 2.0 + half_root3 * w.y, -w.x / 2.0 - half_root3 * w.y].map(|v| tenk((v + w.lightness).clamp(-1.0, 1.0)))
    }
}

/// Shadows, midtones and highlights.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Wheels {
    #[serde(default)]
    pub shadows: Wheel,
    #[serde(default)]
    pub midtones: Wheel,
    #[serde(default)]
    pub highlights: Wheel,
}

impl Wheels {
    pub fn is_neutral(&self) -> bool {
        self.shadows.is_neutral() && self.midtones.is_neutral() && self.highlights.is_neutral()
    }

    pub fn normalized(&self) -> Self {
        Self {
            shadows: self.shadows.normalized(),
            midtones: self.midtones.normalized(),
            highlights: self.highlights.normalized(),
        }
    }
}

/// A `.cube` 3D LUT a clip is looked up through, and how much of it (`0..=1`, in hundredths).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ClipLut {
    /// Absolute path of the `.cube` file.
    pub path: String,
    #[serde(default = "one")]
    pub intensity: f64,
}

impl ClipLut {
    pub fn normalized(&self) -> Self {
        let i = if self.intensity.is_finite() {
            self.intensity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        Self {
            path: self.path.clone(),
            intensity: (i * 100.0).round() / 100.0,
        }
    }

    /// At 0 the LUT is kept but changes nothing, and is left out of the graph.
    pub fn is_neutral(&self) -> bool {
        self.normalized().intensity == 0.0
    }
}

/// Curves, wheels and a LUT, each `None` when unused.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Grade {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curves: Option<Curves>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wheels: Option<Wheels>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lut: Option<ClipLut>,
}

impl Grade {
    /// Nothing is set.
    pub fn is_empty(&self) -> bool {
        self.curves.is_none() && self.wheels.is_none() && self.lut.is_none()
    }

    /// The picture is left as it is (a LUT at intensity 0 included).
    pub fn is_identity(&self) -> bool {
        self.curves().is_none() && self.wheels().is_none() && self.lut().is_none()
    }

    /// Clamped and rounded as the setter keeps them; identity curves and neutral wheels are dropped.
    pub fn normalized(&self) -> Self {
        Self {
            curves: self.curves.as_ref().map(Curves::normalized).filter(|c| !c.is_identity()),
            wheels: self.wheels.map(|w| w.normalized()).filter(|w| !w.is_neutral()),
            lut: self.lut.as_ref().map(ClipLut::normalized),
        }
    }

    /// The curves, when they change the picture.
    pub fn curves(&self) -> Option<Curves> {
        self.curves.as_ref().map(Curves::normalized).filter(|c| !c.is_identity())
    }

    /// The wheels, when they change the picture.
    pub fn wheels(&self) -> Option<Wheels> {
        self.wheels.map(|w| w.normalized()).filter(|w| !w.is_neutral())
    }

    /// The LUT, when it changes the picture (not at intensity 0).
    pub fn lut(&self) -> Option<ClipLut> {
        self.lut.as_ref().map(ClipLut::normalized).filter(|l| !l.is_neutral())
    }

    /// What a review of an edit says changed, or `None`.
    pub(crate) fn changes(before: &Grade, after: &Grade) -> Vec<String> {
        let mut parts = Vec::new();
        if before.curves != after.curves {
            parts.push(
                if after.curves.is_some() {
                    "curves set"
                } else {
                    "curves cleared"
                }
                .to_string(),
            );
        }
        if before.wheels != after.wheels {
            parts.push(
                if after.wheels.is_some() {
                    "colour wheels set"
                } else {
                    "colour wheels cleared"
                }
                .to_string(),
            );
        }
        if before.lut != after.lut {
            parts.push(match &after.lut {
                None => "LUT cleared".to_string(),
                Some(l) => {
                    let name = std::path::Path::new(&l.path)
                        .file_name()
                        .map_or_else(|| l.path.clone(), |n| n.to_string_lossy().into_owned());
                    format!("LUT {name} at {:.0}%", l.intensity * 100.0)
                }
            });
        }
        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(p: &[(f64, f64)]) -> Vec<CurvePoint> {
        p.iter().map(|&(x, y)| CurvePoint { x, y }).collect()
    }

    #[test]
    fn identity_curves_are_dropped() {
        assert!(normalize_curve(&pts(&[(0.0, 0.0), (1.0, 1.0)])).is_empty());
        assert!(normalize_curve(&pts(&[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)])).is_empty());
        assert!(normalize_curve(&pts(&[(0.3, 0.7)])).is_empty(), "one point draws nothing");
        assert_eq!(
            normalize_curve(&pts(&[(0.2, 0.2), (0.8, 0.8)])).len(),
            2,
            "flat ends move the picture"
        );
        let g = Grade {
            curves: Some(Curves {
                master: pts(&[(0.0, 0.0), (1.0, 1.0)]),
                ..Curves::default()
            }),
            wheels: Some(Wheels::default()),
            lut: None,
        };
        assert!(g.is_identity());
        assert!(g.normalized().is_empty());
    }

    #[test]
    fn points_are_clamped_sorted_deduplicated_and_capped() {
        let n = normalize_curve(&pts(&[(1.5, -1.0), (0.5, 0.2), (f64::NAN, 0.3), (0.5, 0.9)]));
        assert_eq!(n, pts(&[(0.0, 0.3), (0.5, 0.2), (1.0, 0.0)]));
        let many: Vec<CurvePoint> = (0..40)
            .map(|i| CurvePoint {
                x: i as f64 / 39.0,
                y: 0.5,
            })
            .collect();
        assert_eq!(normalize_curve(&many).len(), MAX_CURVE_POINTS);
    }

    #[test]
    fn the_curve_goes_through_its_points_and_never_overshoots() {
        let shape = CurveShape::new(&pts(&[(0.0, 0.0), (0.25, 0.1), (0.5, 0.9), (0.75, 0.92), (1.0, 1.0)])).unwrap();
        for p in &shape.points {
            assert!((shape.eval(p.x) - p.y).abs() < 1e-12, "{p:?}");
        }
        let mut prev = -1.0;
        for i in 0..=1000 {
            let y = shape.eval(i as f64 / 1000.0);
            assert!(y >= prev - 1e-12, "monotone data stays monotone at {i}");
            assert!((0.0..=1.0).contains(&y));
            prev = y;
        }
        // A peak: flat at its top, never above it.
        let peak = CurveShape::new(&pts(&[(0.0, 0.0), (0.5, 0.8), (1.0, 0.0)])).unwrap();
        for i in 0..=1000 {
            assert!(peak.eval(i as f64 / 1000.0) <= 0.8 + 1e-12);
        }
    }

    /// The same numbers `grade.test.ts` holds the TS mirror to.
    #[test]
    fn the_curve_values_the_mirror_is_pinned_to() {
        let shape = CurveShape::new(&pts(&[(0.0, 0.0), (0.25, 0.1), (0.5, 0.9), (0.75, 0.92), (1.0, 1.0)])).unwrap();
        for (x, y) in [
            (0.1, 0.016457208444468113),
            (0.3, 0.21877970302616673),
            (0.4, 0.6379377269512614),
            (0.6, 0.914919187180245),
            (0.9, 0.9610172732017916),
        ] {
            assert!((shape.eval(x) - y).abs() < 1e-15, "{x}: {}", shape.eval(x));
        }
    }

    #[test]
    fn a_straight_line_is_exactly_linear_and_ends_hold() {
        let invert = CurveShape::new(&pts(&[(0.0, 1.0), (1.0, 0.0)])).unwrap();
        for i in 0..=255 {
            let x = i as f64 / 255.0;
            assert!((invert.eval(x) - (1.0 - x)).abs() < 1e-12);
        }
        let mid = CurveShape::new(&pts(&[(0.2, 0.3), (0.8, 0.6)])).unwrap();
        assert_eq!(mid.eval(0.0), 0.3);
        assert_eq!(mid.eval(1.0), 0.6);
    }

    #[test]
    fn the_master_runs_on_the_channels_result() {
        let c = Curves {
            master: pts(&[(0.0, 1.0), (1.0, 0.0)]),
            red: pts(&[(0.0, 0.0), (1.0, 0.5)]),
            ..Curves::default()
        };
        assert!((c.apply(0, 1.0) - 0.5).abs() < 1e-12);
        assert!((c.apply(1, 0.25) - 0.75).abs() < 1e-12);
    }

    #[test]
    fn a_wheel_is_a_hue_and_a_lightness() {
        let red = Wheel {
            x: 1.0,
            y: 0.0,
            lightness: 0.0,
        };
        assert_eq!(red.rgb(), [1.0, -0.5, -0.5]);
        let green = Wheel {
            x: -0.5,
            y: 3f64.sqrt() / 2.0,
            lightness: 0.0,
        };
        assert_eq!(green.rgb(), [-0.5, 1.0, -0.5]);
        let lift = Wheel {
            x: 0.0,
            y: 0.0,
            lightness: 0.2,
        };
        assert_eq!(lift.rgb(), [0.2, 0.2, 0.2]);
        let far = Wheel {
            x: 3.0,
            y: 4.0,
            lightness: 5.0,
        }
        .normalized();
        assert_eq!((far.x, far.y, far.lightness), (0.6, 0.8, 1.0));
        assert!(Wheel::default().is_neutral());
    }

    #[test]
    fn a_lut_at_zero_is_neutral_but_kept() {
        let g = Grade {
            lut: Some(ClipLut {
                path: "/a.cube".into(),
                intensity: 0.001,
            }),
            ..Grade::default()
        };
        assert!(g.is_identity());
        assert!(!g.normalized().is_empty());
        assert_eq!(g.normalized().lut.unwrap().intensity, 0.0);
    }

    #[test]
    fn a_grade_round_trips_and_an_old_clip_has_none() {
        let g = Grade {
            curves: Some(Curves {
                red: pts(&[(0.0, 0.1), (1.0, 0.9)]),
                ..Curves::default()
            }),
            wheels: None,
            lut: Some(ClipLut {
                path: "/l.cube".into(),
                intensity: 0.5,
            }),
        };
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(serde_json::from_str::<Grade>(&json).unwrap(), g);
        assert!(!json.contains("master"), "{json}");
        let lut: ClipLut = serde_json::from_str(r#"{"path":"/a.cube"}"#).unwrap();
        assert_eq!(lut.intensity, 1.0);
    }
}
