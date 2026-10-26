//! Blend modes at the `Project` level: one revision that undoes, the field carried by a split, a
//! duplicate and a paste, the store round trip, the diff a reviewer reads, and a sound refused.

use super::linked_tests::{asset, stream};
use super::*;
use crate::model::{BlendMode, EditSource};

fn shot() -> (Project, Clip) {
    let project = Project::open_in_memory().unwrap();
    let a = asset("/av.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]);
    project.insert_asset(&a).unwrap();
    let clip = project.add_clip_to_timeline(a.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    (project, clip)
}

fn mode_of(p: &Project, id: Uuid) -> BlendMode {
    p.timeline().unwrap().clip(id).expect("the clip is gone").blend_mode
}

#[test]
fn a_blend_mode_is_one_revision_that_undoes() {
    let (p, clip) = shot();
    let before = p.history().unwrap().len();
    let set = p.set_blend_mode(clip.id, BlendMode::Screen).unwrap();
    assert_eq!(set.blend_mode, BlendMode::Screen);
    assert_eq!(p.history().unwrap().len(), before + 1);
    assert_eq!(p.history().unwrap().last().unwrap().label, "Set blend mode");
    p.undo().unwrap();
    assert_eq!(mode_of(&p, clip.id), BlendMode::Normal);
    p.redo().unwrap();
    assert_eq!(mode_of(&p, clip.id), BlendMode::Screen);
}

#[test]
fn split_duplicate_and_paste_carry_the_mode() {
    let (p, clip) = shot();
    p.set_blend_mode(clip.id, BlendMode::Multiply).unwrap();
    let (left, right) = p.split_at(clip.id, 4.0).unwrap();
    assert_eq!(
        (left.blend_mode, right.blend_mode),
        (BlendMode::Multiply, BlendMode::Multiply)
    );
    let copies = p.duplicate_clips(&[right.id], 20.0).unwrap();
    assert_eq!(copies[0].blend_mode, BlendMode::Multiply);
    let track = p.timeline().unwrap().tracks[0].id;
    let pasted = p.insert_clips(&[(track, left)], 40.0).unwrap();
    assert_eq!(pasted[0].blend_mode, BlendMode::Multiply);
}

#[test]
fn the_mode_survives_the_store_and_normal_writes_nothing() {
    let (p, clip) = shot();
    assert!(
        !p.timeline_json().unwrap().contains("blend_mode"),
        "normal is the default and omitted"
    );
    p.set_blend_mode(clip.id, BlendMode::HardLight).unwrap();
    let json = p.timeline_json().unwrap();
    assert!(json.contains("\"blend_mode\":\"hard_light\""), "{json}");
    let back: Timeline = serde_json::from_str(&json).unwrap();
    assert_eq!(back.clip(clip.id).unwrap().blend_mode, BlendMode::HardLight);
    p.set_blend_mode(clip.id, BlendMode::Normal).unwrap();
    assert!(!p.timeline_json().unwrap().contains("blend_mode"));
}

#[test]
fn a_clip_of_sound_has_no_picture_to_blend() {
    let (p, clip) = shot();
    let partners = p.detach_audio(clip.id).unwrap();
    let before = p.history().unwrap().len();
    let err = p.set_blend_mode(partners.clip.id, BlendMode::Screen).unwrap_err();
    assert!(err.to_string().contains("video track"), "{err}");
    assert_eq!(p.history().unwrap().len(), before, "a refused edit records nothing");
}

#[test]
fn the_diff_a_reviewer_reads_names_the_mode() {
    let (mut p, clip) = shot();
    p.set_actor(EditSource::Agent);
    p.set_agent_review(false);
    p.begin_staging(None, None, None).unwrap();
    p.set_blend_mode(clip.id, BlendMode::Overlay).unwrap();
    assert_eq!(mode_of(&p, clip.id), BlendMode::Normal, "the live cut never moved");
    let staged = p.staged().unwrap().unwrap();
    assert!(
        staged
            .diff
            .entries
            .iter()
            .any(|e| e.detail.as_deref().is_some_and(|d| d.contains("blend normal → overlay"))),
        "{:?}",
        staged.diff
    );
    p.apply_staged(false).unwrap();
    assert_eq!(mode_of(&p, clip.id), BlendMode::Overlay);
}
