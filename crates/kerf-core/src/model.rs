//! Domain model for a Kerf project: assets, cached analysis metadata, and the
//! non-destructive timeline (edit-decision-list).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
///
/// Three families, and the family is what decides how the cut is rendered:
/// a **dip** takes both sides through a solid colour, a **dissolve** mixes them,
/// and a **motion** transition slides the incoming clip in over the outgoing one
/// (`Slide*`) or shoves the outgoing one out of frame with it (`Push*`). All of
/// them borrow the outgoing clip's unused source handle to keep it playing under
/// the transition, so a cut with no handle left degrades to a hard cut rather
/// than to a fade from black.
///
/// The direction in a motion transition names the direction of **travel**, the
/// way an editor says it: `SlideLeft` brings the new shot in from the right edge
/// and moves it left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    /// Dissolve: the incoming clip fades up over the outgoing clip's tail.
    Crossfade,
    /// Dip to black: the outgoing clip fades to black, the incoming up from it.
    DipToBlack,
    /// Dip to white — the same shape as [`Self::DipToBlack`], through white.
    /// Reads as a brighter, faster beat than black, which is why a montage of
    /// daylight footage usually wants it instead.
    DipToWhite,
    /// The incoming clip travels in from the right edge over the held outgoing one.
    SlideLeft,
    /// The incoming clip travels in from the left edge.
    SlideRight,
    /// The incoming clip travels up from the bottom edge.
    SlideUp,
    /// The incoming clip travels down from the top edge.
    SlideDown,
    /// Both clips travel left: the incoming pushes the outgoing out of frame.
    PushLeft,
    /// Both clips travel right.
    PushRight,
    /// Both clips travel up.
    PushUp,
    /// Both clips travel down.
    PushDown,
}

impl TransitionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TransitionKind::Crossfade => "crossfade",
            TransitionKind::DipToBlack => "dip_to_black",
            TransitionKind::DipToWhite => "dip_to_white",
            TransitionKind::SlideLeft => "slide_left",
            TransitionKind::SlideRight => "slide_right",
            TransitionKind::SlideUp => "slide_up",
            TransitionKind::SlideDown => "slide_down",
            TransitionKind::PushLeft => "push_left",
            TransitionKind::PushRight => "push_right",
            TransitionKind::PushUp => "push_up",
            TransitionKind::PushDown => "push_down",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "crossfade" => Some(TransitionKind::Crossfade),
            "dip_to_black" | "diptoblack" => Some(TransitionKind::DipToBlack),
            "dip_to_white" | "diptowhite" => Some(TransitionKind::DipToWhite),
            "slide_left" => Some(TransitionKind::SlideLeft),
            "slide_right" => Some(TransitionKind::SlideRight),
            "slide_up" => Some(TransitionKind::SlideUp),
            "slide_down" => Some(TransitionKind::SlideDown),
            "push_left" => Some(TransitionKind::PushLeft),
            "push_right" => Some(TransitionKind::PushRight),
            "push_up" => Some(TransitionKind::PushUp),
            "push_down" => Some(TransitionKind::PushDown),
            _ => None,
        }
    }

    /// Every kind, in the order a picker should offer them.
    pub const ALL: [TransitionKind; 11] = [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::DipToWhite,
        TransitionKind::SlideLeft,
        TransitionKind::SlideRight,
        TransitionKind::SlideUp,
        TransitionKind::SlideDown,
        TransitionKind::PushLeft,
        TransitionKind::PushRight,
        TransitionKind::PushUp,
        TransitionKind::PushDown,
    ];

    /// Every kind's wire name, quoted and comma-joined — so an error message
    /// listing what was expected cannot drift from the enum.
    pub fn wire_names() -> String {
        Self::ALL
            .iter()
            .map(|k| format!("\"{}\"", k.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The solid colour this transition dips through, if it is a dip.
    pub fn dip_color(self) -> Option<&'static str> {
        match self {
            TransitionKind::DipToBlack => Some("black"),
            TransitionKind::DipToWhite => Some("white"),
            _ => None,
        }
    }

    /// Where the incoming clip starts, as an offset from its final position in
    /// frame widths and heights, for a motion transition. It travels from here
    /// to `(0, 0)` over the transition, so the vector points back along the
    /// direction of travel: a `SlideLeft` starts one full frame to the right.
    pub fn slide_from(self) -> Option<(f64, f64)> {
        match self {
            TransitionKind::SlideLeft | TransitionKind::PushLeft => Some((1.0, 0.0)),
            TransitionKind::SlideRight | TransitionKind::PushRight => Some((-1.0, 0.0)),
            TransitionKind::SlideUp | TransitionKind::PushUp => Some((0.0, 1.0)),
            TransitionKind::SlideDown | TransitionKind::PushDown => Some((0.0, -1.0)),
            _ => None,
        }
    }

    /// True when the outgoing clip is carried out of frame by the incoming one
    /// instead of being covered where it stands.
    pub fn pushes(self) -> bool {
        matches!(
            self,
            TransitionKind::PushLeft | TransitionKind::PushRight | TransitionKind::PushUp | TransitionKind::PushDown
        )
    }

    /// True when both sides play at once — a dissolve or any motion transition.
    /// Such a transition needs the outgoing clip's source handle; a dip does not,
    /// because the two halves happen either side of the cut.
    pub fn overlaps(self) -> bool {
        !matches!(self, TransitionKind::DipToBlack | TransitionKind::DipToWhite)
    }

    /// Human name, for a diff line or a picker label.
    pub fn label(self) -> &'static str {
        match self {
            TransitionKind::Crossfade => "Crossfade",
            TransitionKind::DipToBlack => "Dip to black",
            TransitionKind::DipToWhite => "Dip to white",
            TransitionKind::SlideLeft => "Slide left",
            TransitionKind::SlideRight => "Slide right",
            TransitionKind::SlideUp => "Slide up",
            TransitionKind::SlideDown => "Slide down",
            TransitionKind::PushLeft => "Push left",
            TransitionKind::PushRight => "Push right",
            TransitionKind::PushUp => "Push up",
            TransitionKind::PushDown => "Push down",
        }
    }
}

/// A transition blending the **start** of a clip with the clip that precedes it
/// on the same track. Realized at export.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
    /// Short name, for listing a chain in a diff or a log line.
    pub fn name(&self) -> &'static str {
        match self {
            VideoEffect::Blur { .. } => "blur",
            VideoEffect::Sharpen { .. } => "sharpen",
            VideoEffect::Grayscale => "grayscale",
            VideoEffect::Invert => "invert",
            VideoEffect::Vignette => "vignette",
            VideoEffect::ChromaKey { .. } => "chroma key",
        }
    }

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

impl AudioEffect {
    /// Short name, for listing a chain in a diff or a log line.
    pub fn name(&self) -> &'static str {
        match self {
            AudioEffect::Highpass { .. } => "highpass",
            AudioEffect::Lowpass { .. } => "lowpass",
            AudioEffect::Equalizer { .. } => "EQ",
            AudioEffect::Compressor { .. } => "compressor",
            AudioEffect::Gate { .. } => "gate",
        }
    }
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Written by [`Timeline::captions`] rather than by hand. Regenerating
    /// captions replaces these and leaves everything else alone, so re-running
    /// after a trim does not stack a second set on top of the first — and does
    /// not throw away the title the editor typed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub generated: bool,
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
            generated: false,
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

/// Shortest a generated caption line is allowed to stay on screen (seconds).
/// Splitting a fast sentence strictly by character share can hand a two-letter
/// chunk a couple of frames, which reads as a flicker rather than as a word, so
/// chunks below this are merged back into a neighbour instead.
pub const MIN_CAPTION: f64 = 0.45;

/// How much of a caption line has to survive a cut for it to be kept (seconds).
/// A line whose words were trimmed away leaves a sliver of overlap at the clip
/// edge; showing it would caption footage that is no longer there.
pub const MIN_CAPTION_VISIBLE: f64 = 0.15;

/// The same two floors for [`CaptionStyle::WordPunch`], where a line *is* one
/// word. Held to [`MIN_CAPTION`] every short word would merge into a neighbour
/// and the style would collapse back into [`CaptionStyle::Lines`]; words still
/// merge — a one-letter word's character share is a couple of frames — just far
/// later.
pub const MIN_WORD_CAPTION: f64 = 0.12;
pub const MIN_WORD_VISIBLE: f64 = 0.06;

/// The shape a generated caption set takes on screen.
///
/// Two, because they are consumed differently. A subtitle line is *read*: it
/// holds still long enough to take several words in at once. The one-word form
/// is *watched* — each word lands on the beat of the speech, which is the look
/// social captions have converged on and most of why a muted feed video holds
/// attention. It is not a font choice: the word count, the size, the position
/// and the floors that stop a line flickering all move together, so it is one
/// decision rather than four.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptionStyle {
    /// A few words at a time, held as a subtitle line low in the frame.
    #[default]
    Lines,
    /// One word at a time, large and bold, cut in and out on the word.
    WordPunch,
}

impl CaptionStyle {
    /// The style's own numbers, before any per-call override.
    fn layout(self) -> CaptionLayout {
        match self {
            Self::Lines => CaptionLayout {
                max_words: 4,
                max_chars: 28,
                pos_y: 0.88,
                size: 0.05,
                bold: false,
                min_line: MIN_CAPTION,
                min_visible: MIN_CAPTION_VISIBLE,
            },
            Self::WordPunch => CaptionLayout {
                max_words: 1,
                max_chars: 28,
                // Higher and much larger than a subtitle: one word carries the
                // whole frame, and sitting it on the bottom edge would put it
                // under the platform's own caption rail.
                pos_y: 0.72,
                size: 0.11,
                bold: true,
                min_line: MIN_WORD_CAPTION,
                min_visible: MIN_WORD_VISIBLE,
            },
        }
    }
}

/// A [`CaptionStyle`]'s numbers with any per-call override applied — what
/// [`Timeline::captions`] actually works from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptionLayout {
    /// Most words on one caption line.
    pub max_words: usize,
    /// Most characters on one caption line; the tighter of the two limits wins.
    pub max_chars: usize,
    /// Vertical position as a fraction of frame height.
    pub pos_y: f64,
    /// Font height as a fraction of frame height.
    pub size: f64,
    /// Whether the text is drawn bold.
    pub bold: bool,
    /// Shortest a line may be before it merges into a neighbour.
    pub min_line: f64,
    /// Shortest a line clipped by a cut may be before it is dropped.
    pub min_visible: f64,
}

