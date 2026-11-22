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

// ---- a range that would leave a sliver is refused, at the rate the cut renders at ------------------

#[test]
fn a_remainder_shorter_than_a_frame_of_the_cut_refuses_the_range_at_the_rate_it_renders_at() {
    // The footage is 30 fps (a frame is 33.3 ms), so 35 ms of head is a clip.
    let cut = pairs(10.0);
    let p = &cut.project;
    let done = p.lift_range(0.035, 3.0, None).unwrap();
    assert_eq!((done.from, done.to), (0.035, 3.0), "the range that was asked for");
    assert_eq!(lane(p, V1), vec![(0.0, 0.035), (3.0, 10.0), (10.0, 16.0)]);
    p.undo().unwrap();

    // Delivered at 24 fps the frame is 41.7 ms, and the same 35 ms is a sliver: refused, changing
    // nothing, with the range that works, which is then taken.
    let delivered = Delivery::new(1920, 1080, crate::model::Fit::Contain)
        .with_fps(Some(24.0))
        .unwrap();
    p.set_delivery_format(Some(delivered)).unwrap();
    let (before, revs) = (timeline(p), revisions(p));
    let why = p.lift_range(0.035, 3.0, None).unwrap_err().to_string();
    assert!(
        why.starts_with("invalid argument: lifting 0.035–3 s would leave 35 ms of /av.mp4 on V1 (clip ")
            && why.contains(") (and 1 more remainder like it), less than a frame (42 ms): start at 0 instead"),
        "the picture's and its sound's: {why}"
    );
    assert_eq!((revisions(p), same(&timeline(p), &before)), (revs, true));
    let done = p.lift_range(0.0, 3.0, None).unwrap();
    assert_eq!((done.from, done.to), (0.0, 3.0));
    assert_eq!(lane(p, V1), vec![(3.0, 10.0), (10.0, 16.0)]);
    assert_eq!(lane(p, A1), lane(p, V1), "the sound is cut with it");
    assert_eq!(last_label(p), "Lift range (2 clips)");
    p.undo().unwrap();

    // The same for an extract, and a cut that would leave a sliver half is no cut (a refusal records nothing).
    let (before, revs) = (timeline(p), revisions(p));
    let why = p.extract_range(0.03, 12.0, None).unwrap_err().to_string();
    assert!(
        why.starts_with("invalid argument: extracting 0.03–12 s would leave 30 ms of /av.mp4 on V1 (clip "),
        "{why}"
    );
    let why = p.split_all_tracks(0.02, None).unwrap_err().to_string();
    assert!(why.contains("already a cut to within a frame"), "{why}");
    assert_eq!((revisions(p), same(&timeline(p), &before)), (revs, true));
}

#[test]
fn a_footage_rate_that_is_absurd_cannot_make_the_threshold_seconds_long() {
    // A half-frame-per-second clip has a two-second frame; it is held to a one-second one (1 fps),
    // so a range that leaves 1.5 s is fine and one that leaves 0.5 s is not.
    let project = Project::open_in_memory().unwrap();
    let mut video = stream(StreamKind::Video);
    video.fps = Some(0.5);
    let slow = asset("/slow.mp4", 60.0, vec![video]);
    project.insert_asset(&slow).unwrap();
    let p = &project;
    p.add_clip_to_timeline(slow.id, None, 0.0, 20.0, Some(0.0)).unwrap();
    p.lift_range(1.5, 5.0, None).unwrap();
    p.undo().unwrap();
    let why = p.lift_range(0.5, 5.0, None).unwrap_err().to_string();
    assert!(
        why.contains("would leave 500 ms of /slow.mp4 on V1") && why.contains("less than a frame (1000 ms)"),
        "{why}"
    );
}

/// V1 `[0,7)` and `[10,15)`, the second linked to a sound that begins two seconds early (A1 `[8,15)`) and
/// the first to its own sound on A1, `[0,7)`: linked to it, unlinked from it or gone, as `first` says.
fn a_leading_sound_after_a_shot(first: &str) -> (Project, Uuid, Uuid, Uuid) {
    let (project, asset) = av_project();
    let c1 = project.add_clip_to_timeline(asset.id, None, 0.0, 7.0, Some(0.0)).unwrap();
    let c2 = project.add_clip_to_timeline(asset.id, None, 10.0, 15.0, Some(10.0)).unwrap();
    let a1 = project.detach_audio(c1.id).unwrap().clip;
    let a2 = project.detach_audio(c2.id).unwrap().clip;
    match first {
        "unlinked" => {
            project.unlink_clips(&[a1.id]).unwrap();
        }
        "gone" => {
            project.with_links(Some(false), |p| p.remove(a1.id)).unwrap();
        }
        _ => {}
    }
    // The sound of the second shot leads its picture by two seconds.
    project
        .with_links(Some(false), |p| p.trim(a2.id, Some(8.0), None, Some(8.0)))
        .unwrap();
    (project, c2.id, a1.id, a2.id)
}

