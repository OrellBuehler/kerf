//! The invariants every stored timeline holds, checked before an edit is committed.
//!
//! Deliberately narrow: only states no op means to make and no reader can handle — a time that
//! is not a number, a clip with no footage, two things with one id. Overlaps on a track are not
//! among them: Kerf allows them (an explicit `add_clip`, a trim into the next clip), and the
//! ripple already treats an overlap the edit did not make as old news.
//!
//! An edit is refused only for what it *introduces*: a file an older build wrote may already hold
//! a broken item, and refusing every edit to it would lock the user out of their own project.

use std::collections::HashSet;

use uuid::Uuid;

use super::{fmt_time, Clip, Timeline, Track};
use crate::error::{Error, Result};

/// Which invariant an item breaks; with the item's id, the same violation before and after an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Rule {
    TrackTwice,
    TrackGain,
    ClipTwice,
    ClipTime,
    ClipEmpty,
    ClipSpeed,
    ClipFade,
    ClipTransition,
    ClipKeyframe,
    OverlayTwice,
    OverlaySpan,
    OverlayKeyframe,
    TitleTwice,
    TitleSpan,
    MarkerTime,
}

/// One broken invariant: the rule, the item, and a sentence naming the item for the user.
struct Violation {
    rule: Rule,
    item: Uuid,
    why: String,
}

/// The violations a timeline already holds, which an edit to it may leave in place.
#[derive(Debug, Default)]
pub struct KnownViolations(HashSet<(Rule, Uuid)>);

impl Timeline {
    /// Refuse a timeline that breaks any invariant: a non-finite time, gain or speed; a clip
    /// whose out-point is not after its in-point; a negative fade or transition; an overlay that
    /// does not end after it starts; or an id used twice.
    pub fn check(&self) -> Result<()> {
        self.check_new(&KnownViolations::default())
    }

    /// What this timeline already breaks, taken before an edit so [`Self::check_new`] can tell
    /// what the edit added.
    pub fn known_violations(&self) -> KnownViolations {
        KnownViolations(self.violations().into_iter().map(|v| (v.rule, v.item)).collect())
    }

    /// [`Self::check`], forgiving what `known` lists. `Project::run_edit` calls it before the
    /// edit is committed, so a failing edit persists nothing.
    pub fn check_new(&self, known: &KnownViolations) -> Result<()> {
        match self.violations().into_iter().find(|v| !known.0.contains(&(v.rule, v.item))) {
            Some(v) => Err(Error::InvalidArgument(format!(
                "that edit would leave the timeline invalid: {}",
                v.why
            ))),
            None => Ok(()),
        }
    }

    fn violations(&self) -> Vec<Violation> {
        let mut out = Vec::new();
        let mut push = |rule, item, why: String| out.push(Violation { rule, item, why });
        let mut tracks = HashSet::new();
        let mut clips = HashSet::new();
        for track in &self.tracks {
            if !tracks.insert(track.id) {
                push(
                    Rule::TrackTwice,
                    track.id,
                    format!("track {} has the same id as another track", track.name),
                );
            }
            if !track.volume.is_finite() || !track.pan.is_finite() {
                push(
                    Rule::TrackGain,
                    track.id,
                    format!("track {} has a gain that is not a number", track.name),
                );
            }
            for clip in &track.clips {
                let name = clip_name(track, clip);
                if !clips.insert(clip.id) {
                    push(Rule::ClipTwice, clip.id, format!("{name} has the same id as another clip"));
                }
                for (rule, why) in clip_violations(clip) {
                    push(rule, clip.id, format!("{name} {why}"));
                }
            }
        }
        let mut overlays = HashSet::new();
        for o in &self.overlays {
            let name = format!("the text {} at {}", quoted(&o.text, "overlay"), fmt_time(o.start));
            if !overlays.insert(o.id) {
                push(Rule::OverlayTwice, o.id, format!("{name} has the same id as another overlay"));
            }
            if !o.start.is_finite() || !o.end.is_finite() || o.end <= o.start {
                push(Rule::OverlaySpan, o.id, format!("{name} must end after it starts"));
            }
            if o.keyframes.iter().any(|k| !k.time.is_finite()) {
                push(
                    Rule::OverlayKeyframe,
                    o.id,
                    format!("{name} has a keyframe at a time that is not a number"),
                );
            }
        }
        for o in &self.svg_overlays {
            let name = format!("the animated title {} at {}", quoted(&o.name, "title"), fmt_time(o.start));
            if !overlays.insert(o.id) {
                push(Rule::TitleTwice, o.id, format!("{name} has the same id as another overlay"));
            }
            if !o.start.is_finite() || !o.end.is_finite() || o.end <= o.start {
                push(Rule::TitleSpan, o.id, format!("{name} must end after it starts"));
            }
        }
        for m in &self.markers {
            if !m.time.is_finite() {
                push(
                    Rule::MarkerTime,
                    m.id,
                    format!("the marker {} is at a time that is not a number", quoted(&m.name, "marker")),
                );
            }
        }
        out
    }
}