/// How a transcript is turned into on-screen captions. Everything but the style
/// is an *override*: omit a field and it follows the style, so asking for
/// [`CaptionStyle::WordPunch`] on its own gets the whole look rather than one
/// word left at subtitle size in the subtitle position.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptionOptions {
    /// The look; defaults to [`CaptionStyle::Lines`].
    #[serde(default)]
    pub style: CaptionStyle,
    /// Most words on one caption line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_words: Option<usize>,
    /// Most characters on one caption line; the tighter of the two limits wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
    /// Vertical position as a fraction of frame height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos_y: Option<f64>,
    /// Font height as a fraction of frame height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f64>,
}

impl CaptionOptions {
    /// A style with no overrides.
    pub fn styled(style: CaptionStyle) -> Self {
        Self {
            style,
            ..Self::default()
        }
    }

    /// The numbers to caption with: the style's, with any override that is
    /// actually usable applied over them.
    pub fn resolve(self) -> CaptionLayout {
        let base = self.style.layout();
        CaptionLayout {
            max_words: self.max_words.map_or(base.max_words, |v| v.max(1)),
            max_chars: self.max_chars.map_or(base.max_chars, |v| v.max(1)),
            pos_y: overridden(self.pos_y, base.pos_y, 0.0, 1.0),
            size: overridden(self.size, base.size, 0.005, 0.5),
            ..base
        }
    }
}

/// Roughly how wide one character is as a fraction of the font size, measured
/// off `drawtext`'s default face. Real caption text runs 0.44–0.75 depending on
/// the word; 0.6 sits above the 0.52–0.55 that *long* text averages, and long
/// text is the only kind that ever reaches the cap.
const CHAR_ADVANCE: f64 = 0.6;

/// How much of the frame width a caption may take.
const CAPTION_WIDTH: f64 = 0.9;

/// The frame captions assume when the project has not picked one. A timeline
/// cannot see its assets, so it cannot derive the footage default `export_format`
/// would use — and 16:9 is wide enough that the fit below never binds, which is
/// what keeps an unframed project captioned exactly as it was before.
const DEFAULT_CAPTION_ASPECT: f64 = 16.0 / 9.0;

/// Shrink a caption's size (a fraction of frame height) until its text fits
/// across a frame of `aspect` (width / height).
///
/// `drawtext` neither wraps nor scales: text wider than the frame is simply
/// drawn off both edges. A 9:16 frame is barely half as wide as it is tall, so
/// the social shape this whole feature is for is exactly where a long word runs
/// off — and `fontsize` cannot be an expression over `text_w`, since the width
/// is what depends on the size. So the fit is estimated here from the character
/// count, which is the only measurement available before the filter runs.
fn fit_size(text: &str, size: f64, aspect: f64) -> f64 {
    let chars = text.chars().count().max(1) as f64;
    size.min(CAPTION_WIDTH * aspect / (chars * CHAR_ADVANCE))
}

/// Apply an optional override, ignoring one that is not a finite number and
/// clamping the rest into range.
fn overridden(v: Option<f64>, base: f64, lo: f64, hi: f64) -> f64 {
    match v {
        Some(v) if v.is_finite() => v.clamp(lo, hi),
        _ => base,
    }
}

/// Break a transcript line into caption-sized groups of words. Greedy: take
/// words until either limit would be exceeded, always at least one (a single
/// word longer than `max_chars` is its own line rather than being cut in half).
fn chunk_words(text: &str, layout: CaptionLayout) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut words = 0usize;
    for word in text.split_whitespace() {
        let extra = if current.is_empty() {
            word.chars().count()
        } else {
            word.chars().count() + 1
        };
        let fits = words < layout.max_words && current.chars().count() + extra <= layout.max_chars;
        if !current.is_empty() && !fits {
            out.push(std::mem::take(&mut current));
            words = 0;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        words += 1;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Spread `span` across `chunks` in proportion to how much text each carries,
/// then merge away any line too short to read. Character share is the honest
/// approximation available here: neither speech backend reports word timings
/// (`TranscriptSegment` has only a start and an end), so within a segment the
/// speaker is assumed to be at a steady pace.
fn time_chunks(chunks: Vec<String>, span: TimeRange, min: f64) -> Vec<(TimeRange, String)> {
    let mut chunks = chunks;
    let duration = (span.end - span.start).max(0.0);
    loop {
        let weights: Vec<f64> = chunks.iter().map(|c| c.chars().count().max(1) as f64).collect();
        let total: f64 = weights.iter().sum();
        let mut timed: Vec<(TimeRange, String)> = Vec::with_capacity(chunks.len());
        let mut at = span.start;
        for (i, text) in chunks.iter().enumerate() {
            let share = if total > 0.0 { weights[i] / total } else { 1.0 };
            let end = if i + 1 == chunks.len() {
                span.end
            } else {
                at + duration * share
            };
            timed.push((TimeRange { start: at, end }, text.clone()));
            at = end;
        }
        // A whole segment shorter than `min` is one line, not a merge loop.
        if chunks.len() < 2 {
            return timed;
        }
        let short = timed.iter().position(|(r, _)| r.end - r.start < min);
        let Some(i) = short else { return timed };
        // Merge into the shorter neighbour so the joined line stays as close to
        // the requested width as the timing allows.
        let merge_back = i > 0 && (i + 1 == chunks.len() || chunks[i - 1].chars().count() <= chunks[i + 1].chars().count());
        let into = if merge_back { i - 1 } else { i };
        let moved = chunks.remove(into + 1);
        chunks[into] = format!("{}{}{}", chunks[into], ' ', moved);
    }
}

impl Timeline {
    /// Caption overlays for the cut as it currently stands.
    ///
    /// The point of doing this over the timeline rather than over an asset: a
    /// transcript is in **source** time, an overlay is in **timeline** time, and
    /// between them sit every trim, every reorder, every speed change and every
    /// silence the editor removed. Each segment is projected through the clips
    /// that actually show its footage ([`Clip::source_span_to_timeline`]), so
    /// captions land on the words that survived the cut — and words that did not
    /// survive get no caption at all.
    ///
    /// Reads through [`Timeline::for_render`], so a muted track and a disabled
    /// clip are as uncaptioned as they are unheard.
    pub fn captions(&self, transcripts: &HashMap<Uuid, Vec<TranscriptSegment>>, opts: CaptionOptions) -> Vec<TextOverlay> {
        let layout = opts.resolve();
        let aspect = self
            .format
            .map_or(DEFAULT_CAPTION_ASPECT, |d| f64::from(d.width) / f64::from(d.height));
        let rendered = self.for_render();
        let mut lines: Vec<(TimeRange, String)> = Vec::new();
        for track in &rendered.tracks {
            for clip in &track.clips {
                let Some(segments) = transcripts.get(&clip.asset_id) else {
                    continue;
                };
                let (visible_start, visible_end) = (clip.timeline_start, clip.timeline_end());
                for seg in segments {
                    let text = seg.text.trim();
                    if text.is_empty() || seg.end <= seg.start || !clip.covers_source(seg.start, seg.end) {
                        continue;
                    }
                    // Chunk over the segment's *whole* projected span, then clip
                    // each line to the clip — so a sentence cut in half captions
                    // only the half that is still in the cut.
                    let span = clip.source_span_to_timeline(seg.start, seg.end);
                    for (range, chunk) in time_chunks(chunk_words(text, layout), span, layout.min_line) {
                        let start = range.start.max(visible_start);
                        let end = range.end.min(visible_end);
                        if end - start < layout.min_visible {
                            continue;
                        }
                        lines.push((TimeRange { start, end }, chunk));
                    }
                }
            }
        }
        lines.sort_by(|a, b| a.0.start.total_cmp(&b.0.start).then_with(|| a.1.cmp(&b.1)));
        // The same words can reach two clips — `extract_audio` leaves the picture
        // and its detached audio both referencing the asset — and drawing one
        // caption twice is drawing it bolder, not twice.
        lines.dedup_by(|a, b| a.1 == b.1 && (a.0.start - b.0.start).abs() < 1e-3);
        // Captions are one lane of text at one screen position, so two at once is
        // two unreadable ones. The same footage reaching the cut twice — a
        // callback shot, or a full source parked under the edit — otherwise
        // collides with whatever is already on screen. First line in wins the
        // slot; the next starts where it ends, or is dropped if nothing readable
        // is left of it.
        let mut placed: Vec<(TimeRange, String)> = Vec::with_capacity(lines.len());
        for (range, text) in lines {
            let start = placed
                .last()
                .map_or(range.start, |(prev, _): &(TimeRange, String)| range.start.max(prev.end));
            if range.end - start < layout.min_visible {
                continue;
            }
            placed.push((TimeRange { start, end: range.end }, text));
        }
        placed
            .into_iter()
            .map(|(range, text)| {
                let size = fit_size(&text, layout.size, aspect);
                let mut o = TextOverlay::new(text, range.start.max(0.0), range.end);
                o.pos_y = layout.pos_y;
                o.size = size;
                o.bold = layout.bold;
                o.bg = Some("black@0.5".to_string());
                o.generated = true;
                o
            })
            .collect()
    }
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

    /// Where a source timestamp of this clip lands on the timeline (seconds).
    /// Honors speed and reverse, so an analysis marker maps to the moment it is
    /// actually heard.
    pub fn source_to_timeline(&self, source: f64) -> f64 {
        let offset = if self.is_reversed() {
            self.source_out - source
        } else {
            source - self.source_in
        };
        self.timeline_start + offset / self.speed_mag()
    }

    /// True when any part of the source span `[from, to)` is inside this clip's
    /// source window — i.e. whether this clip actually shows that footage.
    pub fn covers_source(&self, from: f64, to: f64) -> bool {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        hi.min(self.source_out) > lo.max(self.source_in)
    }

    /// Where a source span lands on the timeline, as an ordered range. The ends
    /// are mapped through [`Clip::source_to_timeline`] and **not** clamped to the
    /// clip, so a span that starts before the in-point maps to a time before
    /// `timeline_start` — the caller decides what to do with the part that was
    /// trimmed away. A reversed clip swaps the ends, which is why the result is
    /// ordered rather than built from `from`/`to` directly.
    pub fn source_span_to_timeline(&self, from: f64, to: f64) -> TimeRange {
        let a = self.source_to_timeline(from);
        let b = self.source_to_timeline(to);
        TimeRange {
            start: a.min(b),
            end: a.max(b),
        }
    }
}

/// Tempo estimates below this confidence are ignored when building a beat grid
/// — the same gate the timeline ruler uses to decide whether to draw beat ticks.
pub const BEAT_MIN_CONFIDENCE: f64 = 0.25;

/// Shortest clip a beat alignment may leave behind (seconds).
pub const MIN_BEAT_CLIP: f64 = 0.05;

/// The beat nearest `time` within `tolerance`, from an ascending beat grid.
pub fn nearest_beat(beats: &[f64], time: f64, tolerance: f64) -> Option<f64> {
    if tolerance <= 0.0 {
        return None;
    }
    let after = beats.partition_point(|b| *b < time);
    beats[after.saturating_sub(1)..beats.len().min(after + 1)]
        .iter()
        .copied()
        .filter(|b| (b - time).abs() <= tolerance)
        .min_by(|a, b| (a - time).abs().total_cmp(&(b - time).abs()))
}

/// Half the median beat interval — the widest tolerance that still has a single
/// answer, so every cut moves to the beat it is already closest to.
pub fn default_beat_tolerance(beats: &[f64]) -> f64 {
    let mut gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    if gaps.is_empty() {
        return 0.0;
    }
    gaps.sort_by(f64::total_cmp);
    gaps[gaps.len() / 2] / 2.0
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

    /// Ripple every cut on this track onto the nearest beat within `tolerance`.
    /// Each clip is retrimmed at its outgoing edge (so the cut lands on a beat)
    /// and the rest of the track follows, keeping the original gaps — a gap's
    /// own incoming cut snaps too. `source_limit` caps each asset's source time
    /// (its duration; `INFINITY` for a still, which loops), so a clip is only
    /// stretched as far as it has footage. Returns how many cuts moved.
    pub fn align_cuts_to_beats(&mut self, beats: &[f64], tolerance: f64, source_limit: &HashMap<Uuid, f64>) -> usize {
        self.sort_by_start();
        let mut moved = 0;
        let mut cursor = 0.0; // end of the previous clip after alignment
        let mut previous_end = 0.0; // ...and where it ended before
        for clip in &mut self.clips {
            let gap = (clip.timeline_start - previous_end).max(0.0);
            previous_end = clip.timeline_end();

            let mut start = cursor + gap;
            if gap > 1e-6 {
                if let Some(beat) = nearest_beat(beats, start, tolerance) {
                    let snapped = beat.max(cursor);
                    if (snapped - start).abs() > 1e-6 {
                        moved += 1;
                        start = snapped;
                    }
                }
            }

            let speed = clip.speed_mag();
            let mut duration = clip.duration();
            if let Some(beat) = nearest_beat(beats, start + duration, tolerance) {
                let limit = source_limit.get(&clip.asset_id).copied().unwrap_or(f64::INFINITY);
                let source_left = if clip.is_reversed() {
                    clip.source_out
                } else {
                    (limit - clip.source_in).max(0.0)
                };
                let available = source_left / speed;
                let wanted = (beat - start).clamp(MIN_BEAT_CLIP, available.max(MIN_BEAT_CLIP));
                if (wanted - duration).abs() > 1e-6 {
                    moved += 1;
                    duration = wanted;
                }
            }

            clip.timeline_start = start;
            if clip.is_reversed() {
                clip.source_in = (clip.source_out - duration * speed).max(0.0);
            } else {
                clip.source_out = clip.source_in + duration * speed;
            }
            cursor = clip.timeline_end();
        }
        moved
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

/// A batch of edits held back from the live timeline for the user to review.
///
/// This is what makes an agent safe to leave running on someone's cut: while a
/// staging session is open, every agent edit lands here instead of on the
/// timeline the user is looking at, and nothing moves under them until they
/// accept it. The user's own edits are unaffected and keep going straight to the
/// live timeline — which is what `stale` reports, since a proposal branched from
/// a cut that has since moved on would replace that newer work rather than build
/// on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedEdit {
    /// History seq the proposal was branched from.
    pub base_seq: i64,
    /// The task the agent was working when staging began, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<Uuid>,
    /// The agent's own description of what it is proposing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Labels of the individual edits, in the order they were staged.
    pub edits: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The live timeline has moved on since `base_seq`.
    pub stale: bool,
    /// What applying it would do to the cut.
    pub diff: TimelineDiff,
}

/// A named point on the timeline. Purely an annotation — it renders nothing —
/// but it gives the user and the agent a shared vocabulary for places in the
/// cut ("the laugh at 01:12"), which timestamps alone do not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: Uuid,
    /// Position on the timeline, seconds.
    pub time: f64,
    pub name: String,
    /// Optional CSS color for the ruler chip; the UI picks a default when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// How a clip's picture is fitted to an output frame of a different shape.
///
/// This is what makes a vertical or square delivery usable. `Contain` letterboxes,
/// which is right when the delivery matches the footage and wrong the moment it
/// doesn't: a 16:9 shot rendered at 1080x1920 becomes a 1080x608 strip in a
/// mostly-black frame — technically the whole picture, and not something anyone
/// would post. `Cover` fills the frame and throws away the overflow instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Scale to fit inside the frame and pad the remainder with black. The
    /// default, and the historical behaviour.
    #[default]
    Contain,
    /// Scale to cover the frame and crop what hangs over the edges.
    Cover,
}

impl Fit {
    pub fn as_str(self) -> &'static str {
        match self {
            Fit::Contain => "contain",
            Fit::Cover => "cover",
        }
    }
}

/// The frame the project is cut *for* — the shape of the thing being delivered.
///
/// Without one, a timeline's shape is whatever the first video clip happens to
/// be, and a vertical delivery exists only as a resolution typed into the export
/// dialog. That is backwards for short-form work: the 9:16 crop decides which
/// half of every shot survives, so it has to be visible while the cut is made,
/// not discovered in the rendered file. Setting this makes the preview, the
/// scrubbed still, the streamed playback and the export all render the same
/// frame — an explicit export `resolution` still wins, so nothing about the
/// existing dialog changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Delivery {
    pub width: u32,
    pub height: u32,
    /// How footage of a different shape meets that frame. `Cover` is the useful
    /// default for a reframed delivery; `Contain` keeps the whole picture.
    #[serde(default)]
    pub fit: Fit,
}

