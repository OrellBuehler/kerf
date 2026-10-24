//! Speed ramps at the `Project` level: one revision per edit, the length change rippling, the same ramp
//! on a linked sound, a constant speed replacing a ramp, a head trim carrying it, and staging.

use super::linked_tests::{asset, stream};
use super::*;
use crate::model::{EditSource, RampEasing, SpeedKey};

fn av_project() -> (Project, Asset) {
    let project = Project::open_in_memory().unwrap();
    let a = asset("/av.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]);
    project.insert_asset(&a).unwrap();
    (project, a)
}

fn timeline(p: &Project) -> Timeline {
    p.timeline().unwrap()
}

fn clip_of(p: &Project, id: Uuid) -> Clip {
    timeline(p).clip(id).cloned().unwrap_or_else(|| panic!("clip {id} is gone"))
}

fn revisions(p: &Project) -> usize {
    p.history().unwrap().len()
}

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} is not {b}");
}

/// 1x for a second, then down to a quarter over a second, then held there.
fn slow_down() -> Vec<SpeedKey> {
    vec![SpeedKey::new(1.0, 1.0), SpeedKey::new(2.0, 0.25)]
}

/// Source seconds 0..10 at 0, then 20..26 at 10 — two shots on V1.
fn two_shots() -> (Project, Uuid, Clip, Clip) {
    let (project, asset) = av_project();
    let c1 = project.add_clip_to_timeline(asset.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let c2 = project.add_clip_to_timeline(asset.id, None, 20.0, 26.0, Some(10.0)).unwrap();
    (project, asset.id, c1, c2)
}

#[test]
fn a_ramp_is_one_revision_that_keeps_the_window_changes_the_length_and_undoes() {
    let (p, _, c1, c2) = two_shots();
    let before = revisions(&p);
    let set = p.set_speed_ramp(c1.id, slow_down()).unwrap();
    assert_eq!(revisions(&p), before + 1);
    assert_eq!(p.history().unwrap().last().unwrap().label, "Set speed ramp");
    assert_eq!((set.source_in, set.source_out, set.speed), (0.0, 10.0, 1.0));
    assert_eq!(set.speed_ramp, slow_down());
    // 1 s at 1x, 1 s sliding to 0.25x (0.625 source s), then 8.375 source s at a quarter: 33.5 s.
    near(set.duration(), 1.0 + 1.0 + 8.375 / 0.25);
    near(clip_of(&p, c1.id).duration(), set.duration());
    // Without ripple the next clip stays where it was.
    assert_eq!(clip_of(&p, c2.id).timeline_start, 10.0);

    p.undo().unwrap();
    let back = clip_of(&p, c1.id);
    assert!(back.speed_ramp.is_empty());
    near(back.duration(), 10.0);
}

#[test]
fn a_ramp_is_a_length_change_so_ripple_mode_follows_it() {
    let (p, _, c1, c2) = two_shots();
    p.set_ripple_mode(true).unwrap();
    let set = p.set_speed_ramp(c1.id, slow_down()).unwrap();
    near(clip_of(&p, c2.id).timeline_start, set.timeline_end());
    p.set_speed_ramp(c1.id, Vec::new()).unwrap();
    near(clip_of(&p, c2.id).timeline_start, 10.0);
}

#[test]
fn clearing_needs_a_ramp_and_bad_keys_and_locked_tracks_change_nothing() {
    let (p, _, c1, _) = two_shots();
    let refused = |keys: Vec<SpeedKey>| p.set_speed_ramp(c1.id, keys).unwrap_err().to_string();
    assert!(refused(Vec::new()).contains("no speed ramp to clear"));
    assert!(refused(vec![SpeedKey::new(0.0, 0.0)]).contains("speed"));
    assert!(refused(vec![SpeedKey::new(f64::NAN, 1.0)]).contains("time"));
    assert!(refused(vec![SpeedKey::new(1.0, 1.0), SpeedKey::new(1.0, 2.0)]).contains("two speed ramp keys"));
    assert!(p
        .set_speed_ramp(Uuid::new_v4(), slow_down())
        .unwrap_err()
        .to_string()
        .contains("not found"));
    let v1 = timeline(&p).tracks[0].id;
    p.set_track_locked(v1, true).unwrap();
    let (revs, json) = (revisions(&p), p.timeline_json().unwrap());
    assert!(refused(slow_down()).contains("locked"));
    assert!(refused(Vec::new()).contains("locked") || refused(Vec::new()).contains("no speed ramp"));
    assert_eq!(
        (revisions(&p), p.timeline_json().unwrap()),
        (revs, json),
        "a refusal records nothing"
    );
}

#[test]
fn a_constant_speed_replaces_the_ramp() {
    let (p, _, c1, _) = two_shots();
    p.set_speed_ramp(c1.id, slow_down()).unwrap();
    let set = p.set_speed(c1.id, 2.0).unwrap();
    assert!(set.speed_ramp.is_empty());
    near(set.duration(), 5.0);
    assert!(clip_of(&p, c1.id).speed_ramp.is_empty());
    // Even to the speed it already had: the ramp goes.
    p.set_speed_ramp(c1.id, slow_down()).unwrap();
    p.set_speed(c1.id, 2.0).unwrap();
    assert!(clip_of(&p, c1.id).speed_ramp.is_empty());
    // Reversing keeps the direction's sign in the ramp's place.
    p.set_speed_ramp(c1.id, slow_down()).unwrap();
    p.set_speed(c1.id, -1.0).unwrap();
    assert!(clip_of(&p, c1.id).speed_ramp.is_empty() && clip_of(&p, c1.id).is_reversed());
}

#[test]
fn a_ramp_keeps_the_clips_direction() {
    let (p, _, c1, _) = two_shots();
    p.set_speed(c1.id, -1.0).unwrap();
    let set = p.set_speed_ramp(c1.id, slow_down()).unwrap();
    assert!(set.is_reversed());
    near(set.timeline_to_source(set.timeline_start), 10.0);
    near(set.timeline_to_source(set.timeline_end()), 0.0);
    // Clearing it goes back to the speed the clip had, direction and all.
    let cleared = p.set_speed_ramp(c1.id, Vec::new()).unwrap();
    assert_eq!(cleared.speed, -1.0);
}

/// A picture and its detached sound, a 10 s shot with a 6 s one after it.
fn detached() -> (Project, [Clip; 2], [Clip; 2]) {
    let (p, _, c1, c2) = two_shots();
    let a1 = p.detach_audio(c1.id).unwrap().clip;
    let a2 = p.detach_audio(c2.id).unwrap().clip;
    (p, [c1, c2], [a1, a2])
}

#[test]
fn a_linked_sound_gets_the_same_ramp_in_the_same_revision() {
    let (p, c, a) = detached();
    let before = revisions(&p);
    p.set_speed_ramp(c[0].id, slow_down()).unwrap();
    assert_eq!(revisions(&p), before + 1, "picture and sound are one edit");
    let (picture, sound) = (clip_of(&p, c[0].id), clip_of(&p, a[0].id));
    assert_eq!(sound.speed_ramp, slow_down());
    near(sound.duration(), picture.duration());
    near(sound.timeline_end(), picture.timeline_end());
    // The next pair is untouched, and ripple off leaves it where it was.
    assert!(clip_of(&p, c[1].id).speed_ramp.is_empty() && clip_of(&p, a[1].id).speed_ramp.is_empty());
    p.undo().unwrap();
    assert!(clip_of(&p, a[0].id).speed_ramp.is_empty());
}

#[test]
fn ripple_closes_both_tracks_by_the_pairs_new_length() {
    let (p, c, a) = detached();
    p.set_ripple_mode(true).unwrap();
    let set = p.set_speed_ramp(c[0].id, slow_down()).unwrap();
    near(clip_of(&p, c[1].id).timeline_start, set.timeline_end());
    near(clip_of(&p, a[1].id).timeline_start, set.timeline_end());
    near(clip_of(&p, a[0].id).timeline_end(), set.timeline_end());
}

#[test]
fn the_links_off_ramps_the_named_clip_alone() {
    let (p, c, a) = detached();
    p.with_links(Some(false), |p| p.set_speed_ramp(c[0].id, slow_down())).unwrap();
    assert_eq!(clip_of(&p, c[0].id).speed_ramp, slow_down());
    assert!(clip_of(&p, a[0].id).speed_ramp.is_empty());
    // Put the sound on it alone too, and a clear that links reach clears the group...
    p.with_links(Some(false), |p| p.set_speed_ramp(a[0].id, slow_down())).unwrap();
    p.set_speed_ramp(c[0].id, Vec::new()).unwrap();
    assert!(clip_of(&p, c[0].id).speed_ramp.is_empty() && clip_of(&p, a[0].id).speed_ramp.is_empty());
    // ...and a constant speed replaces every ramp.
    p.set_speed_ramp(c[0].id, slow_down()).unwrap();
    assert_eq!(clip_of(&p, a[0].id).speed_ramp, slow_down());
    p.set_speed(a[0].id, 2.0).unwrap();
    assert!(clip_of(&p, c[0].id).speed_ramp.is_empty() && clip_of(&p, a[0].id).speed_ramp.is_empty());
}

#[test]
fn trimming_the_head_of_a_ramped_pair_keeps_both_on_the_footage_they_showed() {
    let (p, c, a) = detached();
    p.set_speed_ramp(c[0].id, vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(2.0, 0.25)])
        .unwrap();
    let was = clip_of(&p, c[0].id);
    // The GUI's left-edge trim: a new in-point and the start moved to match (1.5 source s in).
    let head = was.local_at_along(1.5);
    let trimmed = p.trim(c[0].id, Some(1.5), None, Some(head)).unwrap();
    near(trimmed.timeline_end(), was.timeline_end());
    assert_eq!(trimmed.speed_ramp[0].time, 0.0, "the speed the clip opens at is pinned");
    near(trimmed.speed_ramp[0].speed, was.speed_at(head));
    for i in 0..=20 {
        let t = trimmed.timeline_start + trimmed.duration() * i as f64 / 20.0;
        near(trimmed.timeline_to_source(t), was.timeline_to_source(t));
    }
    let sound = clip_of(&p, a[0].id);
    assert_eq!(
        sound.speed_ramp, trimmed.speed_ramp,
        "the sound's head moved with the picture's"
    );
    near(sound.timeline_start, trimmed.timeline_start);
    near(sound.source_in, trimmed.source_in);
    // The tail is the plain case: the ramp stays where it is.
    let again = p.trim(c[0].id, None, Some(9.0), None).unwrap();
    assert_eq!(again.speed_ramp, trimmed.speed_ramp);
}

