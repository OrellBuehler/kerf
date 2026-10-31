//! Sound effects at the `Project` level: placing them (the `FX` lanes, the hit point on the
//! requested time, one revision per batch), reshaping one with its hit kept, what is refused before
//! anything is rendered or stored, and the staging of an agent's effects. Nothing is rendered or
//! probed here (a stand-in asset of the right length goes in the render's place) except in the
//! `#[ignore]`d test that drives the real engine and ffprobe.

use super::linked_tests::{asset, stream};
use super::*;
use crate::model::{
    sfx_anchor_s, AssetAnalysis, EditSource, SfxDensity, SfxKind, SfxSource, SfxSpec, SfxSuggestRequest, TranscriptSegment,
    Transition, TransitionKind, Word,
};

fn spec(kind: SfxKind) -> SfxSpec {
    SfxSpec::new(kind)
}

fn at(kind: SfxKind, at: f64) -> SfxPlacement {
    SfxPlacement {
        spec: spec(kind),
        at,
        volume_db: None,
        track_id: None,
    }
}

/// An asset standing in for the render of `spec` (nothing is rendered or probed): the same spec is
/// the same path, so the same asset.
fn rendered(spec: &SfxSpec) -> Asset {
    let spec = spec.resolved();
    let mut a = asset(
        &format!("/fx/{}.wav", serde_json::to_string(&spec).unwrap()),
        spec.resolved_duration_s(),
        vec![stream(StreamKind::Audio)],
    );
    a.name = spec.title();
    a.sound_effect = Some(spec);
    a
}

/// Check, "render" and place a batch, the way the three steps do around a real render.
fn add(project: &Project, placements: &[SfxPlacement]) -> Result<Vec<Clip>> {
    let plan = project.sound_effects_inputs(placements)?;
    let assets: Vec<Asset> = plan.effects.iter().map(|e| rendered(&e.spec)).collect();
    project.place_sound_effects(&assets, &plan)
}

fn lanes(project: &Project) -> Vec<(String, usize)> {
    project
        .working_timeline()
        .unwrap()
        .tracks
        .iter()
        .filter(|t| t.name == SFX_TRACK || t.name.starts_with("FX "))
        .map(|t| (t.name.clone(), t.clips.len()))
        .collect()
}

fn lane_of(project: &Project, clip: &Clip) -> String {
    let timeline = project.working_timeline().unwrap();
    let (ti, _) = timeline.locate(clip.id).unwrap();
    timeline.tracks[ti].name.clone()
}

fn labels(project: &Project) -> Vec<String> {
    project.history().unwrap().into_iter().map(|r| r.label).collect()
}

#[test]
fn effects_land_on_the_fx_track_with_their_hit_on_the_requested_time() {
    let project = Project::open_in_memory().unwrap();
    let mut reversed = at(SfxKind::Impact, 30.0);
    reversed.spec.reverse = true;
    let clips = add(
        &project,
        &[
            at(SfxKind::Whoosh, 10.0),
            at(SfxKind::Impact, 4.0),
            at(SfxKind::Riser, 20.0),
            reversed,
        ],
    )
    .unwrap();
    assert_eq!(clips.len(), 4);

    // A whoosh peaks 55 % in, an impact hits at once, a riser builds to its end, a reversed impact
    // ends on its hit: the clip starts that much before the time asked for.
    let want = [
        (10.0 - 0.55, 10.0),
        (4.0 - 0.004, 4.0),
        (20.0 - 3.0, 20.0),
        (30.0 - 1.596, 30.0),
    ];
    for (clip, (start, hit)) in clips.iter().zip(want) {
        assert!((clip.timeline_start - start).abs() < 1e-9, "{clip:?}");
        let asset = project.get_asset(clip.asset_id).unwrap().unwrap();
        let spec = asset.sound_effect.clone().expect("the asset carries its spec");
        assert!((clip.source_to_timeline(sfx_anchor_s(&spec, asset.duration)) - hit).abs() < 1e-9);
        assert_eq!((clip.source_in, clip.source_out), (0.0, asset.duration));
        assert_eq!(clip.volume, 1.0);
        assert_eq!(asset.name, spec.title());
    }
    // One lane, in time order, made once; a single revision for the whole batch.
    let timeline = project.timeline().unwrap();
    let fx = timeline.tracks.iter().find(|t| t.name == SFX_TRACK).expect("FX track");
    assert_eq!(fx.kind, StreamKind::Audio);
    assert_eq!(timeline.tracks.iter().filter(|t| t.name == SFX_TRACK).count(), 1);
    let starts: Vec<f64> = fx.clips.iter().map(|c| c.timeline_start).collect();
    assert!(starts.windows(2).all(|w| w[0] <= w[1]), "{starts:?}");
    assert_eq!(labels(&project)[1..], ["Add sound effects"]);

    // One effect is a singular revision, and the gain is a gain in dB.
    let mut quiet = at(SfxKind::Pop, 40.0);
    quiet.volume_db = Some(-6.0);
    let one = add(&project, &[quiet]).unwrap();
    assert!((one[0].volume - 0.501_187).abs() < 1e-5, "{}", one[0].volume);
    assert_eq!(labels(&project)[1..], ["Add sound effects", "Add sound effect"]);
    assert_eq!(lanes(&project), vec![("FX".to_string(), 5)]);
}