impl Delivery {
    /// Even-clamped, and never zero — the dimensions reach a filtergraph.
    pub fn new(width: u32, height: u32, fit: Fit) -> Self {
        let even = |v: u32| v.max(2) & !1;
        Self {
            width: even(width),
            height: even(height),
            fit,
        }
    }

    pub fn aspect(&self) -> f64 {
        self.width as f64 / self.height.max(1) as f64
    }
}

/// A coarse map of where a shot's *content* is: `rows`×`cols` non-negative
/// weights sampled across a source window, row-major.
///
/// Built by [`crate::engine::salience_map`] from a handful of tiny grayscale
/// frames — per cell, the edge energy of the picture plus how much it moved.
/// That combination is what makes it usable on both kinds of shot a social cut
/// is made of: a locked-off talking head has no motion but plenty of facial
/// detail against a soft background, and a handheld follow has both. It is
/// deliberately *not* face detection — no model to ship, no licence to carry,
/// and the answer only has to be good enough to beat a centre crop, which is
/// what the alternative actually is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SalienceMap {
    pub cols: usize,
    pub rows: usize,
    /// `rows * cols` weights, row-major.
    pub cells: Vec<f32>,
}

/// How far the salient window is allowed to pull away from centre before the
/// pull has to be *earned*. Scored against the window's share of total
/// salience, so a flat map (an evenly-lit wide shot, a gradient, black) resolves
/// to the centre crop rather than to whichever edge won by rounding.
const CENTER_BIAS: f64 = 0.25;

/// Candidate window positions evaluated across the cropped axis. The window
/// edges are interpolated within a bucket, so this is finer than `cols`.
const CROP_SEARCH_STEPS: usize = 240;

/// Aspect ratios within this relative tolerance are the same shape — 1920x1080
/// into a 1280x720 frame needs no crop, and neither does 1080x1350 into 4:5.
const ASPECT_TOLERANCE: f64 = 0.01;

/// A crop window as the per-edge source fractions [`Transform`] takes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CropFrame {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    /// How far the window sits from a plain centre crop, as a fraction of the
    /// travel available to it (0.0 = dead centre, 1.0 = hard against an edge).
    /// Reported so a caller can say *why* the shot moved.
    pub offset: f64,
}

impl CropFrame {
    /// The centred crop keeping `keep` of `axis` — what a `Cover` fit does on
    /// its own, and the answer whenever the content gives no reason to move.
    fn centered(keep: f64, horizontal: bool) -> Self {
        Self::at(0.5 * (1.0 - keep), keep, horizontal, 0.0)
    }

    fn at(start: f64, keep: f64, horizontal: bool, offset: f64) -> Self {
        let (near, far) = (start, (1.0 - start - keep).max(0.0));
        if horizontal {
            Self {
                left: near,
                right: far,
                top: 0.0,
                bottom: 0.0,
                offset,
            }
        } else {
            Self {
                left: 0.0,
                right: 0.0,
                top: near,
                bottom: far,
                offset,
            }
        }
    }

    /// Whether this window is (near enough) the plain centre crop.
    pub fn is_centered(&self) -> bool {
        self.offset.abs() < 1e-6
    }
}

/// Whether footage of this shape has to lose part of itself to fill a frame of
/// `target_aspect`. False when the two are the same shape within
/// [`ASPECT_TOLERANCE`] — 1920x1080 into a 1280x720 frame keeps all of itself —
/// and when either shape is nonsense.
pub fn needs_crop(source_w: u32, source_h: u32, target_aspect: f64) -> bool {
    let source_aspect = source_w as f64 / source_h.max(1) as f64;
    if !source_aspect.is_finite() || source_aspect <= 0.0 || !target_aspect.is_finite() || target_aspect <= 0.0 {
        return false;
    }
    ((source_aspect - target_aspect) / target_aspect).abs() > ASPECT_TOLERANCE
}

impl SalienceMap {
    pub fn new(cols: usize, rows: usize, cells: Vec<f32>) -> Self {
        Self { cols, rows, cells }
    }

    fn is_valid(&self) -> bool {
        self.cols > 0 && self.rows > 0 && self.cells.len() == self.cols * self.rows
    }

    /// Salience collapsed onto one axis: per column when `horizontal`, else per
    /// row. Negative weights are clamped away so a bad sample can't subtract.
    fn axis(&self, horizontal: bool) -> Vec<f64> {
        let n = if horizontal { self.cols } else { self.rows };
        let mut out = vec![0.0; n];
        for (i, cell) in self.cells.iter().enumerate() {
            let bucket = if horizontal { i % self.cols } else { i / self.cols };
            out[bucket] += (*cell as f64).max(0.0);
        }
        out
    }