#[test]
fn splitting_a_ramped_pair_keeps_each_half_on_its_footage() {
    let (p, c, a) = detached();
    p.set_speed_ramp(c[0].id, slow_down()).unwrap();
    let was = clip_of(&p, c[0].id);
    let (left, right) = p.split_at(c[0].id, 2.5).unwrap();
    for half in [&left, &right] {
        for i in 0..=20 {
            let t = half.timeline_start + half.duration() * i as f64 / 20.0;
            near(half.timeline_to_source(t), was.timeline_to_source(t));
        }
    }
    near(right.speed_at(0.0), was.speed_at(2.5));
    let sound_right = clip_of(&p, p.timeline().unwrap().link_partners(right.id)[0]);
    assert_eq!(sound_right.speed_ramp, right.speed_ramp);
    assert!(clip_of(&p, a[0].id).speed_ramp == left.speed_ramp);
}

#[test]
fn the_diff_a_reviewer_reads_names_the_ramp() {
    let (mut p, _, c1, _) = two_shots();
    p.set_actor(EditSource::Agent);
    p.set_agent_review(false);
    p.begin_staging(None, None, None).unwrap();
    p.set_speed_ramp(c1.id, slow_down()).unwrap();
    assert!(clip_of(&p, c1.id).speed_ramp.is_empty(), "the live cut never moved");
    let staged = p.staged().unwrap().unwrap();
    assert!(
        staged.diff.summary().contains("speed ramp 2 keys, 0.25×–1.00×")
            || staged.diff.entries.iter().any(|e| e
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("speed ramp 2 keys, 0.25×–1.00×"))),
        "{:?}",
        staged.diff
    );
    p.apply_staged(false).unwrap();
    assert_eq!(clip_of(&p, c1.id).speed_ramp, slow_down());
}

