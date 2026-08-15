//! Domain model for a Kerf project: assets, cached analysis metadata, and the
//! non-destructive timeline (edit-decision-list).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Kind of an elementary media stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Data,
}

/// How a video stream maps the world onto its frame. `Flat` is ordinary
/// rectilinear video; the rest describe 360 sources — a raw Insta360 `.insv` is
/// `DualFisheye` (two circular hemispheres side by side), a stitched Insta360
/// Studio export is `Equirect`. Drives the `v360` reprojection at export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Projection {
    Equirect,
    DualFisheye,
    Fisheye,
    Flat,
}

impl Projection {
    /// The `v360` `input=` / `output=` token naming this projection.
    pub fn v360_name(self) -> &'static str {
        match self {
            Projection::Equirect => "e",
            Projection::DualFisheye => "dfisheye",
            Projection::Fisheye => "fisheye",
            Projection::Flat => "flat",
        }
    }

    /// True when this projection covers the sphere, i.e. is worth reframing.
    pub fn is_spherical(self) -> bool {
        !matches!(self, Projection::Flat)
    }

    /// True when the source is lens-shaped, so the lens field of view
    /// (`ih_fov`/`iv_fov`) is meaningful on input.
    pub fn is_fisheye(self) -> bool {
        matches!(self, Projection::DualFisheye | Projection::Fisheye)
    }

    /// Parse the wire name (`equirect`, `dual_fisheye`, `fisheye`, `flat`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "equirect" => Some(Projection::Equirect),
            "dual_fisheye" => Some(Projection::DualFisheye),
            "fisheye" => Some(Projection::Fisheye),
            "flat" => Some(Projection::Flat),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Projection::Equirect => "equirect",
            Projection::DualFisheye => "dual_fisheye",
            Projection::Fisheye => "fisheye",
            Projection::Flat => "flat",
        }
    }
}

/// Structured description of a single stream inside an imported asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInfo {
    pub index: u32,
    pub kind: StreamKind,
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u16>,
    /// True for a single-frame still image (PNG/JPEG/…): the stream has no real
    /// duration, so the engine loops it for the clip's length on export and never
    /// seeks into it. Defaulted (and omitted when false) so older `.kerf` JSON —
    /// which predates the flag — still deserializes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub image: bool,
    /// Spherical projection of a video stream, when the source is 360 footage.
    /// Detected at probe time from the file's spherical metadata or its geometry
    /// (see `engine::cli::detect_projection`); `None` for ordinary flat video.
    /// Defaulted (and omitted when unset) so older `.kerf` JSON still
    /// deserializes — this rides along in the `streams` JSON column, which is
    /// why 360 support needs no schema migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<Projection>,
}

/// An imported media file plus the structured metadata probed from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: Uuid,
    /// Absolute path on disk to the source media.
    pub path: String,
    pub name: String,
    /// Total duration in seconds.
    pub duration: f64,
    pub streams: Vec<StreamInfo>,
    pub imported_at: DateTime<Utc>,
    /// The original capture files this asset was derived from, when `path` is
    /// something Kerf produced at import rather than a file the user picked —
    /// today only an Insta360 lens pair stitched into one equirect video. Empty
    /// for an ordinary asset, whose `path` *is* its source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_paths: Vec<String>,
}

impl Asset {
    /// The dominant stream kind, used when auto-selecting a target track.
    pub fn primary_kind(&self) -> StreamKind {
        if self.streams.iter().any(|s| s.kind == StreamKind::Video) {
            StreamKind::Video
        } else if self.streams.iter().any(|s| s.kind == StreamKind::Audio) {
            StreamKind::Audio
        } else {
            StreamKind::Data
        }
    }

    pub fn has_audio(&self) -> bool {
        self.streams.iter().any(|s| s.kind == StreamKind::Audio)
    }

    /// True when this asset is a still image (a single-frame PNG/JPEG/…). Such an
    /// asset has no intrinsic duration, so it is placed on the timeline with a
    /// default length and looped — not seeked — on export.
    pub fn is_image(&self) -> bool {
        self.streams.iter().any(|s| s.image)
    }

    /// The spherical projection of this asset's video, if it is 360 footage.
    /// Clips cut from such an asset are reframed to flat by default.
    pub fn projection(&self) -> Option<Projection> {
        self.streams.iter().find_map(|s| s.projection).filter(|p| p.is_spherical())
    }
}

