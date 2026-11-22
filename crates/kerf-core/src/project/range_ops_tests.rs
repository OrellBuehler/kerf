//! Range edits at the `Project` level: cut every track, lift, extract and delete a gap are each one
//! revision, never rippled twice, carried to linked sound by the sync lock, staged for an agent, and
//! leave no trace when refused.

use super::linked_tests::{asset, stream};
use super::*;
use crate::model::EditSource;

fn av_project() -> (Project, Asset) {
    let project = Project::open_in_memory().unwrap();
    let a = asset("/av.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]);
    project.insert_asset(&a).unwrap();
    (project, a)
}

/// V1 holds two shots, `c1` source 0..10 at 0 and `c2` source 20..26 at `gap_to`, each with its sound
/// detached onto A1 and linked: two pairs with `gap_to - 10` seconds of nothing between them.
struct Cut {
    project: Project,
    c: [Uuid; 2],
    a: [Uuid; 2],
}

fn pairs(gap_to: f64) -> Cut {
    let (project, asset) = av_project();
    let c1 = project.add_clip_to_timeline(asset.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let c2 = project
        .add_clip_to_timeline(asset.id, None, 20.0, 26.0, Some(gap_to))
        .unwrap();
    let a1 = project.detach_audio(c1.id).unwrap().clip;
    let a2 = project.detach_audio(c2.id).unwrap().clip;
    Cut {
        project,
        c: [c1.id, c2.id],
        a: [a1.id, a2.id],
    }
}

fn timeline(p: &Project) -> Timeline {
    p.timeline().unwrap()
}

fn clip_of(p: &Project, id: Uuid) -> Clip {
    timeline(p).clip(id).cloned().unwrap_or_else(|| panic!("clip {id} is gone"))
}

fn span(p: &Project, id: Uuid) -> (f64, f64) {
    let c = clip_of(p, id);
    (c.timeline_start, c.timeline_end())
}

fn lane(p: &Project, track: usize) -> Vec<(f64, f64)> {
    timeline(p).tracks[track]
        .clips
        .iter()
        .map(|c| (c.timeline_start, c.timeline_end()))
        .collect()
}

fn revisions(p: &Project) -> usize {
    p.history().unwrap().len()
}

fn last_label(p: &Project) -> String {
    p.history().unwrap().last().unwrap().label.clone()
}

const V1: usize = 0;
const A1: usize = 1;

fn same(a: &Timeline, b: &Timeline) -> bool {
    serde_json::to_string(a).unwrap() == serde_json::to_string(b).unwrap()
}

#[test]
fn cutting_every_track_is_one_revision_that_undoes_as_one() {
    let cut = pairs(10.0);
    let p = &cut.project;
    let before = timeline(p);
    let revs = revisions(p);
    let done = p.split_all_tracks(4.0, None).unwrap();
    assert_eq!(
        done.cuts.len(),
        2,
        "the first shot's picture and its sound; the second starts at 10"
    );
    assert_eq!(revisions(p), revs + 1);
    assert_eq!(last_label(p), "Split all tracks (2 clips)");
    assert_eq!(lane(p, V1), vec![(0.0, 4.0), (4.0, 10.0), (10.0, 16.0)]);
    assert_eq!(lane(p, A1), lane(p, V1));
    p.undo().unwrap();
    assert!(same(&timeline(p), &before));
}

#[test]
fn a_lift_leaves_the_gap_and_an_extract_closes_it_whatever_ripple_mode_says() {
    for ripple in [false, true] {
        let cut = pairs(10.0);
        let p = &cut.project;
        p.set_ripple_mode(ripple).unwrap();
        let before = timeline(p);

        let revs = revisions(p);
        let lifted = p.lift_range(3.0, 7.0, None).unwrap();
        assert_eq!(revisions(p), revs + 1, "one revision (ripple {ripple})");
        assert_eq!(last_label(p), "Lift range (2 clips)");
        assert_eq!((lifted.trimmed, lifted.split), (0, 2), "{lifted:?}");
        assert_eq!(
            lane(p, V1),
            vec![(0.0, 3.0), (7.0, 10.0), (10.0, 16.0)],
            "the gap stays (ripple {ripple})"
        );
        assert_eq!(lane(p, A1), vec![(0.0, 3.0), (7.0, 10.0), (10.0, 16.0)]);
        p.undo().unwrap();
        assert!(same(&timeline(p), &before));

        let extracted = p.extract_range(3.0, 7.0, None).unwrap();
        assert_eq!(last_label(p), "Extract range (2 clips)");
        assert_eq!(extracted.closed, 4.0);
        let closed = vec![(0.0, 3.0), (3.0, 6.0), (6.0, 12.0)];
        assert_eq!(lane(p, V1), closed, "closed by the range once, not twice (ripple {ripple})");
        assert_eq!(lane(p, A1), closed);
        p.undo().unwrap();
        assert!(same(&timeline(p), &before));
    }
}

#[test]
fn cutting_every_track_shifts_nothing_under_ripple() {
    let cut = pairs(12.0);
    let p = &cut.project;
    p.set_ripple_mode(true).unwrap();
    let before = timeline(p);
    let revs = revisions(p);
    let done = p.split_all_tracks(4.0, None).unwrap();
    assert_eq!(done.cuts.len(), 2, "V1's shot and its sound");
    assert_eq!(revisions(p), revs + 1);
    assert_eq!(last_label(p), "Split all tracks (2 clips)");
    assert_eq!(lane(p, V1), vec![(0.0, 4.0), (4.0, 10.0), (12.0, 18.0)]);
    assert_eq!(lane(p, A1), lane(p, V1));
    p.undo().unwrap();
    assert!(same(&timeline(p), &before));
}

#[test]
fn a_refusal_records_nothing() {
    let cut = pairs(12.0);
    let p = &cut.project;
    let (before, revs) = (timeline(p), revisions(p));
    let v1 = before.tracks[V1].id;
    assert!(p.split_all_tracks(11.0, None).is_err(), "nothing is under 11");
    assert!(p.split_all_tracks(10.0, None).is_err(), "10 is already a cut");
    assert!(p.lift_range(10.5, 11.5, None).is_err());
    assert!(p.extract_range(30.0, 40.0, None).is_err());
    assert!(p.ripple_delete_gap(v1, 3.0).is_err(), "3 is inside a clip");
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(p.split_all_tracks(bad, None).is_err());
        assert!(p.lift_range(bad, 1.0, None).is_err());
        assert!(p.extract_range(0.0, bad, None).is_err());
        assert!(p.ripple_delete_gap(v1, bad).is_err());
    }
    assert!(p.lift_range(5.0, 2.0, None).is_err());
    assert!(p.lift_range(2.0, 4.0, Some(&[Uuid::new_v4()])).is_err());
    assert_eq!(revisions(p), revs);
    assert!(same(&timeline(p), &before));
}

#[test]
fn a_locked_track_is_left_alone_and_its_partner_refuses_the_edit() {
    let cut = pairs(12.0);
    let p = &cut.project;
    let a1 = timeline(p).tracks[A1].id;
    p.set_track_locked(a1, true).unwrap();
    let (before, revs) = (timeline(p), revisions(p));
    // V1's shot is linked to a sound on the locked A1: the group edit is refused whole.
    for err in [
        p.split_all_tracks(4.0, None).unwrap_err(),
        p.lift_range(3.0, 7.0, None).unwrap_err(),
        p.extract_range(3.0, 7.0, None).unwrap_err(),
    ] {
        assert!(err.to_string().contains("locked track A1"), "{err}");
    }
    assert_eq!((revisions(p), same(&timeline(p), &before)), (revs, true));
    // Told to leave the links alone, the picture is cut and the locked sound is not.
    p.with_links(Some(false), |p| p.lift_range(3.0, 7.0, None)).unwrap();
    assert_eq!(lane(p, V1), vec![(0.0, 3.0), (7.0, 10.0), (12.0, 18.0)]);
    assert_eq!(lane(p, A1), vec![(0.0, 10.0), (12.0, 18.0)]);
}

#[test]
fn naming_one_track_still_takes_the_linked_sound_along() {
    let cut = pairs(10.0);
    let p = &cut.project;
    let v1 = timeline(p).tracks[V1].id;
    let done = p.extract_range(3.0, 7.0, Some(&[v1])).unwrap();
    assert_eq!(done.tracks.len(), 2, "the sound's track changed with it");
    assert_eq!(lane(p, V1), vec![(0.0, 3.0), (3.0, 6.0), (6.0, 12.0)]);
    assert_eq!(lane(p, A1), lane(p, V1), "the sound is cut and closed with its picture");
}

#[test]
fn closing_a_gap_is_one_revision_and_carries_the_linked_sound() {
    let cut = pairs(14.0);
    let p = &cut.project;
    let v1 = timeline(p).tracks[V1].id;
    let before = timeline(p);
    let revs = revisions(p);
    let done = p.ripple_delete_gap(v1, 12.0).unwrap();
    assert_eq!(revisions(p), revs + 1);
    assert_eq!(last_label(p), "Ripple delete gap");
    assert_eq!((done.from, done.to, done.moved), (10.0, 14.0, 1));
    assert_eq!(span(p, cut.c[1]), (10.0, 16.0));
    assert_eq!(span(p, cut.a[1]), (10.0, 16.0), "its sound followed through the sync lock");
    assert_eq!(span(p, cut.a[0]), (0.0, 10.0));
    p.undo().unwrap();
    assert!(same(&timeline(p), &before));
}

#[test]
fn a_gap_close_that_would_push_a_sound_onto_an_unlinked_clip_refuses() {
    let cut = pairs(14.0);
    let p = &cut.project;
    let a1 = timeline(p).tracks[A1].id;
    // Music on A1 right where the second sound would come to rest.
    let music = asset("/music.mp3", 30.0, vec![stream(StreamKind::Audio)]);
    p.insert_asset(&music).unwrap();
    p.add_clip_to_timeline(music.id, Some(a1), 0.0, 3.0, Some(10.5)).unwrap();
    let v1 = timeline(p).tracks[V1].id;
    let (before, revs) = (timeline(p), revisions(p));
    let why = p.ripple_delete_gap(v1, 12.0).unwrap_err().to_string();
    assert!(why.contains("A1"), "{why}");
    assert_eq!((revisions(p), same(&timeline(p), &before)), (revs, true));
}

#[test]
fn an_agents_range_edit_stages_and_lands_as_one_revision_on_accept() {
    let mut cut = pairs(10.0);
    cut.project.set_actor(EditSource::Agent);
    cut.project.set_agent_review(false);
    let p = &cut.project;
    p.begin_staging(None, Some("tighten"), None).unwrap();
    let live = timeline(p);
    p.extract_range(3.0, 7.0, None).unwrap();
    p.split_all_tracks(1.0, None).unwrap();
    assert!(same(&timeline(p), &live), "the live cut did not move");
    let staged = p.working_timeline().unwrap();
    assert_eq!(staged.tracks[V1].clips.len(), 4);
    assert_eq!(staged.clip(cut.c[1]).unwrap().timeline_start, 6.0);
    assert_eq!(staged.clip(cut.a[1]).unwrap().timeline_start, 6.0);
    let before = revisions(p);
    p.apply_staged(false).unwrap();
    assert_eq!(revisions(p), before + 1);
    assert_eq!(span(p, cut.a[1]), (6.0, 12.0));
}

#[test]
fn with_review_on_the_agents_first_range_edit_opens_the_proposal() {
    let mut cut = pairs(10.0);
    cut.project.set_actor(EditSource::Agent);
    cut.project.set_agent_review(true);
    let p = &cut.project;
    let before = timeline(p);
    p.lift_range(3.0, 7.0, None).unwrap();
    assert!(same(&timeline(p), &before));
    let staged = p.staged().unwrap().expect("a proposal is open");
    assert_eq!(staged.edits.len(), 1);
    assert!(staged.edits[0].starts_with("Lift range"), "{}", staged.edits[0]);
    // A refused one stages nothing.
    assert!(p.extract_range(40.0, 50.0, None).is_err());
    assert_eq!(p.staged().unwrap().unwrap().edits.len(), 1);
}

#[test]
fn a_range_edit_can_be_batched_with_other_edits_into_one_step() {
    let cut = pairs(10.0);
    let p = &cut.project;
    let revs = revisions(p);
    p.batch(Some("Tighten"), false, |p| -> Result<()> {
        p.extract_range(3.0, 7.0, None)?;
        p.split_all_tracks(1.0, None)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(revisions(p), revs + 1);
    assert_eq!(last_label(p), "Tighten");
    assert_eq!(lane(p, V1).len(), 4);
}