#[test]
fn the_same_spec_is_the_same_asset() {
    let project = Project::open_in_memory().unwrap();
    let clips = add(&project, &[at(SfxKind::Click, 1.0), at(SfxKind::Click, 5.0)]).unwrap();
    assert_eq!(clips[0].asset_id, clips[1].asset_id);
    assert_eq!(project.list_assets().unwrap().len(), 1);
    let mut other = at(SfxKind::Click, 9.0);
    other.spec.seed = 4;
    add(&project, &[other]).unwrap();
    assert_eq!(project.list_assets().unwrap().len(), 2, "another seed is another render");
}

#[test]
fn an_effect_that_overlaps_goes_to_the_next_lane_and_a_free_lane_is_used_again() {
    let project = Project::open_in_memory().unwrap();
    // A riser (7..10 s) and an impact that starts 4 ms before its end: the impact needs its own lane.
    let clips = add(
        &project,
        &[
            at(SfxKind::Riser, 10.0),
            at(SfxKind::Impact, 10.0),
            at(SfxKind::Riser, 10.0),
            at(SfxKind::Thud, 14.0),
        ],
    )
    .unwrap();
    let on: Vec<String> = clips.iter().map(|c| lane_of(&project, c)).collect();
    assert_eq!(on, vec!["FX", "FX 2", "FX 3", "FX"], "the thud fits after the first riser");
    assert_eq!(
        lanes(&project),
        vec![("FX".to_string(), 2), ("FX 2".to_string(), 1), ("FX 3".to_string(), 1)]
    );
    assert!(project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .filter(|t| t.name.starts_with("FX"))
        .all(|t| t.kind == StreamKind::Audio));

    // A later call looks at what the lanes hold now: a whoosh that clears FX 2's impact goes there if
    // FX is taken, else on FX.
    let busy = at(SfxKind::Whoosh, 14.2);
    let later = add(&project, &[busy]).unwrap();
    assert_eq!(lane_of(&project, &later[0]), "FX 2", "FX holds the thud, FX 2 is free there");
    let early = add(&project, &[at(SfxKind::Click, 1.0)]).unwrap();
    assert_eq!(lane_of(&project, &early[0]), "FX");
    assert_eq!(lanes(&project).len(), 3, "no lane is made that a free one could have taken");

    // Touching is not overlapping.
    let timeline = project.timeline().unwrap();
    let fx = timeline.tracks.iter().find(|t| t.name == "FX").unwrap();
    let end = fx.clips.iter().find(|c| c.timeline_start > 10.0).unwrap().timeline_end();
    let touching = add(&project, &[at(SfxKind::Thud, end + 0.003)]).unwrap();
    assert_eq!(lane_of(&project, &touching[0]), "FX");
}

#[test]
fn a_locked_lane_is_skipped_and_the_first_free_name_is_made() {
    let project = Project::open_in_memory().unwrap();
    add(&project, &[at(SfxKind::Whoosh, 5.0)]).unwrap();
    let fx = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.name == SFX_TRACK)
        .unwrap()
        .id;
    project.set_track_locked(fx, true).unwrap();
    let next = add(&project, &[at(SfxKind::Whoosh, 50.0)]).unwrap();
    assert_eq!(lane_of(&project, &next[0]), "FX 2", "FX is the user's now");
    assert_eq!(
        lanes(&project)[0],
        ("FX".to_string(), 1),
        "nothing was put on the locked lane"
    );
    project.set_track_locked(fx, false).unwrap();
    let back = add(&project, &[at(SfxKind::Whoosh, 80.0)]).unwrap();
    assert_eq!(lane_of(&project, &back[0]), "FX");

    // A hole in the numbering is filled, and a video track called FX is no lane.
    let timeline = project.timeline().unwrap();
    let two = timeline.tracks.iter().find(|t| t.name == "FX 2").unwrap().id;
    project.set_track_locked(fx, true).unwrap();
    project.set_track_locked(two, true).unwrap();
    let third = add(&project, &[at(SfxKind::Whoosh, 90.0)]).unwrap();
    assert_eq!(lane_of(&project, &third[0]), "FX 3");
    let video = project.add_track(StreamKind::Video, Some("FX 4".into())).unwrap();
    project
        .set_track_locked(
            project
                .timeline()
                .unwrap()
                .tracks
                .iter()
                .find(|t| t.name == "FX 3")
                .unwrap()
                .id,
            true,
        )
        .unwrap();
    let fourth = add(&project, &[at(SfxKind::Whoosh, 100.0)]).unwrap();
    let timeline = project.timeline().unwrap();
    let (ti, _) = timeline.locate(fourth[0].id).unwrap();
    assert_eq!(timeline.tracks[ti].kind, StreamKind::Audio);
    assert_eq!(timeline.tracks[ti].name, "FX 4", "the first audio name nobody has");
    assert_ne!(timeline.tracks[ti].id, video.id);
}