/// Default timeline length, in seconds, given to a still image on import (it has
/// no intrinsic duration). The clip can be trimmed like any other afterwards.
pub const DEFAULT_IMAGE_DURATION: f64 = 5.0;

/// A half-open time range `[start, end)` in seconds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

/// A transcript line with timecodes (seconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// EBU R128 loudness measurement of an asset's audio, from a single `loudnorm`
/// analysis pass. Lets an agent level a clip to a target or balance a voiceover
/// against a music bed instead of guessing at a linear gain.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Loudness {
    /// Integrated (program) loudness in LUFS.
    pub integrated_lufs: f64,
    /// Loudness range (LRA) in LU.
    pub loudness_range: f64,
    /// Maximum true peak in dBTP.
    pub true_peak_dbtp: f64,
    /// Gating threshold used for the measurement, in LUFS.
    pub threshold_lufs: f64,
}

/// Coarse content class of an asset's audio. Heuristic (energy continuity +
/// zero-crossing-rate variability), so it is a hint, not a trained classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioClass {
    /// Predominantly spoken word (gappy energy, variable ZCR).
    Speech,
    /// Predominantly music (continuous energy, steady ZCR).
    Music,
    /// Both present (e.g. dialogue over a music bed).
    Mixed,
    /// Could not be determined.
    Unknown,
}

/// An [`AudioClass`] verdict with a confidence in 0.0–1.0.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AudioClassification {
    pub class: AudioClass,
    pub confidence: f64,
}

/// Estimated tempo and beat grid for an asset's audio. Best-effort: derived by
/// autocorrelating the onset envelope, so it is most reliable on percussive
/// music and may land on a tempo octave — gate on `confidence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tempo {
    /// Estimated tempo in beats per minute.
    pub bpm: f64,
    /// Beat timestamps in seconds across the asset.
    pub beats: Vec<f64>,
    /// How periodic the audio is, 0.0–1.0 (the normalized autocorrelation peak).
    pub confidence: f64,
}

/// Everything the rhythm analysis pass derives from one decoded PCM stream:
/// onsets, tempo and the speech/music class. Bundled because the three share
/// the decode (and onsets/tempo the onset envelope) — computing them together
/// costs one full-file ffmpeg decode instead of three.
#[derive(Debug, Clone, Default)]
pub struct Rhythm {
    pub onsets: Vec<f64>,
    pub tempo: Option<Tempo>,
    pub audio_class: Option<AudioClassification>,
}

/// Cached, pluggable analysis results for an asset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AssetAnalysis {
    pub asset_id: Uuid,
    #[serde(default)]
    pub silence_segments: Vec<TimeRange>,
    #[serde(default)]
    pub scene_changes: Vec<f64>,
    #[serde(default)]
    pub transcript: Vec<TranscriptSegment>,
    /// EBU R128 loudness of the asset's audio, when it has any. `None` until the
    /// asset is analyzed (and for silent / video-only assets).
    #[serde(default)]
    pub loudness: Option<Loudness>,
    /// Onset (transient) timestamps in seconds — moments where new sound energy
    /// arrives. Snap cut points to these to land edits on the beat.
    #[serde(default)]
    pub onsets: Vec<f64>,
    /// Estimated tempo and beat grid, when the audio is rhythmic enough. `None`
    /// for silent / video-only assets and non-rhythmic material.
    #[serde(default)]
    pub tempo: Option<Tempo>,
    /// Coarse speech/music classification of the audio. `None` for silent /
    /// video-only assets. Route ducking/leveling decisions off this.
    #[serde(default)]
    pub audio_class: Option<AudioClassification>,
}

fn one() -> f64 {
    1.0
}

/// Per-clip geometric transform applied when compositing at export. A default
/// transform is the identity (full-frame, centered, opaque, uncropped).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Uniform scale multiplier applied after the clip is fit to the frame
    /// (1.0 = fit). Values < 1.0 shrink the picture for picture-in-picture.
    #[serde(default = "one")]
    pub scale: f64,
    /// Horizontal offset as a fraction of the frame width (0.0 = centered).
    #[serde(default)]
    pub pos_x: f64,
    /// Vertical offset as a fraction of the frame height (0.0 = centered).
    #[serde(default)]
    pub pos_y: f64,
    /// Clockwise rotation in degrees.
    #[serde(default)]
    pub rotation: f64,
    /// Opacity in 0.0–1.0 (1.0 = fully opaque).
    #[serde(default = "one")]
    pub opacity: f64,
    /// Fraction of the source cropped from each edge (0.0 = no crop).
    #[serde(default)]
    pub crop_left: f64,
    #[serde(default)]
    pub crop_right: f64,
    #[serde(default)]
    pub crop_top: f64,
    #[serde(default)]
    pub crop_bottom: f64,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            crop_left: 0.0,
            crop_right: 0.0,
            crop_top: 0.0,
            crop_bottom: 0.0,
        }
    }
}

