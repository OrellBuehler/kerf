//! Asset tags at the `Project` level: storing them, the agent's staged edits, the vocabulary, and the cut
//! `assemble_by_tag` builds from them. (The tag model's own rules are tested in `model/tags.rs`.)

use super::linked_tests::{asset, stream};
use super::*;
use crate::model::{EditSource, Projection, Tag, TagEdit, TagFilter};

fn tags(list: &[&str]) -> Vec<Tag> {
    list.iter().map(|s| Tag::parse(s).unwrap()).collect()
}

fn add(list: &[&str]) -> TagEdit {
    TagEdit {
        add: tags(list),
        ..Default::default()
    }
}

fn shown(tags: &[Tag]) -> Vec<String> {
    tags.iter().map(Tag::to_string).collect()
}

/// What the current actor sees on the asset.
fn seen(p: &Project, id: Uuid) -> Vec<String> {
    shown(&p.get_asset(id).unwrap().unwrap().tags)
}

/// A project of 10 s picture-and-sound assets, imported a second apart in the order given, each named
/// `<name>.mp4` and carrying its tags.
fn library(specs: &[(&str, &[&str])]) -> (Project, Vec<Asset>) {
    let project = Project::open_in_memory().unwrap();
    let start = Utc::now();
    let assets = specs
        .iter()
        .enumerate()
        .map(|(i, (name, tagged))| {
            let mut a = asset(
                &format!("/{name}.mp4"),
                10.0,
                vec![stream(StreamKind::Video), stream(StreamKind::Audio)],
            );
            a.name = format!("{name}.mp4");
            a.imported_at = start + chrono::Duration::seconds(i as i64);
            a.tags = tags(tagged);
            project.insert_asset(&a).unwrap();
            a
        })
        .collect();
    (project, assets)
}

fn revisions(p: &Project) -> usize {
    p.history().unwrap().len()
}

// ---- storing -------------------------------------------------------------------------------

#[test]
fn tags_are_written_for_every_named_asset_without_a_revision() {
    let (p, a) = library(&[("one", &[]), ("two", &["jump"]), ("three", &[])]);
    let before = revisions(&p);
    let edit = TagEdit {
        add: tags(&["location: Cumbuco", "Jump"]),
        ..Default::default()
    };
    let done = p.edit_asset_tags(&[a[1].id, a[0].id, a[1].id], &edit).unwrap();
    assert_eq!(done.len(), 2, "a repeated id is one asset");
    assert_eq!(
        shown(&done[0].tags),
        ["jump", "location: Cumbuco"],
        "the first spelling stays"
    );
    assert_eq!(shown(&done[1].tags), ["location: Cumbuco", "Jump"]);
    assert_eq!(seen(&p, a[1].id), ["jump", "location: Cumbuco"]);
    assert_eq!(seen(&p, a[0].id), ["location: Cumbuco", "Jump"]);
    assert!(seen(&p, a[2].id).is_empty());
    assert_eq!(revisions(&p), before, "the library is not in the edit history");

    let swap = TagEdit {
        add: tags(&["location: Jeri"]),
        remove_keys: vec!["location".into()],
        remove: tags(&["jump"]),
    };
    p.edit_asset_tags(&[a[1].id], &swap).unwrap();
    assert_eq!(seen(&p, a[1].id), ["location: Jeri"]);
}

#[test]
fn a_refused_tag_edit_writes_nothing() {
    let (p, a) = library(&[("one", &["keep"]), ("two", &[])]);
    let ghost = Uuid::new_v4();
    let err = p.edit_asset_tags(&[a[0].id, ghost], &add(&["x"])).unwrap_err();
    assert!(matches!(err, Error::AssetNotFound(id) if id == ghost), "{err:?}");
    assert_eq!(seen(&p, a[0].id), ["keep"], "the asset before the unknown one is untouched");

    let invalid = TagEdit {
        add: vec![
            Tag::parse("fine").unwrap(),
            Tag {
                key: None,
                value: "  ".into(),
            },
        ],
        ..Default::default()
    };
    assert!(p.edit_asset_tags(&[a[0].id, a[1].id], &invalid).is_err());
    assert!(
        p.edit_asset_tags(&[a[0].id, a[1].id], &TagEdit::default()).is_err(),
        "nothing to change"
    );
    assert!(p.edit_asset_tags(&[], &add(&["x"])).is_err(), "no asset named");

    // One asset already full: the other, which the edit would have fitted, is not written either.
    let full: Vec<String> = (0..64).map(|i| format!("t{i}")).collect();
    let refs: Vec<&str> = full.iter().map(String::as_str).collect();
    p.edit_asset_tags(&[a[1].id], &add(&refs)).unwrap();
    assert!(p.edit_asset_tags(&[a[0].id, a[1].id], &add(&["one more"])).is_err());
    assert_eq!(seen(&p, a[0].id), ["keep"]);
    assert_eq!(seen(&p, a[1].id).len(), 64);
}

