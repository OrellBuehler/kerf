//! Linked A/V at the `Project` level: every edit that honours links, as one
//! revision, through staging, with the escape hatch, and refusing a locked partner
//! without leaving a trace.

use super::*;
use crate::model::EditSource;

pub(super) fn stream(kind: StreamKind) -> StreamInfo {
    StreamInfo {
        index: 0,
        kind,
        codec: if kind == StreamKind::Video { "h264" } else { "aac" }.into(),
        width: (kind == StreamKind::Video).then_some(1920),
        height: (kind == StreamKind::Video).then_some(1080),
        fps: (kind == StreamKind::Video).then_some(30.0),
        sample_rate: (kind == StreamKind::Audio).then_some(48_000),
        channels: (kind == StreamKind::Audio).then_some(2),
        image: false,
        projection: None,
        rotation: 0,
        color_transfer: None,
        color_primaries: None,
        pix_fmt: None,
        color_space: None,
    }
}

pub(super) fn asset(path: &str, duration: f64, streams: Vec<StreamInfo>) -> Asset {
    Asset {
        id: Uuid::new_v4(),
        path: path.into(),
        name: path.into(),
        duration,
        streams,
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
    }
}

/// A project with one 60 s video+audio asset.
fn av_project() -> (Project, Asset) {
    let project = Project::open_in_memory().unwrap();
    let a = asset("/av.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]);
    project.insert_asset(&a).unwrap();
    (project, a)
}

/// Two shots on V1 — `c1` source 0..10 at 0, `c2` source 20..26 at 10 — with their
/// sound detached onto A1, so V1 and A1 are two linked pairs side by side.
struct Cut {
    project: Project,
    c: [Uuid; 2],
    a: [Uuid; 2],
}

fn detached_cut() -> Cut {
    let (project, asset) = av_project();
    let c1 = project.add_clip_to_timeline(asset.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let c2 = project.add_clip_to_timeline(asset.id, None, 20.0, 26.0, Some(10.0)).unwrap();
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

fn revisions(p: &Project) -> usize {
    p.history().unwrap().len()
}

fn lock(p: &Project, track: usize) {
    let id = timeline(p).tracks[track].id;
    p.set_track_locked(id, true).unwrap();
}

/// A1 is the second track of a fresh project (V1, A1).
const A1: usize = 1;

#[test]
fn detaching_is_one_revision_that_undoes_as_one() {
    let (project, asset) = av_project();
    let c = project.add_clip_to_timeline(asset.id, None, 5.0, 15.0, Some(2.0)).unwrap();
    let before = revisions(&project);
    let d = project.detach_audio(c.id).unwrap();
    assert_eq!(revisions(&project), before + 1);
    assert_eq!(project.history().unwrap().last().unwrap().label, "Detach audio");
    let t = timeline(&project);
    assert_eq!(t.tracks[A1].clips.len(), 1);
    assert_eq!(d.track_id, t.tracks[A1].id);
    assert!(!clip_of(&project, c.id).source_audio);
    assert_eq!(span(&project, d.clip.id), (2.0, 12.0));

    project.undo().unwrap();
    let t = timeline(&project);
    assert!(t.tracks[A1].clips.is_empty());
    let back = clip_of(&project, c.id);
    assert!(
        back.source_audio && back.link_id.is_none(),
        "one undo restores the picture too"
    );
}

#[test]
fn reattaching_puts_the_picture_back_as_it_was() {
    let (project, asset) = av_project();
    let c = project.add_clip_to_timeline(asset.id, None, 5.0, 15.0, Some(2.0)).unwrap();
    project.set_volume(c.id, 0.6).unwrap();
    let original = serde_json::to_value(clip_of(&project, c.id)).unwrap();
    let d = project.detach_audio(c.id).unwrap();
    // Editing the detached sound is not carried back: reattach restores the picture.
    project.set_volume(d.clip.id, 0.1).unwrap();
    let back = project.reattach_audio(d.clip.id).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), original);
    assert!(timeline(&project).tracks[A1].clips.is_empty());
    assert_eq!(project.history().unwrap().last().unwrap().label, "Reattach audio");
}

#[test]
fn detaching_a_clip_whose_asset_has_no_sound_is_refused() {
    let project = Project::open_in_memory().unwrap();
    let silent = asset("/silent.mp4", 20.0, vec![stream(StreamKind::Video)]);
    project.insert_asset(&silent).unwrap();
    let c = project.cut_clip(silent.id, 0.0, 5.0).unwrap();
    let before = revisions(&project);
    assert!(project.detach_audio(c.id).is_err());
    assert_eq!(revisions(&project), before, "a refusal records nothing");
}

#[test]
fn extract_audio_detaches_every_clip_of_the_asset_in_one_revision() {
    let (project, asset) = av_project();
    let c1 = project.add_clip_to_timeline(asset.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let c2 = project.add_clip_to_timeline(asset.id, None, 20.0, 26.0, Some(10.0)).unwrap();
    let before = revisions(&project);
    let done = project.extract_audio(asset.id).unwrap();
    assert_eq!(revisions(&project), before + 1, "one revision for the lot");
    assert_eq!(project.history().unwrap().last().unwrap().label, "Extract audio");
    let t = timeline(&project);
    assert_eq!(t.tracks[A1].clips.len(), 2);
    assert_eq!((done.detached.len(), done.skipped.len()), (2, 0));
    assert_eq!(done.detached[0].clip.timeline_start, 0.0);
    for c in [c1.id, c2.id] {
        let picture = clip_of(&project, c);
        assert!(!picture.source_audio);
        assert_eq!(t.link_partners(c).len(), 1);
    }
    // A second extract finds nothing still sounding. It says so — it does not fall
    // through to appending the whole asset, which is `add_asset_audio`'s job.
    let before = revisions(&project);
    let err = project.extract_audio(asset.id).unwrap_err().to_string();
    assert!(err.contains("already on an audio track"), "{err}");
    assert_eq!(revisions(&project), before);
    assert_eq!(timeline(&project).tracks[A1].clips.len(), 2);
}

#[test]
fn extract_audio_skips_a_clip_on_a_locked_track_and_reports_it() {
    let (project, asset) = av_project();
    let p = &project;
    let c1 = p.add_clip_to_timeline(asset.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    p.add_track(StreamKind::Video, None).unwrap();
    let v2 = timeline(p).tracks[1].id;
    let c2 = p.add_clip_to_timeline(asset.id, Some(v2), 0.0, 6.0, Some(0.0)).unwrap();
    p.set_track_locked(v2, true).unwrap();
    let done = p.extract_audio(asset.id).unwrap();
    assert_eq!(done.detached.len(), 1);
    assert_eq!(done.skipped.len(), 1);
    assert_eq!(done.skipped[0].clip_id, c2.id);
    assert!(done.skipped[0].reason.contains("locked"), "{}", done.skipped[0].reason);
    assert!(!clip_of(p, c1.id).source_audio);
    assert!(clip_of(p, c2.id).source_audio, "the locked track's clip is untouched");
    // Nothing detachable at all is an error and records nothing.
    let before = revisions(p);
    assert!(p.extract_audio(asset.id).unwrap_err().to_string().contains("locked"));
    assert_eq!(revisions(p), before);
}

#[test]
fn adding_an_assets_audio_is_its_own_operation() {
    let (project, asset) = av_project();
    let p = &project;
    let before = revisions(p);
    let a = p.add_asset_audio(asset.id).unwrap();
    let b = p.add_asset_audio(asset.id).unwrap();
    assert_eq!(revisions(p), before + 2);
    assert_eq!(p.history().unwrap().last().unwrap().label, "Add audio");
    assert_eq!((a.source_in, a.source_out, a.timeline_start), (0.0, 60.0, 0.0));
    assert_eq!(b.timeline_start, 60.0, "appended after the first, not on top of it");
    let silent = asset_without_sound(p);
    assert!(p.add_asset_audio(silent.id).is_err());
}

fn asset_without_sound(p: &Project) -> Asset {
    let silent = asset("/silent.mp4", 20.0, vec![stream(StreamKind::Video)]);
    p.insert_asset(&silent).unwrap();
    silent
}

#[test]
fn detaching_several_clips_is_one_revision_and_skips_what_cannot_be() {
    let (project, asset) = av_project();
    let p = &project;
    let silent = asset_without_sound(p);
    let c1 = p.add_clip_to_timeline(asset.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let c2 = p.add_clip_to_timeline(asset.id, None, 20.0, 26.0, Some(10.0)).unwrap();
    let mute = p.add_clip_to_timeline(silent.id, None, 0.0, 4.0, Some(16.0)).unwrap();
    let before = revisions(p);
    let done = p.detach_audio_clips(&[c1.id, c2.id, mute.id, c1.id]).unwrap();
    assert_eq!(revisions(p), before + 1, "one revision however many");
    assert_eq!(p.history().unwrap().last().unwrap().label, "Detach audio (2 clips)");
    assert_eq!(done.detached.len(), 2, "a clip named twice is detached once");
    assert_eq!(done.skipped.len(), 1);
    assert_eq!(done.skipped[0].clip_id, mute.id);
    assert_eq!(timeline(p).tracks[A1].clips.len(), 2);
    p.undo().unwrap();
    assert!(timeline(p).tracks[A1].clips.is_empty(), "and one undo takes them all back");
    assert!(clip_of(p, c1.id).source_audio && clip_of(p, c2.id).source_audio);

    // Nothing detachable: an error with the reason, and no revision.
    let before = revisions(p);
    assert!(p.detach_audio_clips(&[mute.id]).is_err());
    assert_eq!(revisions(p), before);
    // One clip is the plain label.
    p.detach_audio_clips(&[c1.id]).unwrap();
    assert_eq!(p.history().unwrap().last().unwrap().label, "Detach audio");
}

#[test]
fn detaching_folds_the_picture_tracks_fader_into_the_new_clip() {
    let (project, asset) = av_project();
    let p = &project;
    let c = p.add_clip_to_timeline(asset.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    p.set_volume(c.id, 0.8).unwrap();
    let (v1, a1) = (timeline(p).tracks[0].id, timeline(p).tracks[A1].id);
    p.set_track_volume(v1, 0.5).unwrap();
    p.set_track_volume(a1, 2.0).unwrap();
    let d = p.detach_audio(c.id).unwrap();
    // Through A1's fader (2.0) the clip comes out at 0.8 * 0.5, as it did through V1's.
    assert!((d.clip.volume * 2.0 - 0.8 * 0.5).abs() < 1e-6, "{}", d.clip.volume);
    // Reattaching puts the picture back with its own volume, and the fader never moved.
    let back = p.reattach_audio(d.clip.id).unwrap();
    assert_eq!(back.volume, 0.8);
    assert_eq!(timeline(p).tracks[0].volume, 0.5);
}

#[test]
fn reattaching_a_picture_whose_sound_still_plays_elsewhere_is_refused() {
    let cut = detached_cut();
    let p = &cut.project;
    // Unlink the pair: the picture stays muted and its sound stays on A1, as an
    // ordinary audio clip. Unmuting the picture now would play the footage twice.
    p.unlink_clips(&[cut.c[0]]).unwrap();
    let (before, json) = (revisions(p), p.timeline_json().unwrap());
    let err = p.reattach_audio(cut.c[0]).unwrap_err().to_string();
    assert!(err.contains("heard twice"), "{err}");
    assert_eq!((revisions(p), p.timeline_json().unwrap()), (before, json));
    // Take that clip away and the picture can have its sound back: nothing doubles.
    p.remove(cut.a[0]).unwrap();
    let back = p.reattach_audio(cut.c[0]).unwrap();
    assert!(back.source_audio);

    // A clip of the same footage that is *not* in step with it is not a double.
    let p2 = detached_cut();
    let q = &p2.project;
    q.unlink_clips(&[p2.c[1]]).unwrap();
    q.with_links(Some(false), |q| q.slip_clip(p2.a[1], 2.0)).unwrap();
    assert!(q.reattach_audio(p2.c[1]).is_ok());
}

#[test]
fn detach_and_reattach_are_never_rippled() {
    // With ripple on, deleting the sound clip is not "footage removed ahead of" the later
    // pair, and a detach adds a clip onto empty lane: neither may move anything else.
    let cut = detached_cut();
    let p = &cut.project;
    p.set_ripple_mode(true).unwrap();
    let before = (span(p, cut.c[1]), span(p, cut.a[1]));
    p.reattach_audio(cut.c[0]).unwrap();
    assert_eq!(
        (span(p, cut.c[1]), span(p, cut.a[1])),
        before,
        "the second pair stayed where it was"
    );
    assert!(clip_of(p, cut.c[0]).source_audio);
    p.detach_audio(cut.c[0]).unwrap();
    assert_eq!((span(p, cut.c[1]), span(p, cut.a[1])), before);
    // And a link edit is not a ripple either.
    let sound = timeline(p).link_partners(cut.c[0])[0];
    p.unlink_clips(&[sound]).unwrap();
    assert_eq!((span(p, cut.c[1]), span(p, cut.a[1])), before);
}

#[test]
fn pasting_a_picture_without_its_sound_gives_the_copy_its_own_sound() {
    let cut = detached_cut();
    let p = &cut.project;
    let lone = p.duplicate_clips(&[cut.c[1]], 50.0).unwrap();
    let copy = clip_of(p, lone[0].id);
    assert!(copy.source_audio, "the copy was silent for good: its partner was not pasted");
    assert_eq!(copy.link_id, None);
    // With its sound pasted alongside it stays a muted picture in a pair of its own.
    let both = p.duplicate_clips(&[cut.c[1], cut.a[1]], 70.0).unwrap();
    let pic = clip_of(p, both[0].id);
    assert!(!pic.source_audio && pic.link_id.is_some());
    // A pair pasted in two steps: the picture alone sounds again, the sound alone is a plain clip.
    let snd_only = p.duplicate_clips(&[cut.a[1]], 90.0).unwrap();
    assert_eq!(clip_of(p, snd_only[0].id).link_id, None);
    // The original is untouched.
    assert!(!clip_of(p, cut.c[1]).source_audio);
}

#[test]
fn a_move_carries_the_partner_as_one_revision() {
    let cut = detached_cut();
    let p = &cut.project;
    let before = revisions(p);
    let moved = p.move_clip(cut.c[0], 3.0, None);
    assert!(
        moved.is_err(),
        "the picture would land on the next shot (10..) and so would its sound"
    );
    assert_eq!(revisions(p), before, "all or nothing");
    // Into the free space after the cut.
    p.move_clip(cut.c[1], 20.0, None).unwrap();
    assert_eq!(revisions(p), before + 1);
    assert_eq!(p.history().unwrap().last().unwrap().label, "Move 2 clips");
    assert_eq!((span(p, cut.c[1]), span(p, cut.a[1])), ((20.0, 26.0), (20.0, 26.0)));
    p.undo().unwrap();
    assert_eq!((span(p, cut.c[1]), span(p, cut.a[1])), ((10.0, 16.0), (10.0, 16.0)));
}

#[test]
fn the_escape_hatch_moves_one_clip_alone_and_is_undone_when_the_call_returns() {
    let cut = detached_cut();
    let p = &cut.project;
    assert!(p.links_active());
    p.with_links(Some(false), |p| {
        assert!(!p.links_active());
        p.move_clip(cut.c[1], 20.0, None).unwrap();
    });
    assert!(p.links_active(), "the override ends with the call");
    assert_eq!(span(p, cut.c[1]), (20.0, 26.0));
    assert_eq!(span(p, cut.a[1]), (10.0, 16.0), "the sound stayed");
    assert!(
        clip_of(p, cut.a[1]).link_id.is_some(),
        "still linked — only the edit ignored it"
    );
    // `None` inherits whatever is in force, and an error cannot leave it stuck.
    let r: Result<()> = p.with_links(Some(false), |p| {
        p.with_links(None, |p| p.move_clip(Uuid::new_v4(), 0.0, None).map(|_| ()))
    });
    assert!(r.is_err() && p.links_active());
}

#[test]
fn a_locked_partner_refuses_the_edit_and_leaves_no_trace() {
    let cut = detached_cut();
    let p = &cut.project;
    lock(p, A1);
    let (before, json) = (revisions(p), p.timeline_json().unwrap());
    let c = cut.c[1];
    let err = |r: Result<()>| r.unwrap_err().to_string();
    assert!(err(p.move_clip(c, 20.0, None).map(|_| ())).contains("locked"));
    assert!(err(p
        .move_clips(&[ClipMove {
            clip_id: c,
            timeline_start: 20.0,
            track_id: None
        }])
        .map(|_| ()))
    .contains("locked"));
    assert!(err(p.trim(c, None, Some(23.0), None).map(|_| ())).contains("locked"));
    assert!(err(p.split_at(c, 12.0).map(|_| ())).contains("locked"));
    assert!(err(p.remove(c)).contains("locked"));
    assert!(err(p.remove_clips(&[c]).map(|_| ())).contains("locked"));
    assert!(err(p.ripple_delete(c)).contains("locked"));
    assert!(err(p.set_speed(c, 2.0).map(|_| ())).contains("locked"));
    assert!(err(p.cut_clip_range(c, 22.0, 24.0).map(|_| ())).contains("locked"));
    assert!(err(p.roll_edit(cut.c[0], c, 0.5).map(|_| ())).contains("locked"));
    assert!(err(p.slip_clip(c, 0.5).map(|_| ())).contains("locked"));
    assert!(err(p.slide_clip(c, 0.5).map(|_| ())).contains("locked"));
    assert!(err(p.split_remove(c, 12.0, SplitSide::Left).map(|_| ())).contains("locked"));
    assert_eq!(revisions(p), before);
    assert_eq!(p.timeline_json().unwrap(), json, "not a byte changed");
    // With links off the picture alone can be edited.
    p.with_links(Some(false), |p| p.trim(c, None, Some(23.0), None)).unwrap();
}

#[test]
fn a_trim_follows_the_shared_edge_and_ripple_closes_both_tracks() {
    let cut = detached_cut();
    let p = &cut.project;
    p.set_ripple_mode(true).unwrap();
    let before = revisions(p);
    // Shorten the first shot's tail by 4 s.
    let trimmed = p.trim(cut.c[0], None, Some(6.0), None).unwrap();
    assert_eq!(trimmed.timeline_end(), 6.0);
    assert_eq!(revisions(p), before + 1, "the picture, the sound and the ripple are one edit");
    assert_eq!(span(p, cut.a[0]), (0.0, 6.0));
    assert_eq!(span(p, cut.c[1]), (6.0, 12.0));
    assert_eq!(span(p, cut.a[1]), (6.0, 12.0), "the second pair moved together");
    p.undo().unwrap();
    assert_eq!(span(p, cut.a[1]), (10.0, 16.0));
}

#[test]
fn without_ripple_a_trim_still_carries_the_edge_but_leaves_the_gap() {
    let cut = detached_cut();
    let p = &cut.project;
    p.trim(cut.c[0], None, Some(6.0), None).unwrap();
    assert_eq!(span(p, cut.a[0]), (0.0, 6.0));
    assert_eq!(span(p, cut.c[1]), (10.0, 16.0));
    // And a left-edge trim, as the GUI commits it, brings the sound's head with it.
    p.trim(cut.c[1], Some(22.0), None, Some(12.0)).unwrap();
    assert_eq!(span(p, cut.a[1]), (12.0, 16.0));
    assert_eq!(clip_of(p, cut.a[1]).source_in, 22.0);
}

#[test]
fn ripple_carries_the_sound_of_a_shot_after_a_title_that_has_none() {
    let (project, a) = av_project();
    let p = &project;
    let title = asset(
        "/title.png",
        5.0,
        vec![{
            let mut s = stream(StreamKind::Video);
            s.image = true;
            s
        }],
    );
    p.insert_asset(&title).unwrap();
    let t = p.add_clip_to_timeline(title.id, None, 0.0, 5.0, Some(0.0)).unwrap();
    let shot = p.add_clip_to_timeline(a.id, None, 0.0, 8.0, Some(5.0)).unwrap();
    let sound = p.detach_audio(shot.id).unwrap().clip;
    p.set_ripple_mode(true).unwrap();
    p.trim(t.id, None, Some(3.0), None).unwrap();
    assert_eq!(span(p, shot.id), (3.0, 11.0));
    assert_eq!(
        span(p, sound.id),
        (3.0, 11.0),
        "the sync lock: the sound followed the picture it belongs to"
    );
    // With the links off, tracks are independent again.
    p.undo().unwrap();
    p.with_links(Some(false), |p| p.trim(t.id, None, Some(3.0), None)).unwrap();
    assert_eq!(span(p, shot.id), (3.0, 11.0));
    assert_eq!(span(p, sound.id), (5.0, 13.0));
}

#[test]
fn a_split_cuts_the_sound_too_and_links_the_new_halves() {
    let cut = detached_cut();
    let p = &cut.project;
    let before = revisions(p);
    let (left, right) = p.split_at(cut.c[0], 4.0).unwrap();
    assert_eq!(revisions(p), before + 1);
    let t = timeline(p);
    assert_eq!(t.tracks[A1].clips.len(), 3, "the sound has a new half too");
    assert_eq!(span(p, cut.a[0]), (0.0, 4.0));
    assert_eq!(t.link_partners(left.id), vec![cut.a[0]]);
    let right_sound = t.link_partners(right.id);
    assert_eq!(right_sound.len(), 1);
    assert_eq!(span(p, right_sound[0]), (4.0, 10.0));
    assert!(!clip_of(p, right.id).source_audio, "a half of a muted picture stays muted");
    p.undo().unwrap();
    assert_eq!(timeline(p).tracks[A1].clips.len(), 2);

    // Links off: the picture alone.
    p.with_links(Some(false), |p| p.split_at(cut.c[0], 4.0)).unwrap();
    assert_eq!(timeline(p).tracks[A1].clips.len(), 2);
}

#[test]
fn removing_a_clip_removes_its_partner_and_counts_both() {
    let cut = detached_cut();
    let p = &cut.project;
    let before = revisions(p);
    p.remove(cut.c[0]).unwrap();
    assert_eq!(revisions(p), before + 1);
    assert_eq!(p.history().unwrap().last().unwrap().label, "Remove 2 clips");
    let t = timeline(p);
    assert!(t.clip(cut.c[0]).is_none() && t.clip(cut.a[0]).is_none());
    assert!(t.clip(cut.c[1]).is_some() && t.clip(cut.a[1]).is_some());
    p.undo().unwrap();
    assert!(timeline(p).clip(cut.a[0]).is_some());

    assert_eq!(p.remove_clips(&[cut.c[0], cut.c[1]]).unwrap(), 4);
    assert!(timeline(p).tracks.iter().all(|t| t.clips.is_empty()));
    p.undo().unwrap();
    assert_eq!(
        p.remove_clips(&[cut.a[1]]).unwrap(),
        2,
        "naming the sound removes the picture"
    );
    p.undo().unwrap();
    // Links off: exactly what is named.
    assert_eq!(p.with_links(Some(false), |p| p.remove_clips(&[cut.c[0]])).unwrap(), 1);
    assert!(timeline(p).clip(cut.a[0]).is_some());
}

#[test]
fn a_ripple_removal_closes_the_gap_on_both_tracks() {
    let cut = detached_cut();
    let p = &cut.project;
    p.set_ripple_mode(true).unwrap();
    p.remove(cut.c[0]).unwrap();
    assert_eq!((span(p, cut.c[1]), span(p, cut.a[1])), ((0.0, 6.0), (0.0, 6.0)));
    p.undo().unwrap();
    p.set_ripple_mode(false).unwrap();
    p.ripple_delete(cut.c[0]).unwrap();
    assert_eq!(
        (span(p, cut.c[1]), span(p, cut.a[1])),
        ((0.0, 6.0), (0.0, 6.0)),
        "ripple_delete takes its partner and closes both"
    );
    assert_eq!(p.history().unwrap().last().unwrap().label, "Ripple delete");
}

#[test]
fn a_speed_change_retimes_the_sound_by_the_same_ratio() {
    let cut = detached_cut();
    let p = &cut.project;
    p.set_speed(cut.c[0], 2.0).unwrap();
    assert_eq!(clip_of(p, cut.a[0]).speed, 2.0);
    assert_eq!((span(p, cut.c[0]), span(p, cut.a[0])), ((0.0, 5.0), (0.0, 5.0)));
    // Ripple: both tracks close up behind the 5 s that went.
    p.undo().unwrap();
    p.set_ripple_mode(true).unwrap();
    p.set_speed(cut.c[0], 2.0).unwrap();
    assert_eq!((span(p, cut.c[1]), span(p, cut.a[1])), ((5.0, 11.0), (5.0, 11.0)));
}

#[test]
fn cutting_a_range_cuts_the_sound_and_closes_up_both_tracks() {
    let cut = detached_cut();
    let p = &cut.project;
    let before = revisions(p);
    let kept = p.cut_clip_range(cut.c[0], 3.0, 5.0).unwrap();
    assert_eq!(revisions(p), before + 1);
    assert_eq!(kept.len(), 2);
    assert_eq!((span(p, cut.c[0]), span(p, cut.a[0])), ((0.0, 3.0), (0.0, 3.0)));
    assert_eq!((span(p, cut.c[1]), span(p, cut.a[1])), ((8.0, 14.0), (8.0, 14.0)));
    let t = timeline(p);
    let tail = kept[1].id;
    assert_eq!(t.link_partners(tail).len(), 1, "the new halves are a pair");
    p.undo().unwrap();
    assert_eq!(span(p, cut.a[1]), (10.0, 16.0));
}

#[test]
fn trim_to_the_playhead_cuts_the_sound_with_the_picture() {
    let cut = detached_cut();
    let p = &cut.project;
    let kept = p.split_remove(cut.c[0], 4.0, SplitSide::Left).unwrap();
    assert_eq!(kept.id, cut.c[0], "the named clip comes back first");
    assert_eq!((span(p, cut.c[0]), span(p, cut.a[0])), ((4.0, 10.0), (4.0, 10.0)));
    assert_eq!(p.history().unwrap().last().unwrap().label, "Split and remove left (2 clips)");
    p.undo().unwrap();
    p.set_ripple_mode(true).unwrap();
    p.split_remove_clips(
        &[ClipCut {
            clip_id: cut.c[0],
            at: 6.0,
        }],
        SplitSide::Right,
    )
    .unwrap();
    assert_eq!((span(p, cut.c[1]), span(p, cut.a[1])), ((6.0, 12.0), (6.0, 12.0)));
}

#[test]
fn the_edit_modes_act_on_the_group() {
    let cut = detached_cut();
    let p = &cut.project;
    // Slip: both windows move.
    let out = p.slip_clip(cut.c[0], 2.0).unwrap();
    assert_eq!(out.clips.len(), 2);
    assert_eq!((clip_of(p, cut.c[0]).source_in, clip_of(p, cut.a[0]).source_in), (2.0, 2.0));
    // Roll the cut between the two shots (they touch at 10): both lanes roll.
    let out = p.roll_edit(cut.c[0], cut.c[1], 1.0).unwrap();
    assert_eq!(out.clips.len(), 4);
    assert_eq!((span(p, cut.a[0]), span(p, cut.a[1])), ((0.0, 11.0), (11.0, 16.0)));
    assert_eq!(p.history().unwrap().last().unwrap().label, "Roll edit");
    // Slide the second shot: the sound's slides with it.
    p.undo().unwrap();
    let out = p.slide_clip(cut.c[1], 1.0).unwrap();
    assert!(out.applied > 0.0);
    assert_eq!((span(p, cut.c[1]).0, span(p, cut.a[1]).0), (11.0, 11.0));
    // Off: the picture's only.
    p.with_links(Some(false), |p| p.slip_clip(cut.c[1], 1.0)).unwrap();
    assert_eq!(clip_of(p, cut.a[1]).source_in, 20.0);
}

#[test]
fn duplicating_a_pair_links_the_copies_to_each_other_and_a_lone_copy_to_nothing() {
    let cut = detached_cut();
    let p = &cut.project;
    let copies = p.duplicate_clips(&[cut.c[0], cut.a[0]], 30.0).unwrap();
    assert_eq!(copies.len(), 2);
    let t = timeline(p);
    let (pic, snd) = (copies[0].id, copies[1].id);
    assert_eq!(t.link_partners(pic), vec![snd], "the copies are a pair of their own");
    assert!(!t.link_partners(cut.c[0]).contains(&pic), "…and not part of the original's");
    assert_eq!(t.link_partners(cut.c[0]), vec![cut.a[0]]);
    assert!(!clip_of(p, pic).source_audio, "a copy of a muted picture is muted");

    let lone = p.duplicate_clips(&[cut.c[1]], 50.0).unwrap();
    assert_eq!(clip_of(p, lone[0].id).link_id, None, "its sound was not copied with it");
}

#[test]
fn staged_agent_edits_propagate_in_the_proposal_and_not_the_live_cut() {
    let mut cut = detached_cut();
    cut.project.set_actor(EditSource::Agent);
    let p = &cut.project;
    p.begin_staging(None, Some("tighten")).unwrap();
    p.move_clip(cut.c[1], 20.0, None).unwrap();
    assert_eq!(span(p, cut.c[1]), (10.0, 16.0), "the live cut did not move");
    let staged = p.staged_timeline().unwrap().unwrap();
    assert_eq!(staged.clip(cut.c[1]).unwrap().timeline_start, 20.0);
    assert_eq!(
        staged.clip(cut.a[1]).unwrap().timeline_start,
        20.0,
        "the sound moved in the proposal"
    );
    let before = revisions(p);
    p.apply_staged(false).unwrap();
    assert_eq!(revisions(p), before + 1);
    assert_eq!(span(p, cut.a[1]), (20.0, 26.0));
}

#[test]
fn a_proposal_that_only_links_or_detaches_is_not_thrown_away_as_empty() {
    let (mut project, asset) = av_project();
    let c = project.add_clip_to_timeline(asset.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let a = project
        .add_clip_to_timeline(asset.id, Some(timeline(&project).tracks[A1].id), 0.0, 10.0, Some(0.0))
        .unwrap();
    project.set_actor(EditSource::Agent);
    project.begin_staging(None, None).unwrap();
    project.link_clips(&[c.id, a.id]).unwrap();
    let staged = project.staged().unwrap().unwrap();
    assert!(!staged.diff.is_empty(), "linking is a change a reviewer must see");
    assert!(staged.diff.summary().contains("linked"), "{}", staged.diff.summary());
    project.apply_staged(false).unwrap();
    assert!(clip_of(&project, c.id).link_id.is_some());

    project.begin_staging(None, None).unwrap();
    project.unlink_clips(&[c.id]).unwrap();
    project.apply_staged(false).unwrap();
    assert!(clip_of(&project, c.id).link_id.is_none());

    // Muting a picture's own sound alone is a change too.
    project.begin_staging(None, None).unwrap();
    project.detach_audio(c.id).unwrap();
    let diff = project.staged().unwrap().unwrap().diff;
    assert!(diff.summary().contains("own sound off"), "{}", diff.summary());
}

#[test]
fn link_and_unlink_are_one_revision_each() {
    let (project, asset) = av_project();
    let p = &project;
    let c = p.add_clip_to_timeline(asset.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let a = p
        .add_clip_to_timeline(asset.id, Some(timeline(p).tracks[A1].id), 0.0, 10.0, Some(0.0))
        .unwrap();
    let before = revisions(p);
    let group = p.link_clips(&[c.id, a.id]).unwrap();
    assert_eq!(revisions(p), before + 1);
    assert_eq!(clip_of(p, c.id).link_id, Some(group));
    assert!(p.link_clips(&[c.id, a.id]).is_err(), "already a pair: not an edit");
    assert_eq!(revisions(p), before + 1);
    assert_eq!(p.unlink_clips(&[a.id]).unwrap(), 1);
    assert_eq!(revisions(p), before + 2);
    assert!(clip_of(p, c.id).link_id.is_none());
    assert!(p.unlink_clips(&[c.id]).is_err(), "nothing to unlink");
    p.undo().unwrap();
    assert!(clip_of(p, c.id).link_id.is_some(), "undo re-links");
}

#[test]
fn a_clip_without_the_new_fields_loads_unlinked_and_with_its_own_sound() {
    let (project, asset) = av_project();
    let c = project.add_clip_to_timeline(asset.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let mut json: serde_json::Value = serde_json::from_str(&project.timeline_json().unwrap()).unwrap();
    let clip = &mut json["tracks"][0]["clips"][0];
    assert!(
        clip.get("link_id").is_none() && clip.get("source_audio").is_none(),
        "neutral values are not written"
    );
    project.save_timeline_str(&json.to_string()).unwrap();
    let back = clip_of(&project, c.id);
    assert!(back.link_id.is_none() && back.source_audio);
}

#[test]
fn the_platform_check_does_not_hear_a_muted_picture() {
    let (project, asset) = av_project();
    let c = project.add_clip_to_timeline(asset.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    assert!(project.cut_summary(None).unwrap().has_audio);
    project.detach_audio(c.id).unwrap();
    assert!(
        project.cut_summary(None).unwrap().has_audio,
        "its sound moved to A1, so the cut still has some"
    );
    project
        .with_links(Some(false), |p| p.remove(timeline(p).tracks[A1].clips[0].id))
        .unwrap();
    assert!(
        !project.cut_summary(None).unwrap().has_audio,
        "a muted picture alone is silent"
    );
}

#[test]
fn the_beat_snap_carries_each_retimed_clip_to_its_partner() {
    let project = Project::open_in_memory().unwrap();
    let video = asset(
        "/beat-video.mp4",
        10.0,
        vec![stream(StreamKind::Video), stream(StreamKind::Audio)],
    );
    let music = asset("/beat-music.wav", 10.0, vec![stream(StreamKind::Audio)]);
    project.insert_asset(&video).unwrap();
    project.insert_asset(&music).unwrap();
    project
        .set_analysis(&AssetAnalysis {
            asset_id: music.id,
            tempo: Some(crate::model::Tempo {
                bpm: 120.0,
                beats: (0..=20).map(|i| i as f64 * 0.5).collect(),
                confidence: 0.8,
            }),
            ..Default::default()
        })
        .unwrap();
    project.add_asset_audio(music.id).unwrap();
    let v1 = project.cut_clip(video.id, 0.0, 1.1).unwrap();
    let v2 = project.cut_clip(video.id, 2.0, 3.4).unwrap();
    let s1 = project.detach_audio(v1.id).unwrap().clip;
    let s2 = project.detach_audio(v2.id).unwrap().clip;
    project.snap_to_beats(None, None).unwrap();
    // The picture's cuts land on 1.0 and 2.5; its sound follows each cut it shared.
    assert_eq!(span(&project, v1.id), (0.0, 1.0));
    assert_eq!(span(&project, s1.id), (0.0, 1.0));
    assert_eq!(span(&project, v2.id), (1.0, 2.5));
    assert_eq!(span(&project, s2.id), (1.0, 2.5));
}

#[test]
fn a_clip_whose_partner_is_gone_edits_as_if_it_were_unlinked() {
    let cut = detached_cut();
    let p = &cut.project;
    // Take the first sound away with links off: the edit that removes the last
    // partner dissolves the link, so the picture is not left linked to nothing.
    p.with_links(Some(false), |p| p.remove(cut.a[0])).unwrap();
    let c = cut.c[0];
    assert!(clip_of(p, c).link_id.is_none(), "a group of one is not a link");
    // A file from before that rule can still hold one; every op treats it as unlinked.
    let mut stale = timeline(p);
    let (ti, ci) = stale.locate(c).unwrap();
    stale.tracks[ti].clips[ci].link_id = Some(Uuid::new_v4());
    p.save_timeline(&stale).unwrap();
    assert!(clip_of(p, c).link_id.is_some());
    assert!(timeline(p).link_partners(c).is_empty(), "a group of one is not a link");

    // Every op behaves as it does on any clip: one revision, no companion, the
    // plain label — and nothing here depends on links being forced off.
    let before = revisions(p);
    p.move_clip(c, 30.0, None).unwrap();
    assert_eq!(p.history().unwrap().last().unwrap().label, "Move clip");
    assert_eq!(span(p, c), (30.0, 40.0));
    p.trim(c, None, Some(8.0), None).unwrap();
    let (left, right) = p.split_at(c, 33.0).unwrap();
    assert_eq!((left.id, right.link_id), (c, None));
    p.set_speed(c, 2.0).unwrap();
    p.cut_clip_range(right.id, 4.0, 5.0).unwrap();
    p.slip_clip(c, 1.0).unwrap();
    p.remove(c).unwrap();
    assert_eq!(p.history().unwrap().last().unwrap().label, "Remove clip");
    assert_eq!(revisions(p), before + 7);
    // The other pair is untouched by any of it.
    assert_eq!((span(p, cut.c[1]), span(p, cut.a[1])), ((10.0, 16.0), (10.0, 16.0)));
}

#[test]
fn a_linked_edit_is_the_same_edit_whichever_clip_is_named() {
    let a = detached_cut();
    let b = detached_cut();
    a.project.move_clip(a.c[1], 20.0, None).unwrap();
    b.project.move_clip(b.a[1], 20.0, None).unwrap();
    for (cut, name) in [(&a, "picture"), (&b, "sound")] {
        assert_eq!(
            (span(&cut.project, cut.c[1]), span(&cut.project, cut.a[1])),
            ((20.0, 26.0), (20.0, 26.0)),
            "naming the {name}"
        );
    }
}

// ---- the invariant, under fire ----------------------------------------------

/// A tiny deterministic generator (xorshift64*): the same seed always takes the same
/// path, so a failure names a seed that replays.
pub(super) struct Rng(pub(super) u64);

impl Rng {
    pub(super) fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub(super) fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub(super) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// V1 and A1 mirror each other: `n` shots (the same source windows and positions, with
/// gaps), each picture linked to the sound that is its copy. With `titles`, V1 also holds
/// unlinked, silent clips between them — footage the sound's track has nothing for — so
/// the two lanes are no longer copies and a ripple on V1 has to *carry* the sound.
fn mirrored_project(rng: &mut Rng, titles: bool) -> (Project, Asset) {
    let (project, asset) = av_project();
    let title_asset = self::asset("/title.mp4", 30.0, vec![stream(StreamKind::Video)]);
    project.insert_asset(&title_asset).unwrap();
    let mut timeline = timeline(&project);
    let mut at = rng.unit() * 2.0;
    for _ in 0..(3 + rng.below(3)) {
        if titles && rng.below(2) == 0 {
            let len = 0.5 + rng.unit() * 3.0;
            timeline.tracks[0].clips.push(Clip::new(title_asset.id, 0.0, len, at));
            at += len;
        }
        let len = 1.0 + rng.unit() * 6.0;
        let source_in = rng.unit() * 40.0;
        let mut picture = Clip::new(asset.id, source_in, source_in + len, at);
        let mut sound = picture.clone();
        sound.id = Uuid::new_v4();
        let group = Uuid::new_v4();
        picture.link_id = Some(group);
        sound.link_id = Some(group);
        timeline.tracks[0].clips.push(picture);
        timeline.tracks[A1].clips.push(sound);
        at += len + if rng.below(3) == 0 { rng.unit() * 2.0 } else { 0.0 };
    }
    project.save_timeline(&timeline).unwrap();
    (project, asset)
}

/// V1 and A1 as a **J/L-cut** edit: the same shots, but the sound of each leads or trails
/// its picture (`-1..1.5` s at each cut, so the lanes never overlap and the clips stay in
/// step — equal offsets — while covering different stretches). Every cut is a place where
/// picture and sound change hands at different moments, which is what a mirrored pair never
/// exercises.
fn jl_project(rng: &mut Rng) -> (Project, Asset) {
    let (project, asset) = av_project();
    let mut timeline = timeline(&project);
    let n = 3 + rng.below(3);
    // The cuts of the picture, then where the sound changes hands at each.
    let mut at = 2.0 + rng.unit();
    let mut cuts = vec![at];
    for _ in 0..n {
        at += 2.0 + rng.unit() * 5.0;
        cuts.push(at);
    }
    let mut shifts: Vec<f64> = (0..=n).map(|_| -1.0 + rng.unit() * 2.5).collect();
    // Every sound lasts at least a second, whatever the cuts around it do.
    for i in 0..n {
        shifts[i + 1] = shifts[i + 1].max(1.0 + cuts[i] + shifts[i] - cuts[i + 1]);
    }
    for i in 0..n {
        let source_in = 5.0 + rng.unit() * 30.0;
        let len = cuts[i + 1] - cuts[i];
        let mut picture = Clip::new(asset.id, source_in, source_in + len, cuts[i]);
        let (lead, trail) = (shifts[i], shifts[i + 1]);
        let mut sound = Clip::new(asset.id, source_in + lead, source_in + len + trail, cuts[i] + lead);
        let group = Uuid::new_v4();
        picture.link_id = Some(group);
        sound.link_id = Some(group);
        timeline.tracks[0].clips.push(picture);
        timeline.tracks[A1].clips.push(sound);
    }
    project.save_timeline(&timeline).unwrap();
    (project, asset)
}

/// The lanes as one line each — `start-end (source in..out) #link` — for a failure message.
fn dump(p: &Project) -> String {
    timeline(p)
        .tracks
        .iter()
        .map(|t| {
            let clips: Vec<String> = t
                .clips
                .iter()
                .map(|c| {
                    format!(
                        "{:.3}-{:.3} ({:.3}..{:.3}{}) #{}",
                        c.timeline_start,
                        c.timeline_end(),
                        c.source_in,
                        c.source_out,
                        if c.speed == 1.0 {
                            String::new()
                        } else {
                            format!(" x{}", c.speed)
                        },
                        c.link_id.map_or("-".to_string(), |l| l.to_string()[..4].to_string())
                    )
                })
                .collect();
            format!("{}: {}", t.name, clips.join(" | "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every link group is one clip per track and its members agree on where they sit and
/// what they show: a picture and its sound are still one piece of material.
fn assert_in_step(p: &Project, what: &str, strict: bool, before: &str) {
    let t = timeline(p);
    let mut groups: HashMap<Uuid, Vec<(usize, Clip)>> = HashMap::new();
    for (ti, track) in t.tracks.iter().enumerate() {
        for clip in &track.clips {
            assert!(
                clip.duration() > 0.0,
                "{what}: a clip with no length\nbefore:\n{before}\nafter:\n{}",
                dump(p)
            );
            if let Some(link) = clip.link_id {
                groups.entry(link).or_default().push((ti, clip.clone()));
            }
        }
    }
    for (link, members) in groups {
        assert_eq!(members.len(), 2, "{what}: group {link} has {} members", members.len());
        assert_ne!(members[0].0, members[1].0, "{what}: two clips of a group on one track");
        let (a, b) = (&members[0].1, &members[1].1);
        let near = |x: f64, y: f64| (x - y).abs() < 1e-6;
        // What sync means: the moment of footage playing at a given timeline time is the
        // same in both (the content offset), whatever stretch of it each one shows.
        let offset = |c: &Clip| {
            if c.is_reversed() {
                c.timeline_start + c.source_out / c.speed_mag()
            } else {
                c.timeline_start - c.source_in / c.speed_mag()
            }
        };
        assert!(
            near(a.speed, b.speed) && near(offset(a), offset(b)),
            "{what}: a pair came out of sync: offset {} vs {}, speed {} vs {}\nbefore:\n{before}\nafter:\n{}",
            offset(a),
            offset(b),
            a.speed,
            b.speed,
            dump(p)
        );
        if strict {
            // With the lanes copies of each other, the pair also covers the same stretch.
            assert!(
                near(a.timeline_start, b.timeline_start) && near(a.timeline_end(), b.timeline_end()),
                "{what}: a pair came apart: {:?} vs {:?}\nbefore:\n{before}\nafter:\n{}",
                (a.timeline_start, a.timeline_end()),
                (b.timeline_start, b.timeline_end()),
                dump(p)
            );
            assert!(
                near(a.source_in, b.source_in) && near(a.source_out, b.source_out),
                "{what}: a pair shows different footage"
            );
        }
    }
}

/// Whether any lane has two clips on the same stretch of time.
fn lanes_overlap(t: &Timeline) -> bool {
    t.tracks.iter().any(|tr| {
        let mut order: Vec<&Clip> = tr.clips.iter().collect();
        order.sort_by(|a, b| a.timeline_start.total_cmp(&b.timeline_start));
        order.windows(2).any(|w| w[1].timeline_start < w[0].timeline_end() - 1e-6)
    })
}

/// The shape of the cut a fuzz run starts from.
#[derive(Clone, Copy, PartialEq)]
enum Cut0 {
    /// V1 and A1 copies of each other.
    Mirrored,
    /// Mirrored, plus unlinked silent clips on V1 only: the lanes are not copies.
    Titles,
    /// The sound leads or trails its picture at every cut.
    JlCut,
}

/// What a fuzz run did: how many edits applied, how many were refused, and the edits that
/// were refused although nothing genuinely stood in their way.
struct Fuzzed {
    applied: usize,
    refused: usize,
    /// Refusals that name a genuine block: an unlinked clip in the way, a linked clip
    /// that would be covered completely, a partner a trim would take entirely.
    blocked: usize,
    /// Per edit: `(attempted, refused by a genuine block)`.
    by_edit: std::collections::BTreeMap<String, (usize, usize)>,
    /// `(edit, reason)` of every refusal of an edit that has no business being refused on
    /// a J/L-cut: no lane is locked and no such block stands in its way.
    unexpected: Vec<(String, String)>,
}

/// 250 seeds of ten random link-aware edits each.
fn fuzz_linked_edits(start: Cut0) -> Fuzzed {
    let titles = start == Cut0::Titles;
    let mut out = Fuzzed {
        applied: 0,
        refused: 0,
        blocked: 0,
        by_edit: std::collections::BTreeMap::new(),
        unexpected: Vec::new(),
    };
    for seed in 1..=250u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let (project, _) = if start == Cut0::JlCut {
            jl_project(&mut rng)
        } else {
            mirrored_project(&mut rng, titles)
        };
        project.set_ripple_mode(rng.below(2) == 0).unwrap();
        for step in 0..10 {
            let t = timeline(&project);
            // An edit that makes a lane overlap itself (a slower clip with ripple off)
            // ends the sequence: the lanes are no longer legal ground to judge sync on.
            if lanes_overlap(&t) {
                break;
            }
            let all: Vec<Clip> = t.tracks.iter().flat_map(|tr| tr.clips.clone()).collect();
            if all.is_empty() {
                break;
            }
            let c = all[rng.below(all.len())].clone();
            let (json, revs) = (project.timeline_json().unwrap(), revisions(&project));
            let lanes_before = dump(&project);
            let frac = 0.1 + rng.unit() * 0.8;
            let at = c.timeline_start + frac * c.duration();
            let what = format!("seed {seed} step {step}");
            let (label, result): (&str, Result<()>) = match rng.below(12) {
                0 => ("move", project.move_clip(c.id, rng.unit() * 30.0, None).map(|_| ())),
                1 => (
                    "right trim",
                    project
                        .trim(c.id, None, Some(c.source_in + frac * (c.source_out - c.source_in)), None)
                        .map(|_| ()),
                ),
                2 => {
                    let source_in = c.source_in + frac * (c.source_out - c.source_in);
                    let start = c.timeline_start + frac * c.duration();
                    (
                        "left trim",
                        project.trim(c.id, Some(source_in), None, Some(start)).map(|_| ()),
                    )
                }
                3 => ("split", project.split_at(c.id, at).map(|_| ())),
                4 => ("remove", project.remove(c.id)),
                5 => ("ripple delete", project.ripple_delete(c.id)),
                6 => (
                    "speed",
                    project.set_speed(c.id, [0.5, 2.0, -1.0, 1.0][rng.below(4)]).map(|_| ()),
                ),
                7 => {
                    let (from, to) = (
                        c.source_in + frac * 0.5 * (c.source_out - c.source_in),
                        c.source_in + frac * (c.source_out - c.source_in),
                    );
                    ("cut range", project.cut_clip_range(c.id, from, to).map(|_| ()))
                }
                8 => ("slip", project.slip_clip(c.id, (rng.unit() - 0.5) * 6.0).map(|_| ())),
                9 => ("slide", project.slide_clip(c.id, (rng.unit() - 0.5) * 4.0).map(|_| ())),
                10 => {
                    let side = if rng.below(2) == 0 {
                        SplitSide::Left
                    } else {
                        SplitSide::Right
                    };
                    ("split-remove", project.split_remove(c.id, at, side).map(|_| ()))
                }
                _ => {
                    // The next clip on the same track, if the two touch.
                    let lane = t.tracks.iter().find(|tr| tr.clips.iter().any(|x| x.id == c.id)).unwrap();
                    let mut order: Vec<&Clip> = lane.clips.iter().collect();
                    order.sort_by(|a, b| a.timeline_start.total_cmp(&b.timeline_start));
                    let pos = order.iter().position(|x| x.id == c.id).unwrap();
                    match order.get(pos + 1) {
                        Some(next) => ("roll", project.roll_edit(c.id, next.id, (rng.unit() - 0.5) * 2.0).map(|_| ())),
                        None => continue,
                    }
                }
            };
            let what = format!("{what} ({label})");
            out.by_edit.entry(label.to_string()).or_default().0 += 1;
            match result {
                Ok(()) => {
                    out.applied += 1;
                    assert_in_step(&project, &what, start == Cut0::Mirrored, &lanes_before);
                }
                Err(e) => {
                    out.refused += 1;
                    let why = e.to_string();
                    let genuine = ["not linked to it", "cover another linked clip", "trimmed away by this edit"]
                        .iter()
                        .any(|m| why.contains(m));
                    if genuine {
                        out.blocked += 1;
                        out.by_edit.entry(label.to_string()).or_default().1 += 1;
                    } else if start == Cut0::JlCut
                        && ["ripple delete", "speed", "remove", "right trim", "left trim", "cut range"].contains(&label)
                    {
                        out.unexpected.push((
                            format!("{what} [ripple {}]\n{lanes_before}", project.ripple_active().unwrap()),
                            why,
                        ));
                    }
                    assert_eq!(
                        project.timeline_json().unwrap(),
                        json,
                        "{what}: a refusal changed the timeline"
                    );
                    assert_eq!(revisions(&project), revs, "{what}: a refusal recorded a revision");
                }
            }
        }
    }
    out
}

#[test]
fn random_linked_edits_keep_a_mirrored_picture_and_sound_in_step_and_refusals_leave_no_trace() {
    let run = fuzz_linked_edits(Cut0::Mirrored);
    assert!(
        run.applied > 500 && run.refused > 100,
        "the run exercised both paths: {} applied, {} refused",
        run.applied,
        run.refused
    );
}

#[test]
fn random_linked_edits_keep_every_pair_in_sync_when_the_lanes_are_not_copies() {
    let run = fuzz_linked_edits(Cut0::Titles);
    assert!(
        run.applied > 500 && run.refused > 100,
        "the run exercised both paths: {} applied, {} refused",
        run.applied,
        run.refused
    );
}

#[test]
fn a_jl_cut_edits_without_refusal_unless_something_genuinely_stands_in_the_way() {
    // The sound leads or trails its picture at every cut — the shape the old per-track
    // ripple could not carry (refused 10-65% of the time for these edits). Now every
    // refusal of the edits that move footage must be a block that names itself: an
    // *unlinked* clip in the way, or *linked* material that the follower would cover
    // entirely / a trim would take away entirely.
    let run = fuzz_linked_edits(Cut0::JlCut);
    assert!(
        run.unexpected.is_empty(),
        "refused for no stated reason:\n{}",
        run.unexpected
            .iter()
            .take(3)
            .map(|(what, why)| format!("{what}\n=> {why}"))
            .collect::<Vec<_>>()
            .join("\n---\n")
    );
    let (attempted, blocked) = [
        "ripple delete",
        "speed",
        "remove",
        "right trim",
        "left trim",
        "cut range",
        "split-remove",
    ]
    .iter()
    .map(|edit| run.by_edit.get(*edit).copied().unwrap_or_default())
    .fold((0, 0), |(a, b), (n, k)| (a + n, b + k));
    assert!(
        run.applied > 1500 && attempted > 1000,
        "the run exercised the edits: {} applied, {attempted} of the moving ones",
        run.applied
    );
    // Ten random edits in a row on small clips chop a cut to bits, and a block is what
    // is left when two scraps meet; it stays the exception.
    assert!(
        blocked * 100 <= attempted * 8,
        "{blocked} of {attempted} footage-moving edits were blocked: {:?}",
        run.by_edit
    );
    // The edits that only *remove* material meet almost nothing.
    for edit in ["ripple delete", "remove", "cut range"] {
        let (n, k) = run.by_edit[edit];
        assert!(k * 100 <= n * 4, "{edit}: {k} of {n} blocked");
    }
}

/// After a split or a cut at `lo`, every link group the edit touched (the named clip's group
/// and any group of a clip the edit made) has **all** its members on one side of it — a
/// member left over on the other side would be dragged along by an edit to a clip it no longer
/// has anything to do with.
fn assert_one_side(p: &Project, lo: f64, old_group: Option<Uuid>, before_ids: &HashSet<Uuid>, what: &str) {
    const EPS: f64 = 1e-6;
    let t = timeline(p);
    let mut groups: HashMap<Uuid, Vec<Clip>> = HashMap::new();
    for clip in t.tracks.iter().flat_map(|tr| tr.clips.iter()) {
        if let Some(link) = clip.link_id {
            groups.entry(link).or_default().push(clip.clone());
        }
    }
    for (link, members) in groups {
        let touched = Some(link) == old_group || members.iter().any(|m| !before_ids.contains(&m.id));
        if !touched {
            continue;
        }
        let sides: Vec<Option<bool>> = members
            .iter()
            .map(|m| {
                if m.timeline_end() <= lo + EPS {
                    Some(false)
                } else if m.timeline_start >= lo - EPS {
                    Some(true)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            sides.iter().all(|s| s.is_some()) && sides.windows(2).all(|w| w[0] == w[1]),
            "{what}: group {link} has members on both sides of {lo:.3}: {:?}\n{}",
            members
                .iter()
                .map(|m| (m.timeline_start, m.timeline_end()))
                .collect::<Vec<_>>(),
            dump(p)
        );
    }
}

#[test]
fn a_split_or_a_cut_leaves_every_group_it_touched_on_one_side_of_the_cut() {
    let mut checked = 0;
    for seed in 1..=300u64 {
        let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03));
        let (project, _) = if seed % 2 == 0 {
            jl_project(&mut rng)
        } else {
            mirrored_project(&mut rng, false)
        };
        project.set_ripple_mode(rng.below(2) == 0).unwrap();
        for step in 0..8 {
            let t = timeline(&project);
            if lanes_overlap(&t) {
                break;
            }
            let all: Vec<Clip> = t.tracks.iter().flat_map(|tr| tr.clips.clone()).collect();
            if all.is_empty() {
                break;
            }
            let c = all[rng.below(all.len())].clone();
            let before_ids: HashSet<Uuid> = all.iter().map(|x| x.id).collect();
            let frac = 0.1 + rng.unit() * 0.8;
            let what = format!("seed {seed} step {step}");
            let json = project.timeline_json().unwrap();
            let before = dump(&project);
            if rng.below(2) == 0 {
                let at = c.timeline_start + frac * c.duration();
                if project.split_at(c.id, at).is_ok() {
                    assert_in_step(&project, &format!("{what} (split)"), false, &before);
                    assert_one_side(&project, at, c.link_id, &before_ids, &format!("{what} (split)"));
                    checked += 1;
                }
            } else {
                let (from, to) = (
                    c.source_in + frac * 0.4 * (c.source_out - c.source_in),
                    c.source_in + frac * (c.source_out - c.source_in),
                );
                let lo = c.source_span_to_timeline(from.max(c.source_in), to.min(c.source_out)).start;
                if project.cut_clip_range(c.id, from, to).is_ok() {
                    assert_in_step(&project, &format!("{what} (cut)"), false, &before);
                    assert_one_side(&project, lo, c.link_id, &before_ids, &format!("{what} (cut)"));
                    checked += 1;
                } else {
                    assert_eq!(
                        project.timeline_json().unwrap(),
                        json,
                        "{what}: a refusal changed the timeline"
                    );
                }
            }
        }
    }
    assert!(checked > 700, "{checked} splits and cuts were checked");
}

#[test]
fn an_edit_that_leaves_a_link_with_one_clip_dissolves_it() {
    // Removing a track takes its clips; the pictures they were linked to are not left
    // linked to nothing.
    let cut = detached_cut();
    let p = &cut.project;
    let a1 = timeline(p).tracks[A1].id;
    p.remove_track(a1).unwrap();
    for c in cut.c {
        assert!(clip_of(p, c).link_id.is_none(), "the sound is gone, so the link is");
    }
    // A cut that takes the last partner of a clip does the same (here: the partner is
    // removed with links off, so only the edit path can tidy up after it).
    let cut = detached_cut();
    let p = &cut.project;
    p.with_links(Some(false), |p| p.remove(cut.a[1])).unwrap();
    assert!(clip_of(&cut.project, cut.c[1]).link_id.is_none());
    assert!(
        clip_of(&cut.project, cut.c[0]).link_id.is_some(),
        "the other pair is untouched"
    );
    // A cut range that leaves a half with nothing to be linked to: the head of a
    // picture whose sound begins inside the stretch.
    let (project, asset) = av_project();
    let p = &project;
    let pic = p.add_clip_to_timeline(asset.id, None, 0.0, 20.0, Some(0.0)).unwrap();
    let a1 = timeline(p).tracks[A1].id;
    let snd = p.add_clip_to_timeline(asset.id, Some(a1), 8.0, 20.0, Some(8.0)).unwrap();
    p.link_clips(&[pic.id, snd.id]).unwrap();
    let kept = p.cut_clip_range(pic.id, 5.0, 12.0).unwrap();
    assert_eq!(clip_of(p, pic.id).link_id, None, "the head has no sound left to go with");
    assert_eq!(timeline(p).link_partners(kept[1].id), vec![snd.id], "the tails are the pair");
}

#[test]
fn a_dissolve_also_happens_in_a_staged_proposal() {
    let mut cut = detached_cut();
    cut.project.set_actor(EditSource::Agent);
    let p = &cut.project;
    p.begin_staging(None, None).unwrap();
    let a1 = timeline(p).tracks[A1].id;
    p.remove_track(a1).unwrap();
    let staged = p.staged_timeline().unwrap().unwrap();
    assert!(staged.clip(cut.c[0]).unwrap().link_id.is_none());
    assert!(clip_of(p, cut.c[0]).link_id.is_some(), "the live cut did not move");
}

#[test]
fn a_project_with_no_links_is_known_to_have_none_without_reading_the_timeline() {
    let (project, asset) = av_project();
    let p = &project;
    let c = p.add_clip_to_timeline(asset.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    assert!(!p.working_has_links().unwrap());
    p.detach_audio(c.id).unwrap();
    assert!(p.working_has_links().unwrap());
    p.unlink_clips(&[c.id]).unwrap();
    assert!(!p.working_has_links().unwrap(), "and a link that was cleared is not written");
    // A staged proposal is judged by its own timeline, not the live one.
    let mut project = project;
    project.set_actor(EditSource::Agent);
    let p = &project;
    p.begin_staging(None, None).unwrap();
    assert!(!p.working_has_links().unwrap());
    p.link_clips(&[c.id, timeline(p).tracks[A1].clips[0].id]).unwrap();
    assert!(p.working_has_links().unwrap());
}

#[test]
fn a_jl_cut_ripple_delete_through_the_project_is_one_revision_and_both_lanes_stay_in_step() {
    let (project, asset) = av_project();
    let p = &project;
    // Picture 5..15 with sound that leads it by 5 s (0..15), then a second shot whose
    // sound leads it by 2 s: footage and offsets as in `jl_cut`.
    let a1 = timeline(p).tracks[A1].id;
    let x1 = p.add_clip_to_timeline(asset.id, None, 5.0, 15.0, Some(5.0)).unwrap();
    let x2 = p.add_clip_to_timeline(asset.id, None, 25.0, 35.0, Some(15.0)).unwrap();
    let y1 = p.add_clip_to_timeline(asset.id, Some(a1), 0.0, 13.0, Some(0.0)).unwrap();
    let y2 = p.add_clip_to_timeline(asset.id, Some(a1), 23.0, 35.0, Some(13.0)).unwrap();
    p.link_clips(&[x1.id, y1.id]).unwrap();
    p.link_clips(&[x2.id, y2.id]).unwrap();
    let before = revisions(p);
    p.ripple_delete(x1.id).unwrap();
    assert_eq!(revisions(p), before + 1);
    assert_eq!((span(p, x2.id), span(p, y2.id)), ((5.0, 15.0), (3.0, 15.0)));
    assert!(timeline(p).clip(y1.id).is_none());
    assert_eq!(clip_of(p, x2.id).link_id, clip_of(p, y2.id).link_id);
    p.undo().unwrap();
    assert_eq!(span(p, y2.id), (13.0, 25.0));

    // Naming the *sound* closes up by the sound's length instead: the named clip's track speaks.
    p.ripple_delete(y1.id).unwrap();
    assert_eq!((span(p, y2.id), span(p, x2.id)), ((0.0, 12.0), (2.0, 12.0)));
}

#[test]
fn a_ripple_that_cannot_carry_the_sound_is_refused_rather_than_leaving_it_behind() {
    let (project, a) = av_project();
    let p = &project;
    let title = asset(
        "/title.png",
        5.0,
        vec![{
            let mut s = stream(StreamKind::Video);
            s.image = true;
            s
        }],
    );
    p.insert_asset(&title).unwrap();
    let t = p.add_clip_to_timeline(title.id, None, 0.0, 5.0, Some(0.0)).unwrap();
    let shot = p.add_clip_to_timeline(a.id, None, 0.0, 8.0, Some(5.0)).unwrap();
    p.detach_audio(shot.id).unwrap();
    // Something unlinked sits on A1 right before the shot's sound, in the way of its ripple.
    let a1 = timeline(p).tracks[A1].id;
    p.add_clip_to_timeline(a.id, Some(a1), 20.0, 24.0, Some(1.0)).unwrap();
    p.set_ripple_mode(true).unwrap();
    let (before, json) = (revisions(p), p.timeline_json().unwrap());
    let err = p.trim(t.id, None, Some(3.0), None).unwrap_err().to_string();
    assert!(
        err.contains("A1") && err.contains("not linked") && !err.contains("links off"),
        "it says what is in the way, not to switch links off: {err}"
    );
    assert_eq!(
        (revisions(p), p.timeline_json().unwrap()),
        (before, json),
        "nothing was recorded or moved"
    );
    // Ripple off for the call, or links off, still go through.
    p.with_ripple(Some(false), |p| p.trim(t.id, None, Some(3.0), None)).unwrap();
    p.undo().unwrap();
    p.with_links(Some(false), |p| p.trim(t.id, None, Some(3.0), None)).unwrap();
}

#[test]
fn reordering_a_lane_carries_the_partners_and_naming_both_apart_is_refused() {
    let cut = detached_cut();
    let p = &cut.project;
    let v1 = timeline(p).tracks[0].id;
    // `reorder` re-lays one lane and knows nothing of partners; the sync lock puts
    // each sound where its picture went, so the swap happens on both lanes.
    p.reorder(v1, cut.c[1], 0).unwrap();
    assert_eq!((span(p, cut.c[1]), span(p, cut.c[0])), ((0.0, 6.0), (6.0, 16.0)));
    assert_eq!((span(p, cut.a[1]), span(p, cut.a[0])), ((0.0, 6.0), (6.0, 16.0)));
    // Links off: the picture's lane only, the sound stayed.
    p.undo().unwrap();
    p.with_links(Some(false), |p| p.reorder(v1, cut.c[1], 0)).unwrap();
    assert_eq!(span(p, cut.c[1]).0, 0.0);
    assert_eq!(span(p, cut.a[1]).0, 10.0, "links off: the sound stayed");
    p.undo().unwrap();

    // What the sync lock cannot choose between is refused: both clips of a pair named
    // in one move, to different places, were parted on purpose — by hand, not by a ripple.
    let (before, json) = (revisions(p), p.timeline_json().unwrap());
    let err = p
        .move_clips(&[
            ClipMove {
                clip_id: cut.c[1],
                timeline_start: 20.0,
                track_id: None,
            },
            ClipMove {
                clip_id: cut.a[1],
                timeline_start: 24.0,
                track_id: None,
            },
        ])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("out of step") && err.contains("unlink") && !err.contains("links off"),
        "{err}"
    );
    assert_eq!((revisions(p), p.timeline_json().unwrap()), (before, json));
}

#[test]
fn a_pair_that_was_already_apart_is_not_policed() {
    let cut = detached_cut();
    let p = &cut.project;
    // Slide the second sound off its picture on purpose (links off), then edit with links on.
    p.with_links(Some(false), |p| p.move_clip(cut.a[1], 12.0, None)).unwrap();
    p.set_volume(cut.c[1], 0.5).unwrap();
    p.trim(cut.c[1], None, Some(24.0), None).unwrap();
    assert_eq!(span(p, cut.a[1]).0, 12.0, "a deliberate offset is left as it is");
}

#[test]
fn a_project_that_links_nothing_pays_for_no_guard() {
    let (project, asset) = av_project();
    let p = &project;
    let a = p.add_clip_to_timeline(asset.id, None, 0.0, 5.0, Some(0.0)).unwrap();
    let b = p.add_clip_to_timeline(asset.id, None, 0.0, 5.0, Some(5.0)).unwrap();
    let v1 = timeline(p).tracks[0].id;
    // Exactly what it always did.
    p.reorder(v1, b.id, 0).unwrap();
    assert_eq!((span(p, b.id).0, span(p, a.id).0), (0.0, 5.0));
}