#[test]
fn a_named_track_must_be_an_unlocked_audio_track_with_room() {
    let project = Project::open_in_memory().unwrap();
    let v = asset("/v.mp4", 60.0, vec![stream(StreamKind::Video)]);
    project.insert_asset(&v).unwrap();
    let picture = project.add_clip_to_timeline(v.id, None, 0.0, 5.0, Some(0.0)).unwrap();
    let timeline = project.timeline().unwrap();
    let video_track = timeline.tracks[timeline.locate(picture.id).unwrap().0].id;
    let named = |track: Uuid, t: f64| SfxPlacement {
        track_id: Some(track),
        ..at(SfxKind::Whoosh, t)
    };

    let e = project
        .sound_effects_inputs(&[named(video_track, 3.0)])
        .unwrap_err()
        .to_string();
    assert!(e.contains("audio track"), "{e}");
    assert!(matches!(
        project.sound_effects_inputs(&[named(Uuid::new_v4(), 3.0)]),
        Err(Error::TrackNotFound(_))
    ));

    let audio = project.add_track(StreamKind::Audio, Some("Sting".into())).unwrap();
    let first = add(&project, &[named(audio.id, 10.0)]).unwrap();
    assert_eq!(lane_of(&project, &first[0]), "Sting", "a named track is used as it is");
    // On top of the first (9.45..10.45): refused with edit_timeline's words, before any render ...
    let e = project
        .with_ripple(Some(false), |p| p.sound_effects_inputs(&[named(audio.id, 10.2)]))
        .unwrap_err()
        .to_string();
    assert!(e.contains("would overlap another clip on Sting"), "{e}");
    // ... and with ripple on, an effect that lands on footage pushes it along.
    let pushed = project
        .with_ripple(Some(true), |p| {
            let plan = p.sound_effects_inputs(&[named(audio.id, 9.6)])?;
            let assets = vec![rendered(&plan.effects[0].spec)];
            p.place_sound_effects(&assets, &plan)
        })
        .unwrap();
    let timeline = project.timeline().unwrap();
    let sting = timeline.tracks.iter().find(|t| t.name == "Sting").unwrap();
    let old = sting.clips.iter().find(|c| c.id == first[0].id).unwrap();
    assert!(
        old.timeline_start > pushed[0].timeline_start + 0.9,
        "pushed behind the new one: {old:?}"
    );

    // A locked track is refused, however it is named, and says so.
    project.set_track_locked(audio.id, true).unwrap();
    let e = project
        .sound_effects_inputs(&[named(audio.id, 40.0)])
        .unwrap_err()
        .to_string();
    assert!(e.contains("is locked"), "{e}");
}

#[test]
fn a_refused_batch_stores_no_asset_makes_no_lane_and_no_revision() {
    let project = Project::open_in_memory().unwrap();
    let audio = project.add_track(StreamKind::Audio, Some("Sting".into())).unwrap();
    add(
        &project,
        &[SfxPlacement {
            track_id: Some(audio.id),
            ..at(SfxKind::Whoosh, 10.0)
        }],
    )
    .unwrap();
    let (assets, history, tracks) = (
        project.list_assets().unwrap().len(),
        project.history().unwrap().len(),
        project.timeline().unwrap().tracks.len(),
    );

    // The first would have made the FX lane, the second runs into the whoosh.
    let batch = [
        at(SfxKind::Impact, 2.0),
        SfxPlacement {
            track_id: Some(audio.id),
            ..at(SfxKind::Pop, 10.0)
        },
    ];
    let e = project
        .with_ripple(Some(false), |p| p.sound_effects_inputs(&batch))
        .unwrap_err()
        .to_string();
    assert!(e.contains("would overlap another clip on Sting"), "{e}");

    // Placed anyway (the plan was made, the lock was let go, another clip arrived), the edit is refused
    // and the assets that were about to be stored are not.
    let free = [at(SfxKind::Impact, 2.0), at(SfxKind::Pop, 60.0)];
    let plan = project.sound_effects_inputs(&free).unwrap();
    let mut late = plan.clone();
    late.effects[1].track_id = Some(audio.id);
    late.effects[1].at = 10.0;
    let rendered: Vec<Asset> = late.effects.iter().map(|e| self::rendered(&e.spec)).collect();
    let e = project
        .with_ripple(Some(false), |p| p.place_sound_effects(&rendered, &late))
        .unwrap_err()
        .to_string();
    assert!(e.contains("would overlap another clip on Sting"), "{e}");
    assert_eq!(project.list_assets().unwrap().len(), assets, "no asset");
    assert!(project.asset_by_path(&rendered[0].path).unwrap().is_none());
    assert_eq!(project.history().unwrap().len(), history, "no revision");
    assert_eq!(project.timeline().unwrap().tracks.len(), tracks, "no FX lane");

    // A render that was not made as an effect, or too few of them, is not placed either.
    let plain = asset("/plain.wav", 1.0, vec![stream(StreamKind::Audio)]);
    assert!(project.place_sound_effects(&[plain], &plan).is_err());
    assert!(project.place_sound_effects(&rendered[..1], &plan).is_err());
    assert_eq!(project.list_assets().unwrap().len(), assets);
}

#[test]
fn an_agents_effects_are_staged_but_their_assets_land_at_once() {
    let mut project = Project::open_in_memory().unwrap();
    project.set_actor(EditSource::Agent);
    let clips = add(&project, &[at(SfxKind::Whoosh, 10.0), at(SfxKind::Impact, 10.0)]).unwrap();
    // The assets are in the library; the placement waits in the proposal.
    for clip in &clips {
        assert!(project.get_asset(clip.asset_id).unwrap().is_some());
    }
    assert!(
        project.timeline().unwrap().tracks.iter().all(|t| !t.name.starts_with("FX")),
        "the live cut is untouched"
    );
    assert_eq!(
        lanes(&project),
        vec![("FX".to_string(), 1), ("FX 2".to_string(), 1)],
        "the agent sees its own staged lanes, and the next effect is placed against them"
    );
    let more = add(&project, &[at(SfxKind::Click, 10.2)]).unwrap();
    assert_eq!(lane_of(&project, &more[0]), "FX 3", "it overlaps both lanes' effects");
    assert_eq!(project.history().unwrap().len(), 1, "nothing was recorded on the live cut");
    project.set_actor(EditSource::User);
    project.apply_staged(true).unwrap();
    assert_eq!(
        project
            .timeline()
            .unwrap()
            .tracks
            .iter()
            .map(|t| t.clips.len())
            .sum::<usize>(),
        3
    );
}