#[test]
fn extracting_an_empty_stretch_carries_a_leading_sound_like_closing_the_gap_does() {
    // A clip of A1 that is linked to nothing sits where the sound would land: both refuse, in the same
    // words, and change nothing.
    let (project, _, _, _) = a_leading_sound_after_a_shot("unlinked");
    let p = &project;
    let v1 = timeline(p).tracks[V1].id;
    let (before, revs) = (timeline(p), revisions(p));
    let by_extract = p.extract_range(7.0, 10.0, Some(&[v1])).unwrap_err().to_string();
    let by_gap = p.ripple_delete_gap(v1, 8.5).unwrap_err().to_string();
    assert!(by_extract.contains("would run into another clip on A1"), "{by_extract}");
    assert_eq!(
        by_extract, by_gap,
        "extract and the gap delete refuse the same case with the same sentence"
    );
    assert_eq!((revisions(p), same(&timeline(p), &before)), (revs, true));

    // With room before the sound, both carry it up with its picture; its lead-in is still there.
    let (project, c2, _, a2) = a_leading_sound_after_a_shot("gone");
    let p = &project;
    let v1 = timeline(p).tracks[V1].id;
    let done = p.extract_range(7.0, 10.0, Some(&[v1])).unwrap();
    assert_eq!((done.removed, done.trimmed, done.split), (0, 0, 0));
    assert!(done.lock_trimmed.is_empty(), "nothing was in the way");
    assert_eq!(span(p, c2), (7.0, 12.0));
    assert_eq!(span(p, a2), (5.0, 12.0), "the sound moved with its picture, whole");
    assert_eq!(clip_of(p, a2).source_in, 8.0);
}

#[test]
fn a_carried_sound_trims_the_linked_sound_of_the_shot_before_and_the_reply_says_so() {
    // Both shots have their sound, each pair linked: the sound that leads the second picture lands on the
    // first shot's sound, which the sync lock trims back (the rule it has for any ripple) — not a refusal.
    for gap in [false, true] {
        let (project, c2, a1, a2) = a_leading_sound_after_a_shot("linked");
        let p = &project;
        let v1 = timeline(p).tracks[V1].id;
        let revs = revisions(p);
        let (trimmed, label) = if gap {
            let done = p.ripple_delete_gap(v1, 8.5).unwrap();
            assert_eq!((done.from, done.to, done.moved), (7.0, 10.0, 1));
            (done.lock_trimmed, "Ripple delete gap (trimmed sound on A1)")
        } else {
            let done = p.extract_range(7.0, 10.0, Some(&[v1])).unwrap();
            assert_eq!((done.removed, done.trimmed, done.split, done.moved), (0, 0, 0, 1));
            (done.lock_trimmed, "Extract range (trimmed sound on A1)")
        };
        assert_eq!(revisions(p), revs + 1);
        assert_eq!(last_label(p), label);
        let a1_track = timeline(p).tracks[A1].id;
        assert_eq!(trimmed.len(), 1, "{trimmed:?}");
        let t = &trimmed[0];
        assert_eq!((t.track_id, t.track.as_str(), t.clip_id), (a1_track, "A1", a1));
        assert!((t.seconds - 2.0).abs() < 1e-9, "{t:?}");
        assert_eq!(span(p, c2), (7.0, 12.0));
        assert_eq!(span(p, a2), (5.0, 12.0), "the leading sound came up with its picture, whole");
        assert_eq!(span(p, a1), (0.0, 5.0), "and the sound it landed on lost its tail");
        // The pair that lost its tail is still in step: picture [0,7), sound [0,5) from the same footage.
        assert_eq!(clip_of(p, a1).source_in, 0.0);
    }
}

