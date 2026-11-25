//! Tracks, the revisions and staged edits recorded against them, and markers.

use super::*;

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
    /// renders; every edit that changes what is on it, where, how long or whether it
    /// plays is refused (`Timeline::ensure_editable`), while its clips' look and sound
    /// stay editable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    /// The track fader: a linear gain riding every clip on the track *after* its
    /// own volume and effect chain, the way a channel strip works — so pulling a
    /// music bed down does not change what its compressor was reacting to.
    /// 1.0 is unity. Defaulted, so a project written before there was a fader
    /// reads back at unity and renders identically.
    #[serde(default = "unity_gain", skip_serializing_if = "is_unity_gain")]
    pub volume: f32,
    /// Stereo placement, -1 (hard left) to 1 (hard right); 0 is centre. Applied
    /// as a balance (see [`Track::pan_gains`] — deliberately *not* a
    /// constant-power law), so panning a track never makes it louder — and it
    /// is a no-op on a mono delivery, where there is nowhere to pan to.
    #[serde(default, skip_serializing_if = "is_centred")]
    pub pan: f32,
    pub kind: StreamKind,
    pub name: String,
    #[serde(default)]
    pub clips: Vec<Clip>,
}

fn unity_gain() -> f32 {
    1.0
}

fn is_unity_gain(v: &f32) -> bool {
    (*v - 1.0).abs() < f32::EPSILON
}

fn is_centred(v: &f32) -> bool {
    v.abs() < f32::EPSILON
}

impl Track {
    /// An empty track: not ducked, muted, soloed or locked, at unity and centred.
    pub fn new(kind: StreamKind, name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            duck: false,
            muted: false,
            solo: false,
            locked: false,
            volume: 1.0,
            pan: 0.0,
            kind,
            name: name.into(),
            clips: Vec::new(),
        }
    }

    /// The left / right gains for this track's `pan`, as a fraction of unity.
    ///
    /// A **balance**, not a constant-power pan: the side you turn towards stays
    /// at unity and the other is attenuated away. A constant-power law would
    /// boost the near side by 3 dB at the extremes, which is right for placing a
    /// mono source in a field and wrong for leaning a finished stereo track —
    /// nudging a music bed left should not make it louder. Centre is exactly
    /// `(1, 1)`, so an untouched track is bit-for-bit what it always was.
    pub fn pan_gains(&self) -> (f64, f64) {
        let p = self.pan.clamp(-1.0, 1.0) as f64;
        if p < 0.0 {
            (1.0, 1.0 + p)
        } else {
            (1.0 - p, 1.0)
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

            let mut duration = clip.duration();
            if let Some(beat) = nearest_beat(beats, start + duration, tolerance) {
                let limit = source_limit.get(&clip.asset_id).copied().unwrap_or(f64::INFINITY);
                let source_left = if clip.is_reversed() {
                    clip.source_out
                } else {
                    (limit - clip.source_in).max(0.0)
                };
                let available = clip.local_at_along(source_left);
                let wanted = (beat - start).clamp(MIN_BEAT_CLIP, available.max(MIN_BEAT_CLIP));
                if (wanted - duration).abs() > 1e-6 {
                    moved += 1;
                    duration = wanted;
                }
            }

            clip.timeline_start = start;
            let travelled = clip.along(duration);
            if clip.is_reversed() {
                clip.source_in = (clip.source_out - travelled).max(0.0);
            } else {
                clip.source_out = clip.source_in + travelled;
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
    /// The agent session (its MCP session id) the proposal belongs to, when one was named. A
    /// second agent's edits are refused while another session owns the proposal. **Never
    /// serialised**: a session id is a bearer-ish handle on the MCP connection, and what reaches
    /// the webview or another agent is [`StagedEdit::owned_by_agent`].
    #[serde(skip)]
    pub owner: Option<String>,
    /// Some agent session owns the proposal (see `owner`, which is Rust-side only).
    #[serde(default)]
    pub owned_by_agent: bool,
    /// Labels of the individual edits, in the order they were staged.
    pub edits: Vec<String>,
    /// How many of the latest staged steps [`crate::project::Project::undo_staged`] can still take back.
    #[serde(default)]
    pub undo_depth: usize,
    /// Changes to the library (an asset's projection) held back with the proposal; they land with it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asset_changes: Vec<StagedAssetChange>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The live timeline has moved on since `base_seq`.
    pub stale: bool,
    /// What applying it would do to the cut.
    pub diff: TimelineDiff,
}

/// A change to an *asset* an agent made while a proposal was open. The library is not the
/// timeline, so it cannot ride in the proposal's timeline; it is held back beside it instead
/// and written when the proposal is accepted (or the entry is picked in a partial accept).
///
/// Two kinds: a projection change (`tag_edit` absent) and a tag change (`tag_edit` set).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StagedAssetChange {
    /// `asset_projection:<asset id>` or `asset_tags:<asset id>` — usable wherever a [`DiffEntry::id`] is.
    pub id: String,
    pub asset_id: Uuid,
    /// The spherical projection the asset is to be marked with; `None` clears it. Not read by a tag change.
    pub projection: Option<Projection>,
    /// A tag change: everything the agent's edits did to the asset's tags, composed into one edit ([`TagEdit::then`]). It
    /// is run over the asset's tags as they are when it is read, so it keeps what the user tagged in between.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_edit: Option<TagEdit>,
    /// One line for the review card, e.g. `Mark interview.mp4 as equirect 360`.
    pub summary: String,
}

impl StagedAssetChange {
    /// The id every projection change to `asset_id` has — one pending change per asset and kind.
    pub fn projection_id(asset_id: Uuid) -> String {
        format!("asset_projection:{asset_id}")
    }

    /// The id every tag change to `asset_id` has.
    pub fn tags_id(asset_id: Uuid) -> String {
        format!("asset_tags:{asset_id}")
    }
}

/// A named point on the timeline. Purely an annotation — it renders nothing —
/// but it gives the user and the agent a shared vocabulary for places in the
/// cut ("the laugh at 01:12"), which timestamps alone do not.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: Uuid,
    /// Position on the timeline, seconds.
    pub time: f64,
    pub name: String,
    /// Optional CSS color for the ruler chip; the UI picks a default when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// How long the marked span runs from `time`, seconds; 0 is a point.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub duration: f64,
    /// A free-form note (a review comment, a chapter description).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
    #[serde(default, skip_serializing_if = "MarkerKind::is_comment")]
    pub kind: MarkerKind,
}

