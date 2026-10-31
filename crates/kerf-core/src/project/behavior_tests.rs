//! Live behaviours at the project level: one revision for several clips, undone whole, refused whole,
//! staged for an agent, carried through a split, and baked into keys.

use super::linked_tests::{asset, stream};
use super::*;
use crate::model::{Behavior, EditSource, MotionMode, MotionPreset, Property, MAX_BEHAVIORS};

fn two_shots() -> (Project, Clip, Clip) {
    let project = Project::open_in_memory().unwrap();
    let a = asset("/av.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]);
    project.insert_asset(&a).unwrap();
    let c1 = project.add_clip_to_timeline(a.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let c2 = project.add_clip_to_timeline(a.id, None, 20.0, 26.0, Some(10.0)).unwrap();
    (project, c1, c2)
}

fn sway() -> Behavior {
    Behavior::Sway {
        amount: 5.0,
        speed: 0.5,
        phase: 0.0,
    }
}

fn shake() -> Behavior {
    Behavior::Shake {
        amount: 0.01,
        speed: 3.0,
        seed: 9,
        phase: 0.0,
    }
}

fn behaviors(p: &Project, id: Uuid) -> Vec<Behavior> {
    p.working_timeline()
        .unwrap()
        .clip(id)
        .expect("the clip is gone")
        .behaviors
        .clone()
}

fn revisions(p: &Project) -> usize {
    p.history().unwrap().len()
}

#[test]
fn behaviours_on_several_clips_are_one_revision_that_undoes() {
    let (p, c1, c2) = two_shots();
    let before = revisions(&p);
    let done = p.set_behaviors(&[c1.id, c2.id], vec![sway(), shake()]).unwrap();
    assert_eq!(revisions(&p), before + 1);
    assert_eq!(p.history().unwrap().last().unwrap().label, "Set behaviours (2 clips)");
    assert_eq!(done.len(), 2);
    assert_eq!(behaviors(&p, c1.id), vec![sway(), shake()]);
    assert_eq!(behaviors(&p, c2.id), vec![sway(), shake()]);
    p.undo().unwrap();
    assert!(behaviors(&p, c1.id).is_empty() && behaviors(&p, c2.id).is_empty());
    p.redo().unwrap();
    assert_eq!(behaviors(&p, c2.id).len(), 2);

    // Replacing, then clearing: an empty list is the clear.
    p.set_behaviors(&[c1.id], vec![sway()]).unwrap();
    assert_eq!(behaviors(&p, c1.id), vec![sway()], "the list is replaced, not appended to");
    assert_eq!(p.history().unwrap().last().unwrap().label, "Set behaviours");
    p.set_behaviors(&[c1.id], Vec::new()).unwrap();
    assert!(behaviors(&p, c1.id).is_empty());
    assert_eq!(p.history().unwrap().last().unwrap().label, "Clear behaviours");
    assert_eq!(behaviors(&p, c2.id).len(), 2, "the other clip is as it was");
}

#[test]
fn a_refusal_writes_nothing_at_all() {
    let (p, c1, c2) = two_shots();
    let before = revisions(&p);
    let ghost = Uuid::new_v4();
    let err = p.set_behaviors(&[c1.id, ghost], vec![sway()]).unwrap_err();
    assert!(matches!(err, Error::ClipNotFound(id) if id == ghost), "{err}");
    assert!(p.set_behaviors(&[], vec![sway()]).is_err());
    for bad in [
        vec![sway(); MAX_BEHAVIORS + 1],
        vec![Behavior::Sway {
            amount: f64::NAN,
            speed: 1.0,
            phase: 0.0,
        }],
        vec![Behavior::Breathe {
            amount: 0.95,
            speed: 1.0,
            phase: 0.0,
        }],
        vec![Behavior::Shake {
            amount: 0.01,
            speed: 99.0,
            seed: 0,
            phase: 0.0,
        }],
    ] {
        let err = p.set_behaviors(&[c1.id, c2.id], bad).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "{err}");
    }
    assert_eq!(revisions(&p), before, "a refused edit records nothing");
    assert!(behaviors(&p, c1.id).is_empty());
    assert!(
        p.set_behaviors(&[c1.id, c1.id], vec![sway()]).is_ok(),
        "a clip named twice is written once"
    );
    assert_eq!(p.history().unwrap().last().unwrap().label, "Set behaviours");
}

#[test]
fn a_sound_has_no_picture_to_move_and_a_lock_guards_only_the_layout() {
    let (p, c1, c2) = two_shots();
    let partners = p.detach_audio(c1.id).unwrap();
    let before = revisions(&p);
    let err = p.set_behaviors(&[c2.id, partners.clip.id], vec![sway()]).unwrap_err();
    assert!(err.to_string().contains("audio track"), "{err}");
    assert_eq!(revisions(&p), before);
    assert!(
        behaviors(&p, c2.id).is_empty(),
        "the picture named beside it was not written either"
    );

    let track = p.timeline().unwrap().tracks[0].id;
    p.set_track_locked(track, true).unwrap();
    p.set_behaviors(&[c1.id], vec![sway()]).unwrap();
    let t = p.timeline().unwrap();
    assert!(t.tracks[0].locked, "the track is still locked");
    assert_eq!(t.clip(c1.id).unwrap().behaviors, vec![sway()]);
    assert!(
        t.clip(partners.clip.id).unwrap().behaviors.is_empty(),
        "a linked partner is not carried"
    );
}

#[test]
fn they_are_stored_as_a_plain_field_and_read_back() {
    let (p, c1, c2) = two_shots();
    assert!(!p.timeline_json().unwrap().contains("behaviors"), "none is written");
    p.set_behaviors(&[c1.id], vec![sway(), shake()]).unwrap();
    let json = p.timeline_json().unwrap();
    assert!(
        json.contains(r#""behaviors":[{"kind":"sway","amount":5.0,"speed":0.5,"phase":0.0},{"kind":"shake""#),
        "{json}"
    );
    assert_eq!(json.matches("behaviors").count(), 1, "the other clip carries none");
    let back: Timeline = serde_json::from_str(&json).unwrap();
    assert_eq!(back.clip(c1.id).unwrap().behaviors, vec![sway(), shake()]);
    assert!(back.clip(c2.id).unwrap().behaviors.is_empty());
}

#[test]
fn an_agents_behaviours_stage_and_the_diff_says_what_they_are() {
    let (mut p, c1, _) = two_shots();
    p.set_actor(EditSource::Agent);
    p.set_agent_review(true);
    p.set_behaviors(&[c1.id], vec![sway()]).unwrap();
    assert!(
        p.timeline().unwrap().clip(c1.id).unwrap().behaviors.is_empty(),
        "the live cut never moved"
    );
    assert_eq!(behaviors(&p, c1.id), vec![sway()], "the proposal holds it");
    p.set_behaviors(&[c1.id], vec![sway(), shake()]).unwrap();
    let staged = p.staged().unwrap().unwrap();
    let phrases: Vec<&str> = staged.diff.entries.iter().filter_map(|e| e.detail.as_deref()).collect();
    assert!(
        phrases.iter().any(|d| d.contains("behaviours: sway added, shake added")),
        "{phrases:?}"
    );
    p.set_actor(EditSource::User);
    p.apply_staged(false).unwrap();
    assert_eq!(p.timeline().unwrap().clip(c1.id).unwrap().behaviors.len(), 2);
    assert_eq!(p.history().unwrap().last().unwrap().source, EditSource::Agent);
}

#[test]
fn a_split_carries_the_motion_on_in_the_right_half() {
    let (p, c1, _) = two_shots();
    p.set_property_keyframes(
        c1.id,
        Property::PosX,
        vec![PropertyKey::new(0.0, -0.2), PropertyKey::new(10.0, 0.2)],
    )
    .unwrap();
    p.set_behaviors(&[c1.id], vec![sway(), shake()]).unwrap();
    let whole = p.timeline().unwrap().clip(c1.id).unwrap().clone();
    for at in [0.7, 3.25, 5.0, 9.4] {
        let (left, right) = p.split_at(c1.id, at).unwrap();
        assert_eq!(left.behaviors.len(), 2, "both halves keep the behaviours");
        assert_eq!(right.behaviors.len(), 2);
        for i in 0..40 {
            let t = f64::from(i) * 0.07;
            let (a, b) = (left.transform_at(t.min(at)), whole.transform_at(t.min(at)));
            assert!(
                (a.rotation - b.rotation).abs() < 1e-9 && (a.pos_x - b.pos_x).abs() < 1e-9,
                "left at {t}"
            );
            let (a, b) = (right.transform_at(t), whole.transform_at(t + at));
            assert!(
                (a.rotation - b.rotation).abs() < 1e-9,
                "right at {t}: {} vs {}",
                a.rotation,
                b.rotation
            );
            assert!(
                (a.pos_x - b.pos_x).abs() < 1e-9 && (a.pos_y - b.pos_y).abs() < 1e-9,
                "right at {t}"
            );
        }
        p.undo().unwrap();
    }
}

#[test]
fn keys_written_on_top_of_behaviours_are_the_pose_not_the_wobble() {
    let (p, c1, _) = two_shots();
    p.set_behaviors(&[c1.id], vec![sway(), shake()]).unwrap();
    // A new keyframe at a moment: the other numbers are the static pose, not the swaying one.
    let clip = p.add_keyframe(c1.id, 1.3, Some(1.5), None, None, None, None).unwrap();
    let key = clip.keyframes[0];
    assert_eq!((key.pos_x, key.pos_y, key.rotation), (0.0, 0.0, 0.0), "{key:?}");
    // A motion preset reads the pose: a pop-in ends on the clip's own scale, wobble or no.
    p.apply_motion_preset(&[c1.id], MotionPreset::SlideInFromLeft, MotionMode::Add, None, None, None)
        .unwrap();
    let t = p.timeline().unwrap();
    let c = t.clip(c1.id).unwrap();
    let x = c.property_keys(Property::PosX);
    assert_eq!(
        x.last().unwrap().value,
        0.0,
        "ends on the static pose, not on where the shake was"
    );
    assert!(x.iter().all(|k| k.value.abs() < 2.0));
    assert_eq!(c.behaviors.len(), 2, "the behaviours are still there on top");
}

#[test]
fn baking_is_one_revision_exact_at_the_frames() {
    let (p, c1, c2) = two_shots();
    p.set_behaviors(&[c1.id], vec![sway(), shake()]).unwrap();
    let live = p.timeline().unwrap().clip(c1.id).unwrap().clone();
    let before = revisions(&p);
    let done = p.bake_behaviors(&[c1.id], Some(25.0)).unwrap();
    assert_eq!(revisions(&p), before + 1);
    assert_eq!(p.history().unwrap().last().unwrap().label, "Bake behaviours");
    assert_eq!(done[0].baked.fps, 25.0);
    assert_eq!(
        done[0].baked.properties,
        vec![Property::PosX, Property::PosY, Property::Rotation]
    );
    let baked = p.timeline().unwrap().clip(c1.id).unwrap().clone();
    assert!(baked.behaviors.is_empty());
    for k in 0..250 {
        let t = f64::from(k) / 25.0;
        let (a, b) = (live.transform_at(t), baked.transform_at(t));
        assert!(
            (a.rotation - b.rotation).abs() < 1e-9 && (a.pos_x - b.pos_x).abs() < 1e-9 && (a.pos_y - b.pos_y).abs() < 1e-9,
            "frame {k}"
        );
    }
    p.undo().unwrap();
    assert_eq!(behaviors(&p, c1.id).len(), 2, "undo brings the behaviours back");

    // Nothing to bake, and a clip too long for its keys: refused whole, nothing written.
    let before = revisions(&p);
    let err = p.bake_behaviors(&[c1.id, c2.id], Some(25.0)).unwrap_err();
    assert!(err.to_string().contains("no behaviours"), "{err}");
    let err = p.bake_behaviors(&[c1.id], Some(120.0)).unwrap_err();
    assert!(err.to_string().contains("split the clip"), "{err}");
    assert!(p.bake_behaviors(&[], None).is_err());
    assert_eq!(revisions(&p), before);
    assert_eq!(behaviors(&p, c1.id).len(), 2);
}

#[test]
fn a_bake_without_a_rate_follows_the_delivery_then_the_footage() {
    let (p, c1, _) = two_shots();
    p.set_behaviors(&[c1.id], vec![sway()]).unwrap();
    // No delivery rate: the footage's (the test stream's), which is 30.
    let a = p.bake_behaviors(&[c1.id], None).unwrap();
    assert_eq!(a[0].baked.fps, 30.0);
    p.undo().unwrap();
    p.set_delivery_format(Some(
        crate::model::Delivery::new(1920, 1080, crate::model::Fit::Contain)
            .with_fps(Some(24.0))
            .unwrap(),
    ))
    .unwrap();
    let b = p.bake_behaviors(&[c1.id], None).unwrap();
    assert_eq!(b[0].baked.fps, 24.0);
}