    /// The crop that frames this shot's content for `target_aspect`.
    ///
    /// `None` when the source is already that shape — there is nothing to
    /// choose, and writing a no-op crop into every clip would only be noise in
    /// the inspector. Otherwise the long axis is cropped to the target ratio and
    /// the window is placed where the content is, which is the whole point: a
    /// 16:9 interview with the subject on the left third loses their head to a
    /// centre crop, and that is the default every other path here would take.
    pub fn crop_for(&self, source_w: u32, source_h: u32, target_aspect: f64) -> Option<CropFrame> {
        if !needs_crop(source_w, source_h, target_aspect) {
            return None;
        }
        let source_aspect = source_w as f64 / source_h.max(1) as f64;
        // Wider than the frame → crop the width; taller → crop the height.
        let horizontal = source_aspect > target_aspect;
        let keep = if horizontal {
            target_aspect / source_aspect
        } else {
            source_aspect / target_aspect
        }
        .clamp(0.01, 1.0);

        if !self.is_valid() {
            return Some(CropFrame::centered(keep, horizontal));
        }
        let weights = self.axis(horizontal);
        let total: f64 = weights.iter().sum();
        if total <= 0.0 {
            return Some(CropFrame::centered(keep, horizontal));
        }

        let travel = 1.0 - keep;
        let mut best = (f64::NEG_INFINITY, 0.5 * travel);
        for step in 0..=CROP_SEARCH_STEPS {
            let start = travel * step as f64 / CROP_SEARCH_STEPS as f64;
            let share = window_sum(&weights, start, start + keep) / total;
            let drift = ((start + 0.5 * keep) - 0.5).abs() * 2.0;
            let score = share - CENTER_BIAS * drift;
            if score > best.0 {
                best = (score, start);
            }
        }

        let start = best.1;
        // Report — and store — the exact centre when the search landed on it, so
        // an unmoved shot reads as unmoved instead of as a 0.4% pan.
        let offset = if travel > 1e-9 { (start / travel - 0.5) * 2.0 } else { 0.0 };
        if offset.abs() < 0.02 {
            return Some(CropFrame::centered(keep, horizontal));
        }
        Some(CropFrame::at(start, keep, horizontal, offset))
    }
}

/// Salience between two positions on a 0..1 axis, with the end buckets counted
/// by the fraction of them the window actually covers — so sliding the window
/// by less than a bucket changes the score smoothly instead of in steps.
fn window_sum(weights: &[f64], from: f64, to: f64) -> f64 {
    let n = weights.len() as f64;
    let (from, to) = (from.clamp(0.0, 1.0) * n, to.clamp(0.0, 1.0) * n);
    let mut sum = 0.0;
    for (i, w) in weights.iter().enumerate() {
        let (lo, hi) = (i as f64, i as f64 + 1.0);
        let overlap = to.min(hi) - from.max(lo);
        if overlap > 0.0 {
            sum += w * overlap;
        }
    }
    sum
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
    /// The frame this cut is being made for. `None` (the default, and every
    /// timeline saved before this existed) keeps the historical behaviour:
    /// the shape follows the first video clip's footage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Delivery>,
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
            format: None,
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
            format: self.format,
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
            // A slice is still the same delivery: range export and playback both
            // build from one, and either would otherwise fall back to footage shape.
            format: self.format,
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

// ---- diff ------------------------------------------------------------------

/// Below this, two timeline values are the same value — the timeline round-trips
/// through JSON on every edit, so an exact compare would be honest but noisy.
const DIFF_EPS: f64 = 1e-6;

fn num_changed(a: f64, b: f64) -> bool {
    (a - b).abs() > DIFF_EPS
}

/// `m:ss.d`, the way an editor reads a timeline position.
fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0);
    let m = (s / 60.0).floor();
    format!("{}:{:04.1}", m as i64, s - m * 60.0)
}

fn fmt_delta(secs: f64) -> String {
    if secs >= 0.0 {
        format!("+{secs:.1}s")
    } else {
        format!("{secs:.1}s")
    }
}

/// What one [`DiffEntry`] is about. The UI groups and tints by this; the kinds
/// are deliberately editorial (a *retrim* is a different thing to review than a
/// *move*) rather than one generic "clip changed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    TrackAdded,
    TrackRemoved,
    TrackChanged,
    ClipAdded,
    ClipRemoved,
    ClipMoved,
    ClipRetrimmed,
    ClipChanged,
    OverlayAdded,
    OverlayRemoved,
    OverlayChanged,
    MarkerAdded,
    MarkerRemoved,
    MarkerChanged,
    FormatChanged,
}

impl DiffKind {
    /// Whether this entry adds, removes, or alters something — the three tints a
    /// diff needs.
    pub fn polarity(self) -> &'static str {
        match self {
            DiffKind::TrackAdded | DiffKind::ClipAdded | DiffKind::OverlayAdded | DiffKind::MarkerAdded => "added",
            DiffKind::TrackRemoved | DiffKind::ClipRemoved | DiffKind::OverlayRemoved | DiffKind::MarkerRemoved => "removed",
            _ => "changed",
        }
    }
}

/// One change between two timelines, phrased for a human reviewing a cut.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffEntry {
    pub kind: DiffKind,
    /// One line, e.g. `Trimmed clip on V1 at 0:04.0 — 4.0s → 2.5s (-1.5s)`.
    pub summary: String,
    /// Field-level specifics behind a `*Changed` entry, e.g. `volume 100% → 40%`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_id: Option<Uuid>,
    /// Where on the timeline to look, so the reviewer can jump straight to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
}

impl DiffEntry {
    fn new(kind: DiffKind, summary: String) -> Self {
        Self {
            kind,
            summary,
            detail: None,
            track_id: None,
            clip_id: None,
            at: None,
        }
    }

    fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    fn on_track(mut self, id: Uuid) -> Self {
        self.track_id = Some(id);
        self
    }

    fn on_clip(mut self, id: Uuid) -> Self {
        self.clip_id = Some(id);
        self
    }

    fn at(mut self, time: f64) -> Self {
        self.at = Some(time);
        self
    }
}

/// What a set of edits did to a cut: the individual changes plus the two numbers
/// an editor checks first — how long it is now, and how many clips it has.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineDiff {
    pub entries: Vec<DiffEntry>,
    pub duration_before: f64,
    pub duration_after: f64,
    pub clips_before: usize,
    pub clips_after: usize,
}

impl TimelineDiff {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A single line: how many changes and what they did to the runtime.
    pub fn headline(&self) -> String {
        if self.entries.is_empty() {
            return "No changes".to_string();
        }
        let n = self.entries.len();
        let mut s = format!("{n} change{}", if n == 1 { "" } else { "s" });
        if num_changed(self.duration_before, self.duration_after) {
            s.push_str(&format!(
                " · {} → {} ({})",
                fmt_time(self.duration_before),
                fmt_time(self.duration_after),
                fmt_delta(self.duration_after - self.duration_before)
            ));
        } else {
            s.push_str(&format!(" · {}", fmt_time(self.duration_after)));
        }
        if self.clips_before != self.clips_after {
            s.push_str(&format!(" · {} → {} clips", self.clips_before, self.clips_after));
        }
        s
    }

    /// The headline followed by one line per change — what an agent reads back
    /// and what the review card renders.
    pub fn summary(&self) -> String {
        let mut out = self.headline();
        for e in &self.entries {
            out.push_str("\n  • ");
            out.push_str(&e.summary);
            if let Some(d) = &e.detail {
                out.push_str(" (");
                out.push_str(d);
                out.push(')');
            }
        }
        out
    }
}

fn clip_index(timeline: &Timeline) -> HashMap<Uuid, (&Track, &Clip)> {
    let mut map = HashMap::new();
    for track in &timeline.tracks {
        for clip in &track.clips {
            map.insert(clip.id, (track, clip));
        }
    }
    map
}

fn kind_name(kind: StreamKind) -> &'static str {
    match kind {
        StreamKind::Video => "video",
        StreamKind::Audio => "audio",
        StreamKind::Subtitle => "subtitle",
        StreamKind::Data => "data",
    }
}