#[test]
fn a_hit_before_the_start_of_the_timeline_is_cut_not_moved() {
    let project = Project::open_in_memory().unwrap();
    let clips = add(&project, &[at(SfxKind::Whoosh, 0.2)]).unwrap();
    let clip = &clips[0];
    assert_eq!(clip.timeline_start, 0.0, "the clip never starts before 0");
    assert!((clip.source_in - 0.35).abs() < 1e-9, "the head is cut instead: {clip:?}");
    assert!((clip.duration() - 0.65).abs() < 1e-9);
    assert!(
        clip.fade_in > 0.0 && clip.fade_in <= 0.004,
        "so the cut does not click: {}",
        clip.fade_in
    );
    let asset = project.get_asset(clip.asset_id).unwrap().unwrap();
    let hit = clip.source_to_timeline(sfx_anchor_s(asset.sound_effect.as_ref().unwrap(), asset.duration));
    assert!((hit - 0.2).abs() < 1e-9, "the hit is where it was asked for");

    // A hit that would leave next to nothing of the sound is refused, and says when it can land.
    let e = project
        .sound_effects_inputs(&[at(SfxKind::Riser, 0.005)])
        .unwrap_err()
        .to_string();
    assert!(e.contains("place it at 0.02 s or later"), "{e}");
    assert!(project.sound_effects_inputs(&[at(SfxKind::Riser, 0.0)]).is_err());
    assert!(project.sound_effects_inputs(&[at(SfxKind::Riser, 0.5)]).is_ok());
    let mut reversed = at(SfxKind::Riser, 0.0);
    reversed.spec.reverse = true;
    project.sound_effects_inputs(&[reversed]).unwrap();
}

/// A whoosh at 10 s on the effects lane, quieter than it came, with a fade-out.
fn whoosh_at_ten(project: &Project) -> Clip {
    let mut p = at(SfxKind::Whoosh, 10.0);
    p.volume_db = Some(-6.0);
    let clip = add(project, &[p]).unwrap().remove(0);
    project.set_fade(clip.id, None, Some(0.2)).unwrap();
    project.timeline().unwrap().clip(clip.id).cloned().unwrap()
}

fn reshape(project: &Project, clip: Uuid, spec: &SfxSpec, volume_db: Option<f64>) -> Result<Clip> {
    let update = project.update_sound_effect_inputs(clip, spec, volume_db)?;
    project.apply_sound_effect_update(&rendered(&update.spec), &update)
}

#[test]
fn reshaping_an_effect_keeps_its_hit_gain_and_fades() {
    let project = Project::open_in_memory().unwrap();
    let old = whoosh_at_ten(&project);
    let before = project.history().unwrap().len();

    // Twice as long, an octave up: the peak moves later in the sound, so the clip starts earlier.
    let longer = SfxSpec {
        duration_s: Some(2.0),
        pitch: 12.0,
        ..spec(SfxKind::Whoosh)
    };
    let update = project.update_sound_effect_inputs(old.id, &longer, None).unwrap();
    assert!((update.anchor_at - 10.0).abs() < 1e-9);
    let new = project.apply_sound_effect_update(&rendered(&update.spec), &update).unwrap();

    assert_eq!(new.id, old.id, "the same clip");
    assert_ne!(new.asset_id, old.asset_id);
    assert!((new.timeline_start - (10.0 - 1.1)).abs() < 1e-9, "{new:?}");
    let asset = project.get_asset(new.asset_id).unwrap().unwrap();
    assert_eq!(asset.sound_effect, Some(longer.resolved()));
    assert!(
        (new.source_to_timeline(sfx_anchor_s(&longer, asset.duration)) - 10.0).abs() < 1e-9,
        "the hit stayed at 10 s"
    );
    assert!((new.volume - old.volume).abs() < 1e-9, "the gain was kept");
    assert_eq!(new.fade_out, 0.2, "and the fade");
    assert_eq!((new.source_in, new.source_out, new.speed), (0.0, asset.duration, 1.0));
    assert_eq!(project.history().unwrap().len(), before + 1, "one revision");
    assert_eq!(labels(&project).last().unwrap(), "Edit sound effect");
    assert_eq!(
        project
            .timeline()
            .unwrap()
            .tracks
            .iter()
            .map(|t| t.clips.len())
            .sum::<usize>(),
        1
    );

    // A gain, when given, replaces the clip's; a riser's end is its hit, so a longer one starts earlier.
    let riser = add(&project, &[at(SfxKind::Riser, 30.0)]).unwrap().remove(0);
    let longer_riser = SfxSpec {
        duration_s: Some(5.0),
        ..spec(SfxKind::Riser)
    };
    let new = reshape(&project, riser.id, &longer_riser, Some(-3.0)).unwrap();
    assert!(
        (new.timeline_start - 25.0).abs() < 1e-9 && (new.timeline_end() - 30.0).abs() < 1e-9,
        "{new:?}"
    );
    assert!((new.volume - 0.707_946).abs() < 1e-5);

    // Unchanged but for the gain: the same render, the same asset, the same place.
    let same = reshape(&project, riser.id, &longer_riser, Some(0.0)).unwrap();
    assert_eq!(same.asset_id, new.asset_id);
    assert_eq!(same.timeline_start, new.timeline_start);
    assert_eq!(same.volume, 1.0);

    // A hit asked for on a trimmed, slowed clip is read through the clip's own timing.
    project.set_speed(riser.id, 0.5).unwrap();
    let slowed = project.timeline().unwrap().clip(riser.id).cloned().unwrap();
    let hit = slowed.source_to_timeline(sfx_anchor_s(&longer_riser, 5.0));
    let back = reshape(&project, riser.id, &spec(SfxKind::Riser), None).unwrap();
    assert!(
        (back.timeline_end() - hit).abs() < 1e-9,
        "the end of the riser stayed on its hit: {back:?} {hit}"
    );
    assert_eq!(back.speed, 1.0);
}