/// What a [`Marker`] is for. A `Chapter` starts a chapter of the delivered video:
/// it is written into the export's chapter list and the YouTube chapter text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MarkerKind {
    #[default]
    Comment,
    Chapter,
}

impl MarkerKind {
    pub fn is_comment(&self) -> bool {
        *self == MarkerKind::Comment
    }

    pub fn as_str(self) -> &'static str {
        match self {
            MarkerKind::Comment => "comment",
            MarkerKind::Chapter => "chapter",
        }
    }
}

/// The longest a marker's comment may be (characters).
pub const MAX_MARKER_COMMENT: usize = 10_000;

/// A change to a [`Marker`]; each `None` leaves that field alone. An empty
/// `color` clears it back to the UI default.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarkerPatch {
    pub time: Option<f64>,
    pub name: Option<String>,
    pub color: Option<String>,
    pub duration: Option<f64>,
    pub comment: Option<String>,
    pub kind: Option<MarkerKind>,
}

impl Marker {
    /// Apply `patch`, refusing it whole when a field is out of range.
    pub fn apply(&mut self, patch: MarkerPatch) -> Result<()> {
        if patch.time.is_some_and(|t| !t.is_finite() || t < 0.0) {
            return Err(Error::InvalidArgument("marker time must be >= 0".to_string()));
        }
        if patch.duration.is_some_and(|d| !d.is_finite() || d < 0.0) {
            return Err(Error::InvalidArgument("marker duration must be >= 0".to_string()));
        }
        let comment = patch.comment.map(|c| {
            c.chars()
                .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
                .collect::<String>()
        });
        if comment.as_ref().is_some_and(|c| c.chars().count() > MAX_MARKER_COMMENT) {
            return Err(Error::InvalidArgument(format!(
                "marker comment is longer than {MAX_MARKER_COMMENT} characters"
            )));
        }
        if let Some(t) = patch.time {
            self.time = t;
        }
        if let Some(n) = patch.name {
            self.name = n;
        }
        if let Some(c) = patch.color {
            self.color = if c.is_empty() { None } else { Some(c) };
        }
        if let Some(d) = patch.duration {
            self.duration = d;
        }
        if let Some(c) = comment {
            self.comment = c.trim_end().to_string();
        }
        if let Some(k) = patch.kind {
            self.kind = k;
        }
        Ok(())
    }

    /// Where the marked span ends (`time` for a point marker).
    pub fn end(&self) -> f64 {
        self.time + self.duration.max(0.0)
    }
}