impl Transform {
    /// True when the transform leaves the picture untouched (full-frame fit).
    pub fn is_identity(&self) -> bool {
        *self == Transform::default()
    }

    /// True when compositing this clip needs an alpha channel (rotation leaves
    /// transparent corners; opacity blends; both require alpha).
    pub fn needs_alpha(&self) -> bool {
        self.opacity < 1.0 || self.rotation != 0.0
    }

    /// True when any edge crop is requested.
    pub fn has_crop(&self) -> bool {
        self.crop_left > 0.0 || self.crop_right > 0.0 || self.crop_top > 0.0 || self.crop_bottom > 0.0
    }
}

/// Per-clip color correction applied at export via the `eq` filter. A default
/// is the identity (no change).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Color {
    /// Additive brightness in -1.0–1.0 (0.0 = unchanged).
    #[serde(default)]
    pub brightness: f64,
    /// Contrast multiplier (1.0 = unchanged).
    #[serde(default = "one")]
    pub contrast: f64,
    /// Saturation multiplier (1.0 = unchanged).
    #[serde(default = "one")]
    pub saturation: f64,
    /// Gamma (1.0 = unchanged).
    #[serde(default = "one")]
    pub gamma: f64,
    /// Warm/cool shift in -1.0–1.0 (0.0 = unchanged): positive warms the
    /// picture (lifts red, lowers blue), negative cools it. Rendered as
    /// opposing per-channel gammas — what makes one-click warm / cool looks
    /// possible, since plain saturation/gamma can't tint.
    #[serde(default)]
    pub temperature: f64,
}

impl Default for Color {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            gamma: 1.0,
            temperature: 0.0,
        }
    }
}

impl Color {
    /// True when the color correction leaves the picture untouched.
    pub fn is_identity(&self) -> bool {
        *self == Color::default()
    }
}

/// How a clip blends with the preceding clip on its track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    /// Dissolve: the incoming clip fades up over the outgoing clip's tail.
    Crossfade,
    /// Dip to black: the outgoing clip fades to black, the incoming up from it.
    DipToBlack,
}

impl TransitionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TransitionKind::Crossfade => "crossfade",
            TransitionKind::DipToBlack => "dip_to_black",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "crossfade" => Some(TransitionKind::Crossfade),
            "dip_to_black" | "diptoblack" => Some(TransitionKind::DipToBlack),
            _ => None,
        }
    }
}

/// A transition blending the **start** of a clip with the clip that precedes it
/// on the same track. Realized at export.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Transition {
    pub kind: TransitionKind,
    /// Duration of the transition in seconds.
    pub duration: f64,
}

/// A per-clip video effect, realized as a filter inserted into the clip's video
/// chain at export (after color correction). The order in `Clip::effects` is the
/// order they are applied. `ChromaKey` is the one effect that establishes an
/// alpha channel, so the clip composites with transparency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VideoEffect {
    /// Gaussian blur (`gblur`); larger `sigma` = softer.
    Blur { sigma: f64 },
    /// Unsharp-mask sharpen; `amount` is the luma strength.
    Sharpen { amount: f64 },
    /// Desaturate to grayscale.
    Grayscale,
    /// Invert colors (negative).
    Invert,
    /// Darken the frame edges.
    Vignette,
    /// Key out a color to transparency (green/blue screen). `color` is any ffmpeg
    /// color (e.g. `green`, `0x00ff00`); `similarity`/`blend` in 0.0–1.0.
    ChromaKey { color: String, similarity: f64, blend: f64 },
}

impl VideoEffect {
    /// True when applying this effect leaves the frame with an alpha channel.
    pub fn produces_alpha(&self) -> bool {
        matches!(self, VideoEffect::ChromaKey { .. })
    }
}