#[test]
fn only_an_unlocked_audio_effect_that_fits_can_be_reshaped() {
    let project = Project::open_in_memory().unwrap();
    let old = whoosh_at_ten(&project);
    let longer = SfxSpec {
        duration_s: Some(3.0),
        ..spec(SfxKind::Whoosh)
    };

    // An ordinary clip, a clip that is not there, a picture.
    let plain = asset("/voice.wav", 8.0, vec![stream(StreamKind::Audio)]);
    project.insert_asset(&plain).unwrap();
    let voice = project.add_clip_to_timeline(plain.id, None, 0.0, 8.0, Some(40.0)).unwrap();
    let e = project
        .update_sound_effect_inputs(voice.id, &longer, None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("not a sound effect"), "{e}");
    assert!(matches!(
        project.update_sound_effect_inputs(Uuid::new_v4(), &longer, None),
        Err(Error::ClipNotFound(_))
    ));
    let v = asset("/v.mp4", 60.0, vec![stream(StreamKind::Video)]);
    project.insert_asset(&v).unwrap();
    let picture = project.add_clip_to_timeline(v.id, None, 0.0, 5.0, Some(0.0)).unwrap();
    let e = project
        .update_sound_effect_inputs(picture.id, &longer, None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("audio track"), "{e}");

    // A bad spec or gain names the field.
    let bad = SfxSpec { tail: 3.0, ..longer };
    assert!(project
        .update_sound_effect_inputs(old.id, &bad, None)
        .unwrap_err()
        .to_string()
        .contains("tail"));
    assert!(project
        .update_sound_effect_inputs(old.id, &longer, Some(40.0))
        .unwrap_err()
        .to_string()
        .contains("volume_db"));

    // A render that is not the edited spec is not swapped in.
    let update = project.update_sound_effect_inputs(old.id, &longer, None).unwrap();
    assert!(project
        .apply_sound_effect_update(&rendered(&spec(SfxKind::Pop)), &update)
        .is_err());

    // A longer render runs into the clip behind it: refused now, ripple off; pushed along, ripple on.
    let fx = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.name == SFX_TRACK)
        .unwrap()
        .id;
    let behind = SfxPlacement {
        track_id: Some(fx),
        ..at(SfxKind::Thud, 10.9)
    };
    let behind = add(&project, &[behind]).unwrap().remove(0);
    let history = project.history().unwrap().len();
    let e = project
        .with_ripple(Some(false), |p| p.update_sound_effect_inputs(old.id, &longer, None))
        .unwrap_err()
        .to_string();
    assert!(e.contains("would overlap another clip on FX"), "{e}");
    let assets = project.list_assets().unwrap().len();
    let planned = SfxUpdate {
        clip_id: old.id,
        spec: longer.resolved(),
        volume_db: None,
        anchor_at: 10.0,
    };
    let e = project
        .with_ripple(Some(false), |p| {
            p.apply_sound_effect_update(&rendered(&planned.spec), &planned)
        })
        .unwrap_err()
        .to_string();
    assert!(e.contains("would overlap another clip on FX"), "{e}");
    assert_eq!(
        project.list_assets().unwrap().len(),
        assets,
        "the refused swap stored nothing"
    );
    assert_eq!(project.history().unwrap().len(), history);
    assert_eq!(project.timeline().unwrap().clip(old.id).unwrap().asset_id, old.asset_id);
    project
        .with_ripple(Some(true), |p| {
            p.apply_sound_effect_update(&rendered(&planned.spec), &planned)
        })
        .unwrap();
    let timeline = project.timeline().unwrap();
    assert!(
        timeline.clip(behind.id).unwrap().timeline_start > behind.timeline_start,
        "pushed along"
    );

    // A locked lane is the user's.
    project.set_track_locked(fx, true).unwrap();
    let e = project
        .update_sound_effect_inputs(old.id, &spec(SfxKind::Whoosh), None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("is locked"), "{e}");
}

#[test]
fn an_agents_reshape_is_staged_and_reads_the_working_timeline() {
    let mut project = Project::open_in_memory().unwrap();
    let live = whoosh_at_ten(&project);
    project.set_actor(EditSource::Agent);
    let update = project
        .update_sound_effect_inputs(
            live.id,
            &SfxSpec {
                duration_s: Some(2.0),
                ..spec(SfxKind::Whoosh)
            },
            None,
        )
        .unwrap();
    project.apply_sound_effect_update(&rendered(&update.spec), &update).unwrap();
    assert_eq!(
        project.timeline().unwrap().clip(live.id).unwrap().asset_id,
        live.asset_id,
        "the live cut is untouched"
    );
    let staged = project.working_timeline().unwrap().clip(live.id).cloned().unwrap();
    assert_ne!(staged.asset_id, live.asset_id);
    // A second reshape in the same proposal starts from the first: the hit is still at 10 s.
    let again = project
        .update_sound_effect_inputs(
            live.id,
            &SfxSpec {
                duration_s: Some(1.5),
                ..spec(SfxKind::Whoosh)
            },
            None,
        )
        .unwrap();
    assert!((again.anchor_at - 10.0).abs() < 1e-9, "{}", again.anchor_at);
}