/// "the clip at 0:04.0 on V1", or "a clip on V1" when it has no usable position.
fn clip_name(track: &Track, clip: &Clip) -> String {
    if clip.timeline_start.is_finite() {
        format!("the clip at {} on {}", fmt_time(clip.timeline_start), track.name)
    } else {
        format!("a clip on {}", track.name)
    }
}

/// The text in curly quotes, cut short, or "(overlay without text)" when there is none.
fn quoted(text: &str, noun: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return format!("({noun} without text)");
    }
    let short: String = text.chars().take(24).collect();
    let more = if short.len() < text.len() { "\u{2026}" } else { "" };
    format!("\u{201c}{short}{more}\u{201d}")
}

fn clip_violations(c: &Clip) -> Vec<(Rule, &'static str)> {
    let mut out = Vec::new();
    if !(c.source_in.is_finite() && c.source_out.is_finite() && c.timeline_start.is_finite()) {
        out.push((Rule::ClipTime, "has a time that is not a number"));
    } else if c.source_out <= c.source_in {
        out.push((Rule::ClipEmpty, "must end after it starts in its source"));
    }
    if !c.speed.is_finite() || !c.volume.is_finite() {
        out.push((Rule::ClipSpeed, "has a speed or volume that is not a number"));
    }
    if !(c.fade_in.is_finite() && c.fade_in >= 0.0 && c.fade_out.is_finite() && c.fade_out >= 0.0) {
        out.push((Rule::ClipFade, "has a negative fade"));
    }
    if c.transition_in
        .as_ref()
        .is_some_and(|t| !t.duration.is_finite() || t.duration < 0.0)
    {
        out.push((Rule::ClipTransition, "has a negative transition"));
    }
    if c.keyframes.iter().any(|k| !k.time.is_finite()) {
        out.push((Rule::ClipKeyframe, "has a keyframe at a time that is not a number"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{StreamKind, TextOverlay, Track, Transition, TransitionKind};
    use uuid::Uuid;

    fn timeline() -> Timeline {
        let mut t = Timeline::new();
        let asset = Uuid::new_v4();
        let v = t.first_track_of(StreamKind::Video).unwrap();
        let clips = &mut t.track_mut(v).unwrap().clips;
        clips.push(Clip::new(asset, 0.0, 4.0, 0.0));
        clips.push(Clip::new(asset, 4.0, 8.0, 4.0));
        t.overlays.push(TextOverlay::new("hi", 1.0, 2.0));
        t
    }

    fn rejects(why: &str, edit: impl FnOnce(&mut Timeline)) {
        let mut t = timeline();
        edit(&mut t);
        let err = t.check().expect_err(why);
        assert!(err.to_string().contains("would leave the timeline invalid"), "{why}: {err}");
    }

    fn first(t: &mut Timeline) -> &mut Clip {
        &mut t.tracks[0].clips[0]
    }

    #[test]
    fn an_ordinary_timeline_passes() {
        timeline().check().unwrap();
        Timeline::new().check().unwrap();
    }

    #[test]
    fn overlapping_clips_and_reversed_speed_pass() {
        let mut t = timeline();
        t.tracks[0].clips[1].timeline_start = 2.0;
        first(&mut t).speed = -2.0;
        first(&mut t).timeline_start = -0.5;
        t.check().unwrap();
    }

    #[test]
    fn non_finite_numbers_are_refused() {
        rejects("nan start", |t| first(t).timeline_start = f64::NAN);
        rejects("inf out", |t| first(t).source_out = f64::INFINITY);
        rejects("nan speed", |t| first(t).speed = f64::NAN);
        rejects("nan volume", |t| first(t).volume = f32::NAN);
        rejects("nan fade", |t| first(t).fade_in = f64::NAN);
        rejects("nan track gain", |t| t.tracks[1].volume = f32::INFINITY);
        rejects("nan overlay", |t| t.overlays[0].end = f64::NAN);
        rejects("nan marker", |t| {
            t.markers.push(crate::model::Marker {
                id: Uuid::new_v4(),
                time: f64::NAN,
                name: "m".into(),
                ..Default::default()
            });
        });
    }

    #[test]
    fn a_clip_with_no_footage_is_refused() {
        rejects("empty", |t| first(t).source_out = first(t).source_in);
        rejects("backwards", |t| first(t).source_in = 9.0);
    }

    #[test]
    fn negative_fades_and_transitions_are_refused() {
        rejects("fade", |t| first(t).fade_out = -1.0);
        rejects("transition", |t| {
            t.tracks[0].clips[1].transition_in = Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: -0.5,
            });
        });
    }

    #[test]
    fn an_overlay_that_ends_before_it_starts_is_refused() {
        rejects("overlay", |t| t.overlays[0].end = 0.5);
    }

    #[test]
    fn the_message_names_the_item() {
        let mut t = timeline();
        let track = t.tracks[0].name.clone();
        t.tracks[0].clips[1].fade_out = -1.0;
        let err = t.check().unwrap_err().to_string();
        assert!(
            err.contains(&format!("the clip at 0:04.0 on {track} has a negative fade")),
            "{err}"
        );
        let mut t = timeline();
        t.overlays[0].end = 0.5;
        let err = t.check().unwrap_err().to_string();
        assert!(
            err.contains("the text \u{201c}hi\u{201d} at 0:01.0 must end after it starts"),
            "{err}"
        );
    }

    #[test]
    fn what_was_already_broken_is_forgiven_and_nothing_else() {
        let mut t = timeline();
        t.overlays[0].end = t.overlays[0].start;
        let known = t.known_violations();
        assert!(t.check().is_err());
        t.check_new(&known).unwrap();
        // Editing the broken item without mending it is still allowed.
        t.overlays[0].text = "renamed".into();
        t.check_new(&known).unwrap();
        // A new break is not.
        first(&mut t).fade_in = -1.0;
        assert!(t.check_new(&known).is_err());
    }

    #[test]
    fn an_id_used_twice_is_refused() {
        rejects("clip", |t| {
            let copy = t.tracks[0].clips[0].clone();
            t.tracks[0].clips.push(copy);
        });
        rejects("clip across tracks", |t| {
            let copy = t.tracks[0].clips[0].clone();
            t.tracks[1].clips.push(copy);
        });
        rejects("track", |t| {
            let mut copy = Track::new(StreamKind::Audio, "A2");
            copy.id = t.tracks[1].id;
            t.tracks.push(copy);
        });
        rejects("overlay", |t| {
            let copy = t.overlays[0].clone();
            t.overlays.push(copy);
        });
    }
}