/// A per-clip audio effect, realized as a filter inserted into the clip's audio
/// chain at export (after the clip gain). The order in `Clip::audio` is the order
/// they are applied. Thresholds/gains are in dB at the model boundary and
/// converted to the linear units ffmpeg's dynamics filters want by the engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AudioEffect {
    /// High-pass: attenuate below `hz` (cut rumble / handling noise).
    Highpass { hz: f64 },
    /// Low-pass: attenuate above `hz` (cut hiss).
    Lowpass { hz: f64 },
    /// Single parametric EQ band at `hz`, `width` Hz wide, `gain_db` boost/cut.
    Equalizer { hz: f64, width: f64, gain_db: f64 },
    /// Dynamic-range compressor.
    Compressor {
        threshold_db: f64,
        ratio: f64,
        attack_ms: f64,
        release_ms: f64,
        makeup_db: f64,
    },
    /// Noise gate: silence audio below `threshold_db`.
    Gate { threshold_db: f64 },
}

fn half() -> f64 {
    0.5
}
fn lower_third_y() -> f64 {
    0.82
}
fn default_text_size() -> f64 {
    0.06
}
fn default_text_color() -> String {
    "white".to_string()
}

/// One keyframe of a clip's animated transform: the value of each animatable
/// channel at `time` (seconds from the clip's start). With two or more keyframes
/// the engine interpolates linearly between them and renders the motion with
/// per-frame ffmpeg expressions; crop and the rest of the static [`Transform`]
/// are unaffected.
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
}

impl Keyframe {
    /// A keyframe at `time` carrying the values of `transform`'s animatable
    /// channels (the static defaults for a fresh keyframe).
    pub fn from_transform(time: f64, t: &Transform) -> Self {
        Self {
            time,
            scale: t.scale,
            pos_x: t.pos_x,
            pos_y: t.pos_y,
            rotation: t.rotation,
            opacity: t.opacity,
        }
    }
}

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

/// One keyframe of an animated [`TextOverlay`]: position and opacity at `time`
/// (seconds from the overlay's `start`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TextKeyframe {
    pub time: f64,
    #[serde(default = "half")]
    pub pos_x: f64,
    #[serde(default = "lower_third_y")]
    pub pos_y: f64,
    #[serde(default = "one")]
    pub opacity: f64,
}

/// A timed text element drawn over the composited picture at export (titles,
/// lower-thirds, captions, watermarks). Positions are fractions of the output
/// frame with the text centered on `(pos_x, pos_y)`; `size` is the font height
/// as a fraction of the frame height. Rendered with `drawtext`. Captions are
/// just a batch of these generated from a transcript (see
/// `Project::captions_from_transcript`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextOverlay {
    pub id: Uuid,
    pub text: String,
    /// When the overlay appears / disappears, in timeline seconds.
    pub start: f64,
    pub end: f64,
    #[serde(default = "half")]
    pub pos_x: f64,
    #[serde(default = "lower_third_y")]
    pub pos_y: f64,
    /// Font height as a fraction of the frame height.
    #[serde(default = "default_text_size")]
    pub size: f64,
    /// Any ffmpeg color (e.g. `white`, `#ffcc00`, `yellow@0.9`).
    #[serde(default = "default_text_color")]
    pub color: String,
    /// Optional box color behind the text (e.g. `black@0.5`); `None` = no box.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    /// Optional system font family name (see `fonts::list_system_fonts`);
    /// `None` = FFmpeg's `drawtext` default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(default)]
    pub bold: bool,
    /// Optional position/opacity animation; with ≥1 keyframe the position and
    /// opacity animate over the overlay's lifetime.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<TextKeyframe>,
}

impl TextOverlay {
    pub fn new(text: impl Into<String>, start: f64, end: f64) -> Self {
        Self {
            id: Uuid::new_v4(),
            text: text.into(),
            start,
            end,
            pos_x: 0.5,
            pos_y: 0.82,
            size: 0.06,
            color: "white".to_string(),
            bg: None,
            font: None,
            bold: false,
            keyframes: Vec::new(),
        }
    }