#[test]
fn a_request_is_checked_before_anything_is_rendered() {
    let project = Project::open_in_memory().unwrap();
    let err = |p: SfxPlacement| {
        project
            .sound_effects_inputs(&[at(SfxKind::Click, 1.0), p])
            .unwrap_err()
            .to_string()
    };
    let mut p = at(SfxKind::Impact, 3.0);
    p.spec.pitch = 20.0;
    let e = err(p);
    assert!(e.contains("effects[1] (impact)") && e.contains("pitch"), "{e}");
    assert_eq!(e.matches("invalid argument").count(), 1, "one prefix, not two: {e}");
    for bad in [f64::NAN, -1.0, f64::INFINITY] {
        assert!(err(at(SfxKind::Pop, bad)).contains("at must be"), "{bad}");
    }
    for db in [f64::NAN, 13.0, -61.0] {
        let mut p = at(SfxKind::Pop, 1.0);
        p.volume_db = Some(db);
        assert!(err(p).contains("volume_db"), "{db}");
    }
    let mut edge = at(SfxKind::Pop, 1.0);
    edge.volume_db = Some(12.0);
    edge.spec.seed = crate::model::MAX_SEED;
    project.sound_effects_inputs(&[edge]).unwrap();
    assert!(project
        .sound_effects_inputs(&[])
        .unwrap_err()
        .to_string()
        .contains("no sound effects"));
    let many: Vec<SfxPlacement> = (0..MAX_SFX_PER_CALL + 1).map(|i| at(SfxKind::Click, i as f64)).collect();
    assert!(project
        .sound_effects_inputs(&many)
        .unwrap_err()
        .to_string()
        .contains("at most"));
    let all: Vec<SfxPlacement> = (0..MAX_SFX_PER_CALL).map(|i| at(SfxKind::Click, 2.0 * i as f64)).collect();
    assert_eq!(project.sound_effects_inputs(&all).unwrap().effects.len(), MAX_SFX_PER_CALL);

    // The plan spells everything out and changes nothing: no lane, no asset, no revision.
    let plan = project.sound_effects_inputs(&[at(SfxKind::Whoosh, 3.0)]).unwrap();
    assert_eq!(plan.effects[0].spec.duration_s, Some(1.0));
    assert_eq!(plan.effects[0].volume_db, 0.0);
    assert!(project.timeline().unwrap().tracks.iter().all(|t| !t.name.starts_with("FX")));
    assert!(project.list_assets().unwrap().is_empty());
    assert_eq!(project.history().unwrap().len(), 1, "only the baseline");
}