#[test]
fn tags_survive_save_reopen_and_a_second_import_of_the_same_file() {
    let dir = std::env::temp_dir().join(format!("kerf-tags-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cut.kerf");
    let (p, a) = library(&[("one", &[]), ("two", &[])]);
    p.edit_asset_tags(&[a[0].id], &add(&["location: Cumbuco", "jump"])).unwrap();
    p.save_as(&path).unwrap();
    drop(p);

    let p = Project::open(&path).unwrap();
    assert_eq!(seen(&p, a[0].id), ["location: Cumbuco", "jump"]);
    assert!(seen(&p, a[1].id).is_empty());
    let column: Option<String> = p
        .conn
        .query_row("SELECT tags FROM assets WHERE id = ?1", params![a[1].id.to_string()], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(column, None, "an untagged asset stores nothing");

    // The same file imported again is the asset it already is, tags and all.
    let again = asset("/one.mp4", 10.0, vec![stream(StreamKind::Video)]);
    let kept = p.insert_or_get_asset(&again).unwrap();
    assert_eq!(kept.id, a[0].id);
    assert_eq!(shown(&kept.tags), ["location: Cumbuco", "jump"]);

    // An asset's own rewrite (marking it 360) leaves its tags alone.
    p.set_asset_projection(a[0].id, Some(Projection::Equirect)).unwrap();
    assert_eq!(seen(&p, a[0].id), ["location: Cumbuco", "jump"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_stored_change_of_an_older_build_still_reads() {
    let old = r#"{"id":"asset_projection:00000000-0000-0000-0000-000000000001","asset_id":"00000000-0000-0000-0000-000000000001","projection":"equirect","summary":"Mark a.mp4 as equirect 360"}"#;
    let change: StagedAssetChange = serde_json::from_str(old).unwrap();
    assert_eq!(change.projection, Some(Projection::Equirect));
    assert_eq!(change.tag_edit, None);
    let cleared: StagedAssetChange = serde_json::from_str(
        r#"{"id":"asset_projection:00000000-0000-0000-0000-000000000001","asset_id":"00000000-0000-0000-0000-000000000001","projection":null,"summary":"x"}"#,
    )
    .unwrap();
    assert_eq!((cleared.projection, cleared.tag_edit), (None, None));
    assert!(
        !serde_json::to_string(&change).unwrap().contains("tag_edit"),
        "a projection change is written as it always was"
    );

    let tagged = StagedAssetChange {
        id: StagedAssetChange::tags_id(Uuid::nil()),
        asset_id: Uuid::nil(),
        projection: None,
        tag_edit: Some(TagEdit {
            add: tags(&["jump"]),
            remove_keys: vec!["location".into()],
            ..Default::default()
        }),
        summary: "Tag a.mp4".into(),
    };
    let json = serde_json::to_value(&tagged).unwrap();
    assert_eq!(
        json["tag_edit"],
        serde_json::json!({"add": [{"value": "jump"}], "remove_keys": ["location"]}),
        "the composed edit, with its empty parts left out"
    );
    assert_eq!(serde_json::from_value::<StagedAssetChange>(json).unwrap(), tagged);

    // And a proposal row an older build stored loads with its projection change.
    let (mut p, a) = library(&[("one", &[])]);
    p.set_actor(EditSource::Agent);
    p.set_asset_projection(a[0].id, Some(Projection::Equirect)).unwrap();
    let held: String = p.conn.query_row("SELECT assets FROM staged", [], |r| r.get(0)).unwrap();
    assert!(!held.contains("tag_edit"), "{held}");
    p.set_actor(EditSource::User);
    let staged = p.staged().unwrap().unwrap();
    assert_eq!(staged.asset_changes[0].projection, Some(Projection::Equirect));
}

// ---- the agent's edits stage ---------------------------------------------------------------

#[test]
fn an_agents_tags_are_held_with_its_proposal_and_land_with_it() {
    let (mut p, a) = library(&[("one", &["keep"]), ("two", &[])]);
    let before = revisions(&p);
    p.set_actor(EditSource::Agent);
    let done = p.edit_asset_tags(&[a[0].id, a[1].id], &add(&["location: Cumbuco"])).unwrap();
    assert_eq!(shown(&done[0].tags), ["keep", "location: Cumbuco"]);

    // The agent sees its pending work everywhere it reads the library; the user's library is untouched.
    assert_eq!(seen(&p, a[0].id), ["keep", "location: Cumbuco"]);
    assert_eq!(shown(&p.list_assets().unwrap()[1].tags), ["location: Cumbuco"]);
    assert_eq!(p.list_tags().unwrap().keys[0].values[0].assets, 2);
    assert_eq!(
        p.list_assets_tagged(&[TagFilter::parse("location:*").unwrap()])
            .unwrap()
            .len(),
        2
    );
    p.set_actor(EditSource::User);
    assert_eq!(seen(&p, a[0].id), ["keep"]);
    assert!(seen(&p, a[1].id).is_empty());
    assert!(p.list_tags().unwrap().keys.is_empty());

    let staged = p.staged().unwrap().unwrap();
    assert_eq!(staged.asset_changes.len(), 2);
    assert_eq!(staged.asset_changes[0].id, format!("asset_tags:{}", a[0].id));
    assert_eq!(
        staged.asset_changes[0].summary, "Tag one.mp4: +location: Cumbuco",
        "{:?}",
        staged.asset_changes
    );
    assert_eq!(
        staged.asset_changes[0].tag_edit,
        Some(add(&["location: Cumbuco"])),
        "the edit is held, not the list it made"
    );
    assert_eq!(staged.edits, ["Tag 2 assets"], "one step for the call");
    assert!(staged.diff.is_empty(), "the cut is untouched");

    p.apply_staged(false).unwrap();
    assert_eq!(seen(&p, a[0].id), ["keep", "location: Cumbuco"]);
    assert_eq!(seen(&p, a[1].id), ["location: Cumbuco"]);
    assert_eq!(revisions(&p), before, "no revision for the library");
    assert!(p.staged().unwrap().is_none());
}

#[test]
fn a_discarded_tag_proposal_never_lands_and_can_be_restored() {
    let (mut p, a) = library(&[("one", &[])]);
    p.set_actor(EditSource::Agent);
    p.edit_asset_tags(&[a[0].id], &add(&["jump"])).unwrap();
    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();
    assert!(seen(&p, a[0].id).is_empty());
    assert!(p.staged().unwrap().is_none());

    let back = p.restore_discarded().unwrap();
    assert_eq!(back.asset_changes.len(), 1);
    p.apply_staged(false).unwrap();
    assert_eq!(seen(&p, a[0].id), ["jump"]);
}

#[test]
fn an_agents_later_tag_edit_builds_on_its_pending_one_and_asking_for_what_is_stored_takes_it_back() {
    let (mut p, a) = library(&[("one", &["keep"])]);
    p.set_actor(EditSource::Agent);
    p.edit_asset_tags(&[a[0].id], &add(&["jump"])).unwrap();
    p.edit_asset_tags(&[a[0].id], &add(&["wave"])).unwrap();
    let staged = p.staged().unwrap().unwrap();
    assert_eq!(staged.asset_changes.len(), 1, "one pending change per asset");
    assert_eq!(seen(&p, a[0].id), ["keep", "jump", "wave"]);
    assert_eq!(staged.edits.len(), 2);

    // Back to the stored list: the change is withdrawn, the proposal is left empty.
    let undo = TagEdit {
        remove: tags(&["jump", "wave"]),
        ..Default::default()
    };
    p.edit_asset_tags(&[a[0].id], &undo).unwrap();
    assert!(p.staged().unwrap().unwrap().asset_changes.is_empty());
    assert_eq!(seen(&p, a[0].id), ["keep"]);

    // Asking for what the library has, with nothing pending, stages nothing at all.
    p.set_actor(EditSource::User);
    p.discard_staged().unwrap();
    p.set_actor(EditSource::Agent);
    p.edit_asset_tags(&[a[0].id], &add(&["keep"])).unwrap();
    assert!(p.staged().unwrap().is_none());
}

#[test]
fn undoing_a_staged_step_takes_back_its_tags() {
    let (mut p, a) = library(&[("one", &[]), ("two", &[])]);
    p.set_actor(EditSource::Agent);
    p.edit_asset_tags(&[a[0].id, a[1].id], &add(&["jump"])).unwrap();
    p.edit_asset_tags(&[a[0].id], &add(&["wave"])).unwrap();
    p.undo_staged().unwrap();
    assert_eq!(seen(&p, a[0].id), ["jump"]);
    assert_eq!(seen(&p, a[1].id), ["jump"]);
    p.undo_staged().unwrap();
    assert!(seen(&p, a[0].id).is_empty() && seen(&p, a[1].id).is_empty());
}

#[test]
fn tags_and_a_projection_are_separate_pending_changes_of_one_asset() {
    let (mut p, a) = library(&[("one", &[])]);
    p.set_actor(EditSource::Agent);
    p.set_asset_projection(a[0].id, Some(Projection::Equirect)).unwrap();
    p.edit_asset_tags(&[a[0].id], &add(&["jump"])).unwrap();
    let got = p.get_asset(a[0].id).unwrap().unwrap();
    assert_eq!(
        (got.projection(), shown(&got.tags)),
        (Some(Projection::Equirect), vec!["jump".to_string()])
    );
    p.set_actor(EditSource::User);
    let ids: Vec<String> = p.staged().unwrap().unwrap().asset_changes.into_iter().map(|c| c.id).collect();
    assert_eq!(
        ids,
        [format!("asset_projection:{}", a[0].id), format!("asset_tags:{}", a[0].id)]
    );

    // Pick only the tags.
    let done = p.apply_staged_subset(&[format!("asset_tags:{}", a[0].id)], false).unwrap();
    assert_eq!((done.applied, done.remaining, done.closed), (1, 1, false));
    let now = p.get_asset(a[0].id).unwrap().unwrap();
    assert_eq!((now.projection(), shown(&now.tags)), (None, vec!["jump".to_string()]));
    p.apply_staged(false).unwrap();
    assert_eq!(
        p.get_asset(a[0].id).unwrap().unwrap().projection(),
        Some(Projection::Equirect)
    );
}

#[test]
fn removing_an_asset_drops_its_pending_tags() {
    let (mut p, a) = library(&[("one", &[]), ("two", &[])]);
    p.set_actor(EditSource::Agent);
    p.edit_asset_tags(&[a[0].id, a[1].id], &add(&["jump"])).unwrap();
    p.set_actor(EditSource::User);
    p.remove_asset(a[0].id, false).unwrap();
    let staged = p.staged().unwrap().unwrap();
    assert_eq!(staged.asset_changes.len(), 1);
    assert_eq!(staged.asset_changes[0].asset_id, a[1].id);
    p.apply_staged(false).unwrap();
    assert_eq!(seen(&p, a[1].id), ["jump"]);
}

#[test]
fn an_agent_with_review_off_and_no_proposal_tags_directly() {
    let (mut p, a) = library(&[("one", &[])]);
    p.set_agent_review(false);
    p.set_actor(EditSource::Agent);
    p.edit_asset_tags(&[a[0].id], &add(&["jump"])).unwrap();
    assert!(p.staged().unwrap().is_none());
    p.set_actor(EditSource::User);
    assert_eq!(seen(&p, a[0].id), ["jump"]);

    // A proposal that is open still holds the agent's tags back, review on or off.
    p.set_actor(EditSource::Agent);
    p.begin_staging(None, None, None).unwrap();
    p.edit_asset_tags(&[a[0].id], &add(&["wave"])).unwrap();
    p.set_actor(EditSource::User);
    assert_eq!(seen(&p, a[0].id), ["jump"]);
    assert_eq!(p.staged().unwrap().unwrap().asset_changes.len(), 1);
}

#[test]
fn one_session_cannot_tag_into_a_proposal_another_owns() {
    let (mut p, a) = library(&[("one", &[])]);
    p.set_actor(EditSource::Agent);
    p.begin_staging(None, None, Some("session-1")).unwrap();
    p.declare_agent(Some("session-2"));
    let err = p.edit_asset_tags(&[a[0].id], &add(&["jump"])).unwrap_err();
    assert!(matches!(err, Error::StagedEditOwnedByOther), "{err:?}");
    assert!(p.staged().unwrap().unwrap().asset_changes.is_empty());
}

#[test]
fn what_the_user_tags_while_a_proposal_is_open_is_kept_when_it_lands() {
    let (mut p, a) = library(&[("one", &["keep", "location: Jeri"])]);
    p.set_actor(EditSource::Agent);
    let swap = TagEdit {
        add: tags(&["location: Cumbuco", "jump"]),
        remove_keys: vec!["location".into()],
        ..Default::default()
    };
    p.edit_asset_tags(&[a[0].id], &swap).unwrap();
    assert_eq!(seen(&p, a[0].id), ["keep", "location: Cumbuco", "jump"]);

    // The user tags the same asset live: they take a tag off and put two on.
    p.set_actor(EditSource::User);
    let theirs = TagEdit {
        add: tags(&["wave", "rider: Orell"]),
        remove: tags(&["keep"]),
        ..Default::default()
    };
    p.edit_asset_tags(&[a[0].id], &theirs).unwrap();
    assert_eq!(seen(&p, a[0].id), ["location: Jeri", "wave", "rider: Orell"]);

    // The agent's view is its edit over what the library has now.
    p.set_actor(EditSource::Agent);
    assert_eq!(seen(&p, a[0].id), ["wave", "rider: Orell", "location: Cumbuco", "jump"]);
    assert_eq!(
        p.list_tags().unwrap().keys.iter().map(|k| k.key.as_str()).collect::<Vec<_>>(),
        ["location", "rider"]
    );

    // And accepting runs the edit over those tags, not over the ones it was made against.
    p.set_actor(EditSource::User);
    p.apply_staged(false).unwrap();
    assert_eq!(seen(&p, a[0].id), ["wave", "rider: Orell", "location: Cumbuco", "jump"]);
}

#[test]
fn a_later_agent_edit_is_composed_with_the_earlier_ones() {
    let (mut p, a) = library(&[("one", &["keep", "location: Jeri"])]);
    p.set_actor(EditSource::Agent);
    p.edit_asset_tags(&[a[0].id], &add(&["jump", "location: Cumbuco"])).unwrap();
    let later = TagEdit {
        add: tags(&["wave"]),
        remove: tags(&["JUMP", "keep"]),
        remove_keys: vec!["rider".into()],
    };
    p.edit_asset_tags(&[a[0].id], &later).unwrap();
    let held = p.staged().unwrap().unwrap().asset_changes;
    assert_eq!(held.len(), 1);
    let edit = held[0].tag_edit.clone().unwrap();
    assert_eq!(
        shown(&edit.add),
        ["location: Cumbuco", "wave"],
        "the earlier add of `jump` was taken off again"
    );
    assert_eq!(shown(&edit.remove), ["JUMP", "keep"]);
    assert_eq!(edit.remove_keys, ["rider"]);
    p.set_actor(EditSource::User);
    p.apply_staged(false).unwrap();
    assert_eq!(seen(&p, a[0].id), ["location: Jeri", "location: Cumbuco", "wave"]);
}

#[test]
fn landing_a_tag_edit_never_fails_on_the_limit_it_runs_into() {
    let full: Vec<String> = (0..62).map(|i| format!("t{i}")).collect();
    let refs: Vec<&str> = full.iter().map(String::as_str).collect();
    let (mut p, a) = library(&[("one", &refs)]);
    p.set_actor(EditSource::Agent);
    p.edit_asset_tags(&[a[0].id], &add(&["x1", "x2"])).unwrap();
    assert_eq!(seen(&p, a[0].id).len(), 64);

    // The user has since put another tag on, so there is room for one of the agent's two.
    p.set_actor(EditSource::User);
    p.edit_asset_tags(&[a[0].id], &add(&["u1"])).unwrap();
    p.set_actor(EditSource::Agent);
    assert_eq!(seen(&p, a[0].id).len(), 64, "the agent's view is capped too");
    p.set_actor(EditSource::User);
    p.apply_staged(false).unwrap();
    let landed = seen(&p, a[0].id);
    assert_eq!(landed.len(), 64);
    assert!(landed.contains(&"u1".to_string()) && landed.contains(&"x1".to_string()));
    assert!(!landed.contains(&"x2".to_string()), "the add that did not fit was left off");
}

#[test]
fn a_tag_the_user_also_added_is_taken_off_by_the_agents_remove() {
    let (mut p, a) = library(&[("one", &["keep"])]);
    p.set_actor(EditSource::Agent);
    p.edit_asset_tags(&[a[0].id], &add(&["jump"])).unwrap();
    p.set_actor(EditSource::User);
    p.edit_asset_tags(&[a[0].id], &add(&["jump"])).unwrap();
    p.set_actor(EditSource::Agent);
    // The user already tagged `jump`, so asking for it again changes nothing the agent sees; taking it off is a change.
    let before = p.staged().unwrap().unwrap();
    p.edit_asset_tags(&[a[0].id], &add(&["jump"])).unwrap();
    assert_eq!(p.staged().unwrap().unwrap().edits, before.edits);
    let off = TagEdit {
        remove: tags(&["jump"]),
        ..Default::default()
    };
    p.edit_asset_tags(&[a[0].id], &off).unwrap();
    assert_eq!(seen(&p, a[0].id), ["keep"]);
    p.set_actor(EditSource::User);
    p.apply_staged(false).unwrap();
    assert_eq!(
        seen(&p, a[0].id),
        ["keep"],
        "the proposal took `jump` off, the user's tag with it"
    );
}

#[test]
fn an_edit_that_changes_nothing_for_the_agent_pushes_no_step() {
    let (mut p, a) = library(&[("one", &["keep"]), ("two", &[])]);
    p.set_actor(EditSource::Agent);
    p.edit_asset_tags(&[a[0].id, a[1].id], &add(&["jump"])).unwrap();
    let held = p.staged().unwrap().unwrap();
    assert_eq!((held.edits.len(), held.undo_depth), (1, 1));

    // The same call again, a spelling of it, and taking off then putting back in one call: nothing moves.
    p.edit_asset_tags(&[a[0].id, a[1].id], &add(&["jump"])).unwrap();
    p.edit_asset_tags(&[a[0].id], &add(&["JUMP"])).unwrap();
    let churn = TagEdit {
        add: tags(&["keep"]),
        remove: tags(&["keep"]),
        ..Default::default()
    };
    p.edit_asset_tags(&[a[0].id], &churn).unwrap();
    let after = p.staged().unwrap().unwrap();
    assert_eq!((after.edits.clone(), after.undo_depth), (held.edits.clone(), 1));
    assert_eq!(after.asset_changes, held.asset_changes);

    // One asset changing among others that do not is a step of its own, labelled for the one that moved.
    p.edit_asset_tags(&[a[0].id, a[1].id], &add(&["jump", "wave"])).unwrap();
    let more = p.staged().unwrap().unwrap();
    assert_eq!((more.edits.len(), more.undo_depth), (2, 2));
    assert_eq!(more.edits[1], "Tag 2 assets");
    p.undo_staged().unwrap();
    assert_eq!(seen(&p, a[1].id), ["jump"]);
}

#[test]
fn putting_a_tag_back_in_another_spelling_is_a_change() {
    let (mut p, a) = library(&[("one", &["location: cumbuco"])]);
    let respell = TagEdit {
        add: tags(&["location: Cumbuco"]),
        remove: tags(&["location: cumbuco"]),
        ..Default::default()
    };
    p.set_actor(EditSource::Agent);
    p.edit_asset_tags(&[a[0].id], &respell).unwrap();
    assert_eq!(seen(&p, a[0].id), ["location: Cumbuco"]);
    p.set_actor(EditSource::User);
    p.apply_staged(false).unwrap();
    assert_eq!(seen(&p, a[0].id), ["location: Cumbuco"]);
    p.edit_asset_tags(
        &[a[0].id],
        &TagEdit {
            add: tags(&["location: CUMBUCO"]),
            remove: tags(&["location: cumbuco"]),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        seen(&p, a[0].id),
        ["location: CUMBUCO"],
        "and the same for the user's own edit"
    );
}

// ---- the vocabulary ------------------------------------------------------------------------

#[test]
fn the_vocabulary_and_the_filter_read_the_library() {
    let (p, _) = library(&[
        ("one", &["location: Cumbuco", "rider: Orell", "jump"]),
        ("two", &["location: Cumbuco", "wave"]),
        ("three", &["location: Jeri", "jump"]),
        ("four", &[]),
    ]);
    let summary = p.list_tags().unwrap();
    assert_eq!(
        summary.keys.iter().map(|k| k.key.as_str()).collect::<Vec<_>>(),
        ["location", "rider"]
    );
    assert_eq!(
        summary.keys[0]
            .values
            .iter()
            .map(|v| (v.value.as_str(), v.assets))
            .collect::<Vec<_>>(),
        [("Cumbuco", 2), ("Jeri", 1)]
    );
    assert_eq!(
        summary.plain.iter().map(|v| (v.value.as_str(), v.assets)).collect::<Vec<_>>(),
        [("jump", 2), ("wave", 1)]
    );

    let term = |t: &str| TagFilter::parse(t).unwrap();
    let names =
        |filter: &[TagFilter]| -> Vec<String> { p.list_assets_tagged(filter).unwrap().into_iter().map(|a| a.name).collect() };
    assert_eq!(names(&[term("location: cumbuco"), term("jump")]), ["one.mp4"]);
    assert_eq!(names(&[term("location:*")]).len(), 3);
    assert_eq!(names(&[]).len(), 4);
    assert!(p.list_assets_tagged(&[TagFilter::default()]).is_err());
}

// ---- assemble_by_tag -------------------------------------------------------------------------

/// Six 10 s assets: two in Jeri, two in Cumbuco, one in Atins and one with no location.
fn trip() -> (Project, Vec<Asset>) {
    library(&[
        ("jeri-a", &["location: Jeri", "jump"]),
        ("cumbuco-a", &["location: Cumbuco", "jump"]),
        ("atins", &["location: Atins"]),
        ("jeri-b", &["location: Jeri"]),
        ("cumbuco-b", &["location: Cumbuco", "wave"]),
        ("loose", &["jump"]),
    ])
}

fn picture(p: &Project) -> Vec<Clip> {
    let t = p.timeline().unwrap();
    t.tracks
        .iter()
        .find(|t| t.kind == StreamKind::Video)
        .unwrap()
        .clips
        .iter()
        .filter(|c| !c.is_generated())
        .cloned()
        .collect()
}

fn titles(p: &Project) -> Vec<(String, f64, f64)> {
    let t = p.timeline().unwrap();
    let mut found: Vec<_> = t
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .filter_map(|c| {
            c.title_clip()
                .map(|title| (title.name.clone(), c.timeline_start, c.duration()))
        })
        .collect();
    found.sort_by(|a, b| a.1.total_cmp(&b.1));
    found
}

#[test]
fn groups_come_out_in_order_a_clip_each_in_one_revision() {
    let (p, a) = trip();
    let before = revisions(&p);
    let done = p
        .assemble_by_tag(AssembleByTag {
            order: Some(vec!["jeri".into()]),
            ..AssembleByTag::new("Location")
        })
        .unwrap();
    assert_eq!(revisions(&p), before + 1, "one revision for the whole assembly");
    assert_eq!(
        p.history().unwrap().last().unwrap().label,
        "Assemble by location (3 groups, 5 clips)"
    );

    let values: Vec<_> = done.groups.iter().map(|g| g.value.as_deref().unwrap()).collect();
    assert_eq!(values, ["Jeri", "Atins", "Cumbuco"], "the listed one first, then A to Z");
    let ids = |g: usize| done.groups[g].asset_ids.clone();
    assert_eq!(ids(0), [a[0].id, a[3].id], "imported order within a group");
    assert_eq!(ids(1), [a[2].id]);
    assert_eq!(ids(2), [a[1].id, a[4].id]);
    assert_eq!(
        done.groups.iter().map(|g| (g.start, g.end)).collect::<Vec<_>>(),
        [(0.0, 20.0), (20.0, 30.0), (30.0, 50.0)]
    );

    let clips = picture(&p);
    assert_eq!(
        clips.iter().map(|c| c.id).collect::<Vec<_>>(),
        done.clip_ids,
        "play order = clip_ids"
    );
    assert_eq!(
        clips.iter().map(|c| c.asset_id).collect::<Vec<_>>(),
        [a[0].id, a[3].id, a[2].id, a[1].id, a[4].id]
    );
    assert_eq!(
        clips.iter().map(|c| c.timeline_start).collect::<Vec<_>>(),
        [0.0, 10.0, 20.0, 30.0, 40.0]
    );
    assert!(
        clips.iter().all(|c| c.duration() == 10.0 && c.source_in == 0.0),
        "whole assets"
    );
    let t = p.timeline().unwrap();
    let a1 = t.tracks.iter().find(|t| t.kind == StreamKind::Audio).unwrap();
    assert!(
        a1.clips.is_empty(),
        "the sound plays from the picture clip, as concatenate lays it"
    );
    assert!(done.unmatched.is_empty());
}

#[test]
fn an_overlay_title_sits_over_the_first_seconds_of_each_groups_first_clip() {
    let (p, _) = trip();
    let done = p.assemble_by_tag(AssembleByTag::new("location")).unwrap();
    assert_eq!(
        titles(&p),
        [
            ("Atins".to_string(), 0.0, 3.0),
            ("Cumbuco".to_string(), 10.0, 3.0),
            ("Jeri".to_string(), 30.0, 3.0)
        ]
    );
    assert!(done.groups.iter().all(|g| g.title_clip_id.is_some()));
    let t = p.timeline().unwrap();
    let (clip, title) = t.title_clip(done.groups[0].title_clip_id.unwrap()).unwrap();
    assert!(title.svg.contains("Atins"), "the group's value is the text");
    assert_eq!(clip.timeline_start, 0.0);
    let video: Vec<_> = t.tracks.iter().filter(|t| t.kind == StreamKind::Video).collect();
    assert_eq!(video.len(), 2, "titles go on a track above the picture");
    assert!(video[0].clips.iter().all(|c| !c.is_generated()) && video[1].clips.iter().all(|c| c.is_generated()));

    // A title never runs past a short first clip, and a different length can be asked for.
    let (p, _) = library(&[("tiny", &["location: Here"])]);
    let mut tiny = p.list_assets().unwrap().remove(0);
    tiny.duration = 1.5;
    p.insert_asset(&tiny).unwrap();
    p.assemble_by_tag(AssembleByTag::new("location")).unwrap();
    assert_eq!(titles(&p), [("Here".to_string(), 0.0, 1.5)]);
    let (p, _) = trip();
    p.assemble_by_tag(AssembleByTag {
        title_seconds: 5.0,
        ..AssembleByTag::new("location")
    })
    .unwrap();
    assert!(titles(&p).iter().all(|t| t.2 == 5.0));
}

#[test]
fn a_card_title_makes_room_before_its_group() {
    let (p, a) = trip();
    let done = p
        .assemble_by_tag(AssembleByTag {
            title: GroupTitle::Card,
            title_seconds: 2.0,
            filter: vec![TagFilter::parse("jump").unwrap()],
            ..AssembleByTag::new("location")
        })
        .unwrap();
    // jump: jeri-a (Jeri) and cumbuco-a (Cumbuco); `loose` has no location and is left out.
    assert_eq!(
        done.groups.iter().map(|g| g.value.as_deref().unwrap()).collect::<Vec<_>>(),
        ["Cumbuco", "Jeri"]
    );
    assert_eq!(
        titles(&p),
        [("Cumbuco".to_string(), 0.0, 2.0), ("Jeri".to_string(), 12.0, 2.0)]
    );
    let clips = picture(&p);
    assert_eq!(
        clips.iter().map(|c| (c.asset_id, c.timeline_start)).collect::<Vec<_>>(),
        [(a[1].id, 2.0), (a[0].id, 14.0)]
    );
    assert_eq!(
        done.groups.iter().map(|g| (g.start, g.end)).collect::<Vec<_>>(),
        [(0.0, 12.0), (12.0, 24.0)]
    );
    let t = p.timeline().unwrap();
    assert_eq!(
        t.tracks.iter().filter(|t| t.kind == StreamKind::Video).count(),
        1,
        "a card sits on the picture track itself"
    );
}

#[test]
fn no_title_and_untagged_assets_last_without_one() {
    let (p, a) = trip();
    let done = p
        .assemble_by_tag(AssembleByTag {
            title: GroupTitle::None,
            include_untagged: true,
            ..AssembleByTag::new("location")
        })
        .unwrap();
    assert!(titles(&p).is_empty());
    assert_eq!(done.groups.len(), 4);
    assert_eq!(done.groups[3].value, None);
    assert_eq!(done.groups[3].asset_ids, [a[5].id]);
    assert_eq!(picture(&p).last().unwrap().asset_id, a[5].id);

    // With titles on, the group with no value is still untitled.
    let (p, _) = trip();
    let done = p
        .assemble_by_tag(AssembleByTag {
            include_untagged: true,
            ..AssembleByTag::new("location")
        })
        .unwrap();
    assert_eq!(titles(&p).len(), 3);
    assert_eq!(done.groups[3].title_clip_id, None);
}

#[test]
fn assets_are_ordered_within_a_group_by_import_or_by_name() {
    let by = |sort_within| {
        let (p, a) = library(&[("b-shot", &["spot: X"]), ("c-shot", &["spot: X"]), ("a-shot", &["spot: X"])]);
        let done = p
            .assemble_by_tag(AssembleByTag {
                sort_within,
                title: GroupTitle::None,
                ..AssembleByTag::new("spot")
            })
            .unwrap();
        done.groups[0]
            .asset_ids
            .iter()
            .map(|id| a.iter().find(|a| a.id == *id).unwrap().name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(by(SortWithin::Imported), ["b-shot.mp4", "c-shot.mp4", "a-shot.mp4"]);
    assert_eq!(by(SortWithin::Name), ["a-shot.mp4", "b-shot.mp4", "c-shot.mp4"]);
}

#[test]
fn an_assembly_is_appended_to_the_end_of_the_cut() {
    let (p, a) = trip();
    let first = p.add_clip_to_timeline(a[5].id, None, 0.0, 4.0, Some(0.0)).unwrap();
    p.assemble_by_tag(AssembleByTag {
        title: GroupTitle::None,
        filter: vec![TagFilter::parse("wave").unwrap()],
        ..AssembleByTag::new("location")
    })
    .unwrap();
    let clips = picture(&p);
    assert_eq!(clips[0].id, first.id);
    assert_eq!((clips[1].asset_id, clips[1].timeline_start), (a[4].id, 4.0));
}

#[test]
fn a_request_nothing_answers_is_an_error_and_changes_nothing() {
    let (p, _) = trip();
    let before = revisions(&p);
    let cut = p.timeline_json().unwrap();
    for req in [
        AssembleByTag::new("wind"),
        AssembleByTag {
            filter: vec![TagFilter::parse("rider:*").unwrap()],
            ..AssembleByTag::new("location")
        },
        AssembleByTag::new("  "),
        AssembleByTag {
            title_seconds: 0.0,
            ..AssembleByTag::new("location")
        },
        AssembleByTag {
            title_seconds: f64::NAN,
            ..AssembleByTag::new("location")
        },
        AssembleByTag {
            title_seconds: 61.0,
            ..AssembleByTag::new("location")
        },
        AssembleByTag {
            filter: vec![TagFilter::default()],
            ..AssembleByTag::new("location")
        },
        AssembleByTag {
            order: Some(vec![" ".into()]),
            ..AssembleByTag::new("location")
        },
    ] {
        assert!(p.assemble_by_tag(req.clone()).is_err(), "{req:?}");
    }
    let err = p.assemble_by_tag(AssembleByTag::new("wind")).unwrap_err().to_string();
    assert!(err.contains("\"wind\""), "{err}");
    assert_eq!(revisions(&p), before);
    assert_eq!(p.timeline_json().unwrap(), cut);
}

#[test]
fn order_values_no_asset_carries_are_reported() {
    let (p, _) = trip();
    let done = p
        .assemble_by_tag(AssembleByTag {
            order: Some(vec!["Fortaleza".into(), "cumbuco".into()]),
            title: GroupTitle::None,
            ..AssembleByTag::new("location")
        })
        .unwrap();
    assert_eq!(done.unmatched, ["Fortaleza"]);
    assert_eq!(done.groups[0].value.as_deref(), Some("Cumbuco"));
}

#[test]
fn a_full_set_of_titles_refuses_the_assembly_whole() {
    let (p, _) = trip();
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 9"><rect width="8" height="4"/></svg>"#;
    for i in 0..crate::model::MAX_TITLE_CLIPS - 1 {
        p.add_title_clip(svg, 100.0 + i as f64, Some(1.0), TitleClipPlacement::default())
            .unwrap();
    }
    let before = (revisions(&p), p.timeline_json().unwrap());
    let err = p.assemble_by_tag(AssembleByTag::new("location")).unwrap_err().to_string();
    assert!(err.contains("at most"), "{err}");
    assert_eq!((revisions(&p), p.timeline_json().unwrap()), before, "no half-built cut");
}

#[test]
fn an_agents_assembly_is_staged_and_lands_when_accepted() {
    let (mut p, a) = trip();
    p.set_actor(EditSource::Agent);
    // The agent tags, then assembles in the same proposal: the assembly reads its pending tags.
    p.edit_asset_tags(&[a[5].id], &add(&["location: Prea"])).unwrap();
    let done = p.assemble_by_tag(AssembleByTag::new("location")).unwrap();
    assert_eq!(done.groups.len(), 4);
    assert_eq!(done.groups[3].value.as_deref(), Some("Prea"));
    p.set_actor(EditSource::User);
    assert!(picture(&p).is_empty() && titles(&p).is_empty(), "the live cut is untouched");
    let staged = p.staged().unwrap().unwrap();
    assert_eq!(staged.edits.len(), 2);
    assert!(staged.edits[1].starts_with("Assemble by location"));
    assert_eq!(staged.asset_changes.len(), 1);

    p.apply_staged(false).unwrap();
    assert_eq!(picture(&p).len(), 6);
    assert_eq!(titles(&p).len(), 4);
    assert_eq!(seen(&p, a[5].id), ["jump", "location: Prea"]);
}

#[test]
fn an_assembly_follows_ripple_and_leaves_linked_clips_linked() {
    let (p, a) = trip();
    let shot = p.add_clip_to_timeline(a[5].id, None, 0.0, 4.0, Some(0.0)).unwrap();
    p.detach_audio(shot.id).unwrap();
    p.set_ripple_mode(true).unwrap();
    let before = p.timeline().unwrap();
    p.assemble_by_tag(AssembleByTag::new("location")).unwrap();
    let after = p.timeline().unwrap();
    let json = |t: &Timeline| serde_json::to_value(t.clip(shot.id).unwrap()).unwrap();
    assert_eq!(json(&after), json(&before), "the linked pair is as it was");
    assert!(after.clip(shot.id).unwrap().link_id.is_some());
    assert_eq!(picture(&p).len(), 6);
    assert_eq!(picture(&p)[1].timeline_start, 4.0);
}

#[test]
fn sound_alone_lands_on_an_audio_track_after_the_picture_before_it() {
    let project = Project::open_in_memory().unwrap();
    let mut video = asset("/v.mp4", 10.0, vec![stream(StreamKind::Video)]);
    let mut song = asset("/s.wav", 5.0, vec![stream(StreamKind::Audio)]);
    video.imported_at = Utc::now();
    song.imported_at = video.imported_at + chrono::Duration::seconds(1);
    video.tags = tags(&["act: one"]);
    song.tags = tags(&["act: one"]);
    project.insert_asset(&video).unwrap();
    project.insert_asset(&song).unwrap();
    project
        .assemble_by_tag(AssembleByTag {
            title: GroupTitle::None,
            ..AssembleByTag::new("act")
        })
        .unwrap();
    let t = project.timeline().unwrap();
    let a1 = t.tracks.iter().find(|t| t.kind == StreamKind::Audio).unwrap();
    assert_eq!((a1.clips[0].asset_id, a1.clips[0].timeline_start), (song.id, 10.0));
    assert_eq!(picture(&project)[0].asset_id, video.id);
}

/// A project holding a 60 s untagged music bed on A1, a 10 s picture and two 5 s sounds, the last three tagged `act: one`.
fn bed_and_footage() -> (Project, Asset, Vec<Asset>) {
    let project = Project::open_in_memory().unwrap();
    let now = Utc::now();
    let make = |name: &str, seconds: f64, kinds: Vec<StreamInfo>, i: i64, tagged: bool| {
        let mut a = asset(&format!("/{name}"), seconds, kinds);
        a.imported_at = now + chrono::Duration::seconds(i);
        if tagged {
            a.tags = tags(&["act: one"]);
        }
        project.insert_asset(&a).unwrap();
        a
    };
    let bed = make("bed.wav", 60.0, vec![stream(StreamKind::Audio)], 0, false);
    project.add_asset_audio(bed.id).unwrap();
    let rest = vec![
        make(
            "v.mp4",
            10.0,
            vec![stream(StreamKind::Video), stream(StreamKind::Audio)],
            1,
            true,
        ),
        make("s1.wav", 5.0, vec![stream(StreamKind::Audio)], 2, true),
        make("s2.wav", 5.0, vec![stream(StreamKind::Audio)], 3, true),
    ];
    (project, bed, rest)
}

fn lane(p: &Project, kind: StreamKind, nth: usize) -> Vec<(Uuid, f64)> {
    let t = p.timeline().unwrap();
    let track = t.tracks.iter().filter(|t| t.kind == kind).nth(nth).unwrap();
    track.clips.iter().map(|c| (c.asset_id, c.timeline_start)).collect()
}

#[test]
fn a_long_music_bed_does_not_push_the_picture_out() {
    let (p, bed, rest) = bed_and_footage();
    let done = p
        .assemble_by_tag(AssembleByTag {
            title: GroupTitle::None,
            ..AssembleByTag::new("act")
        })
        .unwrap();
    assert_eq!(
        done.groups[0].start, 0.0,
        "from the start of the picture, not the end of the bed"
    );
    assert_eq!(lane(&p, StreamKind::Video, 0), [(rest[0].id, 0.0)]);
    // The sounds follow the picture's cursor; the bed holds A1 over them, so they get a track of their own.
    assert_eq!(lane(&p, StreamKind::Audio, 0), [(bed.id, 0.0)]);
    assert_eq!(lane(&p, StreamKind::Audio, 1), [(rest[1].id, 10.0), (rest[2].id, 15.0)]);
    assert_eq!(done.groups[0].end, 20.0);
}

#[test]
fn sound_alone_starts_after_what_is_on_the_audio_track() {
    let (p, bed, rest) = bed_and_footage();
    p.remove_asset(rest[0].id, true).unwrap();
    let done = p
        .assemble_by_tag(AssembleByTag {
            title: GroupTitle::None,
            ..AssembleByTag::new("act")
        })
        .unwrap();
    assert_eq!(done.groups[0].start, 60.0, "no picture in the plan: after the bed");
    assert_eq!(
        lane(&p, StreamKind::Audio, 0),
        [(bed.id, 0.0), (rest[1].id, 60.0), (rest[2].id, 65.0)]
    );
    assert!(lane(&p, StreamKind::Video, 0).is_empty());
}

#[test]
fn the_three_steps_are_the_one_call() {
    let (p, _) = trip();
    let req = AssembleByTag::new("location");
    let plan = p.assemble_by_tag_inputs(&req).unwrap();
    let prepared = plan.prepare().unwrap();
    let done = p.apply_assemble_by_tag(prepared).unwrap();
    assert_eq!(done.groups.len(), 3);
    assert_eq!(titles(&p).len(), 3);
}

#[test]
fn a_request_is_read_from_json_with_its_defaults_and_refuses_a_misspelling() {
    let req: AssembleByTag = serde_json::from_str(r#"{"key":"location"}"#).unwrap();
    assert_eq!(req, AssembleByTag::new("location"));
    assert_eq!(
        (req.title, req.title_seconds, req.sort_within),
        (GroupTitle::Overlay, 3.0, SortWithin::Imported)
    );
    let full: AssembleByTag = serde_json::from_str(
        r#"{"key":"rider","order":["Orell"],"filter":[{"key":"location","value":"Cumbuco"},{"value":"jump"}],
            "include_untagged":true,"title":"card","title_seconds":2.5,"sort_within":"name"}"#,
    )
    .unwrap();
    assert_eq!(
        (full.title, full.sort_within, full.filter.len()),
        (GroupTitle::Card, SortWithin::Name, 2)
    );
    let err = serde_json::from_str::<AssembleByTag>(r#"{"key":"x","tilte":"card"}"#).unwrap_err();
    assert!(err.to_string().contains("tilte"), "{err}");
    assert!(serde_json::from_str::<AssembleByTag>(r#"{"key":"x","title":"banner"}"#).is_err());
    let out = serde_json::to_value(AssembleByTag::new("x")).unwrap();
    assert_eq!(out["title"], "overlay");
}

fn make_clip(path: &Path, seconds: u32) -> bool {
    std::process::Command::new(crate::engine::ffmpeg_path())
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc=size=320x180:rate=24:duration={seconds}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"))
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The cut `assemble_by_tag` makes is one the export accepts, titles on a track above or ahead of the footage.
#[test]
#[ignore = "drives the real ffmpeg / ffprobe binaries"]
fn an_assembled_cut_renders_with_its_titles() {
    let dir = std::env::temp_dir().join(format!("kerf-tags-render-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = Project::open_in_memory().unwrap();
    for (name, place) in [("a", "Cumbuco"), ("b", "Jeri"), ("c", "Cumbuco")] {
        let file = dir.join(format!("{name}.mp4"));
        assert!(make_clip(&file, 2), "ffmpeg could not make a test clip");
        let imported = p.import_asset(&file).unwrap();
        p.edit_asset_tags(&[imported.id], &add(&[&format!("location: {place}")]))
            .unwrap();
    }
    for (title, seconds, total) in [(GroupTitle::Overlay, 3.0, 6.0), (GroupTitle::Card, 1.5, 9.0)] {
        let p2 = Project::open_in_memory().unwrap();
        for asset in p.list_assets().unwrap() {
            p2.insert_asset(&asset).unwrap();
        }
        let done = p2
            .assemble_by_tag(AssembleByTag {
                title,
                title_seconds: seconds,
                ..AssembleByTag::new("location")
            })
            .unwrap();
        assert_eq!(done.groups.len(), 2);
        let out = dir.join(format!("{title:?}.mp4"));
        p2.export(&out, "mp4").unwrap();
        let length = Project::probe_asset(&out).unwrap().duration;
        assert!((length - total).abs() < 0.3, "{title:?}: {length} s, wanted about {total}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