#[test]
fn a_sound_a_few_milliseconds_past_its_picture_does_not_block_an_edit_at_the_cut() {
    // V1 `[0,10)` and `[10,30)`, each linked to its sound on A1, the first sound 10 ms longer than its picture.
    let build = || {
        let (project, asset) = av_project();
        let p = &project;
        let pic = |from: f64, to: f64| p.add_clip_to_timeline(asset.id, None, from, to, Some(from)).unwrap();
        let (x1, x2) = (pic(0.0, 10.0), pic(10.0, 30.0));
        let (s1, s2) = (p.detach_audio(x1.id).unwrap().clip, p.detach_audio(x2.id).unwrap().clip);
        let mut t = timeline(p);
        for (id, (start, end)) in [(s1.id, (0.0, 10.01)), (s2.id, (10.01, 30.0))] {
            let (ti, ci) = t.locate(id).unwrap();
            let sound = &mut t.tracks[ti].clips[ci];
            (sound.source_in, sound.source_out, sound.timeline_start) = (start, end, start);
        }
        p.save_timeline(&t).unwrap();
        let v1 = timeline(p).tracks[V1].id;
        (project, v1, s1.id)
    };
    let (project, v1, _) = build();
    let p = &project;
    let (before, revs) = (timeline(p), revisions(p));
    // Twenty milliseconds into the second shot leaves a sliver of it: refused, and where to start is said.
    let why = p.extract_range(10.02, 15.0, Some(&[v1])).unwrap_err().to_string();
    assert!(
        why.contains("would leave 20 ms of /av.mp4 on V1") && why.ends_with("start at 10 instead"),
        "{why}"
    );
    assert_eq!((revisions(p), same(&timeline(p), &before)), (revs, true));
    // Taking that advice goes through: the sound is 10 ms into its neighbour, which is linked material.
    let done = p.extract_range(10.0, 15.0, Some(&[v1])).unwrap();
    assert!(
        done.lock_trimmed.is_empty(),
        "the lock trimmed nothing: the overlap was left as it is"
    );
    assert_eq!(lane(p, V1), vec![(0.0, 10.0), (10.0, 25.0)]);
    assert_eq!(lane(p, A1), vec![(0.0, 10.01), (10.0, 25.0)]);

    // With the first sound linked to nothing the overlap is an unlinked clip's, and refuses.
    let (project, v1, s1) = build();
    let p = &project;
    p.unlink_clips(&[s1]).unwrap();
    let why = p.extract_range(10.0, 15.0, Some(&[v1])).unwrap_err().to_string();
    assert!(why.contains("would run into another clip on A1"), "{why}");
}

#[test]
fn an_edit_that_grows_an_overlap_that_was_there_is_refused_whole() {
    let (project, asset) = av_project();
    let p = &project;
    let pic = |from: f64, to: f64| p.add_clip_to_timeline(asset.id, None, from, to, Some(from)).unwrap();
    let (x1, x2) = (pic(0.21, 3.645), pic(4.096, 7.993));
    let (s1, s2) = (p.detach_audio(x1.id).unwrap().clip, p.detach_audio(x2.id).unwrap().clip);
    // Give the sounds the 89 ms overlap an older build could leave (an edit made now would refuse it).
    let mut t = timeline(p);
    for (id, (start, end)) in [(s1.id, (0.0, 4.001)), (s2.id, (3.912, 8.163))] {
        let (ti, ci) = t.locate(id).unwrap();
        let sound = &mut t.tracks[ti].clips[ci];
        (sound.source_in, sound.source_out, sound.timeline_start) = (start, end, start);
    }
    p.save_timeline(&t).unwrap();
    let v1 = timeline(p).tracks[V1].id;
    let (before, revs) = (timeline(p), revisions(p));
    let why = p.extract_range(3.761, 6.725, Some(&[v1])).unwrap_err().to_string();
    assert!(why.contains("would run into another clip on A1"), "{why}");
    assert_eq!((revisions(p), same(&timeline(p), &before)), (revs, true));
    // The same range lifted only shrinks the overlap.
    p.lift_range(3.761, 6.725, Some(&[v1])).unwrap();
}

#[test]
fn a_gap_before_the_start_of_the_timeline_records_nothing() {
    let cut = pairs(14.0);
    let p = &cut.project;
    let v1 = timeline(p).tracks[V1].id;
    let (before, revs) = (timeline(p), revisions(p));
    let why = p.ripple_delete_gap(v1, -1.0).unwrap_err().to_string();
    assert!(why.contains("before the start of the timeline"), "{why}");
    assert_eq!((revisions(p), same(&timeline(p), &before)), (revs, true));
}
