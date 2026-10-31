//! Motion presets at the project level: one revision for several clips, undone whole, refused whole,
//! staged for an agent, and not stopped by a lock (a look edit, as every keyframe is).

use super::linked_tests::{asset, stream};
use super::*;
use crate::model::{EditSource, MotionMode, MotionPreset, Property};

fn two_shots() -> (Project, Clip, Clip) {
    let project = Project::open_in_memory().unwrap();
    let a = asset("/av.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]);
    project.insert_asset(&a).unwrap();
    let c1 = project.add_clip_to_timeline(a.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let c2 = project.add_clip_to_timeline(a.id, None, 20.0, 26.0, Some(10.0)).unwrap();
    (project, c1, c2)
}

fn fade_in(p: &Project, ids: &[Uuid]) -> Result<Vec<MotionApplied>> {
    p.apply_motion_preset(ids, MotionPreset::FadeIn, MotionMode::Add, None, None, None)
}

fn opacity(p: &Project, id: Uuid) -> Vec<(f64, f64)> {
    let t = p.working_timeline().unwrap();
    let c = t.clip(id).expect("the clip is gone");
    c.property_keys(Property::Opacity).iter().map(|k| (k.time, k.value)).collect()
}

fn revisions(p: &Project) -> usize {
    p.history().unwrap().len()
}

#[test]
fn a_preset_on_several_clips_is_one_revision_that_undoes() {
    let (p, c1, c2) = two_shots();
    let before = revisions(&p);
    let done = fade_in(&p, &[c1.id, c2.id]).unwrap();
    assert_eq!(revisions(&p), before + 1);
    assert_eq!(
        p.history().unwrap().last().unwrap().label,
        "Apply motion preset Fade in (2 clips)"
    );
    assert_eq!(done.len(), 2);
    assert_eq!((done[0].start, done[0].end), (0.0, 0.5));
    assert_eq!(done[1].clip.id, c2.id);
    assert_eq!(opacity(&p, c1.id), [(0.0, 0.0), (0.5, 1.0)]);
    assert_eq!(opacity(&p, c2.id), [(0.0, 0.0), (0.5, 1.0)]);
    p.undo().unwrap();
    assert!(opacity(&p, c1.id).is_empty() && opacity(&p, c2.id).is_empty());
    p.redo().unwrap();
    assert_eq!(opacity(&p, c2.id), [(0.0, 0.0), (0.5, 1.0)]);

    let one = p
        .apply_motion_preset(&[c2.id], MotionPreset::SlideOutToLeft, MotionMode::Add, None, Some(1.0), None)
        .unwrap();
    assert_eq!(
        (one[0].start, one[0].end),
        (5.0, 6.0),
        "the exit ends on the clip's end (6 s long)"
    );
    assert_eq!(
        p.history().unwrap().last().unwrap().label,
        "Apply motion preset Slide out to left"
    );
}

#[test]
fn a_clip_named_twice_is_written_once_and_the_window_is_clamped_to_each_clip() {
    let (p, c1, c2) = two_shots();
    let done = p
        .apply_motion_preset(&[c1.id, c1.id], MotionPreset::FadeOut, MotionMode::Add, None, Some(8.0), None)
        .unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(p.history().unwrap().last().unwrap().label, "Apply motion preset Fade out");
    assert_eq!((done[0].start, done[0].end), (2.0, 10.0));
    let both = p
        .apply_motion_preset(&[c1.id, c2.id], MotionPreset::FadeIn, MotionMode::Add, None, Some(8.0), None)
        .unwrap();
    assert_eq!((both[0].end, both[1].end), (8.0, 6.0), "the shorter clip is fully faded");
}

#[test]
fn a_refusal_writes_nothing_at_all() {
    let (p, c1, _) = two_shots();
    let before = revisions(&p);
    let ghost = Uuid::new_v4();
    let err = fade_in(&p, &[c1.id, ghost]).unwrap_err();
    assert!(matches!(err, Error::ClipNotFound(id) if id == ghost), "{err}");
    assert!(fade_in(&p, &[]).is_err());
    for (d, at, i) in [
        (Some(0.0), None, None),
        (Some(f64::NAN), None, None),
        (None, Some(-1.0), None),
        (None, Some(99.0), None),
        (None, None, Some(5.0)),
    ] {
        let err = p
            .apply_motion_preset(&[c1.id], MotionPreset::Pulse, MotionMode::Add, at, d, i)
            .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "{err}");
    }
    assert_eq!(revisions(&p), before, "a refused edit records nothing");
    assert!(p.timeline().unwrap().clip(c1.id).unwrap().channels.is_empty());
}

#[test]
fn a_sound_has_no_picture_to_move() {
    let (p, c1, c2) = two_shots();
    let partners = p.detach_audio(c1.id).unwrap();
    let before = revisions(&p);
    let err = fade_in(&p, &[c2.id, partners.clip.id]).unwrap_err();
    assert!(err.to_string().contains("audio track"), "{err}");
    assert_eq!(revisions(&p), before);
    assert!(
        opacity(&p, c2.id).is_empty(),
        "the picture named beside it was not written either"
    );
    // The picture is fine, and its sound is not carried along.
    fade_in(&p, &[c1.id]).unwrap();
    let t = p.timeline().unwrap();
    assert!(t.clip(partners.clip.id).unwrap().channels.is_empty());
    assert_eq!(opacity(&p, c1.id), [(0.0, 0.0), (0.5, 1.0)]);
}

#[test]
fn a_lock_guards_the_layout_not_the_look() {
    let (p, c1, _) = two_shots();
    let track = p.timeline().unwrap().tracks[0].id;
    p.set_track_locked(track, true).unwrap();
    let before = p.timeline().unwrap().clip(c1.id).unwrap().clone();
    fade_in(&p, &[c1.id]).unwrap();
    let t = p.timeline().unwrap();
    assert!(t.tracks[0].locked, "the track is still locked");
    let after = t.clip(c1.id).unwrap();
    assert_eq!(
        (after.timeline_start, after.source_in, after.source_out, after.enabled),
        (before.timeline_start, before.source_in, before.source_out, before.enabled)
    );
    assert_eq!(opacity(&p, c1.id), [(0.0, 0.0), (0.5, 1.0)]);
}

#[test]
fn ripple_mode_has_nothing_to_close() {
    let (p, c1, c2) = two_shots();
    p.set_ripple_mode(true).unwrap();
    fade_in(&p, &[c1.id]).unwrap();
    let t = p.timeline().unwrap();
    assert_eq!(t.clip(c2.id).unwrap().timeline_start, 10.0);
    assert_eq!(t.clip(c1.id).unwrap().duration(), 10.0);
}

#[test]
fn the_keys_are_stored_as_ordinary_channels() {
    let (p, c1, _) = two_shots();
    p.apply_motion_preset(&[c1.id], MotionPreset::PopIn, MotionMode::Add, None, None, None)
        .unwrap();
    let json = p.timeline_json().unwrap();
    assert!(json.contains(r#""channels":[{"prop":"scale""#), "{json}");
    assert!(!json.contains("motion"), "no new field: {json}");
    let back: Timeline = serde_json::from_str(&json).unwrap();
    assert_eq!(
        back.clip(c1.id).unwrap().channels,
        p.timeline().unwrap().clip(c1.id).unwrap().channels
    );
}

#[test]
fn an_agent_s_preset_stages_and_the_next_one_sees_it() {
    let (mut p, c1, _) = two_shots();
    p.set_actor(EditSource::Agent);
    p.set_agent_review(true);
    fade_in(&p, &[c1.id]).unwrap();
    assert!(
        p.timeline().unwrap().clip(c1.id).unwrap().channels.is_empty(),
        "the live cut never moved"
    );
    // The exit reads the proposal: it merges into the entrance the first call staged.
    p.apply_motion_preset(&[c1.id], MotionPreset::FadeOut, MotionMode::Add, None, None, None)
        .unwrap();
    assert_eq!(opacity(&p, c1.id), [(0.0, 0.0), (0.5, 1.0), (9.5, 1.0), (10.0, 0.0)]);
    assert!(p.timeline().unwrap().clip(c1.id).unwrap().channels.is_empty());
    let staged = p.staged().unwrap().unwrap();
    let phrases: Vec<&str> = staged.diff.entries.iter().filter_map(|e| e.detail.as_deref()).collect();
    assert!(phrases.iter().any(|d| d.contains("opacity keyframes 0 → 4")), "{phrases:?}");
    p.set_actor(EditSource::User);
    p.apply_staged(false).unwrap();
    let t = p.timeline().unwrap();
    assert_eq!(t.clip(c1.id).unwrap().property_keys(Property::Opacity).len(), 4);
    assert_eq!(
        p.history().unwrap().last().unwrap().source,
        EditSource::Agent,
        "credited to the agent that made it"
    );
}

#[test]
fn an_agent_with_review_off_lands_live_unless_a_proposal_is_open() {
    let (mut p, c1, _) = two_shots();
    p.set_actor(EditSource::Agent);
    p.set_agent_review(false);
    fade_in(&p, &[c1.id]).unwrap();
    assert_eq!(opacity(&p, c1.id), [(0.0, 0.0), (0.5, 1.0)]);
    assert!(p.staged().unwrap().is_none());
    assert_eq!(p.history().unwrap().last().unwrap().source, EditSource::Agent);
}

#[test]
fn a_track_a_preset_would_push_past_the_key_limit_is_refused() {
    let (p, c1, _) = two_shots();
    let keys: Vec<PropertyKey> = (0..MAX_CHANNEL_KEYS)
        .map(|i| PropertyKey::new(5.0 + i as f64 * 0.004, 1.0))
        .collect();
    p.set_property_keyframes(c1.id, Property::Opacity, keys).unwrap();
    let before = revisions(&p);
    let err = p
        .apply_motion_preset(&[c1.id], MotionPreset::FadeIn, MotionMode::Add, None, None, None)
        .unwrap_err();
    assert!(err.to_string().contains("at most 1000 keyframes"), "{err}");
    assert_eq!(revisions(&p), before);
    assert_eq!(opacity(&p, c1.id).len(), MAX_CHANNEL_KEYS);
    p.apply_motion_preset(&[c1.id], MotionPreset::FadeIn, MotionMode::Replace, None, None, None)
        .unwrap();
    assert_eq!(opacity(&p, c1.id), [(0.0, 0.0), (0.5, 1.0)]);
}