fn joined(parts: Vec<String>) -> Option<String> {
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

fn track_changes(before: &Track, after: &Track) -> Option<String> {
    let mut parts = Vec::new();
    if before.name != after.name {
        parts.push(format!("renamed {} → {}", before.name, after.name));
    }
    for (was, is, on, off) in [
        (before.muted, after.muted, "muted", "unmuted"),
        (before.solo, after.solo, "soloed", "unsoloed"),
        (before.locked, after.locked, "locked", "unlocked"),
        (before.duck, after.duck, "ducking on", "ducking off"),
    ] {
        if was != is {
            parts.push((if is { on } else { off }).to_string());
        }
    }
    joined(parts)
}

fn transform_changes(before: &Transform, after: &Transform) -> Vec<String> {
    let mut parts = Vec::new();
    for (label, a, b) in [
        ("scale", before.scale, after.scale),
        ("x", before.pos_x, after.pos_x),
        ("y", before.pos_y, after.pos_y),
        ("rotation", before.rotation, after.rotation),
        ("opacity", before.opacity, after.opacity),
    ] {
        if num_changed(a, b) {
            parts.push(format!("{label} {a:.2} → {b:.2}"));
        }
    }
    if before.has_crop() != after.has_crop()
        || num_changed(before.crop_left, after.crop_left)
        || num_changed(before.crop_right, after.crop_right)
        || num_changed(before.crop_top, after.crop_top)
        || num_changed(before.crop_bottom, after.crop_bottom)
    {
        parts.push(if after.has_crop() { "cropped" } else { "crop cleared" }.to_string());
    }
    parts
}

fn color_changes(before: &Color, after: &Color) -> Vec<String> {
    let mut parts = Vec::new();
    for (label, a, b) in [
        ("brightness", before.brightness, after.brightness),
        ("contrast", before.contrast, after.contrast),
        ("saturation", before.saturation, after.saturation),
        ("gamma", before.gamma, after.gamma),
        ("temperature", before.temperature, after.temperature),
    ] {
        if num_changed(a, b) {
            parts.push(format!("{label} {a:.2} → {b:.2}"));
        }
    }
    parts
}

fn effect_list<T>(effects: &[T], name: impl Fn(&T) -> &'static str) -> String {
    if effects.is_empty() {
        "none".to_string()
    } else {
        effects.iter().map(name).collect::<Vec<_>>().join("+")
    }
}

fn reframe_changes(before: Option<&Reframe>, after: Option<&Reframe>) -> Vec<String> {
    match (before, after) {
        (None, None) => Vec::new(),
        (None, Some(_)) => vec!["reframe added".to_string()],
        (Some(_), None) => vec!["reframe cleared".to_string()],
        (Some(a), Some(b)) => {
            if a == b {
                return Vec::new();
            }
            let mut parts = Vec::new();
            for (label, x, y) in [
                ("yaw", a.yaw, b.yaw),
                ("pitch", a.pitch, b.pitch),
                ("roll", a.roll, b.roll),
                ("fov", a.fov, b.fov),
            ] {
                if num_changed(x, y) {
                    parts.push(format!("{label} {x:.0}° → {y:.0}°"));
                }
            }
            if a.keyframes.len() != b.keyframes.len() {
                parts.push(format!("reframe keyframes {} → {}", a.keyframes.len(), b.keyframes.len()));
            } else if a.keyframes != b.keyframes {
                parts.push("reframe keyframes retimed".to_string());
            }
            if parts.is_empty() {
                parts.push("reframe changed".to_string());
            }
            parts
        }
    }
}

fn clip_changes(before: &Clip, after: &Clip) -> Option<String> {
    let mut parts = Vec::new();
    if before.asset_id != after.asset_id {
        parts.push("different source asset".to_string());
    }
    if (before.volume - after.volume).abs() > DIFF_EPS as f32 {
        parts.push(format!("volume {:.0}% → {:.0}%", before.volume * 100.0, after.volume * 100.0));
    }
    for (label, a, b) in [
        ("fade in", before.fade_in, after.fade_in),
        ("fade out", before.fade_out, after.fade_out),
    ] {
        if num_changed(a, b) {
            parts.push(format!("{label} {a:.2}s → {b:.2}s"));
        }
    }
    if num_changed(before.speed, after.speed) {
        parts.push(format!("speed {:.2}× → {:.2}×", before.speed, after.speed));
    }
    if before.enabled != after.enabled {
        parts.push(if after.enabled { "re-enabled" } else { "disabled" }.to_string());
    }
    parts.extend(transform_changes(&before.transform, &after.transform));
    parts.extend(color_changes(&before.color, &after.color));
    if before.transition_in != after.transition_in {
        parts.push(match &after.transition_in {
            None => "transition removed".to_string(),
            Some(t) => format!("transition {} {:.2}s", t.kind.as_str(), t.duration),
        });
    }
    if before.effects != after.effects {
        parts.push(format!(
            "video effects {} → {}",
            effect_list(&before.effects, VideoEffect::name),
            effect_list(&after.effects, VideoEffect::name)
        ));
    }
    if before.audio != after.audio {
        parts.push(format!(
            "audio effects {} → {}",
            effect_list(&before.audio, AudioEffect::name),
            effect_list(&after.audio, AudioEffect::name)
        ));
    }
    if before.keyframes.len() != after.keyframes.len() {
        parts.push(format!("keyframes {} → {}", before.keyframes.len(), after.keyframes.len()));
    } else if before.keyframes != after.keyframes {
        parts.push("keyframes retimed".to_string());
    }
    parts.extend(reframe_changes(before.reframe.as_ref(), after.reframe.as_ref()));
    joined(parts)
}

fn overlay_changes(before: &TextOverlay, after: &TextOverlay) -> Option<String> {
    let mut parts = Vec::new();
    if before.text != after.text {
        parts.push(format!("text “{}” → “{}”", before.text, after.text));
    }
    if num_changed(before.start, after.start) || num_changed(before.end, after.end) {
        parts.push(format!(
            "timing {}–{} → {}–{}",
            fmt_time(before.start),
            fmt_time(before.end),
            fmt_time(after.start),
            fmt_time(after.end)
        ));
    }
    if num_changed(before.pos_x, after.pos_x) || num_changed(before.pos_y, after.pos_y) {
        parts.push(format!(
            "position {:.2},{:.2} → {:.2},{:.2}",
            before.pos_x, before.pos_y, after.pos_x, after.pos_y
        ));
    }
    if num_changed(before.size, after.size) {
        parts.push(format!("size {:.3} → {:.3}", before.size, after.size));
    }
    if before.color != after.color {
        parts.push(format!("color {} → {}", before.color, after.color));
    }
    if before.bg != after.bg {
        parts.push("box changed".to_string());
    }
    if before.font != after.font {
        parts.push("font changed".to_string());
    }
    if before.bold != after.bold {
        parts.push(if after.bold { "bold" } else { "not bold" }.to_string());
    }
    if before.keyframes != after.keyframes {
        parts.push(format!("keyframes {} → {}", before.keyframes.len(), after.keyframes.len()));
    }
    joined(parts)
}

fn fmt_delivery(d: &Delivery) -> String {
    format!("{}x{} ({})", d.width, d.height, d.fit.as_str())
}

impl Timeline {
    /// What changed between this timeline and `after`, phrased for a human
    /// reviewing a cut.
    ///
    /// Everything is matched by id — clips keep theirs across a move, a trim and
    /// a retime — so a reordered track reads as the handful of moves it is
    /// rather than as every clip having been replaced. Pure, and the single
    /// source of truth behind both the staged-edit review card and
    /// [`crate::project::Project::diff_revisions`].
    pub fn diff(&self, after: &Timeline) -> TimelineDiff {
        let before_clips = clip_index(self);
        let after_clips = clip_index(after);

        let mut tracks = Vec::new();
        let mut clips = Vec::new();

        for track in &after.tracks {
            match self.track(track.id) {
                None => tracks.push(
                    DiffEntry::new(
                        DiffKind::TrackAdded,
                        format!("Added {} track {}", kind_name(track.kind), track.name),
                    )
                    .on_track(track.id),
                ),
                Some(before) => {
                    if let Some(detail) = track_changes(before, track) {
                        tracks.push(
                            DiffEntry::new(DiffKind::TrackChanged, format!("Changed track {}", track.name))
                                .detail(detail)
                                .on_track(track.id),
                        );
                    }
                }
            }
        }
        for track in &self.tracks {
            if after.track(track.id).is_none() {
                let n = track.clips.len();
                tracks.push(
                    DiffEntry::new(
                        DiffKind::TrackRemoved,
                        format!(
                            "Removed {} track {} ({n} clip{})",
                            kind_name(track.kind),
                            track.name,
                            if n == 1 { "" } else { "s" }
                        ),
                    )
                    .on_track(track.id),
                );
            }
        }

        // Clips whose whole track went away are already covered by the track
        // entry above; listing each of them again would bury the one change the
        // reviewer actually has to judge.
        for track in &self.tracks {
            if after.track(track.id).is_none() {
                continue;
            }
            for clip in &track.clips {
                if after_clips.contains_key(&clip.id) {
                    continue;
                }
                clips.push(
                    DiffEntry::new(
                        DiffKind::ClipRemoved,
                        format!(
                            "Removed clip from {} at {} ({:.1}s)",
                            track.name,
                            fmt_time(clip.timeline_start),
                            clip.duration()
                        ),
                    )
                    .on_track(track.id)
                    .on_clip(clip.id)
                    .at(clip.timeline_start),
                );
            }
        }

        for track in &after.tracks {
            for clip in &track.clips {
                let Some((before_track, before_clip)) = before_clips.get(&clip.id) else {
                    clips.push(
                        DiffEntry::new(
                            DiffKind::ClipAdded,
                            format!(
                                "Added clip to {} at {} ({:.1}s)",
                                track.name,
                                fmt_time(clip.timeline_start),
                                clip.duration()
                            ),
                        )
                        .on_track(track.id)
                        .on_clip(clip.id)
                        .at(clip.timeline_start),
                    );
                    continue;
                };
                if before_track.id != track.id {
                    clips.push(
                        DiffEntry::new(
                            DiffKind::ClipMoved,
                            format!(
                                "Moved clip from {} to {} at {}",
                                before_track.name,
                                track.name,
                                fmt_time(clip.timeline_start)
                            ),
                        )
                        .on_track(track.id)
                        .on_clip(clip.id)
                        .at(clip.timeline_start),
                    );
                } else if num_changed(before_clip.timeline_start, clip.timeline_start) {
                    clips.push(
                        DiffEntry::new(
                            DiffKind::ClipMoved,
                            format!(
                                "Moved clip on {} — {} → {}",
                                track.name,
                                fmt_time(before_clip.timeline_start),
                                fmt_time(clip.timeline_start)
                            ),
                        )
                        .on_track(track.id)
                        .on_clip(clip.id)
                        .at(clip.timeline_start),
                    );
                }
                if num_changed(before_clip.source_in, clip.source_in) || num_changed(before_clip.source_out, clip.source_out) {
                    let (was, is) = (before_clip.source_duration(), clip.source_duration());
                    clips.push(
                        DiffEntry::new(
                            DiffKind::ClipRetrimmed,
                            format!(
                                "Trimmed clip on {} at {} — {was:.1}s → {is:.1}s ({})",
                                track.name,
                                fmt_time(clip.timeline_start),
                                fmt_delta(is - was)
                            ),
                        )
                        .on_track(track.id)
                        .on_clip(clip.id)
                        .at(clip.timeline_start),
                    );
                }
                if let Some(detail) = clip_changes(before_clip, clip) {
                    clips.push(
                        DiffEntry::new(
                            DiffKind::ClipChanged,
                            format!("Adjusted clip on {} at {}", track.name, fmt_time(clip.timeline_start)),
                        )
                        .detail(detail)
                        .on_track(track.id)
                        .on_clip(clip.id)
                        .at(clip.timeline_start),
                    );
                }
            }
        }

        let mut rest = Vec::new();
        for overlay in &after.overlays {
            match self.overlay(overlay.id) {
                None => rest.push(
                    DiffEntry::new(
                        DiffKind::OverlayAdded,
                        format!(
                            "Added text “{}” at {}–{}",
                            overlay.text,
                            fmt_time(overlay.start),
                            fmt_time(overlay.end)
                        ),
                    )
                    .at(overlay.start),
                ),
                Some(before) => {
                    if let Some(detail) = overlay_changes(before, overlay) {
                        rest.push(
                            DiffEntry::new(DiffKind::OverlayChanged, format!("Changed text “{}”", overlay.text))
                                .detail(detail)
                                .at(overlay.start),
                        );
                    }
                }
            }
        }
        for overlay in &self.overlays {
            if after.overlay(overlay.id).is_none() {
                rest.push(
                    DiffEntry::new(
                        DiffKind::OverlayRemoved,
                        format!("Removed text “{}” at {}", overlay.text, fmt_time(overlay.start)),
                    )
                    .at(overlay.start),
                );
            }
        }

        let before_markers: HashMap<Uuid, &Marker> = self.markers.iter().map(|m| (m.id, m)).collect();
        for marker in &after.markers {
            match before_markers.get(&marker.id) {
                None => rest.push(
                    DiffEntry::new(
                        DiffKind::MarkerAdded,
                        format!("Added marker “{}” at {}", marker.name, fmt_time(marker.time)),
                    )
                    .at(marker.time),
                ),
                Some(before) => {
                    let mut parts = Vec::new();
                    if before.name != marker.name {
                        parts.push(format!("renamed “{}” → “{}”", before.name, marker.name));
                    }
                    if num_changed(before.time, marker.time) {
                        parts.push(format!("moved {} → {}", fmt_time(before.time), fmt_time(marker.time)));
                    }
                    if before.color != marker.color {
                        parts.push("recolored".to_string());
                    }
                    if let Some(detail) = joined(parts) {
                        rest.push(
                            DiffEntry::new(DiffKind::MarkerChanged, format!("Changed marker “{}”", marker.name))
                                .detail(detail)
                                .at(marker.time),
                        );
                    }
                }
            }
        }
        for marker in &self.markers {
            if !after.markers.iter().any(|m| m.id == marker.id) {
                rest.push(
                    DiffEntry::new(
                        DiffKind::MarkerRemoved,
                        format!("Removed marker “{}” at {}", marker.name, fmt_time(marker.time)),
                    )
                    .at(marker.time),
                );
            }
        }

        if self.format != after.format {
            let summary = match (&self.format, &after.format) {
                (None, Some(d)) => format!("Set the delivery frame to {}", fmt_delivery(d)),
                (Some(d), None) => format!("Cleared the delivery frame (was {})", fmt_delivery(d)),
                (Some(a), Some(b)) => format!("Delivery frame {} → {}", fmt_delivery(a), fmt_delivery(b)),
                (None, None) => unreachable!(),
            };
            rest.push(DiffEntry::new(DiffKind::FormatChanged, summary));
        }

        tracks.append(&mut clips);
        tracks.append(&mut rest);
        TimelineDiff {
            entries: tracks,
            duration_before: self.duration(),
            duration_after: after.duration(),
            clips_before: before_clips.len(),
            clips_after: after_clips.len(),
        }
    }
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
    fn beat_grid_maps_source_beats_onto_the_timeline() {
        // A music clip cut from 2s into the source and placed at 10s: a beat at
        // source 3.0 is heard at 11.0, and beats outside the window never sound.
        let asset = Uuid::new_v4();
        let clip = Clip::new(asset, 2.0, 6.0, 10.0);
        let tl = Timeline {
            tracks: vec![track(StreamKind::Audio, "A1", vec![clip])],
            ..Timeline::new()
        };
        let tempos = HashMap::from([(
            asset,
            Tempo {
                bpm: 120.0,
                beats: vec![0.0, 1.0, 3.0, 5.0, 8.0],
                confidence: 0.5,
            },
        )]);
        assert_eq!(tl.beat_grid(&tempos), vec![11.0, 13.0]);
    }

    #[test]
    fn beat_grid_ignores_a_low_confidence_tempo() {
        let asset = Uuid::new_v4();
        let tl = Timeline {
            tracks: vec![track(StreamKind::Audio, "A1", vec![Clip::new(asset, 0.0, 4.0, 0.0)])],
            ..Timeline::new()
        };
        let tempos = HashMap::from([(
            asset,
            Tempo {
                bpm: 120.0,
                beats: vec![0.5, 1.0],
                confidence: BEAT_MIN_CONFIDENCE - 0.01,
            },
        )]);
        assert!(tl.beat_grid(&tempos).is_empty());
    }

    #[test]
    fn nearest_beat_takes_the_closest_within_tolerance() {
        let beats = [0.0, 0.5, 1.0, 1.5];
        assert_eq!(nearest_beat(&beats, 0.6, 0.25), Some(0.5));
        assert_eq!(nearest_beat(&beats, 0.9, 0.25), Some(1.0));
        assert_eq!(nearest_beat(&beats, 0.75, 0.1), None);
        assert_eq!(nearest_beat(&beats, 9.0, 0.25), None);
        assert_eq!(default_beat_tolerance(&beats), 0.25);
    }

    #[test]
    fn align_cuts_to_beats_ripples_every_cut_onto_the_grid() {
        let asset = Uuid::new_v4();
        let beats: Vec<f64> = (0..=20).map(|i| i as f64 * 0.5).collect();
        let limits = HashMap::from([(asset, 10.0)]);
        let mut t = track(
            StreamKind::Video,
            "V1",
            vec![Clip::new(asset, 0.0, 1.1, 0.0), Clip::new(asset, 4.0, 4.9, 1.1)],
        );

        assert_eq!(t.align_cuts_to_beats(&beats, 0.25, &limits), 2);
        assert_eq!(t.clips[0].timeline_start, 0.0);
        assert_eq!(t.clips[0].source_out, 1.0, "the first cut moved back onto the beat");
        assert_eq!(t.clips[1].timeline_start, 1.0, "the next clip rippled with it");
        assert_eq!(t.clips[1].timeline_end(), 2.0, "and its own cut landed on a beat too");
        assert_eq!(t.clips[1].source_in, 4.0, "trimming happens at the outgoing edge");

        // Already aligned: nothing moves, so running it twice is a no-op.
        assert_eq!(t.align_cuts_to_beats(&beats, 0.25, &limits), 0);
    }

    #[test]
    fn align_cuts_to_beats_keeps_gaps_and_respects_the_source() {
        let asset = Uuid::new_v4();
        let beats: Vec<f64> = (0..=20).map(|i| i as f64 * 0.5).collect();
        // Only 1.2s of footage left after source_in, so the clip cannot stretch
        // to the 1.5 beat its end is nearest.
        let limits = HashMap::from([(asset, 1.2)]);
        let mut t = track(
            StreamKind::Video,
            "V1",
            vec![Clip::new(asset, 0.0, 0.4, 0.0), Clip::new(asset, 0.0, 1.4, 0.9)],
        );

        t.align_cuts_to_beats(&beats, 0.25, &limits);
        assert_eq!(t.clips[0].timeline_end(), 0.5);
        assert_eq!(t.clips[1].timeline_start, 1.0, "the 0.5s gap survived, snapped to a beat");
        assert_eq!(t.clips[1].source_out, 1.2, "stretched only as far as there is footage");
    }

    #[test]
    fn align_cuts_to_beats_trims_a_reversed_clip_at_its_outgoing_edge() {
        // Played backwards the timeline tail is the *start* of the source, so
        // shortening the clip must move source_in, not source_out.
        let asset = Uuid::new_v4();
        let beats: Vec<f64> = (0..=20).map(|i| i as f64 * 0.5).collect();
        let mut clip = Clip::new(asset, 1.0, 2.1, 0.0);
        clip.speed = -1.0;
        let mut t = track(StreamKind::Video, "V1", vec![clip]);

        t.align_cuts_to_beats(&beats, 0.25, &HashMap::from([(asset, 10.0)]));
        assert_eq!(t.clips[0].source_out, 2.1);
        assert!((t.clips[0].source_in - 1.1).abs() < 1e-9);
        assert!((t.clips[0].timeline_end() - 1.0).abs() < 1e-9);
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
            format: None,
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
            format: None,
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
            format: None,
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
            format: None,
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
            format: None,
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
            format: None,
        };
        let s = tl.slice(2.0, 6.0);
        assert!(s.tracks[0].muted && s.tracks[0].locked && s.tracks[0].duck);
    }

    #[test]
    fn an_untouched_timeline_diffs_to_nothing() {
        let tl = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0), clip_at(4.0, 3.0)])],
            ..Timeline::new()
        };
        let diff = tl.diff(&tl.clone());
        assert!(diff.is_empty());
        assert_eq!(diff.headline(), "No changes");
        assert_eq!(diff.clips_before, 2);
    }

    #[test]
    fn diff_names_the_move_the_trim_and_the_removal() {
        let tl = Timeline {
            tracks: vec![track(
                StreamKind::Video,
                "V1",
                vec![clip_at(0.0, 4.0), clip_at(4.0, 3.0), clip_at(7.0, 2.0)],
            )],
            ..Timeline::new()
        };
        let mut after = tl.clone();
        {
            let clips = &mut after.tracks[0].clips;
            clips[0].source_out = 2.5; // retrim
            clips[1].timeline_start = 5.0; // move
            clips.remove(2); // cut
        }

        let diff = tl.diff(&after);
        let kinds: Vec<DiffKind> = diff.entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![DiffKind::ClipRemoved, DiffKind::ClipRetrimmed, DiffKind::ClipMoved]
        );
        assert_eq!(diff.clips_before, 3);
        assert_eq!(diff.clips_after, 2);
        // The retrim reports the source window, and the entry carries the clip
        // so the reviewer can jump to it.
        assert!(
            diff.entries[1].summary.contains("4.0s → 2.5s (-1.5s)"),
            "{}",
            diff.entries[1].summary
        );
        assert_eq!(diff.entries[1].clip_id, Some(after.tracks[0].clips[0].id));
        assert!(
            diff.entries[2].summary.contains("0:04.0 → 0:05.0"),
            "{}",
            diff.entries[2].summary
        );
        // The headline leads with what the edit did to the runtime.
        assert_eq!(diff.headline(), "3 changes · 0:09.0 → 0:08.0 (-1.0s) · 3 → 2 clips");
    }

    #[test]
    fn a_removed_track_is_one_change_not_one_per_clip() {
        let tl = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0)]),
                track(StreamKind::Audio, "A1", vec![clip_at(0.0, 4.0), clip_at(4.0, 4.0)]),
            ],
            ..Timeline::new()
        };
        let mut after = tl.clone();
        after.tracks.remove(1);

        let diff = tl.diff(&after);
        assert_eq!(diff.entries.len(), 1);
        assert_eq!(diff.entries[0].kind, DiffKind::TrackRemoved);
        assert!(
            diff.entries[0].summary.contains("A1 (2 clips)"),
            "{}",
            diff.entries[0].summary
        );
    }

    #[test]
    fn a_clip_dragged_to_another_track_reads_as_one_move() {
        let mut tl = Timeline {
            tracks: vec![
                track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0)]),
                track(StreamKind::Video, "V2", vec![]),
            ],
            ..Timeline::new()
        };
        tl.tracks[1].kind = StreamKind::Video;
        let mut after = tl.clone();
        let clip = after.tracks[0].clips.remove(0);
        after.tracks[1].clips.push(clip);

        let diff = tl.diff(&after);
        assert_eq!(diff.entries.len(), 1);
        assert_eq!(diff.entries[0].kind, DiffKind::ClipMoved);
        assert!(
            diff.entries[0].summary.contains("from V1 to V2"),
            "{}",
            diff.entries[0].summary
        );
    }

    #[test]
    fn diff_details_what_changed_on_a_clip() {
        let tl = Timeline {
            tracks: vec![track(StreamKind::Video, "V1", vec![clip_at(0.0, 4.0)])],
            ..Timeline::new()
        };
        let mut after = tl.clone();
        {
            let c = &mut after.tracks[0].clips[0];
            c.volume = 0.4;
            c.speed = 2.0;
            c.effects.push(VideoEffect::Vignette);
            c.enabled = false;
        }

        let diff = tl.diff(&after);
        assert_eq!(diff.entries.len(), 1);
        assert_eq!(diff.entries[0].kind, DiffKind::ClipChanged);
        let detail = diff.entries[0].detail.clone().unwrap();
        assert!(detail.contains("volume 100% → 40%"), "{detail}");
        assert!(detail.contains("speed 1.00× → 2.00×"), "{detail}");
        assert!(detail.contains("disabled"), "{detail}");
        assert!(detail.contains("video effects none → vignette"), "{detail}");
    }

    #[test]
    fn diff_covers_overlays_markers_and_the_delivery_frame() {
        let tl = Timeline::new();
        let mut after = tl.clone();
        after.overlays.push(TextOverlay::new("Hello", 1.0, 3.0));
        after.markers.push(Marker {
            id: Uuid::new_v4(),
            time: 72.0,
            name: "the laugh".to_string(),
            color: None,
        });
        after.format = Some(Delivery::new(1080, 1920, Fit::Cover));

        let diff = tl.diff(&after);
        let kinds: Vec<DiffKind> = diff.entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![DiffKind::OverlayAdded, DiffKind::MarkerAdded, DiffKind::FormatChanged]
        );
        assert!(diff.entries[1].summary.contains("1:12.0"), "{}", diff.entries[1].summary);
        assert!(
            diff.entries[2].summary.contains("1080x1920 (cover)"),
            "{}",
            diff.entries[2].summary
        );
        // Every entry lands in the rendered summary the agent reads back.
        assert_eq!(diff.summary().lines().count(), 4);
    }

    // ---- smart crop ---------------------------------------------------------

    /// A map whose salience sits in one horizontal band of `cols`, so a test can
    /// say "the subject is on the left third" and nothing else.
    fn map_with_column_band(cols: usize, from: usize, to: usize) -> SalienceMap {
        let rows = 4;
        let mut cells = vec![0.01f32; cols * rows];
        for r in 0..rows {
            for c in from..to {
                cells[r * cols + c] = 1.0;
            }
        }
        SalienceMap::new(cols, rows, cells)
    }

    #[test]
    fn a_matching_aspect_needs_no_crop() {
        let map = map_with_column_band(32, 0, 32);
        // 1080x1920 delivered at 9:16, and 1920x1080 at a 1280x720 frame.
        assert!(map.crop_for(1080, 1920, 1080.0 / 1920.0).is_none());
        assert!(map.crop_for(1920, 1080, 1280.0 / 720.0).is_none());
    }

    #[test]
    fn a_vertical_delivery_crops_width_toward_the_subject() {
        // 16:9 footage into a 9:16 frame with the subject in the left third.
        let map = map_with_column_band(48, 4, 16);
        let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
        assert_eq!((crop.top, crop.bottom), (0.0, 0.0));
        // 9:16 of 16:9 keeps 0.3164 of the width; the rest is cut.
        assert!((crop.left + crop.right - (1.0 - 1080.0 * 1080.0 / (1920.0 * 1920.0))).abs() < 1e-6);
        // The kept window contains the band, which a centre crop would miss.
        assert!(crop.left < 4.0 / 48.0 && 1.0 - crop.right > 16.0 / 48.0, "{crop:?}");
        assert!(!crop.is_centered());
        assert!(crop.offset < 0.0, "a left-hand subject pulls the window left");
    }

    #[test]
    fn flat_salience_falls_back_to_the_centre_crop() {
        let map = map_with_column_band(48, 0, 48);
        let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
        assert!((crop.left - crop.right).abs() < 1e-9, "{crop:?}");
        assert!(crop.is_centered());
    }

    #[test]
    fn an_off_centre_subject_still_loses_to_a_hard_pull_it_cannot_earn() {
        // Salience a hair off centre: the centre bias should hold the window put
        // rather than pan for a rounding difference.
        let map = map_with_column_band(48, 23, 27);
        let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
        assert!(crop.is_centered(), "{crop:?}");
    }

    #[test]
    fn a_landscape_delivery_crops_height_of_vertical_footage() {
        // 9:16 footage into 16:9, subject in the top rows.
        let cols = 4;
        let rows = 32;
        let mut cells = vec![0.01f32; cols * rows];
        for r in 2..8 {
            for c in 0..cols {
                cells[r * cols + c] = 1.0;
            }
        }
        let map = SalienceMap::new(cols, rows, cells);
        let crop = map.crop_for(1080, 1920, 1920.0 / 1080.0).expect("crops");
        assert_eq!((crop.left, crop.right), (0.0, 0.0));
        // The search grid is finer than a bucket but not exact, so allow the top
        // edge to land a hair inside the band it is framing.
        assert!(crop.top <= 2.0 / 32.0 + 0.01 && 1.0 - crop.bottom > 8.0 / 32.0, "{crop:?}");
    }

    #[test]
    fn a_map_with_nothing_in_it_still_yields_the_centre_crop() {
        for map in [
            SalienceMap::default(),
            SalienceMap::new(8, 2, vec![0.0; 16]),
            SalienceMap::new(8, 2, vec![0.0; 3]),
        ] {
            let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
            assert!(crop.is_centered(), "{crop:?}");
            assert!((crop.left - crop.right).abs() < 1e-9);
        }
    }

    #[test]
    fn a_degenerate_aspect_is_refused_rather_than_guessed() {
        let map = map_with_column_band(8, 0, 4);
        assert!(map.crop_for(1920, 1080, 0.0).is_none());
        assert!(map.crop_for(1920, 1080, f64::NAN).is_none());
        assert!(map.crop_for(0, 1080, 1.0).is_none());
    }

    fn seg(start: f64, end: f64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            start,
            end,
            text: text.to_string(),
        }
    }

    fn captioned(timeline: &Timeline, asset: Uuid, segments: Vec<TranscriptSegment>) -> Vec<(String, f64, f64)> {
        let mut map = HashMap::new();
        map.insert(asset, segments);
        timeline
            .captions(&map, CaptionOptions::default())
            .into_iter()
            .map(|o| (o.text, (o.start * 100.0).round() / 100.0, (o.end * 100.0).round() / 100.0))
            .collect()
    }

    fn one_clip(clip: Clip) -> Timeline {
        let mut track = Track::new(StreamKind::Video, "V1");
        track.clips = vec![clip];
        Timeline {
            tracks: vec![track],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
        }
    }

    #[test]
    fn source_span_maps_through_trim_speed_and_reverse() {
        let asset = Uuid::new_v4();
        // Trimmed: the asset's 10s starts at the clip's in-point, placed at 4s.
        let mut clip = Clip::new(asset, 10.0, 20.0, 4.0);
        let r = clip.source_span_to_timeline(12.0, 14.0);
        assert!((r.start - 6.0).abs() < 1e-9, "{r:?}");
        assert!((r.end - 8.0).abs() < 1e-9, "{r:?}");

        // Double speed halves the distance from the in-point.
        clip.speed = 2.0;
        let r = clip.source_span_to_timeline(12.0, 14.0);
        assert!((r.start - 5.0).abs() < 1e-9, "{r:?}");
        assert!((r.end - 6.0).abs() < 1e-9, "{r:?}");

        // Reversed: the source's tail is heard first, and the range stays ordered.
        clip.speed = -1.0;
        let r = clip.source_span_to_timeline(12.0, 14.0);
        assert!((r.start - 10.0).abs() < 1e-9, "{r:?}");
        assert!((r.end - 12.0).abs() < 1e-9, "{r:?}");
        assert!(r.end > r.start);
    }

    #[test]
    fn covers_source_is_the_clips_own_window() {
        let clip = Clip::new(Uuid::new_v4(), 10.0, 20.0, 0.0);
        assert!(clip.covers_source(12.0, 14.0));
        assert!(clip.covers_source(8.0, 11.0), "straddling the in-point still shows");
        assert!(!clip.covers_source(0.0, 10.0), "ending exactly at the in-point shows nothing");
        assert!(!clip.covers_source(20.0, 25.0));
    }

    #[test]
    fn every_transition_kind_round_trips_and_knows_its_family() {
        for k in TransitionKind::ALL {
            assert_eq!(TransitionKind::parse(k.as_str()), Some(k), "{k:?} must survive the wire");
            assert!(
                TransitionKind::wire_names().contains(k.as_str()),
                "{k:?} must be listed for a caller"
            );
            // Exactly one family each: a dip has a colour and never moves, a
            // motion transition moves and never dips, a dissolve does neither.
            assert!(
                !(k.dip_color().is_some() && k.slide_from().is_some()),
                "{k:?} cannot both dip and travel"
            );
            assert_eq!(k.dip_color().is_some(), !k.overlaps(), "{k:?}: only a dip skips the overlap");
            assert!(!k.pushes() || k.slide_from().is_some(), "{k:?}: a push must have a direction");
        }
        // A slide and its push travel the same way; the difference is what
        // happens to the outgoing clip, not where the incoming one comes from.
        assert_eq!(TransitionKind::SlideLeft.slide_from(), TransitionKind::PushLeft.slide_from());
        assert!(!TransitionKind::SlideLeft.pushes() && TransitionKind::PushLeft.pushes());
        assert_eq!(TransitionKind::parse("nonsense"), None);
    }

    #[test]
    fn captions_follow_a_trimmed_and_moved_clip() {
        let asset = Uuid::new_v4();
        // The interesting case: the transcript says 30s, the cut says 0s.
        let timeline = one_clip(Clip::new(asset, 30.0, 34.0, 0.0));
        let lines = captioned(&timeline, asset, vec![seg(30.0, 34.0, "one two three four")]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0, "one two three four");
        // Timeline time, not the transcript's 30.0.
        assert!(lines[0].1.abs() < 1e-9, "{lines:?}");
        assert!((lines[0].2 - 4.0).abs() < 1e-9, "{lines:?}");
    }

    #[test]
    fn a_sentence_cut_in_half_only_captions_what_survived() {
        let asset = Uuid::new_v4();
        // A four-word line spoken over 0..4s, but the cut keeps only 0..2s.
        let timeline = one_clip(Clip::new(asset, 0.0, 2.0, 0.0));
        let lines = captioned(
            &timeline,
            asset,
            vec![seg(0.0, 4.0, "alpha bravo charlie delta echo foxtrot")],
        );
        assert!(!lines.is_empty());
        // Nothing runs past the end of the clip that carries it.
        for (text, start, end) in &lines {
            assert!(*end <= 2.0 + 1e-9, "{text:?} runs to {end} past the clip");
            assert!(*start >= -1e-9);
        }
        // The words spoken in the discarded half are gone.
        assert!(!lines.iter().any(|(t, _, _)| t.contains("foxtrot")), "{lines:?}");
    }

    #[test]
    fn long_segments_split_into_readable_lines() {
        let asset = Uuid::new_v4();
        let timeline = one_clip(Clip::new(asset, 0.0, 8.0, 0.0));
        let lines = captioned(
            &timeline,
            asset,
            vec![seg(0.0, 8.0, "Today we are talking about non-destructive editing in Kerf")],
        );
        assert!(lines.len() > 1, "a ten-word sentence should not be one caption: {lines:?}");
        for (text, _, _) in &lines {
            assert!(text.split_whitespace().count() <= 4, "{text:?} is too many words");
        }
        // The lines are contiguous, in order, and cover the segment.
        assert!(lines[0].1.abs() < 1e-9);
        assert!((lines.last().unwrap().2 - 8.0).abs() < 1e-9);
        for pair in lines.windows(2) {
            assert!(pair[1].1 >= pair[0].1);
        }
        // Rejoining the lines gives the sentence back, word for word.
        let rejoined = lines.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>().join(" ");
        assert_eq!(rejoined, "Today we are talking about non-destructive editing in Kerf");
    }

    #[test]
    fn fast_speech_merges_rather_than_flickering() {
        let asset = Uuid::new_v4();
        // Eight words in 0.9s: split four ways each line would last ~0.22s.
        let timeline = one_clip(Clip::new(asset, 0.0, 0.9, 0.0));
        let lines = captioned(&timeline, asset, vec![seg(0.0, 0.9, "a b c d e f g h")]);
        for (text, start, end) in &lines {
            assert!(
                end - start >= MIN_CAPTION - 1e-6 || lines.len() == 1,
                "{text:?} flashes for {}s",
                end - start
            );
        }
        // No words were lost to the merging.
        let rejoined = lines.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>().join(" ");
        assert_eq!(rejoined, "a b c d e f g h");
    }

    #[test]
    fn captions_are_ordered_by_the_cut_not_by_the_source() {
        let asset = Uuid::new_v4();
        // The second half of the source is cut to play first.
        let mut track = Track::new(StreamKind::Video, "V1");
        track.clips = vec![Clip::new(asset, 10.0, 12.0, 0.0), Clip::new(asset, 0.0, 2.0, 2.0)];
        let timeline = Timeline {
            tracks: vec![track],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
        };
        let lines = captioned(&timeline, asset, vec![seg(0.0, 2.0, "first"), seg(10.0, 12.0, "second")]);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0].0, "second", "the reordered cut leads with the later words");
        assert_eq!(lines[1].0, "first");
        assert!(lines[0].1.abs() < 1e-9);
        assert!((lines[1].1 - 2.0).abs() < 1e-9);
    }

    #[test]
    fn two_captions_never_share_the_screen() {
        let asset = Uuid::new_v4();
        // The same footage twice in the cut at different offsets — a callback
        // shot, or a full source parked under the edit. Both would caption the
        // same words on top of each other.
        let mut track = Track::new(StreamKind::Video, "V1");
        track.clips = vec![Clip::new(asset, 3.0, 12.0, 0.0), Clip::new(asset, 0.0, 12.0, 0.0)];
        let timeline = Timeline {
            tracks: vec![track],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
        };
        let lines = captioned(
            &timeline,
            asset,
            vec![seg(0.0, 6.0, "alpha bravo charlie"), seg(6.0, 12.0, "delta echo foxtrot")],
        );
        assert!(lines.len() > 1, "{lines:?}");
        for pair in lines.windows(2) {
            assert!(
                pair[1].1 >= pair[0].2 - 1e-6,
                "{:?} starts before {:?} is off screen",
                pair[1],
                pair[0]
            );
        }
    }

    #[test]
    fn word_punch_puts_one_word_on_screen_at_a_time() {
        let asset = Uuid::new_v4();
        let timeline = one_clip(Clip::new(asset, 0.0, 5.0, 0.0));
        let mut map = HashMap::new();
        map.insert(asset, vec![seg(0.0, 5.0, "alpha bravo charlie delta echo")]);
        let punched = timeline.captions(&map, CaptionOptions::styled(CaptionStyle::WordPunch));
        assert_eq!(
            punched.iter().map(|o| o.text.as_str()).collect::<Vec<_>>(),
            ["alpha", "bravo", "charlie", "delta", "echo"]
        );
        // The whole look, not just the word count: the line style would leave
        // one word at subtitle size on the bottom edge.
        let layout = CaptionStyle::WordPunch.layout();
        assert!(punched
            .iter()
            .all(|o| o.bold && o.size == layout.size && o.pos_y == layout.pos_y));
        // Each word hands the screen to the next with no gap and no overlap.
        for pair in punched.windows(2) {
            assert!((pair[1].start - pair[0].end).abs() < 1e-9, "{:?}", (&pair[0], &pair[1]));
        }
        // The default style is untouched by any of this.
        let lines = timeline.captions(&map, CaptionOptions::default());
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines.iter().all(|o| !o.bold));
    }

    #[test]
    fn a_word_too_short_to_read_joins_its_neighbour() {
        let asset = Uuid::new_v4();
        let timeline = one_clip(Clip::new(asset, 0.0, 2.0, 0.0));
        let mut map = HashMap::new();
        // "a" is one character of thirty, so its character share is ~0.07s —
        // two frames, which is a flicker rather than a word.
        map.insert(asset, vec![seg(0.0, 2.0, "a fairly quickly spoken sentence")]);
        let punched = timeline.captions(&map, CaptionOptions::styled(CaptionStyle::WordPunch));
        assert!(
            punched.iter().all(|o| o.end - o.start >= MIN_WORD_CAPTION - 1e-6),
            "{:?}",
            punched.iter().map(|o| (&o.text, o.end - o.start)).collect::<Vec<_>>()
        );
        assert_eq!(punched[0].text, "a fairly", "the flicker merges instead of being dropped");
    }

    #[test]
    fn an_override_moves_one_number_and_leaves_the_style_alone() {
        let asset = Uuid::new_v4();
        let timeline = one_clip(Clip::new(asset, 0.0, 5.0, 0.0));
        let mut map = HashMap::new();
        map.insert(asset, vec![seg(0.0, 5.0, "alpha bravo charlie delta echo")]);
        let opts = CaptionOptions {
            size: Some(0.2),
            ..CaptionOptions::styled(CaptionStyle::WordPunch)
        };
        let punched = timeline.captions(&map, opts);
        assert_eq!(punched.len(), 5, "still one word each");
        assert!(punched.iter().all(|o| o.size == 0.2 && o.bold));
        assert!(punched.iter().all(|o| o.pos_y == CaptionStyle::WordPunch.layout().pos_y));
        // An unusable override falls back to the style rather than through it.
        let junk = CaptionOptions {
            size: Some(f64::NAN),
            pos_y: Some(9.0),
            ..CaptionOptions::styled(CaptionStyle::WordPunch)
        };
        let layout = junk.resolve();
        assert_eq!(layout.size, CaptionStyle::WordPunch.layout().size);
        assert_eq!(layout.pos_y, 1.0);
    }

    #[test]
    fn a_long_word_is_shrunk_to_fit_a_vertical_frame() {
        let asset = Uuid::new_v4();
        let mut timeline = one_clip(Clip::new(asset, 0.0, 4.0, 0.0));
        let mut map = HashMap::new();
        map.insert(asset, vec![seg(0.0, 4.0, "non-destructive editing")]);
        let opts = CaptionOptions::styled(CaptionStyle::WordPunch);
        let full = CaptionStyle::WordPunch.layout().size;

        // Unframed, so 16:9 — wide enough that nothing is shrunk, which is what
        // keeps every project that never picked a frame captioned as it was.
        let wide = timeline.captions(&map, opts);
        assert!(wide.iter().all(|o| o.size == full), "{wide:?}");

        // 9:16 is barely half as wide as it is tall, and `drawtext` neither
        // wraps nor scales: the long word would be drawn off both edges.
        timeline.format = Some(Delivery::new(1080, 1920, Fit::Cover));
        let tall = timeline.captions(&map, opts);
        let long = tall.iter().find(|o| o.text == "non-destructive").expect("the long word");
        let short = tall.iter().find(|o| o.text == "editing").expect("the short word");
        assert!(long.size < full, "the long word shrinks: {}", long.size);
        assert_eq!(short.size, full, "a word that already fits is left alone");
        let aspect = 1080.0 / 1920.0;
        assert!(
            long.text.chars().count() as f64 * CHAR_ADVANCE * long.size <= CAPTION_WIDTH * aspect + 1e-9,
            "still overflows: {}",
            long.size
        );
    }

    #[test]
    fn a_muted_track_is_not_captioned() {
        let asset = Uuid::new_v4();
        let mut timeline = one_clip(Clip::new(asset, 0.0, 4.0, 0.0));
        assert!(!captioned(&timeline, asset, vec![seg(0.0, 4.0, "heard")]).is_empty());
        timeline.tracks[0].muted = true;
        assert!(captioned(&timeline, asset, vec![seg(0.0, 4.0, "heard")]).is_empty());
    }

    #[test]
    fn the_same_words_on_two_tracks_are_captioned_once() {
        let asset = Uuid::new_v4();
        // What `extract_audio` leaves behind: picture and detached audio, both
        // referencing the same asset over the same source window.
        let mut video = Track::new(StreamKind::Video, "V1");
        video.clips = vec![Clip::new(asset, 0.0, 3.0, 0.0)];
        let mut audio = Track::new(StreamKind::Audio, "A1");
        audio.clips = vec![Clip::new(asset, 0.0, 3.0, 0.0)];
        let timeline = Timeline {
            tracks: vec![video, audio],
            overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
        };
        let lines = captioned(&timeline, asset, vec![seg(0.0, 3.0, "only once")]);
        assert_eq!(lines.len(), 1, "{lines:?}");
    }
}
