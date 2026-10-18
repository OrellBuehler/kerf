//! The `.kerf` file at the `Project` level: atomic Save As, schema versions, the invariant check
//! before a commit, and the bounded history.

use super::linked_tests::{asset, stream};
use super::*;

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-persist-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn version_of(path: &Path) -> u32 {
    store::user_version(&Connection::open(path).unwrap()).unwrap()
}

/// A project with one video asset and `edits` clips appended, one revision each.
fn edited(edits: usize) -> (Project, Uuid) {
    let project = Project::open_in_memory().unwrap();
    let a = asset("/v.mp4", 600.0, vec![stream(StreamKind::Video)]);
    project.insert_asset(&a).unwrap();
    for i in 0..edits {
        project.cut_clip(a.id, i as f64, i as f64 + 1.0).unwrap();
    }
    (project, a.id)
}

fn seqs(project: &Project) -> Vec<i64> {
    project.history().unwrap().iter().map(|r| r.seq).collect()
}

#[test]
fn save_as_replaces_an_existing_file_whole_and_leaves_nothing_beside_it() {
    let dir = scratch();
    let target = dir.join("cut.kerf");
    edited(1).0.save_as(&target).unwrap();
    edited(3).0.save_as(&target).unwrap();
    assert_eq!(Project::open(&target).unwrap().history().unwrap().len(), 4);
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("cut.kerf")]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_failed_save_as_leaves_the_file_it_would_have_replaced() {
    let dir = scratch();
    let target = dir.join("cut.kerf");
    edited(2).0.save_as(&target).unwrap();
    let before = std::fs::read(&target).unwrap();
    assert!(edited(5).0.save_as(dir.join("missing/cut.kerf")).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), before);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn saving_a_project_onto_its_own_file_is_a_no_op() {
    let dir = scratch();
    let target = dir.join("cut.kerf");
    edited(2).0.save_as(&target).unwrap();
    let project = Project::open(&target).unwrap();
    project.save_as(&target).unwrap();
    project.save_as(dir.join(".").join("cut.kerf")).unwrap();
    assert_eq!(project.history().unwrap().len(), 3);
    assert_eq!(Project::open(&target).unwrap().history().unwrap().len(), 3);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_new_file_is_stamped_with_this_schema_and_gets_no_backup() {
    let dir = scratch();
    let path = dir.join("new.kerf");
    drop(Project::create(&path).unwrap());
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    drop(Project::open(&path).unwrap());
    assert!(!schema_backup_path(&path, 0).exists());
    // Save As carries the version over.
    let copy = dir.join("copy.kerf");
    Project::open(&path).unwrap().save_as(&copy).unwrap();
    assert_eq!(version_of(&copy), SCHEMA_VERSION);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_legacy_file_is_backed_up_once_then_upgraded() {
    let dir = scratch();
    let path = dir.join("old cut.kerf");
    edited(2).0.save_as(&path).unwrap();
    // What a build from before schema versions wrote.
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 0)
        .unwrap();

    let project = Project::open(&path).unwrap();
    assert_eq!(project.history().unwrap().len(), 3);
    drop(project);
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    let backup = dir.join("old cut (schema v0 backup).kerf");
    assert_eq!(version_of(&backup), 0);
    assert_eq!(Project::open(&path).unwrap().history().unwrap().len(), 3);

    // A second downgrade-and-open keeps the first backup rather than overwriting it.
    let first = std::fs::read(&backup).unwrap();
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 0)
        .unwrap();
    drop(Project::open(&path).unwrap());
    assert_eq!(std::fs::read(&backup).unwrap(), first);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_file_from_a_newer_kerf_is_refused_untouched() {
    let dir = scratch();
    let path = dir.join("future.kerf");
    edited(1).0.save_as(&path).unwrap();
    let newer = SCHEMA_VERSION as i64 + 1;
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", newer)
        .unwrap();
    let before = std::fs::read(&path).unwrap();

    let err = Project::open(&path).err().expect("a newer file is refused");
    assert!(matches!(err, Error::NewerProject { .. }), "{err:?}");
    assert!(err.to_string().contains("made by a newer Kerf"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no backup, no journal");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_edit_that_breaks_an_invariant_persists_nothing() {
    let (project, _) = edited(2);
    let before = project.timeline_json().unwrap();
    let history = project.history().unwrap().len();
    let err = project
        .edit_timeline("Break it", |t| {
            t.tracks[0].clips[0].source_in = f64::NAN;
            Ok(())
        })
        .unwrap_err();
    assert!(err.to_string().contains("would leave the timeline invalid"), "{err}");
    assert_eq!(project.timeline_json().unwrap(), before);
    assert_eq!(project.history().unwrap().len(), history);
}

#[test]
fn an_agents_bad_edit_leaves_its_proposal_alone() {
    let (mut project, _) = edited(1);
    project.set_actor(EditSource::Agent);
    project.begin_staging(None, None).unwrap();
    let staged = project.staged_row().unwrap().unwrap().timeline;
    assert!(project
        .edit_timeline("Break it", |t| {
            t.tracks[0].clips[0].fade_in = -1.0;
            Ok(())
        })
        .is_err());
    assert_eq!(project.staged_row().unwrap().unwrap().timeline, staged);
}

#[test]
fn the_history_keeps_the_newest_revisions_and_the_oldest_left_is_the_baseline() {
    let (mut project, asset) = edited(0);
    project.set_history_limit(3);
    for i in 0..5 {
        project.cut_clip(asset, i as f64, i as f64 + 1.0).unwrap();
    }
    assert_eq!(seqs(&project), vec![3, 4, 5]);
    assert!(project.revision_diff(3).unwrap().is_empty(), "nothing before it to compare");
    assert!(!project.revision_diff(4).unwrap().is_empty());
    project.undo().unwrap();
    project.undo().unwrap();
    assert!(!project.can_undo().unwrap());
    assert!(project.undo().is_err());
    // Back at 3, a new edit drops the redo branch and then prunes from the new tip.
    project.cut_clip(asset, 9.0, 10.0).unwrap();
    assert_eq!(seqs(&project), vec![3, 4]);
    project.revert_to(3).unwrap();
}

#[test]
fn a_staged_proposals_base_is_never_pruned() {
    let (mut project, asset) = edited(2);
    project.set_history_limit(2);
    project.set_actor(EditSource::Agent);
    let staged = project.begin_staging(None, None).unwrap();
    project.cut_clip(asset, 20.0, 21.0).unwrap();
    project.set_actor(EditSource::User);
    for i in 0..4 {
        project.cut_clip(asset, 30.0 + i as f64, 31.0 + i as f64).unwrap();
    }
    let kept = seqs(&project);
    assert_eq!(kept.first(), Some(&staged.base_seq), "{kept:?}");
    project.set_actor(EditSource::Agent);
    project.apply_staged(true).unwrap();
    project.set_actor(EditSource::User);
    project.cut_clip(asset, 50.0, 51.0).unwrap();
    assert_eq!(seqs(&project).len(), 2, "pruned again once the proposal is gone");
}

#[test]
fn the_history_limit_never_drops_the_head() {
    let (mut project, asset) = edited(0);
    project.set_history_limit(0);
    project.cut_clip(asset, 0.0, 1.0).unwrap();
    assert_eq!(seqs(&project), vec![1]);
    assert!(project.history().unwrap()[0].current);
}
