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

/// The columns are there, so their steps change nothing; the file is still copied aside, once,
/// for v5 to v14, v16 and v17 (JSON steps: `needed` always, so an older build's file is kept before it is locked out).
#[test]
fn a_legacy_file_that_already_has_the_columns_is_backed_up_only_for_the_json_step() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    edited(1).0.save_as(&path).unwrap();
    make_legacy(&path, true);
    // Every step that adds a table or a column finds it there; only v5 to v14, v16 and v17 (JSON steps) are pending.
    let pending: Vec<u32> = store::pending_steps(&Connection::open(&path).unwrap(), 0);
    assert_eq!(
        pending,
        vec![4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16],
        "only the v4 -> v5 to v13 -> v14 and the v15 -> v16 and v16 -> v17 steps"
    );
    drop(Project::open(&path).unwrap());
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            std::ffi::OsString::from("cut (schema v0 backup).kerf"),
            std::ffi::OsString::from("cut.kerf")
        ]
    );
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
    assert_eq!(SCHEMA_VERSION, 17);
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

/// The fonts table is there, so v3 changes nothing; the one backup is for v5 to v14, v16 and v17 (JSON steps).
#[test]
fn a_v2_file_that_already_has_the_fonts_table_is_backed_up_only_for_the_json_step() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    edited(1).0.save_as(&path).unwrap();
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 2)
        .unwrap();
    assert_eq!(
        store::pending_steps(&Connection::open(&path).unwrap(), 2),
        vec![4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 15, 16]
    );
    drop(Project::open(&path).unwrap());
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "the file and its backup");
    assert_eq!(version_of(&schema_backup_path(&path, 2)), 2);
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
        .execute_batch(
            "DROP TABLE fonts; ALTER TABLE staged DROP COLUMN owner; ALTER TABLE staged DROP COLUMN assets; \
             ALTER TABLE assets DROP COLUMN tags;",
        )
        .unwrap();
    drop(Project::open(&path).unwrap());
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert!(has_fonts_table(&path));
    let conn = Connection::open(&path).unwrap();
    assert!(conn.prepare("SELECT voiceover, tags FROM assets").is_ok());
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

/// A template title as applied: its text stored as glyph outlines.
fn outlined_title(project: &Project) -> SvgOverlay {
    let applied = project
        .apply_template(
            &crate::template::fixtures::template(),
            &Default::default(),
            0.0,
            None,
            SvgLayout::default(),
        )
        .unwrap();
    assert!(applied.overlay.svg.contains("data-kerf-text"), "{}", applied.overlay.svg);
    applied.overlay
}

/// A template title as applied with an animation on its tagline: its text as outlines with per-glyph SMIL.
fn animated_title(project: &Project) -> SvgOverlay {
    let id = outlined_title(project).id;
    let rise = crate::svg::text_animation_preset("rise").unwrap().animation;
    match project.set_title_text_animation(id, "tag", Some(rise)).unwrap() {
        crate::project::TitleEdited::Overlay(done) => {
            assert!(done.overlay.svg.contains("animateTransform"), "{}", done.overlay.svg);
            done.overlay
        }
        crate::project::TitleEdited::Clip(_) => unreachable!("a floating title"),
    }
}

/// The same with a spring on its movement (v11): `curves` in the animation on its text.
fn eased_title(project: &Project) -> SvgOverlay {
    let id = outlined_title(project).id;
    let rise = crate::svg::text_animation_preset("rise").unwrap().animation;
    let spring = crate::svg::curve_preset("spring").unwrap().curve;
    let eased = rise.with_motion_curve(spring).unwrap();
    match project.set_title_text_animation(id, "tag", Some(eased)).unwrap() {
        crate::project::TitleEdited::Overlay(done) => {
            assert!(done.overlay.svg.contains("\"curves\""), "{}", done.overlay.svg);
            assert!(done.overlay.svg.contains("animateTransform"), "{}", done.overlay.svg);
            done.overlay
        }
        crate::project::TitleEdited::Clip(_) => unreachable!("a floating title"),
    }
}