    /// Sample `(pos_x, pos_y, opacity)` at timeline time `t`. Static fields when
    /// the overlay is not animated; the interpolated keyframe values otherwise.
    /// Used by the still / preview path, which can't evaluate the export's
    /// per-frame `drawtext` expressions.
    pub fn sample(&self, t: f64) -> (f64, f64, f64) {
        if self.keyframes.is_empty() {
            return (self.pos_x, self.pos_y, 1.0);
        }
        let local = t - self.start;
        let chan = |get: fn(&TextKeyframe) -> f64, fallback: f64| {
            interpolate(&self.keyframes.iter().map(|k| (k.time, get(k))).collect::<Vec<_>>(), local).unwrap_or(fallback)
        };
        (
            chan(|k| k.pos_x, self.pos_x),
            chan(|k| k.pos_y, self.pos_y),
            chan(|k| k.opacity, 1.0),
        )
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

/// Render a transcript as a SubRip (`.srt`) subtitle document.
pub fn transcript_to_srt(segments: &[TranscriptSegment]) -> String {
    fn ts(seconds: f64) -> String {
        let s = seconds.max(0.0);
        let ms = (s * 1000.0).round() as u64;
        let (h, rem) = (ms / 3_600_000, ms % 3_600_000);
        let (m, rem) = (rem / 60_000, rem % 60_000);
        let (sec, milli) = (rem / 1000, rem % 1000);
        format!("{h:02}:{m:02}:{sec:02},{milli:03}")
    }
    let mut out = String::new();
    for (i, seg) in segments.iter().enumerate() {
        out.push_str(&format!(
            "{n}\n{start} --> {end}\n{text}\n\n",
            n = i + 1,
            start = ts(seg.start),
            end = ts(seg.end),
            text = seg.text.trim(),
        ));
    }
    out
}

/// A single non-destructive edit referencing a source range of an asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: Uuid,
    pub asset_id: Uuid,
    /// In-point in the source asset (seconds).
    pub source_in: f64,
    /// Out-point in the source asset (seconds).
    pub source_out: f64,
    /// Position of the clip on the timeline (seconds).
    pub timeline_start: f64,
    /// Linear gain applied to this clip (1.0 = unchanged).
    pub volume: f32,
    /// Fade-in duration at the clip's start (seconds); 0.0 = no fade. Applied to
    /// both picture (fade from black) and audio (fade from silence) at export.
    #[serde(default)]
    pub fade_in: f64,
    /// Fade-out duration at the clip's end (seconds); 0.0 = no fade.
    #[serde(default)]
    pub fade_out: f64,
    /// Playback rate (1.0 = unchanged). > 1.0 speeds up, < 1.0 slows down, and a
    /// negative value plays the source in reverse. The clip's timeline duration
    /// is its source span divided by the magnitude of the speed.
    #[serde(default = "one")]
    pub speed: f64,
    /// Geometric transform (scale / position / crop / rotation / opacity).
    #[serde(default)]
    pub transform: Transform,
    /// Color correction (brightness / contrast / saturation / gamma).
    #[serde(default)]
    pub color: Color,
    /// Transition blending this clip's start with the preceding clip, if any.
    #[serde(default)]
    pub transition_in: Option<Transition>,
    /// Video effects applied in order at export (blur, chroma key, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<VideoEffect>,
    /// Audio effects applied in order at export (EQ, compressor, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<AudioEffect>,
    /// Transform animation. Empty = the static `transform` is used; otherwise the
    /// engine interpolates these keyframes to animate scale / position / rotation
    /// / opacity over the clip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<Keyframe>,
    /// Reprojection of 360 source footage, when this clip references a spherical
    /// asset. `None` for ordinary flat video (and for a 360 clip the user has
    /// explicitly un-reframed, to work in the raw projection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reframe: Option<Reframe>,
    /// Whether this clip renders. A disabled clip keeps its place on the
    /// timeline (and its trims, effects and keyframes) but is dropped before the
    /// render graph is built — the per-clip counterpart of muting a track.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

fn is_yes(b: &bool) -> bool {
    *b
}

/// Smallest speed magnitude allowed, to keep clip durations finite.
pub const MIN_SPEED: f64 = 0.01;

impl Clip {
    /// A new clip with default volume, no fades, full speed and identity
    /// transform / color and no transition.
    pub fn new(asset_id: Uuid, source_in: f64, source_out: f64, timeline_start: f64) -> Self {
        Self {
            id: Uuid::new_v4(),
            asset_id,
            source_in,
            source_out,
            timeline_start,
            volume: 1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 1.0,
            transform: Transform::default(),
            color: Color::default(),
            transition_in: None,
            effects: Vec::new(),
            audio: Vec::new(),
            keyframes: Vec::new(),
            reframe: None,
            enabled: true,
        }
    }

    /// A new clip that also reframes, when `asset` is 360 footage — the shape
    /// every clip-creating op should use so a spherical source lands on the
    /// timeline already looking like ordinary video.
    pub fn for_asset(asset: &Asset, source_in: f64, source_out: f64, timeline_start: f64) -> Self {
        let mut clip = Self::new(asset.id, source_in, source_out, timeline_start);
        clip.reframe = asset.projection().map(Reframe::new);
        clip
    }

    /// True when the clip carries transform keyframes (i.e. is animated).
    pub fn is_animated(&self) -> bool {
        !self.keyframes.is_empty()
    }

    /// The clip's keyframes sorted by time (the stored order is kept sorted by
    /// the editing op, but render code must not assume it).
    pub fn sorted_keyframes(&self) -> Vec<Keyframe> {
        let mut k = self.keyframes.clone();
        k.sort_by(|a, b| a.time.total_cmp(&b.time));
        k
    }

    /// Sample the (possibly animated) transform at `local` seconds from the
    /// clip's start: the static [`Transform`] with its animatable channels
    /// (scale / position / rotation / opacity) overridden by the interpolated
    /// keyframe values when the clip is animated. Used by the still / preview
    /// path, which cannot evaluate the export's per-frame expressions.
    pub fn transform_at(&self, local: f64) -> Transform {
        let mut t = self.transform;
        if self.keyframes.is_empty() {
            return t;
        }
        let k = self.sorted_keyframes();
        let chan = |get: fn(&Keyframe) -> f64| interpolate(&k.iter().map(|kf| (kf.time, get(kf))).collect::<Vec<_>>(), local);
        if let Some(v) = chan(|kf| kf.scale) {
            t.scale = v;
        }
        if let Some(v) = chan(|kf| kf.pos_x) {
            t.pos_x = v;
        }
        if let Some(v) = chan(|kf| kf.pos_y) {
            t.pos_y = v;
        }
        if let Some(v) = chan(|kf| kf.rotation) {
            t.rotation = v;
        }
        if let Some(v) = chan(|kf| kf.opacity) {
            t.opacity = v;
        }
        t
    }

    /// Sample the (possibly animated) reframe at `local` seconds from the clip's
    /// start. `None` when the clip is not reframed. Used by the still / preview
    /// path, which cannot drive `v360` with runtime commands the way export does.
    pub fn reframe_at(&self, local: f64) -> Option<ResolvedReframe> {
        self.reframe.as_ref().map(|r| r.sample(local))
    }

    /// Length of the referenced source span (seconds), ignoring speed.
    pub fn source_duration(&self) -> f64 {
        (self.source_out - self.source_in).max(0.0)
    }

    /// Speed magnitude, clamped away from zero (direction dropped).
    pub fn speed_mag(&self) -> f64 {
        self.speed.abs().max(MIN_SPEED)
    }

    /// True when the clip plays its source in reverse.
    pub fn is_reversed(&self) -> bool {
        self.speed < 0.0
    }

    /// Duration on the timeline (seconds), i.e. the source span retimed by speed.
    pub fn duration(&self) -> f64 {
        self.source_duration() / self.speed_mag()
    }

    pub fn timeline_end(&self) -> f64 {
        self.timeline_start + self.duration()
    }
}

/// A single timeline lane holding clips of one kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: Uuid,
    /// When set, this track's audio is ducked under the rest of the mix on
    /// export: sidechain compression keyed by the non-ducked tracks, so e.g. a
    /// music bed dips automatically under dialogue.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub duck: bool,
    /// Silenced (audio) or hidden (video) — the track's clips are dropped before
    /// the render graph is built, so it neither exports nor previews.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub muted: bool,
    /// Soloed. While any track of a kind is soloed, the others of that kind are
    /// treated as muted. Kinds solo independently, so soloing a music bed does
    /// not blank the picture.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub solo: bool,
    /// Locked against editing. Purely an editing guard — a locked track still
    /// renders; the GUI refuses to drag, trim or razor its clips.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    pub kind: StreamKind,
    pub name: String,
    #[serde(default)]
    pub clips: Vec<Clip>,
}

impl Track {
    /// An empty track: not ducked, muted, soloed or locked.
    pub fn new(kind: StreamKind, name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            duck: false,
            muted: false,
            solo: false,
            locked: false,
            kind,
            name: name.into(),
            clips: Vec::new(),
        }
    }

