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
}

impl Default for Color {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            gamma: 1.0,
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
        }
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
    pub kind: StreamKind,
    pub name: String,
    #[serde(default)]
    pub clips: Vec<Clip>,
}

impl Track {
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

/// The non-destructive timeline (EDL): a set of multi-kind tracks plus the text
/// overlays (titles / lower-thirds / captions) drawn over the composited picture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeline {
    pub tracks: Vec<Track>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlays: Vec<TextOverlay>,
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
            tracks: vec![
                Track {
                    id: Uuid::new_v4(),
                    kind: StreamKind::Video,
                    name: "V1".to_string(),
                    clips: Vec::new(),
                    duck: false,
                },
                Track {
                    id: Uuid::new_v4(),
                    kind: StreamKind::Audio,
                    name: "A1".to_string(),
                    clips: Vec::new(),
                    duck: false,
                },
            ],
            overlays: Vec::new(),
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
        };
        for track in &self.tracks {
            let mut t = Track {
                id: track.id,
                kind: track.kind,
                name: track.name.clone(),
                clips: Vec::new(),
                duck: track.duck,
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
