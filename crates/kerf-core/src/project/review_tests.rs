//! The project-level fixes of the review round: asset rewrites that keep their analysis, numbers
//! that cannot brick a project, colours and ranges checked where they are set, clips that stay in
//! their footage and their lane, an atomic save, the generation and change counters, the staging
//! redesign (owner, auto-review, step undo, partial accept, restorable discard, no agent
//! self-approval), the media library (remove, relink, missing), output-path safety and paging.

use super::linked_tests::{asset, stream};
use super::*;
use crate::model::{AnalysisKind, AudioEffect, DiffKind, EditSource, Projection, StreamKind, TimeRange, VideoEffect};

/// A fresh project with one 60 s picture-and-sound asset.
fn av_project() -> (Project, Asset) {
    let project = Project::open_in_memory().unwrap();
    let a = asset("/av.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]);
    project.insert_asset(&a).unwrap();
    (project, a)
}

/// Two shots on V1 — `c1` 0..10 at 0 and `c2` 20..26 at 10 — made by the user.
fn cut() -> (Project, Asset, Clip, Clip) {
    let (p, a) = av_project();
    let c1 = p.add_clip_to_timeline(a.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let c2 = p.add_clip_to_timeline(a.id, None, 20.0, 26.0, Some(10.0)).unwrap();
    (p, a, c1, c2)
}

fn tl(p: &Project) -> Timeline {
    p.timeline().unwrap()
}

fn revisions(p: &Project) -> usize {
    p.history().unwrap().len()
}

fn volume(p: &Project, id: Uuid) -> f32 {
    tl(p).clip(id).unwrap().volume
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-review-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn silence_patch(asset: Uuid) -> AssetAnalysis {
    AssetAnalysis {
        asset_id: asset,
        silence_segments: vec![TimeRange { start: 1.0, end: 2.0 }],
        ran: vec![AnalysisKind::Silence],
        ..AssetAnalysis::default()
    }
}

// ---- the library: rewrites keep analysis; relink / remove / missing ----------------------------

#[test]
fn rewriting_an_asset_keeps_the_analysis_hanging_off_it() {
    let (p, a) = av_project();
    p.set_analysis(&silence_patch(a.id)).unwrap();

    // Marking the footage 360 rewrites the row; it used to delete it first and cascade the analysis.
    p.set_asset_projection(a.id, Some(Projection::Equirect)).unwrap();
    assert_eq!(p.require_asset(a.id).unwrap().projection(), Some(Projection::Equirect));
    assert_eq!(
        p.get_analysis(a.id).unwrap().expect("still analysed").silence_segments.len(),
        1
    );

    let mut renamed = p.require_asset(a.id).unwrap();
    renamed.name = "renamed".into();
    p.insert_asset(&renamed).unwrap();
    assert_eq!(p.require_asset(a.id).unwrap().name, "renamed");
    assert!(p.get_analysis(a.id).unwrap().is_some());
    assert_eq!(p.list_assets().unwrap().len(), 1, "an update, not a second row");
}

#[test]
fn analysis_for_an_asset_that_is_gone_is_refused_not_stored() {
    let (p, a) = av_project();
    let ghost = Uuid::new_v4();
    assert!(matches!(
        p.merge_analysis(&silence_patch(ghost)),
        Err(Error::AssetNotFound(id)) if id == ghost
    ));
    assert!(matches!(p.set_analysis(&silence_patch(ghost)), Err(Error::AssetNotFound(_))));
    assert!(p.merge_analysis(&silence_patch(a.id)).is_ok());
}

#[test]
fn an_unused_asset_is_removed_with_its_analysis() {
    let (p, a) = av_project();
    p.set_analysis(&silence_patch(a.id)).unwrap();
    let removed = p.remove_asset(a.id, false).unwrap();
    assert_eq!(removed.asset.id, a.id);
    assert_eq!(removed.clips_removed, 0);
    assert!(p.list_assets().unwrap().is_empty());
    assert!(p.get_analysis(a.id).unwrap().is_none());
    assert!(matches!(p.remove_asset(a.id, false), Err(Error::AssetNotFound(_))));
}

#[test]
fn an_asset_in_use_is_refused_unless_forced_and_force_is_one_revision() {
    let (p, a, c1, c2) = cut();
    let before = (revisions(&p), p.timeline_json().unwrap());
    let err = p.remove_asset(a.id, false).unwrap_err();
    assert!(matches!(err, Error::AssetInUse { clips: 2, .. }), "{err:?}");
    assert!(err.to_string().contains("2 clip"), "{err}");
    assert_eq!(
        (revisions(&p), p.timeline_json().unwrap()),
        before,
        "a refusal changes nothing"
    );

    p.set_ripple_mode(true).unwrap();
    let removed = p.remove_asset(a.id, true).unwrap();
    assert_eq!(removed.clips_removed, 2);
    assert_eq!(revisions(&p), before.0 + 1, "one revision for the removal of both clips");
    let last = p.history().unwrap().pop().unwrap();
    assert!(last.label.starts_with("Remove media"), "{}", last.label);
    assert!(tl(&p).clip(c1.id).is_none() && tl(&p).clip(c2.id).is_none());
    assert!(p.list_assets().unwrap().is_empty());

    // The library row is gone for good, so history cannot put clips of it back (see
    // `undo_redo_and_revert_refuse_a_step_that_uses_removed_media`).
    let err = p.undo().unwrap_err().to_string();
    assert!(err.contains("removed from the library") && err.contains("/av.mp4"), "{err}");
    assert!(tl(&p).clip(c1.id).is_none());
}

#[test]
fn undo_redo_and_revert_refuse_a_step_that_uses_removed_media() {
    let (p, a, c1, _c2) = cut();
    let before = revisions(&p);
    // A second asset that stays, so there is a step to go back to that does not use the first.
    let keep = asset("/keep.mp4", 30.0, vec![stream(StreamKind::Video)]);
    p.insert_asset(&keep).unwrap();
    p.remove_asset(a.id, true).unwrap();
    let head = p.history().unwrap().pop().unwrap();
    assert!(head.label.starts_with("Remove media"), "{}", head.label);
    let kept_cut = p.timeline_json().unwrap();

    // Undo would put the two clips back; revert to any step that has them is refused as well.
    for result in [
        p.undo().map(|_| ()),
        p.revert_to(before as i64 - 1).map(|_| ()),
        p.revert_to(1).map(|_| ()),
    ] {
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::InvalidArgument(_))
                && err.to_string().contains("removed from the library")
                && err.to_string().contains("/av.mp4"),
            "{err:?}"
        );
    }
    assert_eq!(p.timeline_json().unwrap(), kept_cut, "a refusal changes nothing");
    assert_eq!(p.history().unwrap().iter().filter(|r| r.current).count(), 1);
    assert_eq!(p.history().unwrap().pop().unwrap().seq, head.seq, "the head did not move");
    assert!(tl(&p).clip(c1.id).is_none());

    // Steps from before the media was used still restore, and redo past them is refused again.
    p.revert_to(0).unwrap();
    assert!(tl(&p).tracks.iter().all(|t| t.clips.is_empty()));
    let err = p.redo().unwrap_err().to_string();
    assert!(err.contains("removed from the library"), "{err}");
    // New work from there branches the history as usual.
    p.add_clip_to_timeline(keep.id, None, 0.0, 5.0, None).unwrap();
    assert!(p.undo().is_ok());
}

#[test]
fn removing_an_unused_asset_also_closes_history_to_the_steps_that_used_it() {
    let (p, a) = av_project();
    let c = p.add_clip_to_timeline(a.id, None, 0.0, 5.0, Some(0.0)).unwrap();
    p.remove_clips(&[c.id]).unwrap();
    p.remove_asset(a.id, false).unwrap();
    assert!(
        p.undo().is_err(),
        "the step that deleted the clip would bring a clip of nothing back"
    );
    // The names are remembered past the removal, but an unknown id is still said, never dropped.
    p.conn.execute("DELETE FROM meta WHERE key = 'removed_assets'", []).unwrap();
    let err = p.undo().unwrap_err().to_string();
    assert!(err.contains(&a.id.to_string()), "{err}");
}

#[test]
fn removing_an_asset_by_force_clears_the_open_proposal_of_it_too() {
    let (mut p, a, c1, _c2) = cut();
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_asset_projection(a.id, Some(Projection::Equirect)).unwrap();
    assert_eq!(p.staged().unwrap().unwrap().asset_changes.len(), 1);

    p.set_actor(EditSource::User);
    assert!(matches!(p.remove_asset(a.id, false), Err(Error::AssetInUse { .. })));
    p.remove_asset(a.id, true).unwrap();

    let staged = p.staged().unwrap().expect("the session stays open");
    assert!(staged.asset_changes.is_empty(), "the change to a removed asset is dropped");
    assert!(p
        .staged_timeline()
        .unwrap()
        .unwrap()
        .tracks
        .iter()
        .all(|t| t.clips.is_empty()));
    assert_eq!(staged.undo_depth, 0, "its remembered steps showed the asset");
}

#[test]
fn an_agent_cannot_remove_or_relink_media_while_its_edits_are_reviewed() {
    let (mut p, a) = av_project();
    p.set_actor(EditSource::Agent);
    assert!(matches!(p.remove_asset(a.id, true), Err(Error::AgentReviewRequired)));
    let probed = asset("/moved.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]);
    assert!(matches!(p.apply_relink(a.id, &probed), Err(Error::AgentReviewRequired)));
}

#[test]
fn an_agent_never_removes_or_relinks_media_even_with_review_off_and_a_proposal_open() {
    let (mut p, a, c1, _c2) = cut();
    p.set_agent_review(false);
    p.set_actor(EditSource::Agent);
    p.begin_staging(None, None, Some("alpha")).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    let staged_before = p.staged_timeline().unwrap();
    p.set_actor(EditSource::User);
    let live = (revisions(&p), p.timeline_json().unwrap());
    p.set_actor(EditSource::Agent);

    // Not a forced removal that would edit the proposal while the row goes from under the live cut…
    let err = p.remove_asset(a.id, true).unwrap_err();
    assert!(
        matches!(err, Error::InvalidArgument(_)) && err.to_string().contains("done by the user"),
        "{err:?}"
    );
    assert!(p.remove_asset(a.id, false).is_err());
    let probed = asset("/moved.mp4", 60.0, a.streams.clone());
    assert!(p.apply_relink(a.id, &probed).is_err());
    assert!(p.relink_inputs(a.id).is_err());
    assert!(p.relink_asset(a.id, "/moved.mp4").is_err());

    p.set_actor(EditSource::User);
    assert_eq!(p.list_assets().unwrap().len(), 1, "the asset is still there");
    assert_eq!((revisions(&p), p.timeline_json().unwrap()), live, "the live cut is untouched");
    assert_eq!(
        serde_json::to_string(&p.staged_timeline().unwrap()).unwrap(),
        serde_json::to_string(&staged_before).unwrap(),
        "so is the proposal"
    );
    assert_eq!(p.require_asset(a.id).unwrap().path, "/av.mp4");
}

#[test]
fn a_relink_to_the_same_media_keeps_the_analysis_and_to_a_different_take_drops_it() {
    let (p, a, c1, _) = cut();
    p.set_analysis(&silence_patch(a.id)).unwrap();
    p.set_proxy_declined(a.id, true).unwrap();

    // The same recording, moved.
    let moved = asset("/moved/av.mp4", 60.0, a.streams.clone());
    let done = p.apply_relink(a.id, &moved).unwrap();
    assert!(done.analysis_kept);
    assert_eq!(done.previous_path, "/av.mp4");
    let now = p.require_asset(a.id).unwrap();
    assert_eq!(
        (now.id, now.path.as_str(), now.name.as_str()),
        (a.id, "/moved/av.mp4", "/av.mp4")
    );
    assert!(p.get_analysis(a.id).unwrap().is_some());
    assert!(tl(&p).clip(c1.id).is_some(), "the clips still point at it");
    assert!(
        !p.proxy_declined(a.id).unwrap(),
        "a proxy declined for the old file is forgiven"
    );

    // Another take of the same length class: usable, but its transcript is not this file's.
    let other = asset("/other-take.mp4", 60.3, a.streams.clone());
    let done = p.apply_relink(a.id, &other).unwrap();
    assert!(!done.analysis_kept);
    assert!(p.get_analysis(a.id).unwrap().is_none());
    assert_eq!(p.require_asset(a.id).unwrap().path, "/other-take.mp4");
}

#[test]
fn a_relink_to_the_wrong_kind_of_file_or_a_taken_path_is_refused() {
    let (p, a) = av_project();
    let other = asset("/other.mp4", 10.0, vec![stream(StreamKind::Video)]);
    p.insert_asset(&other).unwrap();

    let silent = asset("/silent.mp4", 60.0, vec![stream(StreamKind::Video)]);
    let err = p.apply_relink(a.id, &silent).unwrap_err().to_string();
    assert!(err.contains("picture only") && err.contains("picture and sound"), "{err}");

    let short = asset("/short.mp4", 20.0, a.streams.clone());
    let err = p.apply_relink(a.id, &short).unwrap_err().to_string();
    assert!(err.contains("20.0 s") && err.contains("60.0 s"), "{err}");

    let taken = asset("/other.mp4", 60.0, a.streams.clone());
    let err = p.apply_relink(a.id, &taken).unwrap_err().to_string();
    assert!(err.contains("already in the library"), "{err}");
    assert_eq!(p.require_asset(a.id).unwrap().path, "/av.mp4", "refusals change nothing");

    // A path that is not an absolute file never reaches the probe.
    assert!(p.relink_asset(a.id, "relative.mp4").is_err());
    assert!(p.relink_asset(a.id, "http://example.com/a.mp4").is_err());
}

#[test]
fn a_relink_keeps_a_projection_set_by_hand() {
    let (p, a) = av_project();
    p.set_asset_projection(a.id, Some(Projection::Equirect)).unwrap();
    let moved = asset("/moved.mp4", 60.0, a.streams.clone());
    p.apply_relink(a.id, &moved).unwrap();
    assert_eq!(p.require_asset(a.id).unwrap().projection(), Some(Projection::Equirect));
}

#[test]
fn missing_assets_lists_the_files_that_are_gone_with_the_clips_that_use_them() {
    let dir = scratch("missing");
    let here = dir.join("here.mp4");
    std::fs::write(&here, b"x").unwrap();
    let (p, gone, c1, _) = cut();
    let present = asset(here.to_str().unwrap(), 5.0, vec![stream(StreamKind::Video)]);
    p.insert_asset(&present).unwrap();

    let missing = p.missing_assets().unwrap();
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert_eq!((missing[0].asset_id, missing[0].clips), (gone.id, 2));
    assert_eq!(missing[0].path, "/av.mp4");
    let _ = c1;
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_missing_check_is_split_so_no_file_is_touched_under_the_lock() {
    let dir = scratch("missing-split");
    let here = dir.join("here.mp4");
    std::fs::write(&here, b"x").unwrap();
    let (p, gone, ..) = cut();
    let present = asset(here.to_str().unwrap(), 5.0, vec![stream(StreamKind::Video)]);
    p.insert_asset(&present).unwrap();

    // The locked half is the database only: every asset, present or not, with its clips.
    let inputs = p.missing_assets_inputs().unwrap();
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs.iter().find(|f| f.asset_id == gone.id).unwrap().clips, 2);
    assert_eq!(inputs.iter().find(|f| f.asset_id == present.id).unwrap().clips, 0);

    // The other half needs no project at all.
    let found = Project::find_missing_assets(inputs);
    assert_eq!(found.len(), 1);
    assert_eq!((found[0].asset_id, found[0].clips), (gone.id, 2));
    assert_eq!(
        serde_json::to_string(&found).unwrap(),
        serde_json::to_string(&p.missing_assets().unwrap()).unwrap(),
        "the one-call form is the two together"
    );
    std::fs::remove_file(&here).unwrap();
    assert_eq!(Project::find_missing_assets(p.missing_assets_inputs().unwrap()).len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_relink_is_checked_with_the_lock_released_and_applied_without_touching_a_file() {
    let (p, a, c1, _) = cut();
    p.set_analysis(&silence_patch(a.id)).unwrap();
    let old = p.relink_inputs(a.id).unwrap();
    assert_eq!(old.path, "/av.mp4");
    assert!(matches!(p.relink_inputs(Uuid::new_v4()), Err(Error::AssetNotFound(_))));

    let probed = asset("/moved/av.mp4", 60.0, a.streams.clone());
    let probe = |same_file| RelinkProbe {
        probed: probed.clone(),
        old_path: old.path.clone(),
        same_file,
    };

    // The same length and streams from a file with other bytes is another take.
    let done = p.apply_relink_probed(a.id, &probe(Some(false))).unwrap();
    assert!(!done.analysis_kept);
    assert!(p.get_analysis(a.id).unwrap().is_none());
    // Put it back for the next case: relinked to where it was, with analysis again.
    let back = RelinkProbe {
        probed: asset("/av.mp4", 60.0, a.streams.clone()),
        old_path: "/moved/av.mp4".into(),
        same_file: None,
    };
    p.apply_relink_probed(a.id, &back).unwrap();
    p.set_analysis(&silence_patch(a.id)).unwrap();

    // Same bytes, or an old file that is gone and cannot be compared: length and streams decide.
    for same in [Some(true), None] {
        let probe = RelinkProbe {
            probed: asset("/moved/av.mp4", 60.0, a.streams.clone()),
            old_path: "/av.mp4".into(),
            same_file: same,
        };
        let done = p.apply_relink_probed(a.id, &probe).unwrap();
        assert!(done.analysis_kept, "{same:?}");
        p.apply_relink_probed(
            a.id,
            &RelinkProbe {
                probed: asset("/av.mp4", 60.0, a.streams.clone()),
                old_path: "/moved/av.mp4".into(),
                same_file: None,
            },
        )
        .unwrap();
    }
    assert!(tl(&p).clip(c1.id).is_some());

    // The entry changed while the file was being read: retry, never compare with the wrong file.
    let stale = RelinkProbe {
        probed: asset("/elsewhere.mp4", 60.0, a.streams.clone()),
        old_path: "/was/somewhere/else.mp4".into(),
        same_file: Some(true),
    };
    let err = p.apply_relink_probed(a.id, &stale).unwrap_err().to_string();
    assert!(err.contains("try again"), "{err}");
    assert_eq!(p.require_asset(a.id).unwrap().path, "/av.mp4");

    // Bad media is refused up front by the lock-free half, before anything is applied.
    let wrong = asset("/silent.mp4", 60.0, vec![stream(StreamKind::Video)]);
    assert!(check_relink_compatible(&old, &wrong).is_err());
}

#[test]
fn a_relink_says_before_it_is_made_whether_the_analysis_would_be_kept() {
    let (p, a, _, _) = cut();
    assert!(!p.has_analysis(a.id).unwrap());
    p.set_analysis(&silence_patch(a.id)).unwrap();
    assert!(p.has_analysis(a.id).unwrap());
    let old = p.relink_inputs(a.id).unwrap();

    let probe = |probed: Asset, same_file| RelinkProbe {
        probed,
        old_path: old.path.clone(),
        same_file,
    };
    let moved = asset("/moved/av.mp4", 60.0, a.streams.clone());
    assert!(probe(moved.clone(), Some(true)).keeps_analysis(&old));
    assert!(
        probe(moved.clone(), None).keeps_analysis(&old),
        "nothing to compare with: length and streams decide"
    );
    assert!(
        !probe(moved, Some(false)).keeps_analysis(&old),
        "other bytes are another take"
    );
    let longer = asset("/moved/av.mp4", 60.3, a.streams.clone());
    assert!(!probe(longer, Some(true)).keeps_analysis(&old));

    // The answer is the one the relink then gives, and the check changed nothing.
    let other = probe(asset("/other-take.mp4", 60.3, a.streams.clone()), None);
    assert!(!other.keeps_analysis(&old));
    assert!(p.has_analysis(a.id).unwrap());
    assert_eq!(p.require_asset(a.id).unwrap().path, "/av.mp4");
    assert!(!p.apply_relink_probed(a.id, &other).unwrap().analysis_kept);
    assert!(!p.has_analysis(a.id).unwrap());
}

#[test]
fn the_old_apply_relink_still_compares_file_sizes_when_both_files_are_there() {
    let dir = scratch("relink-legacy");
    let (old_file, same_size, other_size) = (dir.join("old.mp4"), dir.join("same.mp4"), dir.join("other.mp4"));
    std::fs::write(&old_file, vec![1u8; 100]).unwrap();
    std::fs::write(&same_size, vec![2u8; 100]).unwrap();
    std::fs::write(&other_size, vec![1u8; 101]).unwrap();
    let p = Project::open_in_memory().unwrap();
    let a = asset(
        old_file.to_str().unwrap(),
        60.0,
        vec![stream(StreamKind::Video), stream(StreamKind::Audio)],
    );
    p.insert_asset(&a).unwrap();
    p.set_analysis(&silence_patch(a.id)).unwrap();

    // Another size is another file…
    let different = asset(other_size.to_str().unwrap(), 60.0, a.streams.clone());
    assert!(!p.apply_relink(a.id, &different).unwrap().analysis_kept);
    assert_eq!(p.require_asset(a.id).unwrap().path, other_size.to_string_lossy());

    // …but the stat-only wrapper cannot see bytes differ at the same size: that is what the
    // `probe_relink` path, which reads both ends of both files, is for.
    p.apply_relink(a.id, &asset(old_file.to_str().unwrap(), 60.0, a.streams.clone()))
        .unwrap();
    p.set_analysis(&silence_patch(a.id)).unwrap();
    let same = asset(same_size.to_str().unwrap(), 60.0, a.streams.clone());
    assert!(p.apply_relink(a.id, &same).unwrap().analysis_kept);
    let _ = std::fs::remove_dir_all(dir);
}

// ---- numbers that cannot brick a project ---------------------------------------------------------

#[test]
fn a_volume_is_a_number_from_silence_up_to_plus_twelve_db() {
    let (p, _, c1, _) = cut();
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1] {
        assert!(p.set_volume(c1.id, bad).is_err(), "{bad}");
    }
    assert_eq!(p.set_volume(c1.id, 1.0e9).unwrap().volume, 4.0, "brought down to the ceiling");
    assert_eq!(p.set_volume(c1.id, 0.0).unwrap().volume, 0.0);
}

#[test]
fn no_numeric_setter_takes_a_non_finite_number_and_a_refusal_writes_nothing() {
    let (p, a, c1, _) = cut();
    let track = tl(&p).tracks[0].id;
    let overlay = p.add_overlay("hi".into(), 0.0, 2.0).unwrap();
    let marker = p.add_marker(1.0, "m".into(), None).unwrap();
    let state = |p: &Project| (revisions(p), p.timeline_json().unwrap());
    let before = state(&p);

    for bad in [f32::NAN, f32::INFINITY] {
        assert!(p.set_track_volume(track, bad).is_err());
        assert!(p.set_track_pan(track, bad).is_err());
    }
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(p.set_fade(c1.id, Some(bad), None).is_err());
        assert!(p.set_fade(c1.id, None, Some(bad)).is_err());
        assert!(p.set_speed(c1.id, bad).is_err());
        assert!(p.set_master_volume(bad).is_err());
        assert!(p
            .set_transform(c1.id, None, Some(bad), None, None, None, None, None, None, None)
            .is_err());
        assert!(p
            .set_transform(c1.id, None, None, Some(bad), None, None, None, None, None, None)
            .is_err());
        assert!(p
            .set_transform(c1.id, None, None, None, Some(bad), None, None, None, None, None)
            .is_err());
        assert!(p.add_keyframe(c1.id, 1.0, None, Some(bad), None, None, None).is_err());
        assert!(p.add_keyframe(c1.id, 1.0, None, None, Some(bad), None, None).is_err());
        assert!(p.add_keyframe(c1.id, 1.0, None, None, None, Some(bad), None).is_err());
        assert!(p.add_marker(bad, "x".into(), None).is_err());
        assert!(p.update_marker(marker.id, Some(bad), None, None).is_err());
        assert!(p.add_overlay("x".into(), 0.0, bad).is_err());
        for (s, e, x, y) in [
            (Some(bad), None, None, None),
            (None, Some(bad), None, None),
            (None, None, Some(bad), None),
            (None, None, None, Some(bad)),
        ] {
            assert!(p
                .update_overlay(overlay.id, None, s, e, x, y, None, None, None, None, None)
                .is_err());
        }
        assert!(p.trim(c1.id, Some(bad), None, None).is_err());
        assert!(p.trim(c1.id, None, Some(bad), None).is_err());
        assert!(p.trim(c1.id, None, None, Some(bad)).is_err());
        assert!(p.move_clip(c1.id, bad, None).is_err());
        assert!(p.split_at(c1.id, bad).is_err());
        assert!(p.cut_clip_range(c1.id, bad, 5.0).is_err());
        assert!(p.roll_edit(c1.id, c1.id, bad).is_err());
        assert!(p.slip_clip(c1.id, bad).is_err());
        assert!(p.slide_clip(c1.id, bad).is_err());
        assert!(p.add_clip_to_timeline(a.id, None, 0.0, bad, None).is_err());
        assert!(p.add_clip_to_timeline(a.id, None, bad, 5.0, None).is_err());
        assert!(p.add_clip_to_timeline(a.id, None, 0.0, 5.0, Some(bad)).is_err());
        assert!(p.snap_to_beats(None, Some(bad)).is_err());
    }
    assert_eq!(state(&p), before, "none of those wrote a revision or a byte");
}

#[test]
fn an_edit_that_leaves_a_non_finite_number_is_refused_live_and_staged() {
    let (mut p, _, c1, _) = cut();
    let poison = |tl: &mut Timeline| {
        tl.tracks[0].clips[0].volume = f32::NAN;
        Ok(())
    };
    let before = (revisions(&p), p.timeline_json().unwrap());
    let err = p.edit_timeline("poison", poison).unwrap_err().to_string();
    assert!(err.contains("volume") && err.contains("finite"), "{err}");
    assert_eq!((revisions(&p), p.timeline_json().unwrap()), before);
    assert_eq!(volume(&p, c1.id), 1.0);

    // The proposal is guarded the same way.
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    let staged_before = p.staged().unwrap().unwrap().edits.len();
    assert!(p.edit_timeline("poison", poison).is_err());
    let staged = p.staged().unwrap().unwrap();
    assert_eq!(staged.edits.len(), staged_before, "nothing was staged");
    assert_eq!(p.staged_timeline().unwrap().unwrap().clip(c1.id).unwrap().volume, 0.5);

    // And so is writing a timeline outright.
    let mut t = tl(&p);
    t.master.volume = f64::INFINITY;
    assert!(p.save_timeline(&t).is_err());
}

// ---- colours and ranges are checked where they are set -------------------------------------------

#[test]
fn colours_are_validated_when_they_are_set() {
    let (p, _, c1, _) = cut();
    let o = p.add_overlay("hi".into(), 0.0, 2.0).unwrap();
    let set = |color: Option<&str>, bg: Option<&str>| {
        p.update_overlay(
            o.id,
            None,
            None,
            None,
            None,
            None,
            None,
            color.map(String::from),
            bg.map(String::from),
            None,
            None,
        )
    };
    assert!(set(Some("white,drawtext=textfile=/etc/passwd"), None).is_err());
    assert!(set(None, Some("black:x=1")).is_err());
    assert!(set(Some("#ffcc00"), Some("black@0.5")).is_ok());
    assert!(set(None, Some("")).is_ok(), "an empty box colour clears it");

    let m = p.add_marker(1.0, "m".into(), Some("#ff0000".into())).unwrap();
    assert!(p
        .add_marker(2.0, "x".into(), Some("red;background:url(//evil)".into()))
        .is_err());
    assert!(p
        .update_marker(m.id, None, None, Some("var(--x)\" onload=\"".into()))
        .is_err());
    assert!(p.update_marker(m.id, None, None, Some("blue".into())).is_ok());

    let key = |color: &str| VideoEffect::ChromaKey {
        color: color.into(),
        similarity: 0.15,
        blend: 0.1,
    };
    assert!(p.set_video_effects(c1.id, vec![key("green")]).is_ok());
    assert!(p.set_video_effects(c1.id, vec![key("0x00ff00")]).is_ok());
    assert!(p.set_video_effects(c1.id, vec![key("green:similarity=1")]).is_err());
    assert!(p.set_video_effects(c1.id, vec![key("")]).is_err());
}

#[test]
fn video_effects_are_held_to_the_ranges_of_their_filters() {
    let (p, _, c1, _) = cut();
    for ok in [
        VideoEffect::Blur { sigma: 1024.0 },
        VideoEffect::Sharpen { amount: -2.0 },
        VideoEffect::Sharpen { amount: 5.0 },
    ] {
        assert!(p.set_video_effects(c1.id, vec![ok.clone()]).is_ok(), "{ok:?}");
    }
    for bad in [
        VideoEffect::Blur { sigma: 1025.0 },
        VideoEffect::Blur { sigma: f64::NAN },
        VideoEffect::Sharpen { amount: 5.1 },
        VideoEffect::Sharpen { amount: -2.1 },
        VideoEffect::ChromaKey {
            color: "green".into(),
            similarity: 0.0,
            blend: 0.0,
        },
    ] {
        assert!(p.set_video_effects(c1.id, vec![bad.clone()]).is_err(), "{bad:?}");
    }
}

fn compressor(threshold_db: f64, ratio: f64, attack_ms: f64, release_ms: f64, makeup_db: f64) -> AudioEffect {
    AudioEffect::Compressor {
        threshold_db,
        ratio,
        attack_ms,
        release_ms,
        makeup_db,
    }
}

/// The audio effects a chain may hold and the ones FFmpeg's own ranges refuse.
fn audio_ranges() -> (Vec<AudioEffect>, Vec<AudioEffect>) {
    let ok = vec![
        AudioEffect::Highpass { hz: 80.0 },
        AudioEffect::Lowpass { hz: 999_999.0 },
        AudioEffect::Equalizer {
            hz: 1000.0,
            width: 200.0,
            gain_db: 900.0,
        },
        AudioEffect::Equalizer {
            hz: 1000.0,
            width: 99_999.0,
            gain_db: -900.0,
        },
        compressor(-18.0, 3.0, 20.0, 250.0, 6.0),
        compressor(-60.0, 1.0, 0.01, 9000.0, 0.0),
        compressor(0.0, 20.0, 2000.0, 0.01, 36.0),
        AudioEffect::Gate { threshold_db: -45.0 },
        AudioEffect::Gate { threshold_db: 0.0 },
        AudioEffect::Gate { threshold_db: -100.0 },
    ];
    let bad = vec![
        AudioEffect::Highpass { hz: 0.0 },
        AudioEffect::Lowpass { hz: 1_000_000.0 },
        AudioEffect::Highpass { hz: f64::NAN },
        AudioEffect::Equalizer {
            hz: 1000.0,
            width: 0.0,
            gain_db: 0.0,
        },
        AudioEffect::Equalizer {
            hz: 1000.0,
            width: 100_000.0,
            gain_db: 0.0,
        },
        AudioEffect::Equalizer {
            hz: 1000.0,
            width: 10.0,
            gain_db: 901.0,
        },
        compressor(0.5, 3.0, 20.0, 250.0, 6.0),
        compressor(-60.5, 3.0, 20.0, 250.0, 6.0),
        compressor(-18.0, 0.9, 20.0, 250.0, 6.0),
        compressor(-18.0, 20.5, 20.0, 250.0, 6.0),
        compressor(-18.0, 3.0, 0.0, 250.0, 6.0),
        compressor(-18.0, 3.0, 2000.5, 250.0, 6.0),
        compressor(-18.0, 3.0, 20.0, 0.0, 6.0),
        compressor(-18.0, 3.0, 20.0, 9001.0, 6.0),
        compressor(-18.0, 3.0, 20.0, 250.0, -0.5),
        compressor(-18.0, 3.0, 20.0, 250.0, 36.5),
        compressor(f64::NAN, 3.0, 20.0, 250.0, 6.0),
        AudioEffect::Gate { threshold_db: 0.5 },
        AudioEffect::Gate { threshold_db: -101.0 },
        AudioEffect::Gate {
            threshold_db: f64::NEG_INFINITY,
        },
    ];
    (ok, bad)
}

#[test]
fn audio_effects_are_held_to_ffmpegs_ranges_and_the_refusal_names_the_range() {
    let (p, _, c1, _) = cut();
    let (ok, bad) = audio_ranges();
    for e in ok {
        assert!(p.set_audio_effects(c1.id, vec![e.clone()]).is_ok(), "{e:?}");
    }
    for e in bad {
        assert!(p.set_audio_effects(c1.id, vec![e.clone()]).is_err(), "{e:?}");
    }
    let err = p
        .set_audio_effects(c1.id, vec![compressor(6.0, 3.0, 20.0, 250.0, 6.0)])
        .unwrap_err()
        .to_string();
    assert!(err.contains("threshold_db") && err.contains("-60..=0"), "{err}");
    let err = p
        .set_audio_effects(c1.id, vec![AudioEffect::Gate { threshold_db: 3.0 }])
        .unwrap_err()
        .to_string();
    assert!(err.contains("-100..=0"), "{err}");
}

/// Every value the check accepts is one FFmpeg accepts, and the ones it refuses are the ones
/// FFmpeg would have refused at export — against the real binary.
#[test]
#[ignore = "drives the real ffmpeg binary"]
fn the_audio_ranges_are_the_ones_ffmpeg_accepts() {
    let accepts = |effect: &AudioEffect| {
        let chain = crate::engine::audio_effects_filter(std::slice::from_ref(effect)).unwrap();
        std::process::Command::new(crate::engine::ffmpeg_path())
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=48000:cl=stereo",
                "-t",
                "0.05",
                "-af",
            ])
            .arg(&chain)
            .args(["-f", "null", "-"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    let (ok, bad) = audio_ranges();
    for e in &ok {
        assert!(accepts(e), "ffmpeg refuses {e:?}, which Kerf allows");
    }
    // Every value Kerf refuses on a range FFmpeg itself enforces is refused by FFmpeg too. A few
    // refusals are Kerf's own and FFmpeg takes them: a zero frequency or width (an inert filter),
    // the gate's floor, and a number that is not one (FFmpeg cannot be handed it at all).
    let kerf_only = |e: &AudioEffect| {
        let text = format!("{e:?}");
        text.contains("NaN")
            || text.contains("inf")
            || text.contains("hz: 0.0")
            || text.contains("width: 0.0")
            || matches!(e, AudioEffect::Gate { threshold_db } if *threshold_db < 0.0)
    };
    for e in bad.iter().filter(|e| !kerf_only(e)) {
        assert!(!accepts(e), "ffmpeg takes {e:?}, which Kerf refuses");
    }
}

// ---- clips stay in their footage and their lane ---------------------------------------------------

#[test]
fn a_clip_is_added_inside_the_footage_and_off_its_neighbours_and_the_track_stays_sorted() {
    let (p, a) = av_project();
    let before = revisions(&p);
    for (label, result) in [
        ("negative in", p.add_clip_to_timeline(a.id, None, -1.0, 5.0, None)),
        ("past the end", p.add_clip_to_timeline(a.id, None, 0.0, 61.0, None)),
        ("negative start", p.add_clip_to_timeline(a.id, None, 0.0, 5.0, Some(-2.0))),
        ("empty", p.add_clip_to_timeline(a.id, None, 5.0, 5.0, None)),
    ] {
        assert!(result.is_err(), "{label}");
    }
    let err = p.add_clip_to_timeline(a.id, None, 0.0, 61.0, None).unwrap_err().to_string();
    assert!(err.contains("past the end") && err.contains("60.000"), "{err}");
    assert_eq!(revisions(&p), before);

    p.add_clip_to_timeline(a.id, None, 0.0, 10.0, Some(20.0)).unwrap();
    p.add_clip_to_timeline(a.id, None, 0.0, 5.0, Some(12.0)).unwrap();
    p.add_clip_to_timeline(a.id, None, 0.0, 12.0, Some(0.0)).unwrap();
    p.add_clip_to_timeline(a.id, None, 55.0, 60.0, Some(40.0)).unwrap();
    let starts: Vec<f64> = tl(&p).tracks[0].clips.iter().map(|c| c.timeline_start).collect();
    assert_eq!(
        starts,
        vec![0.0, 12.0, 20.0, 40.0],
        "kept sorted; abutting is not overlapping"
    );

    let before = (revisions(&p), p.timeline_json().unwrap());
    let err = p
        .add_clip_to_timeline(a.id, None, 0.0, 5.0, Some(14.0))
        .unwrap_err()
        .to_string();
    assert!(err.contains("overlap") && err.contains("V1"), "{err}");
    assert_eq!((revisions(&p), p.timeline_json().unwrap()), before);
}

#[test]
fn a_still_has_no_end_to_run_past() {
    let (p, _) = av_project();
    let mut image = stream(StreamKind::Video);
    image.image = true;
    let still = asset("/still.png", 5.0, vec![image]);
    p.insert_asset(&still).unwrap();
    assert!(p.add_clip_to_timeline(still.id, None, 0.0, 30.0, Some(0.0)).is_ok());
}

#[test]
fn a_trim_stays_inside_the_footage_and_off_its_neighbours_unless_ripple_makes_room() {
    let (p, _, c1, c2) = cut();
    // c1 is 0..10 of a 60 s asset, c2 starts at 10.
    assert!(p.trim(c1.id, Some(-0.5), None, None).is_err());
    let err = p.trim(c2.id, None, Some(61.0), None).unwrap_err().to_string();
    assert!(err.contains("past the end"), "{err}");
    let before = (revisions(&p), p.timeline_json().unwrap());
    let err = p.trim(c1.id, None, Some(12.0), None).unwrap_err().to_string();
    assert!(err.contains("overlap") && err.contains("V1"), "{err}");
    assert_eq!((revisions(&p), p.timeline_json().unwrap()), before);

    // Shortening, and lengthening into a gap, are fine.
    assert_eq!(p.trim(c1.id, None, Some(8.0), None).unwrap().source_out, 8.0);
    assert_eq!(p.trim(c1.id, None, Some(10.0), None).unwrap().source_out, 10.0);

    // With ripple on, the later clips make the room and the trim goes through.
    p.set_ripple_mode(true).unwrap();
    p.trim(c1.id, None, Some(12.0), None).unwrap();
    assert_eq!(tl(&p).clip(c2.id).unwrap().timeline_start, 12.0);
}

#[test]
fn a_clip_may_be_trimmed_that_already_overlapped_a_neighbour() {
    let (p, _, c1, c2) = cut();
    // An overlap from before the check (written directly): shortening the other side stays allowed.
    let mut t = tl(&p);
    let (ti, ci) = t.locate(c2.id).unwrap();
    t.tracks[ti].clips[ci].timeline_start = 8.0;
    p.save_timeline(&t).unwrap();
    assert!(p.trim(c1.id, None, Some(9.0), None).is_ok());
}

#[test]
fn a_clips_speed_stays_within_the_limits() {
    let (p, _, c1, _) = cut();
    for bad in [0.0, 0.001, 0.009, 100.5, 1000.0, -1000.0, f64::NAN, f64::INFINITY] {
        assert!(p.set_speed(c1.id, bad).is_err(), "{bad}");
    }
    let err = p.set_speed(c1.id, 1000.0).unwrap_err().to_string();
    assert!(err.contains("0.01") && err.contains("100"), "{err}");
    for ok in [0.01, 0.5, 2.0, 100.0, -100.0, -0.01] {
        assert!(p.set_speed(c1.id, ok).is_ok(), "{ok}");
    }
}

#[test]
fn a_linked_partner_is_held_to_the_speed_limits_too() {
    let (p, a) = av_project();
    let pic = p.add_clip_to_timeline(a.id, None, 0.0, 40.0, Some(0.0)).unwrap();
    let sound = p.detach_audio(pic.id).unwrap().clip;
    let mut t = tl(&p);
    let (ti, ci) = t.locate(sound.id).unwrap();
    t.tracks[ti].clips[ci].speed = 50.0;
    p.save_timeline(&t).unwrap();
    // Taking the picture to 4× would take its partner to 200×.
    let err = p.set_speed(pic.id, 4.0).unwrap_err().to_string();
    assert!(err.contains("linked clip") && err.contains("200"), "{err}");
}

// ---- save as, the generation and the change counter -----------------------------------------------

#[test]
fn save_as_replaces_an_existing_file_in_one_step_and_leaves_nothing_behind() {
    let dir = scratch("save-replace");
    let dest = dir.join("out.kerf");
    std::fs::write(&dest, b"not a project at all").unwrap();
    let (p, a) = av_project();
    p.save_as(&dest).unwrap();

    let reopened = Project::open(&dest).unwrap();
    assert_eq!(reopened.list_assets().unwrap()[0].id, a.id);
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["out.kerf".to_string()], "no temporary file is left");
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn a_save_as_that_cannot_land_leaves_the_destination_exactly_as_it_was() {
    let dir = scratch("save-fail");
    // A directory cannot be replaced by a file: the rename fails, after the snapshot was written.
    let dest = dir.join("dest.kerf");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("keep.txt"), b"mine").unwrap();
    let (p, _) = av_project();
    assert!(p.save_as(&dest).is_err());
    assert_eq!(std::fs::read(dest.join("keep.txt")).unwrap(), b"mine");
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names.len(), 1, "the half-written snapshot was cleaned up: {names:?}");

    // An existing project at the destination survives a failure that happens before the rename.
    let existing = dir.join("existing.kerf");
    let old = Project::create(&existing).unwrap();
    old.set_meta("name", "old").unwrap();
    drop(old);
    let missing_dir = dir.join("no").join("such").join("dir").join("x.kerf");
    assert!(p.save_as(&missing_dir).is_err());
    assert_eq!(
        Project::open(&existing).unwrap().meta("name").unwrap().as_deref(),
        Some("old")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn saving_onto_the_projects_own_file_changes_nothing() {
    let dir = scratch("save-self");
    let path = dir.join("p.kerf");
    let p = Project::create(&path).unwrap();
    let a = asset("/av.mp4", 60.0, vec![stream(StreamKind::Video)]);
    p.insert_asset(&a).unwrap();
    p.save_as(&path).unwrap();
    assert_eq!(p.list_assets().unwrap().len(), 1, "still open and intact");
    drop(p);
    assert_eq!(Project::open(&path).unwrap().list_assets().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_project_is_never_saved_over_media_it_uses_or_the_cache() {
    let dir = scratch("save-protected");
    let source = dir.join("source.mp4");
    std::fs::write(&source, b"the footage").unwrap();
    let capture = dir.join("front.insv");
    std::fs::write(&capture, b"a lens").unwrap();
    let p = Project::open_in_memory().unwrap();
    let mut a = asset(source.to_str().unwrap(), 5.0, vec![stream(StreamKind::Video)]);
    a.source_paths = vec![capture.to_string_lossy().into_owned()];
    p.insert_asset(&a).unwrap();

    let err = p.save_as(&source).unwrap_err().to_string();
    assert!(err.contains("source file") && err.contains("overwrite"), "{err}");
    assert!(
        p.save_as(dir.join("sub").join("..").join("source.mp4")).is_err(),
        "through a detour"
    );
    assert!(p.save_as(&capture).unwrap_err().to_string().contains("capture file"));
    assert_eq!(std::fs::read(&source).unwrap(), b"the footage", "the footage is untouched");
    assert_eq!(std::fs::read(&capture).unwrap(), b"a lens");
    #[cfg(unix)]
    {
        let link = dir.join("link.kerf");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(p.save_as(&link).is_err(), "a symlink to the footage is the footage");
        assert_eq!(std::fs::read(&source).unwrap(), b"the footage");
    }
    if let Some(cache) = dirs::cache_dir() {
        let inside = cache.join("kerf").join("proxies").join("x.kerf");
        assert!(p.save_as(&inside).is_err());
        assert!(!inside.exists());
    }
    // Anywhere else is fine, and the names of what was refused left nothing behind.
    p.save_as(dir.join("ok.kerf")).unwrap();
    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.ends_with(".saving")), "{names:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_output_check_is_split_into_data_under_the_lock_and_a_static_check_after_it() {
    let dir = scratch("output-split");
    let source = dir.join("source.mp4");
    std::fs::write(&source, b"x").unwrap();
    let db = dir.join("p.kerf");
    let p = Project::create(&db).unwrap();
    p.insert_asset(&asset(source.to_str().unwrap(), 5.0, vec![stream(StreamKind::Video)]))
        .unwrap();

    let guard = p.output_path_inputs().unwrap();
    drop(p);
    // With the project gone (its lock released) the check still runs, from what was read.
    for bad in [source, db, dir.join("sub").join("..").join("source.mp4")] {
        assert!(Project::check_output_path_with(&bad, &guard).is_err(), "{}", bad.display());
    }
    assert!(Project::check_output_path_with(&dir.join("out.mp4"), &guard).is_ok());
    assert!(Project::check_output_path_with(Path::new(""), &guard).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_change_counter_keeps_counting_across_a_swapped_in_project() {
    let (mut shared, a) = av_project();
    for _ in 0..20 {
        shared.set_proxy_declined(a.id, true).unwrap();
        shared.set_proxy_declined(a.id, false).unwrap();
    }
    let before = shared.change_count();
    // The new project's own connection starts again from a handful of schema rows.
    shared.replace_with(Project::open_in_memory().unwrap());
    let after = shared.change_count();
    assert!(after > before, "{after} must not repeat or fall below {before}");
    shared.set_ripple_mode(true).unwrap();
    assert!(shared.change_count() > after);
    // Two swaps in a row (an open that was replaced at once) still only go up.
    let mid = shared.change_count();
    shared.replace_with(Project::open_in_memory().unwrap());
    shared.replace_with(Project::open_in_memory().unwrap());
    assert!(shared.change_count() > mid);
}

#[test]
fn the_generation_moves_when_a_project_is_swapped_in_and_the_review_setting_stays() {
    let mut shared = Project::open_in_memory().unwrap();
    shared.set_agent_review(false);
    let g = shared.generation();
    shared.replace_with(Project::open_in_memory().unwrap());
    assert_eq!(shared.generation(), g + 1);
    assert!(!shared.agent_review(), "the app's setting belongs to the app, not the file");
    shared.replace_with(Project::open_in_memory().unwrap());
    assert_eq!(shared.generation(), g + 2);
}

#[test]
fn a_saved_copy_swapped_in_is_the_same_project_so_its_generation_stays() {
    let dir = scratch("saved-copy");
    let (mut shared, a) = av_project();
    shared.set_agent_review(false);
    let g = shared.generation();
    let before = shared.change_count();
    let copy = dir.join("copy.kerf");
    shared.save_as(&copy).unwrap();
    shared.replace_with_saved_copy(Project::open(&copy).unwrap());
    assert_eq!(shared.generation(), g, "a job that noted it still lands");
    assert!(shared.change_count() > before, "the change counter keeps going up");
    assert!(!shared.agent_review(), "the app's setting stays");
    assert_eq!(shared.path(), Some(copy.as_path()));
    assert!(shared.get_asset(a.id).unwrap().is_some(), "the copy holds the asset");

    // What an in-flight analysis applies to the swapped-in project lands.
    shared.set_analysis(&silence_patch(a.id)).unwrap();
    assert!(shared.get_analysis(a.id).unwrap().is_some());

    // Any other project is another generation.
    shared.replace_with(Project::open_in_memory().unwrap());
    assert_eq!(shared.generation(), g + 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_agent_that_does_not_own_the_proposal_cannot_remove_the_task_it_belongs_to() {
    let (mut p, _, c1, _) = cut();
    let task = p.add_task("tighten").unwrap();
    p.set_actor(EditSource::Agent);
    p.declare_agent(Some("alpha"));
    p.claim_next_task(Some("alpha")).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();

    p.set_actor(EditSource::Agent);
    p.declare_agent(Some("beta"));
    assert!(matches!(p.remove_task(task.id), Err(Error::StagedEditOwnedByOther)));
    assert!(p.get_task(task.id).unwrap().is_some(), "the refusal deleted nothing");
    assert!(p.staged().unwrap().is_some());

    p.set_actor(EditSource::Agent);
    p.declare_agent(Some("alpha"));
    p.remove_task(task.id).unwrap();
    assert!(p.get_task(task.id).unwrap().is_none());
    assert!(p.staged().unwrap().is_none());
    assert!(matches!(p.remove_task(task.id), Err(Error::TaskNotFound(_))));
}

#[test]
fn an_agent_cannot_bring_back_a_proposal_the_user_threw_away() {
    let (mut p, _, c1, _) = cut();
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();

    p.set_actor(EditSource::Agent);
    p.declare_agent(Some("alpha"));
    let err = p.restore_discarded().unwrap_err().to_string();
    assert!(err.contains("only they can bring it back"), "{err}");
    assert!(p.staged().unwrap().is_none());
    p.set_actor(EditSource::User);
    assert_eq!(p.restore_discarded().unwrap().edits, vec!["Set volume"], "the user can");

    // An agent's own discard is its own to take back.
    p.apply_staged(false).unwrap();
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();
    p.set_volume(c1.id, 0.3).unwrap();
    p.discard_staged().unwrap();
    p.set_actor(EditSource::Agent);
    p.declare_agent(Some("alpha"));
    assert_eq!(p.restore_discarded().unwrap().edits, vec!["Set volume"]);
}

#[test]
fn the_change_counter_moves_for_every_kind_of_write_and_for_no_read() {
    let (p, a) = av_project();
    let start = p.change_count();
    let _ = (
        p.list_assets().unwrap(),
        p.timeline().unwrap(),
        p.history().unwrap(),
        p.meta("x").unwrap(),
    );
    assert_eq!(p.change_count(), start, "reads write nothing");

    let mut last = start;
    let mut moved = |what: &str, p: &Project| {
        let now = p.change_count();
        assert!(now > last, "{what} did not move the counter");
        last = now;
    };
    p.set_ripple_mode(true).unwrap();
    moved("the ripple flag", &p);
    p.set_proxy_declined(a.id, true).unwrap();
    moved("a declined proxy", &p);
    p.merge_analysis(&silence_patch(a.id)).unwrap();
    moved("an analysis merge", &p);
    p.set_meta("name", "x").unwrap();
    moved("meta", &p);
    p.add_task("do it").unwrap();
    moved("a task", &p);
    p.add_clip_to_timeline(a.id, None, 0.0, 5.0, None).unwrap();
    moved("an edit", &p);
}

// ---- paging ---------------------------------------------------------------------------------------------

#[test]
fn history_and_the_task_queue_are_read_a_page_at_a_time() {
    let (p, _, c1, _) = cut();
    for v in [0.9, 0.8, 0.7, 0.6] {
        p.set_volume(c1.id, v).unwrap();
    }
    let all = p.history().unwrap();
    assert_eq!(all.len(), 7, "initial + two clips + four volumes");

    let (last2, total) = p.history_page(Page::last(2)).unwrap();
    assert_eq!(total, 7);
    assert_eq!(
        last2.iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![5, 6],
        "the newest two, oldest first"
    );
    assert!(last2[1].current);
    let (older, _) = p
        .history_page(Page {
            limit: Some(3),
            offset: 2,
        })
        .unwrap();
    assert_eq!(older.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![2, 3, 4]);
    assert!(p
        .history_page(Page {
            limit: Some(3),
            offset: 99
        })
        .unwrap()
        .0
        .is_empty());
    assert_eq!(p.history_page(Page::default()).unwrap().0.len(), all.len());

    for i in 0..5 {
        p.add_task(&format!("task {i}")).unwrap();
    }
    let (tail, total) = p.list_tasks_page(Page::last(2)).unwrap();
    assert_eq!(total, 5);
    assert_eq!(
        tail.iter().map(|t| t.prompt.as_str()).collect::<Vec<_>>(),
        vec!["task 3", "task 4"]
    );
    assert_eq!(p.list_tasks().unwrap().len(), 5);
}

// ---- import and output path safety ------------------------------------------------------------------

#[test]
fn only_an_absolute_path_to_a_regular_file_can_be_imported() {
    let dir = scratch("import");
    for bad in ["clip.mp4", "./clip.mp4", "http://example.com/a.mp4", "concat:/a|/b", "pipe:0"] {
        let err = Project::probe_asset(Path::new(bad)).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "{bad}: {err:?}");
    }
    assert!(Project::probe_asset(&dir).is_err(), "a directory");
    assert!(Project::probe_asset(&dir.join("missing.mp4")).is_err());
    let p = Project::open_in_memory().unwrap();
    assert!(p.import_asset("relative.mp4").is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_export_may_not_overwrite_a_source_the_project_file_or_the_cache() {
    let dir = scratch("output");
    let source = dir.join("source.mp4");
    std::fs::write(&source, b"x").unwrap();
    let db = dir.join("p.kerf");
    let p = Project::create(&db).unwrap();
    let a = asset(source.to_str().unwrap(), 5.0, vec![stream(StreamKind::Video)]);
    p.insert_asset(&a).unwrap();

    let err = p.check_output_path(&source).unwrap_err().to_string();
    assert!(err.contains("source file") && err.contains("overwrite"), "{err}");
    assert!(p.check_output_path(&dir.join("sub").join("..").join("source.mp4")).is_err());
    assert!(p
        .check_output_path(&db)
        .unwrap_err()
        .to_string()
        .contains("this project's own file"));
    let mut journal = db.into_os_string();
    journal.push("-journal");
    assert!(p.check_output_path(Path::new(&journal)).is_err());
    assert!(p.check_output_path(&dir.join("out.mp4")).is_ok());

    if let Some(cache) = dirs::cache_dir() {
        assert!(p
            .check_output_path(&cache.join("kerf").join("proxies").join("x.mp4"))
            .is_err());
    }

    // The export entry points refuse before rendering anything.
    assert!(p.export(&source, "mp4").is_err());
    assert!(p.export_with(&source, &engine::ExportOptions::default()).is_err());
    assert!(p.export_still(0.0, &source, None).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), b"x", "the source is untouched");
    let _ = std::fs::remove_dir_all(dir);
}

// ---- staging: owner, auto-review, step undo, partial accept, restorable discard ------------------------

#[test]
fn with_review_on_an_agents_first_edit_opens_a_proposal_and_the_live_cut_never_moves() {
    let (mut p, _, c1, _) = cut();
    assert!(p.agent_review(), "review is on unless the app turns it off");
    let live = (revisions(&p), p.timeline_json().unwrap());

    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_volume(c1.id, 0.25).unwrap();
    assert_eq!(
        (revisions(&p), p.timeline_json().unwrap()),
        live,
        "nothing reached the user's cut"
    );
    let staged = p.staged().unwrap().expect("the first edit opened a session");
    assert_eq!(staged.edits, vec!["Set volume", "Set volume"]);
    assert_eq!(staged.owner, None);
    assert_eq!(
        p.working_timeline().unwrap().clip(c1.id).unwrap().volume,
        0.25,
        "the agent sees its work"
    );

    p.set_actor(EditSource::User);
    p.apply_staged(false).unwrap();
    assert_eq!(volume(&p, c1.id), 0.25);
    let last = p.history().unwrap().pop().unwrap();
    assert_eq!(last.source, EditSource::Agent);
    assert_eq!(revisions(&p), live.0 + 1, "one revision for the proposal");
}

#[test]
fn an_edit_that_fails_leaves_no_empty_proposal_behind() {
    let (mut p, ..) = cut();
    p.set_actor(EditSource::Agent);
    assert!(p.set_volume(Uuid::new_v4(), 0.5).is_err());
    assert!(p.staged().unwrap().is_none());
    assert!(p.trim(Uuid::new_v4(), None, Some(5.0), None).is_err());
    assert!(p.staged().unwrap().is_none());
}

#[test]
fn with_review_off_an_agent_edits_the_live_cut() {
    let (mut p, _, c1, _) = cut();
    p.set_agent_review(false);
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    assert!(p.staged().unwrap().is_none());
    assert_eq!(volume(&p, c1.id), 0.5);
    assert_eq!(p.history().unwrap().pop().unwrap().source, EditSource::Agent);
    // An explicit session is still honoured.
    p.begin_staging(None, None, None).unwrap();
    p.set_volume(c1.id, 0.1).unwrap();
    assert_eq!(volume(&p, c1.id), 0.5);
}

#[test]
fn an_agent_session_owns_its_proposal_and_another_is_turned_away() {
    let (mut p, _, c1, _) = cut();
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();
    assert!(p.staged().unwrap().is_none(), "declaring opens nothing: the edit does");
    p.set_volume(c1.id, 0.5).unwrap();
    let staged = p.staged().unwrap().expect("review on: the edit opened a session");
    assert_eq!(staged.owner.as_deref(), Some("alpha"));
    assert!(staged.owned_by_agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();

    // Another agent: refused before it edits, however it asks.
    for other in [Some("beta"), None] {
        assert!(
            matches!(p.ensure_agent_staging(other), Err(Error::StagedEditOwnedByOther)),
            "{other:?}"
        );
    }
    assert!(Error::StagedEditOwnedByOther.to_string().contains("another agent session"));
    p.set_actor(EditSource::Agent); // a new lock: the declared session is forgotten
    p.ensure_agent_staging(Some("beta")).ok();
    assert!(
        matches!(p.set_volume(c1.id, 0.1), Err(Error::StagedEditOwnedByOther)),
        "a declared stranger cannot edit"
    );
    assert!(matches!(p.discard_staged(), Err(Error::StagedEditOwnedByOther)));
    assert!(matches!(p.undo_staged(), Err(Error::StagedEditOwnedByOther)));
    p.set_actor(EditSource::Agent);
    assert!(
        matches!(p.discard_staged(), Err(Error::StagedEditOwnedByOther)),
        "one that names nobody is a stranger"
    );
    assert_eq!(
        p.staged().unwrap().unwrap().edits.len(),
        1,
        "none of that touched the proposal"
    );

    // The owner may; so may the user, whoever owns it.
    p.ensure_agent_staging(Some("alpha")).unwrap();
    p.undo_staged().unwrap();
    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();
    assert!(
        p.staged().unwrap().is_none(),
        "the user can end an agent's session whoever owns it"
    );
}

#[test]
fn declaring_a_session_lets_its_owner_end_a_proposal_without_opening_one() {
    let (mut p, _, c1, _) = cut();
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();

    p.set_actor(EditSource::Agent);
    p.declare_agent(Some("beta"));
    assert!(matches!(p.discard_staged(), Err(Error::StagedEditOwnedByOther)));
    p.declare_agent(Some("alpha"));
    p.discard_staged().unwrap();

    // Declaring opens nothing, even with review on.
    p.set_actor(EditSource::Agent);
    p.declare_agent(Some("alpha"));
    assert!(p.staged().unwrap().is_none());
    assert!(matches!(p.discard_staged(), Err(Error::NoStagedEdit)));
}

#[test]
fn a_proposal_with_no_owner_is_adopted_by_the_first_named_session_to_edit_it() {
    let (mut p, _, c1, _) = cut();
    p.set_actor(EditSource::Agent);
    p.begin_staging(None, Some("note"), None).unwrap();
    // Declaring takes nothing; the edit that lands does.
    p.ensure_agent_staging(Some("alpha")).unwrap();
    assert_eq!(p.staged().unwrap().unwrap().owner, None);
    p.set_volume(c1.id, 0.5).unwrap();
    assert_eq!(p.staged().unwrap().unwrap().owner.as_deref(), Some("alpha"));
    assert!(matches!(
        p.ensure_agent_staging(Some("beta")),
        Err(Error::StagedEditOwnedByOther)
    ));
}

#[test]
fn ensuring_a_session_writes_nothing_so_a_failed_edit_leaves_no_trace() {
    let (mut p, _, c1, _) = cut();
    let written = p.change_count();
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();
    assert_eq!(p.change_count(), written, "declaring and checking write nothing");

    // Review on, nothing open: an edit that fails opens no proposal for the session.
    assert!(p.set_volume(Uuid::new_v4(), 0.5).is_err());
    assert!(p.set_volume(c1.id, f32::NAN).is_err());
    assert!(p.staged().unwrap().is_none(), "no empty proposal owned by the failing agent");

    // A proposal nobody owns yet (a claimed task's): a failing edit does not adopt it either.
    p.begin_staging(None, Some("task"), None).unwrap();
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();
    assert!(p.set_volume(Uuid::new_v4(), 0.5).is_err());
    let staged = p.staged().unwrap().unwrap();
    assert_eq!((staged.owner, staged.owned_by_agent), (None, false));
    // The edit that lands is what takes it.
    p.set_volume(c1.id, 0.5).unwrap();
    assert_eq!(p.staged().unwrap().unwrap().owner.as_deref(), Some("alpha"));

    // The same goes for the library: a held-back asset change opens a session in its own transaction.
    let (mut q, a) = av_project();
    q.set_actor(EditSource::Agent);
    q.ensure_agent_staging(Some("alpha")).unwrap();
    assert!(q.set_asset_projection(a.id, Some(Projection::Flat)).is_err());
    assert!(q.staged().unwrap().is_none());
    q.set_asset_projection(a.id, Some(Projection::Equirect)).unwrap();
    assert_eq!(q.staged().unwrap().unwrap().owner.as_deref(), Some("alpha"));
}

#[test]
fn what_the_webview_and_other_agents_read_of_a_proposal_never_carries_the_session_id() {
    let (mut p, _, c1, _) = cut();
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("secret-session-id")).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    let staged = p.staged().unwrap().unwrap();
    assert_eq!(staged.owner.as_deref(), Some("secret-session-id"), "kept for the Rust side");
    assert!(staged.owned_by_agent);

    let json = serde_json::to_value(&staged).unwrap();
    assert!(!json.to_string().contains("secret-session-id"), "{json}");
    assert!(json.get("owner").is_none());
    assert_eq!(json["owned_by_agent"], true);

    // An unowned proposal says so.
    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();
    p.begin_staging(None, None, None).unwrap();
    let json = serde_json::to_value(p.staged().unwrap().unwrap()).unwrap();
    assert_eq!(json["owned_by_agent"], false);
}

#[test]
fn with_review_off_ensuring_a_session_opens_nothing_but_still_guards_an_owned_one() {
    let (mut p, ..) = cut();
    p.set_agent_review(false);
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();
    assert!(p.staged().unwrap().is_none());
    p.begin_staging(None, None, Some("alpha")).unwrap();
    assert!(matches!(
        p.ensure_agent_staging(Some("beta")),
        Err(Error::StagedEditOwnedByOther)
    ));
}

#[test]
fn an_agent_cannot_apply_force_apply_resolve_or_rewind_while_review_is_on() {
    let (mut p, _, c1, _) = cut();
    let task = p.add_task("tighten").unwrap();
    p.set_actor(EditSource::Agent);
    p.claim_next_task(Some("alpha")).unwrap().unwrap();
    assert_eq!(p.staged().unwrap().unwrap().owner.as_deref(), Some("alpha"));
    p.set_volume(c1.id, 0.5).unwrap();
    let id = p.staged().unwrap().unwrap().diff.entries[0].id.clone();

    for result in [
        p.apply_staged(false).map(|_| ()),
        p.apply_staged(true).map(|_| ()),
        p.apply_staged_subset(&[id], true).map(|_| ()),
        p.resolve_task(task.id).map(|_| ()),
        p.undo().map(|_| ()),
        p.revert_to(0).map(|_| ()),
    ] {
        assert!(result.is_err());
    }
    assert!(matches!(p.apply_staged(true), Err(Error::AgentReviewRequired)));
    assert!(Error::AgentReviewRequired.to_string().contains("user applies agent edits"));
    assert_eq!(volume(&p, c1.id), 1.0, "nothing was applied");
    assert!(p.staged().unwrap().is_some());

    // Accepting the task is the user's: it lands the proposal.
    p.set_actor(EditSource::User);
    p.resolve_task(task.id).unwrap();
    assert_eq!(volume(&p, c1.id), 0.5);
}

#[test]
fn with_review_off_an_agent_may_apply_only_its_own_proposal() {
    let (mut p, _, c1, _) = cut();
    p.set_agent_review(false);
    p.set_actor(EditSource::Agent);
    p.begin_staging(None, None, Some("alpha")).unwrap();
    p.ensure_agent_staging(Some("alpha")).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();

    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("beta")).ok();
    assert!(matches!(p.apply_staged(false), Err(Error::StagedEditOwnedByOther)));
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();
    p.apply_staged(false).unwrap();
    assert_eq!(volume(&p, c1.id), 0.5);
}

#[test]
fn the_proposals_steps_are_undone_one_at_a_time_and_remembered_a_bounded_way_back() {
    let (mut p, _, c1, c2) = cut();
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_volume(c2.id, 0.2).unwrap();
    p.set_fade(c1.id, Some(0.5), None).unwrap();
    assert_eq!(p.staged().unwrap().unwrap().undo_depth, 3);

    let (label, view) = p.undo_staged().unwrap();
    assert_eq!(label, "Set fade");
    assert_eq!(view.edits, vec!["Set volume", "Set volume"]);
    assert_eq!(view.undo_depth, 2);
    assert_eq!(p.staged_timeline().unwrap().unwrap().clip(c1.id).unwrap().fade_in, 0.0);
    assert_eq!(p.staged_timeline().unwrap().unwrap().clip(c2.id).unwrap().volume, 0.2);
    p.undo_staged().unwrap();
    p.undo_staged().unwrap();
    assert!(p.staged().unwrap().unwrap().diff.is_empty(), "back to the base");
    let err = p.undo_staged().unwrap_err().to_string();
    assert!(err.contains("nothing left to undo"), "{err}");
    assert!(matches!(
        {
            p.discard_staged().ok();
            p.undo_staged()
        },
        Err(Error::NoStagedEdit)
    ));

    // 25 steps: only the last STAGED_UNDO_DEPTH can be taken back.
    for i in 0..25 {
        p.set_volume(c1.id, 0.1 + i as f32 * 0.01).unwrap();
    }
    assert_eq!(p.staged().unwrap().unwrap().undo_depth, STAGED_UNDO_DEPTH);
    for _ in 0..STAGED_UNDO_DEPTH {
        p.undo_staged().unwrap();
    }
    assert!(p.undo_staged().is_err());
    assert_eq!(
        p.staged().unwrap().unwrap().edits.len(),
        5,
        "the five older steps stay, un-undoable"
    );
}

#[test]
fn an_agents_change_to_an_asset_is_held_with_its_proposal_and_lands_with_it() {
    let (mut p, a, c1, _) = cut();
    p.set_analysis(&silence_patch(a.id)).unwrap();
    p.set_actor(EditSource::Agent);
    let returned = p.set_asset_projection(a.id, Some(Projection::Equirect)).unwrap();
    assert_eq!(returned.projection(), Some(Projection::Equirect));

    // The library is the user's until the proposal is accepted; the agent sees its own pending work.
    assert_eq!(p.get_asset(a.id).unwrap().unwrap().projection(), Some(Projection::Equirect));
    assert_eq!(p.list_assets().unwrap()[0].projection(), Some(Projection::Equirect));
    p.set_actor(EditSource::User);
    assert_eq!(p.get_asset(a.id).unwrap().unwrap().projection(), None);
    let staged = p.staged().unwrap().unwrap();
    assert_eq!(staged.asset_changes.len(), 1);
    assert!(
        staged.asset_changes[0].summary.contains("equirect"),
        "{:?}",
        staged.asset_changes
    );
    assert_eq!(staged.asset_changes[0].id, format!("asset_projection:{}", a.id));

    // Taking the step back takes the change back.
    p.set_actor(EditSource::Agent);
    p.undo_staged().unwrap();
    assert!(p.staged().unwrap().unwrap().asset_changes.is_empty());
    assert_eq!(p.get_asset(a.id).unwrap().unwrap().projection(), None);

    // A clip cut while it is pending reframes (the asset is 360 as far as the proposal goes).
    p.set_asset_projection(a.id, Some(Projection::Equirect)).unwrap();
    let c3 = p.add_clip_to_timeline(a.id, None, 30.0, 35.0, Some(30.0)).unwrap();
    assert!(c3.reframe.is_some());

    // Accepting lands the asset change; there is no timeline revision for it unless the cut changed.
    p.set_actor(EditSource::User);
    p.apply_staged(false).unwrap();
    assert_eq!(p.get_asset(a.id).unwrap().unwrap().projection(), Some(Projection::Equirect));
    assert!(p.get_analysis(a.id).unwrap().is_some(), "and the analysis survived it");
    assert!(tl(&p).clip(c3.id).is_some());
    let _ = c1;

    // Discarded instead, it never lands.
    p.set_actor(EditSource::Agent);
    p.set_asset_projection(a.id, None).unwrap();
    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();
    assert_eq!(p.get_asset(a.id).unwrap().unwrap().projection(), Some(Projection::Equirect));
}

#[test]
fn an_asset_only_proposal_is_not_an_empty_one_and_adds_no_revision() {
    let (mut p, a, ..) = cut();
    let before = revisions(&p);
    p.set_actor(EditSource::Agent);
    p.set_asset_projection(a.id, Some(Projection::Fisheye)).unwrap();
    p.set_actor(EditSource::User);
    p.apply_staged(false).unwrap();
    assert_eq!(revisions(&p), before, "the library is not in the edit history");
    assert_eq!(p.get_asset(a.id).unwrap().unwrap().projection(), Some(Projection::Fisheye));

    // Asking for what the library already has stages nothing.
    p.set_actor(EditSource::Agent);
    p.set_asset_projection(a.id, Some(Projection::Fisheye)).unwrap();
    assert!(p.staged().unwrap().is_none());
}

#[test]
fn review_off_and_no_session_an_agent_marks_an_asset_directly() {
    let (mut p, a, ..) = cut();
    p.set_agent_review(false);
    p.set_actor(EditSource::Agent);
    p.set_asset_projection(a.id, Some(Projection::Equirect)).unwrap();
    assert!(p.staged().unwrap().is_none());
    p.set_actor(EditSource::User);
    assert_eq!(p.get_asset(a.id).unwrap().unwrap().projection(), Some(Projection::Equirect));
}

/// A proposal with three independent changes: c1's volume, c2's volume and a marker.
fn three_changes() -> (Project, Clip, Clip, Vec<String>) {
    let (mut p, _, c1, c2) = cut();
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_volume(c2.id, 0.25).unwrap();
    p.add_marker(3.0, "beat".into(), None).unwrap();
    p.set_actor(EditSource::User);
    let ids = p
        .staged()
        .unwrap()
        .unwrap()
        .diff
        .entries
        .iter()
        .map(|e| e.id.clone())
        .collect();
    (p, c1, c2, ids)
}

#[test]
fn a_partial_accept_lands_the_picked_changes_and_leaves_the_rest_for_review() {
    let (p, c1, c2, ids) = three_changes();
    assert_eq!(ids.len(), 3);
    let first = ids
        .iter()
        .find(|i| i.starts_with("clip_changed:") && i.ends_with(&c1.id.to_string()))
        .unwrap();
    let before = revisions(&p);

    let done = p.apply_staged_subset(std::slice::from_ref(first), false).unwrap();
    assert_eq!((done.applied, done.remaining, done.closed), (1, 2, false));
    assert!(done.auto_included.is_empty() && done.skipped.is_empty());
    assert!(
        done.message.contains("Applied 1 of 3") && done.message.contains("2 left"),
        "{}",
        done.message
    );
    assert_eq!(volume(&p, c1.id), 0.5);
    assert_eq!(volume(&p, c2.id), 1.0, "what was not picked is not applied");
    assert!(tl(&p).markers.is_empty());
    assert_eq!(revisions(&p), before + 1);
    let last = p.history().unwrap().pop().unwrap();
    assert_eq!(last.source, EditSource::Agent);
    assert!(last.label.contains("1 of 3"), "{}", last.label);

    // The rest of the proposal is still there, branched from the cut as it now is.
    let staged = p.staged().unwrap().expect("the session carries on");
    assert!(!staged.stale);
    assert_eq!(staged.diff.entries.len(), 2);
    assert_eq!(staged.undo_depth, 0);

    // Picking everything left closes it.
    let rest: Vec<String> = staged.diff.entries.iter().map(|e| e.id.clone()).collect();
    let done = p.apply_staged_subset(&rest, false).unwrap();
    assert!(done.closed && done.remaining == 0);
    assert!(p.staged().unwrap().is_none());
    assert_eq!(volume(&p, c2.id), 0.25);
    assert_eq!(tl(&p).markers.len(), 1);
}

/// c1 (0..10), c2 (10..16) and c3 (16..22) on V1; an agent proposes a new volume for c1 and c2 and
/// a marker, then the user edits c3 (so the proposal is stale).
fn stale_proposal() -> (Project, Clip, Clip, Clip, Vec<String>) {
    let (mut p, a, c1, c2) = cut();
    let c3 = p.add_clip_to_timeline(a.id, None, 30.0, 36.0, Some(16.0)).unwrap();
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_volume(c2.id, 0.25).unwrap();
    p.add_marker(3.0, "beat".into(), None).unwrap();
    p.set_actor(EditSource::User);
    p.set_volume(c3.id, 0.7).unwrap();
    let ids = p
        .staged()
        .unwrap()
        .unwrap()
        .diff
        .entries
        .iter()
        .map(|e| e.id.clone())
        .collect();
    (p, c1, c2, c3, ids)
}

#[test]
fn a_forced_partial_accept_leaves_exactly_the_unpicked_changes_and_keeps_the_users_newer_work() {
    let (p, c1, c2, c3, ids) = stale_proposal();
    assert!(p.staged().unwrap().unwrap().stale);
    // The proposal is stale: picking without force is refused, with it the pick lands.
    let pick = ids
        .iter()
        .find(|i| i.starts_with("clip_changed:") && i.ends_with(&c1.id.to_string()))
        .unwrap()
        .clone();
    assert!(matches!(
        p.apply_staged_subset(std::slice::from_ref(&pick), false),
        Err(Error::StagedEditStale)
    ));

    let done = p.apply_staged_subset(std::slice::from_ref(&pick), true).unwrap();
    assert_eq!((done.applied, done.remaining, done.closed), (1, 2, false), "{}", done.message);
    assert_eq!(volume(&p, c1.id), 0.5, "the pick landed");
    assert_eq!(volume(&p, c3.id), 0.7, "the user's newer edit stands on the live cut");
    assert_eq!(volume(&p, c2.id), 1.0);

    // What is left to review is the two unpicked agent changes and nothing else: the proposal is
    // not "the old proposal against the new cut", which would read as undoing the user's edit of c3.
    let staged = p.staged().unwrap().expect("the rest stays open");
    assert!(!staged.stale, "it is re-based onto the cut as it now is");
    let mut left: Vec<&str> = staged.diff.entries.iter().map(|e| e.id.as_str()).collect();
    left.sort_unstable();
    let mut want: Vec<&str> = ids.iter().filter(|i| **i != pick).map(String::as_str).collect();
    want.sort_unstable();
    assert_eq!(left, want, "exactly the unpicked agent changes");
    assert!(staged.diff.entries.iter().all(|e| e.clip_id != Some(c3.id)));
    let proposed = p.staged_timeline().unwrap().unwrap();
    assert_eq!(
        proposed.clip(c3.id).unwrap().volume,
        0.7,
        "the user's edit is in the rest of the proposal"
    );
    assert_eq!(proposed.clip(c1.id).unwrap().volume, 0.5);
    assert_eq!(proposed.clip(c2.id).unwrap().volume, 0.25);

    // Accepting the rest finishes the job without touching c3.
    let done = p
        .apply_staged_subset(&want.iter().map(|s| s.to_string()).collect::<Vec<_>>(), false)
        .unwrap();
    assert!(done.closed, "{}", done.message);
    assert!(p.staged().unwrap().is_none());
    assert_eq!((volume(&p, c1.id), volume(&p, c2.id), volume(&p, c3.id)), (0.5, 0.25, 0.7));
    assert_eq!(tl(&p).markers.len(), 1);
}

#[test]
fn a_forced_partial_accept_applying_only_a_library_change_still_keeps_the_users_work() {
    let (mut p, a, c1, ..) = cut();
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_asset_projection(a.id, Some(Projection::Equirect)).unwrap();
    p.set_actor(EditSource::User);
    let c3 = p.add_clip_to_timeline(a.id, None, 40.0, 44.0, Some(16.0)).unwrap();
    let staged = p.staged().unwrap().unwrap();
    assert!(staged.stale);
    let asset_change = staged.asset_changes[0].id.clone();

    let done = p.apply_staged_subset(&[asset_change], true).unwrap();
    assert_eq!((done.applied, done.remaining), (1, 1));
    assert_eq!(p.require_asset(a.id).unwrap().projection(), Some(Projection::Equirect));
    let kept = p.staged_timeline().unwrap().unwrap();
    assert!(
        kept.clip(c3.id).is_some(),
        "the clip the user added since is not undone by the rest"
    );
    assert_eq!(kept.clip(c1.id).unwrap().volume, 0.5);
    assert_eq!(p.staged().unwrap().unwrap().diff.entries.len(), 1);
}

#[test]
fn a_change_of_the_rest_that_no_longer_fits_the_cut_is_dropped_and_said() {
    let (p, c1, c2, c3, ids) = stale_proposal();
    // The user removes c2 after the agent changed it, and a forced accept goes ahead.
    p.remove_clips(&[c2.id]).unwrap();
    let pick = ids
        .iter()
        .find(|i| i.starts_with("clip_changed:") && i.ends_with(&c1.id.to_string()))
        .unwrap()
        .clone();
    let done = p.apply_staged_subset(&[pick], true).unwrap();
    assert!(
        done.message.contains("no longer fit") && done.message.contains("dropped"),
        "{}",
        done.message
    );
    assert_eq!(done.remaining, 1, "only the marker is left: {}", done.message);
    let staged = p.staged().unwrap().unwrap();
    assert_eq!(staged.diff.entries.len(), 1);
    assert!(p.staged_timeline().unwrap().unwrap().clip(c2.id).is_none(), "not resurrected");
    assert_eq!(p.staged_timeline().unwrap().unwrap().clip(c3.id).unwrap().volume, 0.7);
}

#[test]
fn a_partial_accept_brings_a_linked_partner_along_and_says_so() {
    let (p, a) = av_project();
    let pic = p.add_clip_to_timeline(a.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    let sound = p.detach_audio(pic.id).unwrap().clip;
    let mut p = p;
    p.set_actor(EditSource::Agent);
    p.set_volume(pic.id, 0.5).unwrap();
    p.set_volume(sound.id, 0.5).unwrap();
    p.set_actor(EditSource::User);
    let diff = p.staged().unwrap().unwrap().diff;
    let pick = diff.entries.iter().find(|e| e.clip_id == Some(pic.id)).unwrap().id.clone();

    let done = p.apply_staged_subset(&[pick], false).unwrap();
    assert_eq!(done.applied, 2);
    assert_eq!(done.auto_included.len(), 1);
    assert!(done.auto_included[0].id.ends_with(&sound.id.to_string()));
    assert!(done.auto_included[0].because.contains("linked"));
    assert!(done.message.contains("came with"), "{}", done.message);
    assert_eq!((volume(&p, pic.id), volume(&p, sound.id)), (0.5, 0.5));
    assert!(done.closed);
}

#[test]
fn a_partial_accept_that_would_stack_two_clips_is_refused_whole() {
    let (mut p, _, c1, c2) = cut();
    p.set_actor(EditSource::Agent);
    p.with_ripple(Some(true), |p| p.trim(c1.id, None, Some(12.0), None)).unwrap();
    p.set_actor(EditSource::User);
    let diff = p.staged().unwrap().unwrap().diff;
    let retrim = diff
        .entries
        .iter()
        .find(|e| e.kind == DiffKind::ClipRetrimmed)
        .unwrap()
        .id
        .clone();
    let before = (revisions(&p), p.timeline_json().unwrap());

    let err = p
        .apply_staged_subset(std::slice::from_ref(&retrim), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("on top of each other") && err.contains("V1"), "{err}");
    assert_eq!((revisions(&p), p.timeline_json().unwrap()), before);
    assert_eq!(
        p.staged().unwrap().unwrap().diff.entries.len(),
        diff.entries.len(),
        "the proposal is untouched"
    );

    // With the push that makes room it goes through.
    let moved = diff
        .entries
        .iter()
        .find(|e| e.kind == DiffKind::ClipMoved && e.clip_id == Some(c2.id))
        .unwrap();
    p.apply_staged_subset(&[retrim, moved.id.clone()], false).unwrap();
    assert_eq!(tl(&p).clip(c2.id).unwrap().timeline_start, 12.0);
}

#[test]
fn a_partial_accept_refuses_unknown_empty_and_stale_picks() {
    let (p, c1, _, ids) = three_changes();
    let before = (revisions(&p), p.timeline_json().unwrap());
    assert!(p.apply_staged_subset(&[], false).is_err());
    let err = p
        .apply_staged_subset(&["clip_changed:nope".to_string()], false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no change clip_changed:nope"), "{err}");
    assert_eq!((revisions(&p), p.timeline_json().unwrap()), before);

    // The user cut on while the proposal waited: refused unless forced, then it lands by identity.
    p.set_volume(c1.id, 0.9).unwrap();
    assert!(matches!(p.apply_staged_subset(&ids[..1], false), Err(Error::StagedEditStale)));
    let done = p.apply_staged_subset(&ids, true).unwrap();
    assert!(done.applied >= 1);
    assert!(p.staged().unwrap().is_none() || !p.staged().unwrap().unwrap().diff.is_empty());
}

#[test]
fn a_partial_accept_can_pick_an_asset_change_and_skips_what_the_user_removed() {
    let (mut p, a, c1, c2) = cut();
    p.set_actor(EditSource::Agent);
    p.set_asset_projection(a.id, Some(Projection::Equirect)).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_volume(c2.id, 0.25).unwrap();
    p.set_actor(EditSource::User);
    let staged = p.staged().unwrap().unwrap();
    let asset_pick = staged.asset_changes[0].id.clone();
    let c1_pick = staged
        .diff
        .entries
        .iter()
        .find(|e| e.clip_id == Some(c1.id))
        .unwrap()
        .id
        .clone();

    // The user deletes c1 while the proposal waits.
    p.remove(c1.id).unwrap();
    let done = p.apply_staged_subset(&[asset_pick, c1_pick.clone()], true).unwrap();
    assert_eq!(done.applied, 1, "the asset change");
    assert_eq!(done.skipped.len(), 1);
    assert_eq!(done.skipped[0].id, c1_pick);
    assert!(done.skipped[0].why.contains("no longer"), "{:?}", done.skipped);
    assert_eq!(p.get_asset(a.id).unwrap().unwrap().projection(), Some(Projection::Equirect));
    assert_eq!(volume(&p, c2.id), 1.0);
    assert!(p.staged().unwrap().unwrap().asset_changes.is_empty());
}

#[test]
fn a_discarded_proposal_can_be_brought_back_once_and_only_when_none_is_open() {
    let (mut p, _, c1, _) = cut();
    assert!(matches!(p.restore_discarded(), Err(Error::NoDiscardedEdit)));
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    p.add_marker(2.0, "m".into(), None).unwrap();
    let was = p.staged().unwrap().unwrap();

    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();
    assert!(p.staged().unwrap().is_none());
    let kept = p.discarded_staged().unwrap().expect("kept");
    assert_eq!(kept.edits, was.edits);

    // Not while a new proposal is open.
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.1).unwrap();
    p.set_actor(EditSource::User);
    assert!(matches!(p.restore_discarded(), Err(Error::StagedEditPending)));
    p.discard_staged().unwrap();
    // …which, being the latest discard, is what comes back.
    let back = p.restore_discarded().unwrap();
    assert_eq!(back.edits, vec!["Set volume"]);
    assert_eq!(volume(&p, c1.id), 1.0, "restoring a proposal applies nothing");
    assert!(p.discarded_staged().unwrap().is_none(), "the copy is spent");
    p.apply_staged(false).unwrap();
    assert!(matches!(p.restore_discarded(), Err(Error::NoDiscardedEdit)));
}

#[test]
fn a_discarded_proposal_is_forgotten_once_a_newer_one_is_applied() {
    let (mut p, _, c1, c2) = cut();
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();
    assert!(p.discarded_staged().unwrap().is_some());

    // A session that changed nothing closing does not take it.
    p.begin_staging(None, None, None).unwrap();
    p.apply_staged(false).unwrap();
    assert!(p.discarded_staged().unwrap().is_some());

    p.set_actor(EditSource::Agent);
    p.set_volume(c2.id, 0.7).unwrap();
    p.set_actor(EditSource::User);
    p.apply_staged(false).unwrap();
    assert!(
        p.discarded_staged().unwrap().is_none(),
        "what was thrown away before is no longer an undo"
    );
    assert!(matches!(p.restore_discarded(), Err(Error::NoDiscardedEdit)));
}

#[test]
fn a_discarded_proposal_is_forgotten_once_part_of_a_newer_one_is_applied() {
    let (mut p, _, c1, c2) = cut();
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();

    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.4).unwrap();
    p.set_volume(c2.id, 0.7).unwrap();
    p.set_actor(EditSource::User);
    let pick = format!("clip_changed:{}", c1.id);
    p.apply_staged_subset(&[pick], false).unwrap();
    assert!(p.discarded_staged().unwrap().is_none());
}

#[test]
fn a_discarded_proposal_is_not_offered_again_in_a_project_that_is_opened_afresh() {
    let dir = scratch("discard-replace");
    let path = dir.join("p.kerf");
    let mut p = Project::create(&path).unwrap();
    let a = asset("/av.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]);
    p.insert_asset(&a).unwrap();
    let c1 = p.add_clip_to_timeline(a.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();
    drop(p);

    // Opening the file keeps it (Save As reopens a copy)…
    assert!(Project::open(&path).unwrap().discarded_staged().unwrap().is_some());
    let mut shared = Project::open_in_memory().unwrap();
    shared.replace_with_saved_copy(Project::open(&path).unwrap());
    assert!(
        shared.discarded_staged().unwrap().is_some(),
        "the same project, in another file"
    );
    // …making it the project the app has open does not.
    shared.replace_with(Project::open(&path).unwrap());
    assert!(shared.discarded_staged().unwrap().is_none());
    assert!(
        Project::open(&path).unwrap().discarded_staged().unwrap().is_none(),
        "nor is it kept in the file"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_restored_proposal_is_what_it_was_and_goes_stale_if_the_cut_moved() {
    let (mut p, _, c1, c2) = cut();
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    let was = p.staged().unwrap().unwrap();
    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();

    p.set_volume(c2.id, 0.9).unwrap(); // the user kept cutting
    let back = p.restore_discarded().unwrap();
    assert_eq!(back.edits, was.edits);
    assert_eq!(back.diff.entries.len(), was.diff.entries.len());
    assert!(back.stale);
    assert_eq!(back.owner, None, "restored by the user it belongs to no agent session");
    assert!(matches!(p.apply_staged(false), Err(Error::StagedEditStale)));
    p.apply_staged(true).unwrap();
    assert_eq!(volume(&p, c1.id), 0.5);
}

#[test]
fn an_empty_proposal_is_not_worth_keeping_and_does_not_replace_one_that_is() {
    let (mut p, _, c1, _) = cut();
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();

    p.begin_staging(None, None, None).unwrap();
    p.discard_staged().unwrap();
    assert_eq!(p.discarded_staged().unwrap().unwrap().edits, vec!["Set volume"]);
}

#[test]
fn a_discarded_proposal_survives_closing_and_reopening_the_file() {
    let dir = scratch("discard-reopen");
    let path = dir.join("p.kerf");
    let mut p = Project::create(&path).unwrap();
    let a = asset("/av.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]);
    p.insert_asset(&a).unwrap();
    let c1 = p.add_clip_to_timeline(a.id, None, 0.0, 10.0, Some(0.0)).unwrap();
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();
    drop(p);

    let p = Project::open(&path).unwrap();
    let back = p.restore_discarded().unwrap();
    assert_eq!(back.edits, vec!["Set volume"]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_project_staged_by_an_older_kerf_opens_with_its_proposal_intact() {
    let dir = scratch("migrate");
    let path = dir.join("old.kerf");
    let (timeline_json, clip) = {
        let (p, _, c1, _) = cut();
        (p.timeline_json().unwrap(), c1)
    };
    let mut proposal = serde_json::from_str::<Timeline>(&timeline_json).unwrap();
    proposal.tracks[0].clips[0].volume = 0.5;

    {
        let p = Project::create(&path).unwrap();
        p.save_timeline_str(&timeline_json).unwrap();
        // The `staged` table as it was before owners and held-back asset changes.
        p.conn
            .execute_batch(
                "DROP TABLE staged;
                 DROP TABLE staged_undo;
                 CREATE TABLE staged (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     base_seq INTEGER NOT NULL, base TEXT NOT NULL, timeline TEXT NOT NULL,
                     edits TEXT NOT NULL, task_id TEXT, note TEXT,
                     created_at TEXT NOT NULL, updated_at TEXT NOT NULL);",
            )
            .unwrap();
        p.conn
            .execute(
                "INSERT INTO staged (id, base_seq, base, timeline, edits, task_id, note, created_at, updated_at)
                 VALUES (1, 0, ?1, ?2, '[\"Set volume\"]', NULL, 'older', ?3, ?3)",
                params![
                    timeline_json,
                    serde_json::to_string(&proposal).unwrap(),
                    Utc::now().to_rfc3339()
                ],
            )
            .unwrap();
    }

    let mut p = Project::open(&path).unwrap();
    let staged = p.staged().unwrap().expect("the old proposal is still open");
    assert_eq!(staged.note.as_deref(), Some("older"));
    assert_eq!(staged.owner, None);
    assert!(staged.asset_changes.is_empty());
    assert_eq!(staged.undo_depth, 0);
    assert_eq!(staged.diff.entries.len(), 1);
    assert!(!staged.diff.entries[0].id.is_empty());

    // …and it is a working proposal: edit it as an agent, undo, apply.
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();
    p.set_volume(clip.id, 0.2).unwrap();
    assert_eq!(p.staged().unwrap().unwrap().owner.as_deref(), Some("alpha"), "adopted");
    p.undo_staged().unwrap();
    p.set_actor(EditSource::User);
    p.apply_staged(true).unwrap();
    assert_eq!(volume(&p, clip.id), 0.5);
    drop(p);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn removing_a_task_discards_its_proposal_but_the_discard_can_be_undone() {
    let (mut p, _, c1, _) = cut();
    let task = p.add_task("tighten").unwrap();
    p.set_actor(EditSource::Agent);
    p.claim_next_task(None).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();
    p.set_actor(EditSource::User);
    p.remove_task(task.id).unwrap();
    assert!(p.staged().unwrap().is_none());

    let back = p.restore_discarded().unwrap();
    assert_eq!(back.task_id, None, "its task is gone");
    assert_eq!(back.edits, vec!["Set volume"]);
}

#[test]
fn the_diff_names_each_change_stably_for_the_review_ui() {
    let (mut p, _, c1, _) = cut();
    p.set_actor(EditSource::Agent);
    p.set_volume(c1.id, 0.5).unwrap();
    let first = p.staged().unwrap().unwrap().diff.entries[0].id.clone();
    p.set_fade(c1.id, Some(1.0), None).unwrap();
    let diff = p.staged().unwrap().unwrap().diff;
    assert_eq!(diff.entries.len(), 1, "both land in one clip entry");
    assert_eq!(diff.entries[0].id, first, "the id does not change as the proposal grows");
    assert_eq!(first, format!("clip_changed:{}", c1.id));
    let json = serde_json::to_value(&diff.entries[0]).unwrap();
    assert_eq!(json["id"], first);
}

// ---- against real media ----------------------------------------------------------------------------

/// A `seconds`-long clip with picture and sound (or picture only) made by the ffmpeg binary.
fn make_media(path: &Path, seconds: u32, with_audio: bool) -> bool {
    let mut cmd = std::process::Command::new(crate::engine::ffmpeg_path());
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc=size=160x90:rate=10:duration={seconds}"));
    if with_audio {
        cmd.args(["-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={seconds}"));
    }
    cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
    if with_audio {
        cmd.args(["-c:a", "aac", "-shortest"]);
    }
    cmd.arg(path).status().map(|s| s.success()).unwrap_or(false)
}

/// Import a real file, relink it to a copy (same media: analysis kept), then to a different
/// recording of the same length (compatible, analysis dropped), and refuse a picture-only file.
#[test]
#[ignore = "drives the real ffmpeg / ffprobe binaries"]
fn relinking_real_media_keeps_analysis_only_for_the_same_recording() {
    let dir = scratch("relink-real");
    let (original, copy, retake, silent) = (
        dir.join("original.mp4"),
        dir.join("copy.mp4"),
        dir.join("retake.mp4"),
        dir.join("silent.mp4"),
    );
    assert!(make_media(&original, 2, true), "ffmpeg could not make a test clip");
    std::fs::copy(&original, &copy).unwrap();
    assert!(make_media(&retake, 2, true));
    assert!(make_media(&silent, 2, false));

    let p = Project::open_in_memory().unwrap();
    let a = p.import_asset(&original).unwrap();
    p.set_analysis(&silence_patch(a.id)).unwrap();

    let done = p.relink_asset(a.id, &copy).unwrap();
    assert!(done.analysis_kept, "a byte-for-byte copy is the same recording");
    assert_eq!(p.require_asset(a.id).unwrap().path, copy.to_string_lossy());
    assert!(p.get_analysis(a.id).unwrap().is_some());

    // Another encode of the same length is usable media, but not what was analysed: the file
    // differs in size, so it is not the old recording moved.
    std::fs::remove_file(&original).unwrap();
    let mut other = std::fs::read(&retake).unwrap();
    other.push(0);
    std::fs::write(&retake, &other).unwrap();
    let done = p.relink_asset(a.id, &retake).unwrap();
    assert!(!done.analysis_kept);
    assert!(p.get_analysis(a.id).unwrap().is_none());

    // Same size, same length, same streams — and other bytes: a retake the size check cannot see.
    let lookalike = dir.join("lookalike.mp4");
    let mut bytes = std::fs::read(&retake).unwrap();
    let at = bytes.len() / 3;
    bytes[at] ^= 0xff;
    std::fs::write(&lookalike, &bytes).unwrap();
    p.set_analysis(&silence_patch(a.id)).unwrap();
    let done = p.relink_asset(a.id, &lookalike).unwrap();
    assert!(
        !done.analysis_kept,
        "another take of the same size is not the old recording moved"
    );
    assert!(p.get_analysis(a.id).unwrap().is_none());
    // …where the same bytes under another name are.
    let twin = dir.join("twin.mp4");
    std::fs::copy(&lookalike, &twin).unwrap();
    p.set_analysis(&silence_patch(a.id)).unwrap();
    assert!(p.relink_asset(a.id, &twin).unwrap().analysis_kept);
    std::fs::remove_file(&lookalike).unwrap();
    std::fs::copy(&twin, &retake).unwrap();
    p.relink_asset(a.id, &retake).unwrap();

    let err = p.relink_asset(a.id, &silent).unwrap_err().to_string();
    assert!(err.contains("picture only"), "{err}");
    assert_eq!(p.require_asset(a.id).unwrap().path, retake.to_string_lossy());
    assert!(p.missing_assets().unwrap().is_empty());
    std::fs::remove_file(&retake).unwrap();
    assert_eq!(p.missing_assets().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// Asking what a relink would cost reads both files and changes nothing.
#[test]
#[ignore = "drives the real ffmpeg / ffprobe binaries"]
fn checking_a_relink_against_real_media_reports_the_cost_and_changes_nothing() {
    let dir = scratch("relink-check-real");
    let (original, copy, retake, silent) = (
        dir.join("original.mp4"),
        dir.join("copy.mp4"),
        dir.join("retake.mp4"),
        dir.join("silent.mp4"),
    );
    assert!(make_media(&original, 2, true), "ffmpeg could not make a test clip");
    std::fs::copy(&original, &copy).unwrap();
    assert!(make_media(&retake, 2, true));
    let mut other = std::fs::read(&retake).unwrap();
    other.push(0);
    std::fs::write(&retake, &other).unwrap();
    assert!(make_media(&silent, 2, false));
    let p = Project::open_in_memory().unwrap();
    let a = p.import_asset(&original).unwrap();
    p.set_analysis(&silence_patch(a.id)).unwrap();

    let old = p.relink_inputs(a.id).unwrap();
    let has = p.has_analysis(a.id).unwrap();
    let never = || false;
    let same = Project::check_relink(&old, has, &copy, &never).unwrap();
    assert_eq!((same.analysis_kept, same.has_analysis), (true, true));
    let again = Project::check_relink(&old, has, &retake, &never).unwrap();
    assert_eq!((again.analysis_kept, again.has_analysis), (false, true));
    let err = Project::check_relink(&old, has, &silent, &never).unwrap_err().to_string();
    assert!(err.contains("picture only"), "{err}");
    assert_eq!(p.require_asset(a.id).unwrap().path, original.to_string_lossy());
    assert!(p.has_analysis(a.id).unwrap(), "asking dropped nothing");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_agent_may_not_flip_the_ripple_setting_while_review_is_on() {
    let (mut p, _) = av_project();
    p.set_actor(EditSource::Agent);
    let err = p.set_ripple_mode(true).unwrap_err().to_string();
    assert!(err.contains("`ripple`"), "{err}");
    p.set_actor(EditSource::User);
    assert!(!p.ripple_mode().unwrap(), "nothing changed");
    p.set_ripple_mode(true).unwrap();
    p.set_agent_review(false);
    p.set_actor(EditSource::Agent);
    p.set_ripple_mode(false).unwrap();
    assert!(!p.ripple_mode().unwrap(), "with review off it is the agent's to flip");
}

#[test]
fn a_proposal_whose_session_is_gone_is_released_and_taken_over_by_the_next_session() {
    let (mut p, _, c1, _) = cut();
    assert!(
        !p.release_orphaned_staging(|_| true).unwrap(),
        "nothing open, nothing to release"
    );
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("alpha")).unwrap();
    p.set_volume(c1.id, 0.5).unwrap();

    // Alpha is still there: nothing changes, and a stranger is still turned away.
    assert!(!p.release_orphaned_staging(|owner| owner == "someone else").unwrap());
    p.set_actor(EditSource::Agent);
    assert!(matches!(
        p.ensure_agent_staging(Some("beta")),
        Err(Error::StagedEditOwnedByOther)
    ));

    // Alpha's connection closed (a reconnect gets a new session id).
    assert!(p.release_orphaned_staging(|owner| owner == "alpha").unwrap());
    assert!(!p.staged().unwrap().unwrap().owned_by_agent);
    p.set_actor(EditSource::Agent);
    p.ensure_agent_staging(Some("beta")).unwrap();
    p.set_volume(c1.id, 0.2).unwrap();
    let taken = p.staged().unwrap().unwrap();
    assert_eq!(taken.owner.as_deref(), Some("beta"), "the next named session adopted it");
    assert_eq!(taken.edits.len(), 2, "its steps are all still there");
}
