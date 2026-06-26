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
}

/// A half-open time range `[start, end)` in seconds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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
        }
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

/// The non-destructive timeline (EDL): a set of multi-kind tracks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeline {
    pub tracks: Vec<Track>,
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
                },
                Track {
                    id: Uuid::new_v4(),
                    kind: StreamKind::Audio,
                    name: "A1".to_string(),
                    clips: Vec::new(),
                },
            ],
        }
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