#[test]
fn a_v8_file_is_backed_up_stamped_v9_and_keeps_its_animated_title() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    let (project, _) = edited(0);
    let title = animated_title(&project);
    project.save_as(&path).unwrap();
    drop(project);
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 8)
        .unwrap();
    assert_eq!(
        store::pending_steps(&Connection::open(&path).unwrap(), 8),
        vec![8, 9, 10, 11, 12, 13, 15, 16]
    );

    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    let kept = project.timeline().unwrap().svg_overlays;
    assert_eq!(
        kept.iter().find(|o| o.id == title.id).map(|o| o.svg.as_str()),
        Some(title.svg.as_str())
    );
    drop(project);
    assert_eq!(version_of(&schema_backup_path(&path, 8)), 8);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_v7_file_is_backed_up_stamped_v8_and_keeps_its_outlined_title() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    let (project, _) = edited(0);
    let title = outlined_title(&project);
    project.save_as(&path).unwrap();
    drop(project);
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 7)
        .unwrap();
    assert_eq!(
        store::pending_steps(&Connection::open(&path).unwrap(), 7),
        vec![7, 8, 9, 10, 11, 12, 13, 15, 16]
    );

    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    let kept = project.timeline().unwrap().svg_overlays;
    assert_eq!(
        kept.iter().find(|o| o.id == title.id).map(|o| o.svg.as_str()),
        Some(title.svg.as_str())
    );
    drop(project);
    assert_eq!(version_of(&schema_backup_path(&path, 7)), 7);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_v6_file_is_backed_up_stamped_v7_and_keeps_its_keyed_mask_and_crop() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    let (project, asset) = edited(0);
    let clip = project.cut_clip(asset, 0.0, 4.0).unwrap();
    project.set_mask(clip.id, Some(Mask::default())).unwrap();
    let keys = vec![PropertyKey::new(0.0, 0.2), PropertyKey::new(2.0, 0.8)];
    project
        .set_property_keyframes(clip.id, Property::MaskX, keys.clone())
        .unwrap();
    let zoom = vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(2.0, 2.0)];
    project
        .set_property_keyframes(clip.id, Property::CropZoom, zoom.clone())
        .unwrap();
    project.save_as(&path).unwrap();
    drop(project);
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 6)
        .unwrap();
    assert_eq!(
        store::pending_steps(&Connection::open(&path).unwrap(), 6),
        vec![6, 7, 8, 9, 10, 11, 12, 13, 15, 16]
    );

    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    let clip = project.timeline().unwrap().clip(clip.id).cloned().unwrap();
    assert_eq!(clip.property_keys(Property::MaskX), keys);
    assert_eq!(clip.property_keys(Property::CropZoom), zoom);
    drop(project);
    assert_eq!(version_of(&schema_backup_path(&path, 6)), 6);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_v4_file_is_backed_up_stamped_v5_and_keeps_its_title_clips() {
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
    assert_eq!(
        version_of(&schema_backup_path(&path, 4)),
        4,
        "an older build is locked out of the upgraded file, so the file as it was is kept"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A video asset stored with the range its file declares keeps it, and one stored before v10 (no `color_range` in
/// its stream JSON) reads as not known, which is not limited range: the plan treats it as it always did.
#[test]
fn a_v9_file_is_backed_up_stamped_v10_and_its_assets_read_as_range_unknown() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    let project = Project::open_in_memory().unwrap();
    let mut tagged = stream(StreamKind::Video);
    tagged.color_range = Some("pc".into());
    let full = asset("/full.mkv", 10.0, vec![tagged.clone()]);
    let old = asset("/old.mp4", 10.0, vec![stream(StreamKind::Video)]);
    project.insert_asset(&full).unwrap();
    project.insert_asset(&old).unwrap();
    project.save_as(&path).unwrap();
    drop(project);

    let json = serde_json::to_value(&old.streams[0]).unwrap();
    assert!(
        json.get("color_range").is_none(),
        "an unset range is omitted, so a stored stream is byte-identical to a v9 one"
    );
    assert_eq!(serde_json::to_value(&tagged).unwrap()["color_range"], "pc");
    let read: StreamInfo = serde_json::from_value(json).unwrap();
    assert_eq!(read.color_range, None, "a stream written before v10 reads as not known");

    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 9)
        .unwrap();
    assert_eq!(
        store::pending_steps(&Connection::open(&path).unwrap(), 9),
        vec![9, 10, 11, 12, 13, 15, 16]
    );

    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    let range = |id| project.require_asset(id).unwrap().streams[0].color_range.clone();
    assert_eq!(range(full.id).as_deref(), Some("pc"));
    assert_eq!(range(old.id), None);
    drop(project);
    assert_eq!(
        version_of(&schema_backup_path(&path, 9)),
        9,
        "an older build drops the tag at its next write, so the file as it was is kept"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A title with curves in its animation is kept as it is, and a file stored before v11 has none: an animation with no
/// curves is written as it always was, so a stored title is byte-identical. The file is copied aside, since an older
/// build refuses an attribute with a field it does not know and so leaves the text as text.
#[test]
fn a_v10_file_is_backed_up_stamped_v11_and_keeps_its_eased_title() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    let (project, _) = edited(0);
    let plain = animated_title(&project);
    let plain_attribute = crate::svg::text_animation_of(&crate::svg::restore_text(&plain.svg).unwrap(), "tag").unwrap();
    assert!(
        !plain_attribute.to_attribute().contains("curves"),
        "an animation with no curves writes none: {}",
        plain_attribute.to_attribute()
    );
    let eased = Project::sample().unwrap();
    let title = eased_title(&eased);
    drop(project);
    eased.save_as(&path).unwrap();
    drop(eased);
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 10)
        .unwrap();
    assert_eq!(
        store::pending_steps(&Connection::open(&path).unwrap(), 10),
        vec![10, 11, 12, 13, 15, 16]
    );

    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    let kept = project.timeline().unwrap().svg_overlays;
    assert_eq!(
        kept.iter().find(|o| o.id == title.id).map(|o| o.svg.as_str()),
        Some(title.svg.as_str())
    );
    drop(project);
    assert_eq!(
        version_of(&schema_backup_path(&path, 10)),
        10,
        "an older build cannot play the curves, so the file as it was is kept"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A delivery frame with a frame rate is kept as it is, and one stored before v12 has none: a delivery with no rate is
/// written as it always was, so a stored one is byte-identical. The file is copied aside, since an older build drops the
/// rate at its next write and the export goes back to the rate of whichever clip is first.
#[test]
fn a_v11_file_is_backed_up_stamped_v12_and_keeps_its_delivery_frame_rate() {
    use crate::model::{Delivery, Fit};
    let dir = scratch();
    let path = dir.join("cut.kerf");
    let plain = Delivery::new(1080, 1920, Fit::Cover);
    let json = serde_json::to_value(plain).unwrap();
    assert!(json.get("fps").is_none(), "no rate is written: {json}");
    let read: Delivery = serde_json::from_value(json).unwrap();
    assert_eq!(read.fps, None, "a delivery stored before v12 reads as following the footage");

    let (project, _) = edited(1);
    project
        .set_delivery_format(Some(plain.with_fps(Some(50.0)).unwrap()))
        .unwrap();
    project.save_as(&path).unwrap();
    drop(project);
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 11)
        .unwrap();
    assert_eq!(
        store::pending_steps(&Connection::open(&path).unwrap(), 11),
        vec![11, 12, 13, 15, 16]
    );

    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert_eq!(project.timeline().unwrap().format.and_then(|f| f.fps), Some(50.0));
    drop(project);
    assert_eq!(
        version_of(&schema_backup_path(&path, 11)),
        11,
        "an older build drops the rate, so the file as it was is kept"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A thumbnail is kept as it is, and a timeline stored before v13 has none, written as it always was. The file is
/// copied aside, since an older build drops the thumbnails at its next write.
#[test]
fn a_v12_file_is_backed_up_stamped_v13_and_keeps_its_thumbnails() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    let (project, _) = edited(1);
    let json = timeline_to_json(&project.timeline().unwrap()).unwrap();
    assert!(!json.contains("thumbnails"), "none is written: {json}");
    let made = project
        .put_thumbnail(None, &crate::project::ThumbnailSpec::default(), None)
        .unwrap()
        .thumbnail;
    project.save_as(&path).unwrap();
    drop(project);
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 12)
        .unwrap();
    assert_eq!(
        store::pending_steps(&Connection::open(&path).unwrap(), 12),
        vec![12, 13, 15, 16]
    );

    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert_eq!(project.timeline().unwrap().thumbnails, vec![made]);
    drop(project);
    assert_eq!(
        version_of(&schema_backup_path(&path, 12)),
        12,
        "an older build drops the thumbnails, so the file as it was is kept"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A speed ramp is kept as it is, and a clip stored before v14 has none, written as it always was. The file is
/// copied aside, since an older build drops the ramp at its next write and the clip then plays at one speed.
#[test]
fn a_v13_file_is_backed_up_stamped_v14_and_keeps_its_speed_ramps() {
    use crate::model::SpeedKey;
    let dir = scratch();
    let path = dir.join("cut.kerf");
    let (project, _) = edited(1);
    let json = timeline_to_json(&project.timeline().unwrap()).unwrap();
    assert!(!json.contains("speed_ramp"), "none is written: {json}");
    let clip = project.timeline().unwrap().tracks[0].clips[0].id;
    let keys = vec![SpeedKey::new(0.0, 2.0), SpeedKey::new(0.5, 0.25)];
    project.set_speed_ramp(clip, keys.clone()).unwrap();
    project.save_as(&path).unwrap();
    drop(project);
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 13)
        .unwrap();
    assert_eq!(store::pending_steps(&Connection::open(&path).unwrap(), 13), vec![13, 15, 16]);

    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert_eq!(project.timeline().unwrap().tracks[0].clips[0].speed_ramp, keys);
    drop(project);
    assert_eq!(
        version_of(&schema_backup_path(&path, 13)),
        13,
        "an older build drops the ramp, so the file as it was is kept"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A blend mode is kept as it is, and a clip stored before v17 has none (`normal`), written as it always was. The
/// file is copied aside, since an older build drops the mode at its next write and the clip then covers what it lit.
#[test]
fn a_v16_file_is_backed_up_stamped_v17_and_keeps_its_blend_modes() {
    use crate::model::BlendMode;
    let dir = scratch();
    let path = dir.join("cut.kerf");
    let (project, _) = edited(1);
    let json = timeline_to_json(&project.timeline().unwrap()).unwrap();
    assert!(!json.contains("blend_mode"), "none is written: {json}");
    let clip = project.timeline().unwrap().tracks[0].clips[0].id;
    project.set_blend_mode(clip, BlendMode::Screen).unwrap();
    project.save_as(&path).unwrap();
    drop(project);
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 16)
        .unwrap();
    assert_eq!(store::pending_steps(&Connection::open(&path).unwrap(), 16), vec![16]);

    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert_eq!(project.timeline().unwrap().tracks[0].clips[0].blend_mode, BlendMode::Screen);
    drop(project);
    assert_eq!(
        version_of(&schema_backup_path(&path, 16)),
        16,
        "an older build drops the blend mode, so the file as it was is kept"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Tags are kept as they are, and a file stored before v15 has none. The file is copied aside, since an older build
/// reads a held-back tag change as the projection change it knows; a build that cannot write refuses a file without
/// the column, which its reads name.
#[test]
fn a_v14_file_gains_the_tags_column_and_keeps_a_backup() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    let (project, asset) = edited(1);
    let json = timeline_to_json(&project.timeline().unwrap()).unwrap();
    assert!(!json.contains("tags"), "the cut itself holds none: {json}");
    project.save_as(&path).unwrap();
    drop(project);
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 14).unwrap();
        conn.execute_batch("ALTER TABLE assets DROP COLUMN tags").unwrap();
        assert_eq!(store::pending_steps(&conn, 14), vec![14, 15, 16]);
    }

    let uri = format!("file:{}?mode=ro", path.display());
    let err = Project::open(&uri).err().expect("reading an asset needs the column");
    assert!(err.to_string().contains("read-only"), "{err}");

    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert_eq!(version_of(&schema_backup_path(&path, 14)), 14, "the file as it was is kept");
    assert!(project.list_assets().unwrap()[0].tags.is_empty());
    let tag = crate::model::Tag::parse("location: Cumbuco").unwrap();
    let edit = crate::model::TagEdit {
        add: vec![tag.clone()],
        ..Default::default()
    };
    project.edit_asset_tags(&[asset], &edit).unwrap();
    drop(project);
    assert_eq!(Project::open(&path).unwrap().list_assets().unwrap()[0].tags, vec![tag]);
    let files = std::fs::read_dir(&dir).unwrap().count();
    drop(Project::open(&path).unwrap());
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        files,
        "opening again makes no second backup"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A noise reduction is kept as it is (v16). The file is copied aside, since an older build cannot read the timeline
/// that holds one.
#[test]
fn a_v15_file_is_backed_up_stamped_v16_and_keeps_its_denoise() {
    use crate::model::AudioEffect;
    let dir = scratch();
    let path = dir.join("cut.kerf");
    let (project, _) = edited(1);
    let clip = project.timeline().unwrap().tracks[0].clips[0].id;
    let chain = vec![
        AudioEffect::Highpass { hz: 80.0 },
        AudioEffect::Denoise {
            reduction_db: 18.0,
            floor_db: -45.0,
            track_noise: true,
        },
    ];
    project.set_audio_effects(clip, chain.clone()).unwrap();
    project.save_as(&path).unwrap();
    drop(project);
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 15)
        .unwrap();
    assert_eq!(store::pending_steps(&Connection::open(&path).unwrap(), 15), vec![15, 16]);

    let project = Project::open(&path).unwrap();
    assert_eq!(version_of(&path), SCHEMA_VERSION);
    assert_eq!(project.timeline().unwrap().tracks[0].clips[0].audio, chain);
    drop(project);
    assert_eq!(
        version_of(&schema_backup_path(&path, 15)),
        15,
        "an older build cannot read the denoise, so the file as it was is kept"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The timelines a file holds, `persisted/timelines-v<N>.json`, frozen at each schema version that moved
/// the stored shape (the sample's and a few of the golden oracle's). Every one must still read; the newest
/// must come back as it was stored, so a change to the shape is a decision (a new version, see
/// `store::MIGRATIONS`) and not a slip. A version that only changes the tables needs no file.
const PERSISTED: &[(u32, &str)] = &[
    (3, include_str!("persisted/timelines-v3.json")),
    (5, include_str!("persisted/timelines-v5.json")),
    (6, include_str!("persisted/timelines-v6.json")),
    (7, include_str!("persisted/timelines-v7.json")),
    (8, include_str!("persisted/timelines-v8.json")),
    (9, include_str!("persisted/timelines-v9.json")),
    (11, include_str!("persisted/timelines-v11.json")),
    (12, include_str!("persisted/timelines-v12.json")),
    (13, include_str!("persisted/timelines-v13.json")),
    (14, include_str!("persisted/timelines-v14.json")),
    (16, include_str!("persisted/timelines-v16.json")),
    (17, include_str!("persisted/timelines-v17.json")),
];

fn persisted_path(version: u32) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("src/project/persisted/timelines-v{version}.json"))
}

#[test]
fn the_timelines_of_every_schema_version_still_read() {
    for (version, json) in PERSISTED {
        serde_json::from_str::<Vec<Timeline>>(json)
            .unwrap_or_else(|e| panic!("a schema v{version} timeline no longer reads: {e}"));
    }
}

/// Bless with `KERF_BLESS_PERSISTED=1`: it writes the current version's file (add it to `PERSISTED`).
/// Re-blessing a shipped version is only right when an older build reads the new shape as it is.
#[test]
fn the_stored_timeline_shape_moves_only_with_the_schema_version() {
    if std::env::var_os("KERF_BLESS_PERSISTED").is_some() {
        let mut timelines = vec![Project::sample().unwrap().timeline().unwrap()];
        timelines.extend(crate::engine::sample_timelines());
        // A template title, its text as outlines (v8).
        let titled = Project::sample().unwrap();
        outlined_title(&titled);
        timelines.push(titled.timeline().unwrap());
        // The same with an animation on its text (v9).
        let animated = Project::sample().unwrap();
        animated_title(&animated);
        timelines.push(animated.timeline().unwrap());
        // The same with curves on the animation (v11).
        let eased = Project::sample().unwrap();
        eased_title(&eased);
        timelines.push(eased.timeline().unwrap());
        // A delivery frame with a frame rate (v12).
        let rated = Project::sample().unwrap();
        rated
            .set_delivery_format(Some(
                crate::model::Delivery::new(1080, 1920, crate::model::Fit::Cover)
                    .with_fps(Some(50.0))
                    .unwrap(),
            ))
            .unwrap();
        timelines.push(rated.timeline().unwrap());
        // A thumbnail with a headline (v13).
        let thumbed = Project::sample().unwrap();
        thumbed
            .put_thumbnail(
                None,
                &crate::project::ThumbnailSpec {
                    time: Some(1.5),
                    size: Some("vertical".into()),
                    layers: Some(vec![crate::project::ThumbnailLayerSpec {
                        text: Some(crate::model::SvgPresetParams::new("Big news")),
                        ..Default::default()
                    }]),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        timelines.push(thumbed.timeline().unwrap());
        // A clip on a speed ramp, one segment held (v14).
        let ramped = Project::sample().unwrap();
        let clip = ramped.timeline().unwrap().tracks[0].clips[0].id;
        ramped
            .set_speed_ramp(
                clip,
                vec![
                    crate::model::SpeedKey::new(0.0, 1.0),
                    crate::model::SpeedKey {
                        time: 1.0,
                        speed: 0.25,
                        easing: crate::model::RampEasing::Hold,
                    },
                    crate::model::SpeedKey::new(2.0, 2.0),
                ],
            )
            .unwrap();
        timelines.push(ramped.timeline().unwrap());
        // A clip whose sound is denoised, its floor tracked (v16).
        let denoised = Project::sample().unwrap();
        let clip = denoised.timeline().unwrap().tracks[0].clips[0].id;
        denoised
            .set_audio_effects(
                clip,
                vec![crate::model::AudioEffect::Denoise {
                    reduction_db: 18.0,
                    floor_db: -45.0,
                    track_noise: true,
                }],
            )
            .unwrap();
        timelines.push(denoised.timeline().unwrap());
        // A clip screened onto the track below (v17).
        let blended = Project::sample().unwrap();
        let clip = blended.timeline().unwrap().tracks[0].clips[0].id;
        blended.set_blend_mode(clip, crate::model::BlendMode::Screen).unwrap();
        timelines.push(blended.timeline().unwrap());
        let lines: Vec<String> = timelines.iter().map(|t| serde_json::to_string(t).unwrap()).collect();
        std::fs::write(persisted_path(SCHEMA_VERSION), format!("[\n{}\n]\n", lines.join(",\n"))).unwrap();
        return;
    }
    let (_, json) = PERSISTED
        .iter()
        .filter(|(v, _)| *v <= SCHEMA_VERSION)
        .max_by_key(|(v, _)| *v)
        .expect("a stored shape at or below this schema version");
    let stored: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
    for (i, stored) in stored.into_iter().enumerate() {
        let timeline: Timeline = serde_json::from_value(stored.clone()).unwrap();
        let written: serde_json::Value = serde_json::from_str(&timeline_to_json(&timeline).unwrap()).unwrap();
        assert!(
            written == stored,
            "stored timeline {i}: its shape changed without a schema version. If an older Kerf would drop or \
             misread it, append a step to store::MIGRATIONS; then bless with KERF_BLESS_PERSISTED=1 and add the \
             new file to PERSISTED"
        );
    }
}
#[test]
fn opening_to_write_records_the_build_and_keeps_the_maker() {
    let dir = scratch();
    let path = dir.join("cut.kerf");
    drop(Project::create(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE meta SET value = '0.1.0' WHERE key IN ('kerf_version', 'last_kerf_version')",
        [],
    )
    .unwrap();
    drop(conn);
    let project = Project::open(&path).unwrap();
    assert_eq!(project.meta("kerf_version").unwrap().as_deref(), Some("0.1.0"));
    assert_eq!(
        project.meta("last_kerf_version").unwrap().as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    std::fs::remove_dir_all(&dir).ok();
}
