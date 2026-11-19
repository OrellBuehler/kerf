//! What changed between two timelines, in words.

use super::*;

// ---- diff ------------------------------------------------------------------

/// Below this, two timeline values are the same value — the timeline round-trips
/// through JSON on every edit, so an exact compare would be honest but noisy.
pub(super) const DIFF_EPS: f64 = 1e-6;

pub(super) fn num_changed(a: f64, b: f64) -> bool {
    (a - b).abs() > DIFF_EPS
}

/// `m:ss.d`, the way an editor reads a timeline position.
pub(crate) fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0);
    let m = (s / 60.0).floor();
    format!("{}:{:04.1}", m as i64, s - m * 60.0)
}

/// A signed shift of footage at the precision a slip is made at — a frame is 0.03 s,
/// which one decimal would print as `+0.0s` — and three decimals when even two would
/// round a real shift to nothing.
fn fmt_shift(secs: f64) -> String {
    let secs = secs + 0.0; // -0.0 is 0
    let places = if secs != 0.0 && (secs.abs() * 100.0).round() == 0.0 {
        3
    } else {
        2
    };
    format!("{}{secs:.places$}s", if secs >= 0.0 { "+" } else { "" })
}

pub(super) fn fmt_delta(secs: f64) -> String {
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
    MasterChanged,
    ThumbnailsChanged,
}

impl DiffKind {
    /// The `snake_case` name this kind serializes as — the first half of a [`DiffEntry::id`].
    pub fn as_str(self) -> &'static str {
        match self {
            DiffKind::TrackAdded => "track_added",
            DiffKind::TrackRemoved => "track_removed",
            DiffKind::TrackChanged => "track_changed",
            DiffKind::ClipAdded => "clip_added",
            DiffKind::ClipRemoved => "clip_removed",
            DiffKind::ClipMoved => "clip_moved",
            DiffKind::ClipRetrimmed => "clip_retrimmed",
            DiffKind::ClipChanged => "clip_changed",
            DiffKind::OverlayAdded => "overlay_added",
            DiffKind::OverlayRemoved => "overlay_removed",
            DiffKind::OverlayChanged => "overlay_changed",
            DiffKind::MarkerAdded => "marker_added",
            DiffKind::MarkerRemoved => "marker_removed",
            DiffKind::MarkerChanged => "marker_changed",
            DiffKind::FormatChanged => "format_changed",
            DiffKind::MasterChanged => "master_changed",
            DiffKind::ThumbnailsChanged => "thumbnails_changed",
        }
    }

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
    /// Stable name of this change — `<kind>:<the clip, text, marker or track it is about>` — which
    /// the same change keeps for as long as the proposal holds it, so a reviewer can send back
    /// the ones it accepts ([`crate::project::Project::apply_staged_subset`]). Unique within a
    /// diff: a thing has at most one change of each kind.
    #[serde(default)]
    pub id: String,
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
    /// The title or caption an `Overlay*` entry is about, so a view of the proposal can mark it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_id: Option<Uuid>,
    /// The marker a `Marker*` entry is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker_id: Option<Uuid>,
    /// Where on the timeline to look, so the reviewer can jump straight to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
}

impl DiffEntry {
    pub(super) fn new(kind: DiffKind, summary: String) -> Self {
        Self {
            id: String::new(),
            kind,
            summary,
            detail: None,
            track_id: None,
            clip_id: None,
            overlay_id: None,
            marker_id: None,
            at: None,
        }
    }

    /// Name the change after its kind and the thing it is about (see [`DiffEntry::id`]).
    fn with_id(mut self) -> Self {
        let target = self.clip_id.or(self.overlay_id).or(self.marker_id).or(self.track_id);
        self.id = match target {
            Some(target) => format!("{}:{target}", self.kind.as_str()),
            None => self.kind.as_str().to_string(),
        };
        self
    }