    /// End time of the last clip on this track (seconds).
    pub fn end(&self) -> f64 {
        self.clips.iter().map(Clip::timeline_end).fold(0.0, f64::max)
    }

    /// Recompute clip positions so the track is gapless and in clip order.
    pub fn reflow(&mut self) {
        let mut cursor = 0.0;
        for clip in &mut self.clips {
            clip.timeline_start = cursor;
            cursor += clip.duration();
        }
    }

    /// Order clips left-to-right by their timeline position. Used after a
    /// free-positioning move so the track stays a well-ordered, non-overlapping
    /// lane.
    pub fn sort_by_start(&mut self) {
        self.clips.sort_by(|a, b| a.timeline_start.total_cmp(&b.timeline_start));
    }
}

/// Who made an edit. The MCP server sets this to [`EditSource::Agent`]; the
/// desktop app leaves the default [`EditSource::User`]; the seq-0 baseline is
/// [`EditSource::System`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditSource {
    User,
    Agent,
    System,
}

impl EditSource {
    pub fn as_str(self) -> &'static str {
        match self {
            EditSource::User => "user",
            EditSource::Agent => "agent",
            EditSource::System => "system",
        }
    }
}

/// One entry in the timeline edit history (a stored snapshot of the timeline).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revision {
    pub seq: i64,
    pub label: String,
    pub source: EditSource,
    pub created_at: DateTime<Utc>,
    /// `true` for the revision currently applied to the live timeline.
    pub current: bool,
}

