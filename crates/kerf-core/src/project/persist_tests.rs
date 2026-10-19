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

/// Make the file at `path` look like one a build from before schema versions wrote; `columns`
/// false also takes away the columns v1 adds, so the upgrade has something to do.
fn make_legacy(path: &Path, columns: bool) {
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "user_version", 0).unwrap();
    if !columns {
        conn.execute_batch("ALTER TABLE assets DROP COLUMN voiceover").unwrap();
    }
}

#[test]
fn a_legacy_file_is_backed_up_once_then_upgraded() {
    let dir = scratch();
    let path = dir.join("old cut.kerf");
    edited(2).0.save_as(&path).unwrap();
    make_legacy(&path, false);

    let project = Project::open(&path).unwrap();
    assert_eq!(project.history().unwrap().len(), 3);
    drop(project);
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    let backup = dir.join("old cut (schema v0 backup).kerf");
    assert_eq!(version_of(&backup), 0);
    assert_eq!(Project::open(&path).unwrap().history().unwrap().len(), 3);

    // A second downgrade-and-open keeps the first backup rather than overwriting it.
    let first = std::fs::read(&backup).unwrap();
    make_legacy(&path, false);
    drop(Project::open(&path).unwrap());
    assert_eq!(std::fs::read(&backup).unwrap(), first);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_legacy_file_that_already_has_the_columns_is_stamped_without_a_backup() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    edited(1).0.save_as(&path).unwrap();
    make_legacy(&path, true);
    drop(Project::open(&path).unwrap());
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("cut.kerf")]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_v1_file_gains_the_proposals_owner_columns_and_keeps_a_backup() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    edited(1).0.save_as(&path).unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch("ALTER TABLE staged DROP COLUMN owner; ALTER TABLE staged DROP COLUMN assets;")
            .unwrap();
    }
    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert_eq!(version_of(&schema_backup_path(&path, 1)), 1, "the file as it was is kept");
    let staged = project.begin_staging(None, None, Some("session-1")).unwrap();
    assert_eq!(staged.owner.as_deref(), Some("session-1"));
    drop(project);
    // Already there: opening again changes nothing and makes no second backup.
    let backups = std::fs::read_dir(&dir).unwrap().count();
    drop(Project::open(&path).unwrap());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), backups);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_v3_file_gains_the_music_score_column_and_keeps_a_backup() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    edited(1).0.save_as(&path).unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();
        conn.execute_batch("ALTER TABLE assets DROP COLUMN music_score").unwrap();
    }
    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert_eq!(version_of(&schema_backup_path(&path, 3)), 3, "the file as it was is kept");
    // The asset that was there reads back with no score, and a composed one can now be stored.
    assert!(project.list_assets().unwrap()[0].music_score.is_none());
    let mut composed = asset("/m/a.wav", 8.0, vec![stream(StreamKind::Audio)]);
    composed.music_score = Some(crate::model::sample_song(crate::model::MusicStyle::Lofi));
    project.insert_asset(&composed).unwrap();
    let back = project
        .list_assets()
        .unwrap()
        .into_iter()
        .find(|a| a.id == composed.id)
        .unwrap();
    assert_eq!(back.music_score, composed.music_score);
    drop(project);
    // Already there: opening again changes nothing and makes no second backup.
    let files = std::fs::read_dir(&dir).unwrap().count();
    drop(Project::open(&path).unwrap());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), files);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_backup_that_cannot_be_written_does_not_stop_an_additive_upgrade() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    edited(1).0.save_as(&path).unwrap();
    make_legacy(&path, false);
    // Something that is not a file sits where the backup goes, so writing it fails.
    std::fs::create_dir_all(schema_backup_path(&path, 0).join("x")).unwrap();
    let project = Project::open(&path).unwrap();
    assert_eq!(project.history().unwrap().len(), 2);
    drop(project);
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_read_only_legacy_file_opens_untouched() {
    let dir = scratch();
    let path = dir.join("locked.kerf");
    edited(2).0.save_as(&path).unwrap();
    make_legacy(&path, true);
    let before = std::fs::read(&path).unwrap();
    // A write-protected file opens read-only; the URI form says so even to root.
    let uri = format!("file:{}?mode=ro", path.display());
    let project = Project::open(&uri).unwrap();
    assert_eq!(project.history().unwrap().len(), 3);
    assert_eq!(project.timeline().unwrap().tracks[0].clips.len(), 2);
    drop(project);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no backup");

    // One that needs a real upgrade says why it cannot be opened.
    make_legacy(&path, false);
    let err = Project::open(&uri).err().expect("needs an upgrade");
    assert!(err.to_string().contains("read-only"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

/// Make the file at `path` look like one schema v2 wrote: stamped 2, and without the table of
/// embedded fonts v3 adds.
fn make_v2(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("DROP TABLE fonts").unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
}

fn has_fonts_table(path: &Path) -> bool {
    store::has_table(&Connection::open(path).unwrap(), "fonts")
}

#[test]
fn a_v2_file_is_backed_up_once_then_gets_the_fonts_table() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    edited(2).0.save_as(&path).unwrap();
    make_v2(&path);
    assert_eq!(SCHEMA_VERSION, 5);
    assert!(!has_fonts_table(&path));

    let project = Project::open(&path).unwrap();
    assert_eq!(project.history().unwrap().len(), 3);
    assert!(project.embedded_fonts().unwrap().is_empty());
    drop(project);
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert!(has_fonts_table(&path));
    let backup = dir.join("cut (schema v2 backup).kerf");
    assert_eq!(version_of(&backup), 2);
    assert!(!has_fonts_table(&backup), "the backup is the file as it was");

    // The upgraded file takes fonts, and a second downgrade keeps the first backup.
    let project = Project::open(&path).unwrap();
    project
        .store_fonts(&[crate::fonts::EmbeddedFont::new(
            "Zeist",
            crate::fonts::shipped_font_renamed(false, "Zeist"),
        )])
        .unwrap();
    drop(project);
    let first = std::fs::read(&backup).unwrap();
    make_v2(&path);
    drop(Project::open(&path).unwrap());
    assert_eq!(std::fs::read(&backup).unwrap(), first);
    crate::fonts::replace_embedded(Vec::new());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_v2_file_that_already_has_the_fonts_table_is_stamped_without_a_backup() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    edited(1).0.save_as(&path).unwrap();
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 2)
        .unwrap();
    drop(Project::open(&path).unwrap());
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no backup");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_file_from_before_every_step_gets_each() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    edited(1).0.save_as(&path).unwrap();
    make_legacy(&path, false);
    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE fonts; ALTER TABLE staged DROP COLUMN owner; ALTER TABLE staged DROP COLUMN assets;")
        .unwrap();
    drop(Project::open(&path).unwrap());
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert!(has_fonts_table(&path));
    let conn = Connection::open(&path).unwrap();
    assert!(conn.prepare("SELECT voiceover FROM assets").is_ok());
    assert!(conn.prepare("SELECT owner, assets FROM staged").is_ok());
    assert_eq!(version_of(&dir.join("cut (schema v0 backup).kerf")), 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_read_only_v1_file_without_the_proposals_columns_is_refused() {
    let dir = scratch();
    let path = dir.join("locked.kerf");
    edited(1).0.save_as(&path).unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch("ALTER TABLE staged DROP COLUMN owner; ALTER TABLE staged DROP COLUMN assets;")
            .unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    let uri = format!("file:{}?mode=ro", path.display());
    let err = Project::open(&uri).err().expect("reading a proposal needs the columns");
    assert!(err.to_string().contains("read-only"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), before, "nothing written");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_read_only_v2_file_without_the_fonts_table_opens_as_it_is() {
    let dir = scratch();
    let path = dir.join("locked.kerf");
    edited(2).0.save_as(&path).unwrap();
    make_v2(&path);
    let before = std::fs::read(&path).unwrap();
    let uri = format!("file:{}?mode=ro", path.display());
    let project = Project::open(&uri).unwrap();
    assert_eq!(project.history().unwrap().len(), 3);
    assert!(project.embedded_fonts().unwrap().is_empty(), "no table, no fonts, no error");
    assert_eq!(project.embedded_font_bytes().unwrap(), 0);
    drop(project);
    assert_eq!(std::fs::read(&path).unwrap(), before, "nothing written");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no backup");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_fonts_a_project_embeds_survive_save_as() {
    let dir = scratch();
    let (project, _) = edited(1);
    let font = crate::fonts::EmbeddedFont::new("Zeist", crate::fonts::shipped_font_renamed(true, "Zeist"));
    // A title that names the family is what keeps the font through a save.
    project
        .add_svg_overlay(
            crate::SvgSource::Custom(
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><text font-family="Zeist">a</text></svg>"#.into(),
            ),
            0.0,
            Some(2.0),
            crate::SvgLayout::default(),
        )
        .unwrap();
    project.store_fonts(std::slice::from_ref(&font)).unwrap();
    let path = dir.join("a.kerf");
    project.save_as(&path).unwrap();
    assert_eq!(Project::open(&path).unwrap().embedded_fonts().unwrap(), vec![font]);
    crate::fonts::replace_embedded(Vec::new());
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
fn a_timeline_an_older_build_left_broken_can_still_be_edited() {
    let (project, asset) = edited(1);
    // What `add_overlay(-2, 0)` used to store: an overlay of no length.
    let mut t = project.timeline().unwrap();
    t.overlays.push(TextOverlay::new("old", 0.0, 0.0));
    project.save_timeline(&t).unwrap();

    project.cut_clip(asset, 5.0, 6.0).unwrap();
    project.add_marker(1.0, "m".into(), None).unwrap();
    // But a new break is still refused, and named.
    let err = project
        .edit_timeline("Break it", |t| {
            t.tracks[0].clips[0].fade_in = -1.0;
            Ok(())
        })
        .unwrap_err();
    assert!(err.to_string().contains("the clip at 0:00.0 on"), "{err}");
}

#[test]
fn an_overlay_clamped_to_no_length_is_refused() {
    let (project, _) = edited(0);
    let err = project.add_overlay("t".into(), -2.0, 0.0).unwrap_err();
    assert!(err.to_string().contains("end must be after start"), "{err}");
    assert!(project.timeline().unwrap().overlays.is_empty());
    project.add_overlay("t".into(), -2.0, 1.0).unwrap();
}

#[test]
fn an_agents_bad_edit_leaves_its_proposal_alone() {
    let (mut project, _) = edited(1);
    project.set_actor(EditSource::Agent);
    project.begin_staging(None, None, None).unwrap();
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
    let staged = project.begin_staging(None, None, None).unwrap();
    project.cut_clip(asset, 20.0, 21.0).unwrap();
    project.set_actor(EditSource::User);
    for i in 0..4 {
        project.cut_clip(asset, 30.0 + i as f64, 31.0 + i as f64).unwrap();
    }
    let kept = seqs(&project);
    assert_eq!(kept.first(), Some(&staged.base_seq), "{kept:?}");
    // The user applies it: with review on, an agent may not approve its own proposal.
    project.apply_staged(true).unwrap();
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

#[test]
fn a_v4_file_is_stamped_v5_without_a_backup_and_keeps_its_title_clips() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    let project = Project::open_in_memory().unwrap();
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 9"><rect width="8" height="4"/></svg>"#;
    let title = project
        .add_title_clip(svg, 1.0, Some(3.0), crate::project::TitleClipPlacement::default())
        .unwrap()
        .clip;
    project.save_as(&path).unwrap();
    drop(project);
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 4)
        .unwrap();

    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    let clip = project.timeline().unwrap().clip(title.id).cloned().unwrap();
    assert_eq!(clip.title_clip().unwrap().svg, title.title_clip().unwrap().svg);
    drop(project);
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("cut.kerf")], "nothing in the file changed, so no backup");
    std::fs::remove_dir_all(&dir).ok();
}