#[test]
fn a_hold_survives_the_round_trip_through_the_store() {
    let (p, _, c1, _) = two_shots();
    let keys = vec![
        SpeedKey {
            time: 0.5,
            speed: 0.3,
            easing: RampEasing::Hold,
        },
        SpeedKey::new(1.5, 2.0),
    ];
    p.set_speed_ramp(c1.id, keys.clone()).unwrap();
    assert_eq!(clip_of(&p, c1.id).speed_ramp, keys);
    let json = p.timeline_json().unwrap();
    assert!(json.contains("\"hold\""), "{json}");
    let linear = serde_json::to_value(SpeedKey::new(0.0, 1.0)).unwrap();
    assert!(linear.get("easing").is_none(), "a plain key writes no easing: {linear}");
}

#[test]
fn music_fit_needs_a_clip_at_normal_speed() {
    let project = Project::open_in_memory().unwrap();
    let music = asset("/music.wav", 120.0, vec![stream(StreamKind::Audio)]);
    project.insert_asset(&music).unwrap();
    let a1 = timeline(&project).tracks[1].id;
    let clip = project
        .add_clip_to_timeline(music.id, Some(a1), 0.0, 30.0, Some(0.0))
        .unwrap();
    project.set_speed_ramp(clip.id, slow_down()).unwrap();
    let err = project.plan_music_fit(clip.id, Some(20.0)).unwrap_err().to_string();
    assert!(err.contains("normal speed"), "{err}");
}