/// A named point on the timeline. Purely an annotation — it renders nothing —
/// but it gives the user and the agent a shared vocabulary for places in the
/// cut ("the laugh at 01:12"), which timestamps alone do not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Marker {
    pub id: Uuid,
    /// Position on the timeline, seconds.
    pub time: f64,
    pub name: String,
    /// Optional CSS color for the ruler chip; the UI picks a default when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// The non-destructive timeline (EDL): a set of multi-kind tracks, the text
/// overlays (titles / lower-thirds / captions) drawn over the composited
/// picture, and the user's markers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeline {
    pub tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<TextOverlay>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
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
            markers: Vec::new(),
        }
    }

    pub fn overlay(&self, id: Uuid) -> Option<&TextOverlay> {
        self.overlays.iter().find(|o| o.id == id)
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
            markers: self.markers.clone(),
        }
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
            // Markers inside the window come along, shifted like everything else;
            // without this a range export would desync every one of them.
            markers: self
                .markers
                .iter()
                .filter(|m| m.time >= start && m.time < end)
                .map(|m| Marker {
                    time: m.time - start,
                    ..m.clone()
                })
                .collect(),
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
                let mag = c.speed_mag();
                let cut_front = (start - cs).max(0.0);
                let cut_back = (ce - end).max(0.0);
                if cut_front > 0.0 {
                    if c.is_reversed() {
                        c.source_out -= cut_front * mag;
                    } else {
                        c.source_in += cut_front * mag;
                    }
                    c.fade_in = 0.0;
                    c.transition_in = None;
                    if !c.keyframes.is_empty() {
                        let pose = clip.transform_at(cut_front);
                        let mut kfs = vec![Keyframe::from_transform(0.0, &pose)];
                        kfs.extend(c.keyframes.iter().filter(|k| k.time > cut_front).map(|k| Keyframe {
                            time: k.time - cut_front,
                            ..*k
                        }));
                        c.keyframes = kfs;
                    }
                    // Same resampling for the reframe camera: pin the pose the
                    // cut lands on, then shift the surviving keyframes back.
                    // Sampled off `clip`, not `c` — `c`'s source points have
                    // already moved above.
                    if let Some(rf) = c.reframe.as_mut().filter(|r| r.is_animated()) {
                        let pose = clip.reframe_at(cut_front).expect("clip reframes");
                        let mut kfs = vec![ReframeKeyframe::from_pose(0.0, &pose)];
                        kfs.extend(rf.keyframes.iter().filter(|k| k.time > cut_front).map(|k| ReframeKeyframe {
                            time: k.time - cut_front,
                            ..*k
                        }));
                        rf.keyframes = kfs;
                    }
                }
                if cut_back > 0.0 {
                    if c.is_reversed() {
                        c.source_in += cut_back * mag;
                    } else {
                        c.source_out -= cut_back * mag;
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
        out
    }
}

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
    /// Reviewed and accepted by the user.
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
/// the same engine the GUI uses, then marks it `ready` (or `failed`). Kerf never
/// edits on its own.
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
mod tests {
    use super::*;

    fn clip_at(start: f64, dur: f64) -> Clip {
        Clip::new(Uuid::new_v4(), 0.0, dur, start)
    }

    fn track(kind: StreamKind, name: &str, clips: Vec<Clip>) -> Track {
        Track {
            clips,
            ..Track::new(kind, name)
        }
    }

