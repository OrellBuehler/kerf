//! The invariants every stored timeline holds, checked before an edit is committed.
//!
//! Deliberately narrow: only states no op means to make and no reader can handle — a time that
//! is not a number, a clip with no footage, two things with one id. Overlaps on a track are not
//! among them: Kerf allows them (an explicit `add_clip`, a trim into the next clip), and the
//! ripple already treats an overlap the edit did not make as old news.

use std::collections::HashSet;

use super::{Clip, Timeline};
use crate::error::{Error, Result};

impl Timeline {
    /// Refuse a timeline no edit should leave behind: a non-finite time, gain or speed; a clip
    /// whose out-point is not after its in-point; a negative fade or transition; an overlay that
    /// ends before it starts; or an id used twice. Pure — `Project::run_edit` calls it before the
    /// edit is committed, so a failing edit persists nothing.
    pub fn check(&self) -> Result<()> {
        let mut tracks = HashSet::new();
        let mut clips = HashSet::new();
        for track in &self.tracks {
            if !tracks.insert(track.id) {
                return invalid(format!("track {} appears twice", track.id));
            }
            if !track.volume.is_finite() || !track.pan.is_finite() {
                return invalid(format!("track {} has a gain that is not a number", track.name));
            }
            for clip in &track.clips {
                if !clips.insert(clip.id) {
                    return invalid(format!("clip {} appears twice", clip.id));
                }
                check_clip(clip)?;
            }
        }
        let mut overlays = HashSet::new();
        for o in &self.overlays {
            if !overlays.insert(o.id) {
                return invalid(format!("overlay {} appears twice", o.id));
            }
            if !o.start.is_finite() || !o.end.is_finite() || o.end <= o.start {
                return invalid(format!("overlay {} must end after it starts", o.id));
            }
            if o.keyframes.iter().any(|k| !k.time.is_finite()) {
                return invalid(format!("overlay {} has a keyframe at a time that is not a number", o.id));
            }
        }
        for o in &self.svg_overlays {
            if !overlays.insert(o.id) {
                return invalid(format!("animated title {} appears twice", o.id));
            }
            if !o.start.is_finite() || !o.end.is_finite() || o.end <= o.start {
                return invalid(format!("animated title {} must end after it starts", o.id));
            }
        }
        if self.markers.iter().any(|m| !m.time.is_finite()) {
            return invalid("a marker is at a time that is not a number".to_string());
        }
        Ok(())
    }
}

fn check_clip(c: &Clip) -> Result<()> {
    let id = c.id;
    if !(c.source_in.is_finite() && c.source_out.is_finite() && c.timeline_start.is_finite()) {
        return invalid(format!("clip {id} has a time that is not a number"));
    }
    if c.source_out <= c.source_in {
        return invalid(format!("clip {id} must end after it starts in its source"));
    }
    if !c.speed.is_finite() || !c.volume.is_finite() {
        return invalid(format!("clip {id} has a speed or volume that is not a number"));
    }
    if !(c.fade_in.is_finite() && c.fade_in >= 0.0 && c.fade_out.is_finite() && c.fade_out >= 0.0) {
        return invalid(format!("clip {id} has a negative fade"));
    }
    if let Some(t) = &c.transition_in {
        if !t.duration.is_finite() || t.duration < 0.0 {
            return invalid(format!("clip {id} has a negative transition"));
        }
    }
    if c.keyframes.iter().any(|k| !k.time.is_finite()) {
        return invalid(format!("clip {id} has a keyframe at a time that is not a number"));
    }
    Ok(())
}

fn invalid(why: String) -> Result<()> {
    Err(Error::InvalidArgument(format!(
        "that edit would leave the timeline invalid: {why}"
    )))
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
                color: None,
            })
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
            })
        });
    }

    #[test]
    fn an_overlay_that_ends_before_it_starts_is_refused() {
        rejects("overlay", |t| t.overlays[0].end = 0.5);
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
