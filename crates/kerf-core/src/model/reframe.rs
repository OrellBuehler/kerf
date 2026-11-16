//! Reframing of 360 footage: a view and its keyframes.

use super::*;

fn default_fov() -> f64 {
    100.0
}

fn default_lens_fov() -> f64 {
    190.0
}

fn flat() -> Projection {
    Projection::Flat
}

/// Narrowest / widest virtual field of view, in degrees. `v360` reads `d_fov=0`
/// as "unset" (derive from `h_fov`/`v_fov`), so the floor must stay above zero,
/// and a command carrying an out-of-range value is *silently discarded* — hence
/// every path that produces a fov clamps into this band first.
pub const MIN_FOV: f64 = 1.0;
pub const MAX_FOV: f64 = 359.0;

/// One keyframe of an animated [`Reframe`]: where the virtual camera points and
/// how wide it sees at `time` (seconds from the clip's start).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReframeKeyframe {
    /// Offset from the clip's `timeline_start`, in seconds.
    pub time: f64,
    #[serde(default)]
    pub yaw: f64,
    #[serde(default)]
    pub pitch: f64,
    #[serde(default)]
    pub roll: f64,
    #[serde(default = "default_fov")]
    pub fov: f64,
}

/// Per-clip reprojection of 360 footage: aim a virtual camera into the sphere
/// and render what it sees. This is the reframing workflow — a 360 source in, an
/// ordinary rectilinear shot out — and with keyframes the camera moves over the
/// clip (a pan across a scene, a whip to a subject) without the source ever being
/// re-encoded.
///
/// `input` is the source's own projection, seeded from the asset's probed
/// [`Projection`] but overridable, since detection is a heuristic. `output` is
/// `Flat` for a normal deliverable, or `Equirect` to stitch a dual-fisheye source
/// without picking a viewing direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Reframe {
    /// Projection of the source footage being read.
    pub input: Projection,
    /// Projection to render into: `Flat` (reframe) or `Equirect` (stitch only).
    #[serde(default = "flat")]
    pub output: Projection,
    /// Field of view of each physical lens, in degrees — only meaningful for a
    /// fisheye `input`. Insta360's lenses run a little past 180°; 190 is a
    /// reasonable starting point, and tuning it moves the stitch seam.
    #[serde(default = "default_lens_fov")]
    pub lens_fov: f64,
    /// Virtual camera heading, in degrees. Wraps at ±180.
    #[serde(default)]
    pub yaw: f64,
    /// Virtual camera elevation, in degrees, clamped to ±90 (straight down to
    /// straight up). Unlike yaw this does *not* wrap — see [`Reframe::sample`].
    #[serde(default)]
    pub pitch: f64,
    /// Virtual camera roll (horizon tilt), in degrees. Wraps at ±180.
    #[serde(default)]
    pub roll: f64,
    /// Diagonal field of view of the virtual camera, in degrees. Maps to `v360`'s
    /// `d_fov`, which derives an aspect-correct horizontal/vertical pair — unlike
    /// `h_fov`, which would need `v_fov` set in lockstep or the picture stretches.
    #[serde(default = "default_fov")]
    pub fov: f64,
    /// Camera animation. Empty = the static pose above; otherwise the engine
    /// interpolates these and drives `v360` over the clip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<ReframeKeyframe>,
}

/// A [`Reframe`] sampled at one instant: the static projection settings plus the
/// virtual camera's pose there, already wrapped and clamped into the ranges
/// `v360` accepts. This is what the engine turns into a `v360` filter, for both
/// the export chain and the still / preview path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedReframe {
    pub input: Projection,
    pub output: Projection,
    pub lens_fov: f64,
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
    pub fov: f64,
}

impl Reframe {
    /// A level, forward-facing 100° view of a source in `input`. This is what a
    /// 360 clip gets when it lands on the timeline, so it previews as ordinary
    /// footage instead of a raw equirect smear or a pair of fisheye circles.
    pub fn new(input: Projection) -> Self {
        Self {
            input,
            output: Projection::Flat,
            lens_fov: default_lens_fov(),
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            fov: default_fov(),
            keyframes: Vec::new(),
        }
    }

    /// True when the clip's camera moves (i.e. carries keyframes).
    pub fn is_animated(&self) -> bool {
        !self.keyframes.is_empty()
    }

    /// The keyframes sorted by time (the stored order is kept sorted by the
    /// editing op, but render code must not assume it).
    pub fn sorted_keyframes(&self) -> Vec<ReframeKeyframe> {
        let mut k = self.keyframes.clone();
        k.sort_by(|a, b| a.time.total_cmp(&b.time));
        k
    }

    /// The static pose, ignoring any animation.
    pub fn pose(&self) -> ResolvedReframe {
        ResolvedReframe {
            input: self.input,
            output: self.output,
            lens_fov: self.lens_fov,
            yaw: wrap180(self.yaw),
            pitch: self.pitch.clamp(-90.0, 90.0),
            roll: wrap180(self.roll),
            fov: self.fov.clamp(MIN_FOV, MAX_FOV),
        }
    }

    /// Sample the virtual camera at `local` seconds from the clip's start.
    ///
    /// Yaw and roll interpolate along the shortest arc, so a pan from 170° to
    /// -170° travels 20° rather than sweeping 340° the long way round. Pitch
    /// deliberately does **not**: panning up and over the pole is never what was
    /// meant, and `v360`'s `|pitch| > 90` region renders an upside-down view.
    pub fn sample(&self, local: f64) -> ResolvedReframe {
        let mut r = self.pose();
        if self.keyframes.is_empty() {
            return r;
        }
        let k = self.sorted_keyframes();
        let pts = |get: fn(&ReframeKeyframe) -> f64| k.iter().map(|kf| (kf.time, get(kf))).collect::<Vec<_>>();
        if let Some(v) = interpolate_angle(&pts(|kf| kf.yaw), local) {
            r.yaw = v;
        }
        if let Some(v) = interpolate(&pts(|kf| kf.pitch), local) {
            r.pitch = v.clamp(-90.0, 90.0);
        }
        if let Some(v) = interpolate_angle(&pts(|kf| kf.roll), local) {
            r.roll = v;
        }
        if let Some(v) = interpolate(&pts(|kf| kf.fov), local) {
            r.fov = v.clamp(MIN_FOV, MAX_FOV);
        }
        r
    }
}

impl ReframeKeyframe {
    /// A keyframe at `time` carrying `pose`'s animatable channels (the values a
    /// fresh keyframe pins).
    pub fn from_pose(time: f64, p: &ResolvedReframe) -> Self {
        Self {
            time,
            yaw: p.yaw,
            pitch: p.pitch,
            roll: p.roll,
            fov: p.fov,
        }
    }
}