    #[test]
    fn for_render_drops_muted_tracks_and_disabled_clips() {
        let mut disabled = clip_at(0.0, 2.0);
        disabled.enabled = false;
        let tl = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0), disabled]),
                Track {
                    muted: true,
                    ..track(StreamKind::Audio, "A1", vec![clip_at(0.0, 2.0)])
                },
            ],
            overlays: Vec::new(),
            markers: Vec::new(),
        };
        let r = tl.for_render();
        // The disabled clip is gone but the enabled one stays.
        assert_eq!(r.tracks[0].clips.len(), 1);
        // A muted track keeps its row (it still carries `duck`) but loses its clips.
        assert_eq!(r.tracks.len(), 2);
        assert!(r.tracks[1].clips.is_empty());
        // Filtering never touches the original.
        assert_eq!(tl.tracks[0].clips.len(), 2);
    }

    #[test]
    fn solo_shadows_other_tracks_of_the_same_kind_only() {
        let tl = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0)]),
                Track {
                    solo: true,
                    ..track(StreamKind::Video, "V2", vec![clip_at(0.0, 2.0)])
                },
                track(StreamKind::Audio, "A1", vec![clip_at(0.0, 2.0)]),
            ],
            overlays: Vec::new(),
            markers: Vec::new(),
        };
        let r = tl.for_render();
        assert!(r.tracks[0].clips.is_empty(), "unsoloed video track is shadowed");
        assert_eq!(r.tracks[1].clips.len(), 1, "soloed video track renders");
        // Soloing a video track must not blank unrelated audio.
        assert_eq!(r.tracks[2].clips.len(), 1, "audio is unaffected by a video solo");
    }

    #[test]
    fn a_muted_track_stays_muted_even_when_soloed() {
        let tl = Timeline {
            tracks: vec![Track {
                muted: true,
                solo: true,
                ..track(StreamKind::Audio, "A1", vec![clip_at(0.0, 2.0)])
            }],
            overlays: Vec::new(),
            markers: Vec::new(),
        };
        assert!(tl.for_render().tracks[0].clips.is_empty());
    }

    #[test]
    fn for_render_is_a_no_op_on_an_untouched_timeline() {
        let tl = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![clip_at(0.0, 2.0), clip_at(2.0, 1.0)]),
                track(StreamKind::Audio, "A1", vec![clip_at(0.0, 3.0)]),
            ],
            overlays: Vec::new(),
            markers: Vec::new(),
        };
        let r = tl.for_render();
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            serde_json::to_string(&tl).unwrap(),
            "an ordinary timeline must reach the graph builders unchanged"
        );
    }

    #[test]
    fn enabled_defaults_to_true_for_clips_saved_before_the_field_existed() {
        // Old projects have no `enabled` key; they must not silently stop rendering.
        let clip: Clip = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000001",
                "asset_id":"00000000-0000-0000-0000-000000000002",
                "source_in":0.0,"source_out":1.0,"timeline_start":0.0,"volume":1.0}"#,
        )
        .unwrap();
        assert!(clip.enabled);
        let t: Track =
            serde_json::from_str(r#"{"id":"00000000-0000-0000-0000-000000000003","kind":"video","name":"V1","clips":[]}"#)
                .unwrap();
        assert!(!t.muted && !t.solo && !t.locked);
    }

    #[test]
    fn slice_shifts_markers_into_the_window_and_drops_the_rest() {
        let mk = |t: f64, n: &str| Marker {
            id: Uuid::new_v4(),
            time: t,
            name: n.into(),
            color: None,
        };
        let tl = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 20.0)])],
            overlays: Vec::new(),
            markers: vec![mk(1.0, "before"), mk(4.0, "inside"), mk(9.0, "after")],
        };
        let s = tl.slice(3.0, 7.0);
        let names: Vec<_> = s.markers.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["inside"], "only markers within the window survive");
        // Shifted with everything else — otherwise a range export desyncs them.
        assert!((s.markers[0].time - 1.0).abs() < 1e-9, "{}", s.markers[0].time);
    }

    #[test]
    fn slice_carries_track_flags_through() {
        let tl = Timeline {
            tracks: vec![Track {
                muted: true,
                locked: true,
                duck: true,
                ..track(StreamKind::Audio, "A1", vec![clip_at(0.0, 10.0)])
            }],
            overlays: Vec::new(),
            markers: Vec::new(),
        };
        let s = tl.slice(2.0, 6.0);
        assert!(s.tracks[0].muted && s.tracks[0].locked && s.tracks[0].duck);
    }
}