#[test]
fn a_placement_reads_flat_and_a_misspelled_field_is_an_error_not_a_default() {
    let p: SfxPlacement = serde_json::from_str(r#"{"kind":"whoosh","at":12.5}"#).unwrap();
    assert_eq!(p, at(SfxKind::Whoosh, 12.5));
    let track = Uuid::new_v4();
    let full: SfxPlacement = serde_json::from_str(&format!(
        r#"{{"kind":"riser","at":8,"duration_s":4,"pitch":-2,"intensity":0.4,"tail":0.8,"reverse":true,"seed":7,"volume_db":-6,"track_id":"{track}"}}"#
    ))
    .unwrap();
    assert_eq!(full.spec.duration_s, Some(4.0));
    assert_eq!((full.spec.pitch, full.spec.reverse, full.spec.seed), (-2.0, true, 7));
    assert_eq!((full.at, full.volume_db, full.track_id), (8.0, Some(-6.0), Some(track)));
    // What is written reads back, and is flat.
    let json = serde_json::to_value(&full).unwrap();
    assert_eq!(json["kind"], "riser");
    assert_eq!(json["at"], 8.0);
    assert!(json.get("spec").is_none(), "{json}");
    assert_eq!(serde_json::from_value::<SfxPlacement>(json).unwrap(), full);

    for (json, field) in [
        (r#"{"kind":"whoosh","at":1,"pitc":2}"#, "pitc"),
        (r#"{"kind":"whoosh","at":1,"volume":2}"#, "volume"),
        (r#"{"kind":"whoosh","at":1,"track":"x"}"#, "track"),
        (r#"{"kind":"whoosh","at":1,"spec":{}}"#, "spec"),
    ] {
        let e = serde_json::from_str::<SfxPlacement>(json).unwrap_err().to_string();
        assert!(e.contains(&format!("unknown field `{field}`")), "{json}: {e}");
    }
    let e = serde_json::from_str::<SfxPlacement>(r#"{"kind":"whoosh"}"#)
        .unwrap_err()
        .to_string();
    assert!(e.contains("missing field `at`"), "{e}");
    assert!(
        serde_json::from_str::<SfxPlacement>(r#"{"at":1}"#).is_err(),
        "a kind is required"
    );
    assert!(serde_json::from_str::<SfxPlacement>(r#"{"kind":"zap","at":1}"#).is_err());

    // The schema a tool carries lists the spec's fields beside the placement's.
    let schema = serde_json::to_value(schemars::schema_for!(SfxPlacement)).unwrap();
    let props = schema["properties"]
        .as_object()
        .expect("properties")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for key in [
        "kind",
        "duration_s",
        "pitch",
        "intensity",
        "tail",
        "reverse",
        "seed",
        "at",
        "volume_db",
        "track_id",
    ] {
        assert!(props.iter().any(|p| p == key), "{key} missing from {props:?}");
    }
}

/// Drives the real engine and ffprobe: effects are rendered into the data directory, probed, placed,
/// auditioned and reshaped. `cargo test -p kerf-core --no-default-features -- --ignored sound_effects`.
#[test]
#[ignore]
fn sound_effects_are_rendered_probed_placed_and_reshaped_for_real() {
    let project = Project::open_in_memory().unwrap();
    let mut hard = at(SfxKind::Impact, 12.0);
    hard.spec.seed = 90_210;
    let plan = project
        .sound_effects_inputs(&[at(SfxKind::Whoosh, 5.0), hard, at(SfxKind::Whoosh, 40.0)])
        .unwrap();
    let assets = Project::synthesize_sound_effects(&plan, &|| false).unwrap();
    assert_eq!(assets[0].id, assets[2].id, "the same spec is rendered and probed once");
    for (asset, effect) in assets.iter().zip(&plan.effects) {
        assert!(std::path::Path::new(&asset.path).exists());
        assert!(
            (asset.duration - effect.spec.resolved_duration_s()).abs() < 0.002,
            "{asset:?}"
        );
        assert_eq!(asset.sound_effect.as_ref(), Some(&effect.spec));
        let audio = asset
            .streams
            .iter()
            .find(|s| s.kind == StreamKind::Audio)
            .expect("an audio stream");
        assert_eq!((audio.sample_rate, audio.channels), (Some(48_000), Some(2)));
    }
    let clips = project.place_sound_effects(&assets, &plan).unwrap();
    assert_eq!(clips.len(), 3);
    let preview = Project::preview_sound_effect(&plan.effects[0].spec, &|| false).unwrap();
    assert_eq!(
        preview.to_string_lossy(),
        assets[0].path,
        "an audition is the file the effect would use"
    );

    let new = project
        .update_sound_effect(
            clips[1].id,
            &SfxSpec {
                duration_s: Some(2.5),
                intensity: 1.0,
                ..spec(SfxKind::Impact)
            },
            Some(-3.0),
        )
        .unwrap();
    assert!(
        (new.source_to_timeline(0.004) - 12.0).abs() < 0.01,
        "the hit stayed on 12 s: {new:?}"
    );
    for asset in project.list_assets().unwrap() {
        std::fs::remove_file(&asset.path).ok();
    }
}

// ---- suggestions -----------------------------------------------------------------------------

/// A project cut from three 60 s shots on V1 (a cut at 5 s, a dissolve into the third at 12 s), a title, and
/// markers: one on a riser too close to the start of the timeline and one for a pop.
fn suggesting_project() -> Project {
    let project = Project::open_in_memory().unwrap();
    let make = |name: &str| {
        let mut a = asset(
            &format!("/{name}.mp4"),
            60.0,
            vec![stream(StreamKind::Video), stream(StreamKind::Audio)],
        );
        a.name = format!("{name}.mp4");
        project.insert_asset(&a).unwrap();
        a
    };
    let (a, b, c) = (make("interview"), make("broll"), make("outro"));
    project.add_clip_to_timeline(a.id, None, 0.0, 5.0, Some(0.0)).unwrap();
    project.add_clip_to_timeline(b.id, None, 0.0, 7.0, Some(5.0)).unwrap();
    let third = project.add_clip_to_timeline(c.id, None, 0.0, 8.0, Some(12.0)).unwrap();
    project
        .set_transition(
            third.id,
            Some(Transition {
                kind: TransitionKind::Crossfade,
                duration: 1.0,
            }),
        )
        .unwrap();
    project.add_overlay("Jane Doe".to_string(), 6.0, 9.0).unwrap();
    project.add_marker(0.01, "sfx:riser".to_string(), None).unwrap();
    project.add_marker(16.0, "sfx:pop".to_string(), None).unwrap();
    project
}

fn punchy() -> SfxSuggestRequest {
    SfxSuggestRequest {
        density: SfxDensity::Punchy,
        ..SfxSuggestRequest::default()
    }
}

#[test]
fn suggestions_are_placements_add_sound_effects_takes_as_they_are() {
    let project = suggesting_project();
    let history = project.history().unwrap().len();
    let suggestions = project.suggest_sound_effects(&punchy()).unwrap();
    let sources: Vec<SfxSource> = suggestions.iter().map(|s| s.source).collect();
    assert_eq!(
        sources,
        [
            SfxSource::Opening,
            SfxSource::Cut,
            SfxSource::Title,
            SfxSource::Transition,
            SfxSource::Marker
        ],
        "{suggestions:#?}"
    );
    assert_eq!(project.history().unwrap().len(), history, "asking is a read: no revision");

    // Every placement is accepted on the same timeline, whatever lanes they have to spread over, with
    // ripple on or off.
    let placements: Vec<SfxPlacement> = suggestions.iter().map(|s| s.placement.clone()).collect();
    for ripple in [false, true] {
        project
            .with_ripple(Some(ripple), |p| p.sound_effects_inputs(&placements))
            .unwrap_or_else(|e| panic!("ripple {ripple}: {e}"));
    }
    let clips = add(&project, &placements).unwrap();
    assert_eq!(clips.len(), suggestions.len());
    for (clip, s) in clips.iter().zip(&suggestions) {
        let asset = project.get_asset(clip.asset_id).unwrap().unwrap();
        let hit = clip.source_to_timeline(sfx_anchor_s(asset.sound_effect.as_ref().unwrap(), asset.duration));
        assert!((hit - s.placement.at).abs() < 1e-9, "{s:?} landed at {hit}");
    }
    assert_eq!(labels(&project).last().unwrap(), "Add sound effects");

    // What is covered now is not suggested again.
    let timeline = project.working_timeline().unwrap();
    let covered: Vec<(f64, f64)> = timeline
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter(|c| {
            project
                .get_asset(c.asset_id)
                .unwrap()
                .is_some_and(|a| a.sound_effect.is_some())
        })
        .map(|c| (c.timeline_start, c.timeline_end()))
        .collect();
    assert_eq!(covered.len(), clips.len());
    let again = project.suggest_sound_effects(&punchy()).unwrap();
    for s in &again {
        assert!(
            covered
                .iter()
                .all(|&(start, end)| s.placement.at < start - 0.25 || s.placement.at > end + 0.25),
            "{s:?} is already covered"
        );
    }
    assert!(again.len() < suggestions.len());
    // ... and the second batch places too.
    if !again.is_empty() {
        add(&project, &again.iter().map(|s| s.placement.clone()).collect::<Vec<_>>()).unwrap();
    }
}

#[test]
fn a_riser_marker_too_close_to_the_start_is_not_suggested_and_one_with_room_is() {
    let project = suggesting_project();
    let times: Vec<(SfxKind, f64)> = project
        .suggest_sound_effects(&punchy())
        .unwrap()
        .iter()
        .map(|s| (s.placement.spec.kind, s.placement.at))
        .collect();
    assert!(!times.contains(&(SfxKind::Riser, 0.01)), "{times:?}");
    assert!(
        project.sound_effects_inputs(&[at(SfxKind::Riser, 0.01)]).is_err(),
        "and the placement would refuse it"
    );
    project.add_marker(0.5, "sfx:riser".to_string(), None).unwrap();
    let suggestions = project.suggest_sound_effects(&punchy()).unwrap();
    let placements: Vec<SfxPlacement> = suggestions.iter().map(|s| s.placement.clone()).collect();
    assert!(placements.iter().any(|p| p.spec.kind == SfxKind::Riser && p.at == 0.5));
    let clips = add(&project, &placements).unwrap();
    let riser = clips
        .iter()
        .find(|c| c.timeline_start == 0.0 && c.source_in > 0.0)
        .expect("the riser is cut at 0");
    assert!((riser.duration() - 0.5).abs() < 1e-9, "{riser:?}");
}

#[test]
fn suggestions_read_the_working_timeline_and_the_cached_transcript() {
    let mut project = suggesting_project();
    let timeline = project.timeline().unwrap();
    let interview = project.get_asset(timeline.tracks[0].clips[0].asset_id).unwrap().unwrap();
    let words = vec![
        Word {
            text: " and".to_string(),
            start: 4.6,
            end: 4.8,
            ..Word::default()
        },
        Word {
            text: " then".to_string(),
            start: 4.85,
            end: 5.3,
            ..Word::default()
        },
    ];
    project
        .set_analysis(&AssetAnalysis {
            asset_id: interview.id,
            transcript: vec![TranscriptSegment {
                start: 4.6,
                end: 5.3,
                text: "and then".to_string(),
                words,
                speaker: None,
            }],
            ..AssetAnalysis::default()
        })
        .unwrap();
    let gain_at = |project: &Project, t: f64| {
        project
            .suggest_sound_effects(&punchy())
            .unwrap()
            .into_iter()
            .find(|s| s.placement.at == t)
            .map(|s| s.placement.volume_db)
    };
    assert_eq!(gain_at(&project, 5.0), Some(Some(-8.0)), "the cut at 5 s is under the voice");
    assert_eq!(gain_at(&project, 16.0), Some(Some(-3.0)));

    // An agent's staged cut is what an agent is suggested for; the live cut is what the user is.
    project.set_actor(EditSource::Agent);
    let outro = project
        .list_assets()
        .unwrap()
        .into_iter()
        .find(|a| a.name == "outro.mp4")
        .unwrap();
    project.add_clip_to_timeline(outro.id, None, 20.0, 30.0, Some(20.0)).unwrap();
    project.add_marker(22.0, "sfx:ding".to_string(), None).unwrap();
    let staged: Vec<f64> = project
        .suggest_sound_effects(&punchy())
        .unwrap()
        .iter()
        .map(|s| s.placement.at)
        .collect();
    assert!(staged.contains(&20.0) && staged.contains(&22.0), "{staged:?}");
    project.set_actor(EditSource::User);
    let live: Vec<f64> = project
        .suggest_sound_effects(&punchy())
        .unwrap()
        .iter()
        .map(|s| s.placement.at)
        .collect();
    assert!(!live.contains(&20.0) && !live.contains(&22.0), "{live:?}");
}

#[test]
#[ignore = "renders with the real engine and probes with ffprobe"]
fn suggested_effects_are_rendered_and_placed_for_real() {
    let project = suggesting_project();
    let suggestions = project.suggest_sound_effects(&punchy()).unwrap();
    let placements: Vec<SfxPlacement> = suggestions.iter().map(|s| s.placement.clone()).collect();
    let clips = project.add_sound_effects(&placements).unwrap();
    assert_eq!(clips.len(), suggestions.len());
    for (clip, s) in clips.iter().zip(&suggestions) {
        let asset = project.get_asset(clip.asset_id).unwrap().unwrap();
        let hit = clip.source_to_timeline(sfx_anchor_s(asset.sound_effect.as_ref().unwrap(), asset.duration));
        assert!((hit - s.placement.at).abs() < 0.01, "{s:?} landed at {hit}");
    }
    for asset in project.list_assets().unwrap().iter().filter(|a| a.sound_effect.is_some()) {
        std::fs::remove_file(&asset.path).ok();
    }
}