    fn on_marker(mut self, id: Uuid) -> Self {
        self.marker_id = Some(id);
        self
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

    fn on_overlay(mut self, id: Uuid) -> Self {
        self.overlay_id = Some(id);
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
    // The mixer strip: without these an agent proposal that only rides a fader
    // or a pan diffs as empty and apply_staged throws it away.
    if (before.volume - after.volume).abs() > DIFF_EPS as f32 {
        parts.push(format!("level {:.0}% → {:.0}%", before.volume * 100.0, after.volume * 100.0));
    }
    if (before.pan - after.pan).abs() > DIFF_EPS as f32 {
        parts.push(format!("pan {:.2} → {:.2}", before.pan, after.pan));
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
    parts.extend(ramp_edit::ramp_changes(before, after));
    if before.enabled != after.enabled {
        parts.push(if after.enabled { "re-enabled" } else { "disabled" }.to_string());
    }
    // Links and the detached-sound flag are edits too: without these a proposal
    // that only links two clips diffs as empty and `apply_staged` discards it.
    if before.source_audio != after.source_audio {
        parts.push(if after.source_audio { "own sound on" } else { "own sound off" }.to_string());
    }
    if before.link_id != after.link_id {
        parts.push(
            match (before.link_id, after.link_id) {
                (_, None) => "unlinked",
                (None, Some(_)) => "linked",
                (Some(_), Some(_)) => "relinked",
            }
            .to_string(),
        );
    }
    parts.extend(generator::generator_changes(before, after));
    parts.extend(transform_changes(&before.transform, &after.transform));
    parts.extend(color_changes(&before.color, &after.color));
    parts.extend(Grade::changes(&before.grade, &after.grade));
    // A framing pass before a multi-format export is an edit like any other,
    // and one an agent proposal has to be able to show.
    let framed: Vec<String> = after
        .framings
        .iter()
        .filter(|f| before.framing_for(f.ratio()) != Some(*f))
        .map(Framing::ratio_label)
        .collect();
    if !framed.is_empty() {
        parts.push(format!("framed for {}", framed.join(", ")));
    }
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
        // Keys that only leave their poses differently were not moved or retimed.
        let plain = |k: &Keyframe| Keyframe {
            easing: Easing::Linear,
            ..*k
        };
        let pairs = || before.keyframes.iter().zip(&after.keyframes);
        if pairs().any(|(a, b)| plain(a) != plain(b)) {
            parts.push("keyframes retimed".to_string());
        }
        let eased = pairs().filter(|(a, b)| a.easing != b.easing).count();
        if eased > 0 {
            parts.push(format!(
                "easing changed on {eased} keyframe{}",
                if eased == 1 { "" } else { "s" }
            ));
        }
    }
    parts.extend(channel_changes(before, after));
    parts.extend(behavior::behavior_changes(before, after));
    parts.extend(reframe_changes(before.reframe.as_ref(), after.reframe.as_ref()));
    if before.mask != after.mask {
        parts.push(match &after.mask {
            None => "mask cleared".to_string(),
            Some(m) => format!("masked ({})", m.shape.as_str()),
        });
    }
    parts.extend(stabilize::stabilize_changes(before, after));
    if before.blend_mode != after.blend_mode {
        parts.push(format!(
            "blend {} → {}",
            before.blend_mode.label().to_lowercase(),
            after.blend_mode.label().to_lowercase()
        ));
    }
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

/// The controls of a template title whose value differs, as `Tagline “A” → “B”`.
fn template_value_changes(before: &SvgOverlay, after: &SvgOverlay) -> Vec<String> {
    let (Some(was), Some(now)) = (&before.template, &after.template) else {
        return Vec::new();
    };
    now.controls
        .iter()
        .filter_map(|c| {
            let (a, b) = (was.values.get(&c.id), now.values.get(&c.id));
            (a != b).then(|| {
                format!(
                    "{} “{}” → “{}”",
                    c.label,
                    a.map_or("", String::as_str),
                    b.map_or("", String::as_str)
                )
            })
        })
        .collect()
}

fn svg_overlay_changes(before: &SvgOverlay, after: &SvgOverlay) -> Option<String> {
    let mut parts = Vec::new();
    if before.name != after.name {
        parts.push(format!("name “{}” → “{}”", before.name, after.name));
    }
    match (&before.preset, &after.preset) {
        (Some(a), Some(b)) => {
            if a.kind != b.kind {
                parts.push(format!("style {} → {}", a.kind.label(), b.kind.label()));
            }
            if a.params.text != b.params.text {
                parts.push(format!("text “{}” → “{}”", a.params.text, b.params.text));
            }
            if a.params != b.params && a.params.text == b.params.text {
                parts.push("preset look changed".to_string());
            }
        }
        (Some(_), None) => parts.push("converted to custom SVG".to_string()),
        (None, Some(_)) => parts.push("replaced by a preset".to_string()),
        (None, None) => {
            if before.svg != after.svg {
                let values = template_value_changes(before, after);
                if values.is_empty() {
                    parts.push("artwork changed".to_string());
                } else {
                    parts.extend(values);
                }
            }
        }
    }
    if before.template.is_some() && after.template.is_none() {
        parts.push("no longer a template title".to_string());
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
    if [
        (before.x, after.x),
        (before.y, after.y),
        (before.width, after.width),
        (before.height, after.height),
    ]
    .iter()
    .any(|(a, b)| num_changed(*a, *b))
    {
        parts.push(format!(
            "placement {:.2},{:.2} {:.2}x{:.2} → {:.2},{:.2} {:.2}x{:.2}",
            before.x, before.y, before.width, before.height, after.x, after.y, after.width, after.height
        ));
    }
    if num_changed(before.opacity, after.opacity) {
        parts.push(format!(
            "opacity {:.0}% → {:.0}%",
            before.opacity * 100.0,
            after.opacity * 100.0
        ));
    }
    joined(parts)
}

/// What differs between two master buses, phrased like the track strip's
/// (`level 100% → 70%`), or `None` when the mix would come out the same. The
/// ceiling only counts while the limiter is on afterwards — a stored ceiling
/// the limiter is not using changes nothing anyone can hear. The true-peak switch
/// always counts: it is the user's choice for the limiter whenever it runs, and an
/// agent's staged switch must not read as an empty proposal.
fn master_changes(before: &MasterBus, after: &MasterBus) -> Option<String> {
    let mut parts = Vec::new();
    if (before.volume - after.volume).abs() > DIFF_EPS {
        parts.push(format!("level {:.0}% → {:.0}%", before.volume * 100.0, after.volume * 100.0));
    }
    if before.limiter != after.limiter {
        parts.push(if after.limiter {
            format!("limiter on at {:.1} dB", after.ceiling_db)
        } else {
            "limiter off".to_string()
        });
    } else if after.limiter && (before.ceiling_db - after.ceiling_db).abs() > DIFF_EPS {
        parts.push(format!(
            "limiter ceiling {:.1} → {:.1} dB",
            before.ceiling_db, after.ceiling_db
        ));
    }
    if before.true_peak != after.true_peak {
        parts.push(format!("true-peak limiting {}", if after.true_peak { "on" } else { "off" }));
    }
    let duck = |d: Option<f64>| match d {
        Some(db) => format!("speech gate {db:.0} dB"),
        None => "compressor".to_string(),
    };
    let (was, now) = (before.safe_duck_depth_db(), after.safe_duck_depth_db());
    let moved = match (was, now) {
        (Some(a), Some(b)) => (a - b).abs() > DIFF_EPS,
        (a, b) => a.is_some() != b.is_some(),
    };
    if moved {
        parts.push(format!("duck {} → {}", duck(was), duck(now)));
    }
    joined(parts)
}

fn fmt_delivery(d: &Delivery) -> String {
    match d.rate() {
        Some(fps) => format!("{}x{} ({}, {} fps)", d.width, d.height, d.fit.as_str(), fmt_fps(fps)),
        None => format!("{}x{} ({})", d.width, d.height, d.fit.as_str()),
    }
}

impl Timeline {
    /// Refuse an edit that left one of `ids` on top of another clip of its track, unless the two
    /// already overlapped in `before` (an overlap that was not this edit's doing). Run after the
    /// ripple, which may have made the room: a clip lengthened over its neighbour is fine with the
    /// neighbour pushed along and refused without. An id that is not on the timeline is ignored.
    pub fn check_own_lanes(&self, before: &Timeline, ids: &[Uuid]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let prior: HashMap<Uuid, (f64, f64)> = before
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| (c.id, (c.timeline_start, c.timeline_end())))
            .collect();
        for track in &self.tracks {
            for p in track.clips.iter().filter(|c| ids.contains(&c.id)) {
                let span = (p.timeline_start, p.timeline_end());
                for q in &track.clips {
                    let qspan = (q.timeline_start, q.timeline_end());
                    if q.id == p.id || !spans_overlap(span, qspan) {
                        continue;
                    }
                    if matches!((prior.get(&p.id), prior.get(&q.id)), (Some(a), Some(b)) if spans_overlap(*a, *b)) {
                        continue;
                    }
                    return Err(Error::InvalidArgument(format!(
                        "the clip would overlap another clip on {} at {} — move or trim that one first",
                        track.name,
                        fmt_time(p.timeline_start.max(q.timeline_start))
                    )));
                }
            }
        }
        Ok(())
    }

    /// How many clips stand somewhere else here than in `before`: a different
    /// start on their track, or a different track. Matched by id, so a clip that
    /// exists on only one side is not counted. This is how a caller learns whether
    /// an edit *rippled* — ripple is the only thing that moves a clip a remove or a
    /// retime did not touch — rather than assuming it did because ripple mode was
    /// on: a locked track never moves, and a ripple that would leave a lane
    /// overlapping is declined, both silently.
    pub fn clips_moved_since(&self, before: &Timeline) -> usize {
        let was = clip_index(before);
        clip_index(self)
            .into_iter()
            .filter(|(id, (track, clip))| {
                was.get(id)
                    .is_some_and(|(t, c)| t.id != track.id || num_changed(c.timeline_start, clip.timeline_start))
            })
            .count()
    }

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
                            "Removed {} from {} at {} ({:.1}s)",
                            generator::title_label(clip).unwrap_or_else(|| "clip".to_string()),
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
                            generator::added_line(clip, &track.name).unwrap_or_else(|| {
                                format!(
                                    "Added clip to {} at {} ({:.1}s)",
                                    track.name,
                                    fmt_time(clip.timeline_start),
                                    clip.duration()
                                )
                            }),
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
                    // A window that moved without changing length is a slip, not a
                    // trim: "4.0s → 4.0s (+0.0s)" would read as nothing having happened.
                    let summary = if num_changed(was, is) {
                        format!(
                            "Trimmed clip on {} at {} — {was:.1}s → {is:.1}s ({})",
                            track.name,
                            fmt_time(clip.timeline_start),
                            fmt_delta(is - was)
                        )
                    } else {
                        // Signed as `slip_clip` documents it: + is *later in its own
                        // footage*, which for a reversed clip is the window moving down.
                        let moved = clip.source_in - before_clip.source_in;
                        let later = if clip.is_reversed() { -moved } else { moved };
                        format!(
                            "Slipped clip on {} at {} — footage {} (in-point {:.2}s → {:.2}s)",
                            track.name,
                            fmt_time(clip.timeline_start),
                            fmt_shift(later),
                            before_clip.source_in,
                            clip.source_in
                        )
                    };
                    clips.push(
                        DiffEntry::new(DiffKind::ClipRetrimmed, summary)
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
                    .on_overlay(overlay.id)
                    .at(overlay.start),
                ),
                Some(before) => {
                    if let Some(detail) = overlay_changes(before, overlay) {
                        rest.push(
                            DiffEntry::new(DiffKind::OverlayChanged, format!("Changed text “{}”", overlay.text))
                                .detail(detail)
                                .on_overlay(overlay.id)
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
                    .on_overlay(overlay.id)
                    .at(overlay.start),
                );
            }
        }

        for overlay in &after.svg_overlays {
            match self.svg_overlay(overlay.id) {
                None => rest.push(
                    DiffEntry::new(
                        DiffKind::OverlayAdded,
                        format!(
                            "Added animated title “{}” at {}–{}",
                            overlay.display_name(),
                            fmt_time(overlay.start),
                            fmt_time(overlay.end)
                        ),
                    )
                    .on_overlay(overlay.id)
                    .at(overlay.start),
                ),
                Some(before) => {
                    if let Some(detail) = svg_overlay_changes(before, overlay) {
                        rest.push(
                            DiffEntry::new(
                                DiffKind::OverlayChanged,
                                format!("Changed animated title “{}”", overlay.display_name()),
                            )
                            .detail(detail)
                            .on_overlay(overlay.id)
                            .at(overlay.start),
                        );
                    }
                }
            }
        }
        for overlay in &self.svg_overlays {
            if after.svg_overlay(overlay.id).is_none() {
                rest.push(
                    DiffEntry::new(
                        DiffKind::OverlayRemoved,
                        format!(
                            "Removed animated title “{}” at {}",
                            overlay.display_name(),
                            fmt_time(overlay.start)
                        ),
                    )
                    .on_overlay(overlay.id)
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
                    .on_marker(marker.id)
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
                    if num_changed(before.duration, marker.duration) {
                        parts.push(format!(
                            "length {} → {}",
                            fmt_time(before.duration),
                            fmt_time(marker.duration)
                        ));
                    }
                    if before.kind != marker.kind {
                        parts.push(format!("now a {}", marker.kind.as_str()));
                    }
                    if before.comment != marker.comment {
                        parts.push("comment edited".to_string());
                    }
                    if let Some(detail) = joined(parts) {
                        rest.push(
                            DiffEntry::new(DiffKind::MarkerChanged, format!("Changed marker “{}”", marker.name))
                                .detail(detail)
                                .on_marker(marker.id)
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
                    .on_marker(marker.id)
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
        // The master bus: without this an agent proposal that only rides the
        // master fader or turns the limiter on diffs as empty, and apply_staged
        // throws it away (the same trap the track faders had).
        if let Some(detail) = master_changes(&self.master, &after.master) {
            rest.push(DiffEntry::new(DiffKind::MasterChanged, "Changed the master bus".to_string()).detail(detail));
        }
        if let Some(detail) = thumbnail::thumbnail_changes(&self.thumbnails, &after.thumbnails) {
            rest.push(DiffEntry::new(DiffKind::ThumbnailsChanged, "Changed the thumbnails".to_string()).detail(detail));
        }

        tracks.append(&mut clips);
        tracks.append(&mut rest);
        TimelineDiff {
            entries: tracks.into_iter().map(DiffEntry::with_id).collect(),
            duration_before: self.duration(),
            duration_after: after.duration(),
            clips_before: before_clips.len(),
            clips_after: after_clips.len(),
        }
    }
}
