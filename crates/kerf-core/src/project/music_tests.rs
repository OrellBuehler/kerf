//! Composed music at the `Project` level: placing it (the `M` track, the analysis written from
//! the score), replacing a clip's music after the score is edited, fitting a score to the picture,
//! the `music_score` column, and resolving a band-balance request.

use super::linked_tests::{asset, stream};
use super::*;
use crate::engine::BandWindow;
use crate::model::{AnalysisKind, EditSource, HitKind, MusicScore, MusicStyle, ScoreSection, SectionKind};

fn score(bpm: f64, bars: u32) -> MusicScore {
    MusicScore {
        bpm,
        key: "Am".into(),
        style: MusicStyle::Pop,
        sections: vec![
            ScoreSection {
                kind: SectionKind::Intro,
                bars: 2,
                chords: vec!["i".into()],
                energy: None,
            },
            ScoreSection {
                kind: SectionKind::Drop,
                bars,
                chords: vec!["i".into(), "VI".into()],
                energy: None,
            },
        ],
        hits: vec![],
        seed: 1,
        instruments: None,
    }
}

/// An asset standing in for the render of `score` (nothing is rendered or probed).
fn composed(path: &str, score: &MusicScore) -> Asset {
    let mut a = asset(path, score.duration_s(), vec![stream(StreamKind::Audio)]);
    a.name = format!("Music: {}", score.title());
    a.music_score = Some(score.clone());
    a
}

fn video_asset(project: &Project) -> Asset {
    let a = asset("/v.mp4", 60.0, vec![stream(StreamKind::Video)]);
    project.insert_asset(&a).unwrap();
    a
}

#[test]
fn music_lands_on_its_own_track_and_the_analysis_comes_from_the_score() {
    let project = Project::open_in_memory().unwrap();
    let s = score(120.0, 8);
    let (asset, clip) = project.place_music(&composed("/m/a.wav", &s), None, Some(2.0), None).unwrap();

    let timeline = project.timeline().unwrap();
    let track = timeline.tracks.iter().find(|t| t.name == MUSIC_TRACK).expect("M track");
    assert_eq!(track.kind, StreamKind::Audio);
    assert_eq!(track.clips.len(), 1);
    assert_eq!(clip.timeline_start, 2.0);
    assert_eq!(clip.source_out, asset.duration);
    assert_eq!(project.history().unwrap().last().unwrap().label, "Add music");

    // The tempo and bars are there at once, exact, with nothing analysed.
    let analysis = project.get_analysis(asset.id).unwrap().expect("analysis written");
    assert!(analysis.done(AnalysisKind::Rhythm));
    let tempo = analysis.tempo.as_ref().unwrap();
    assert_eq!(tempo.bpm, 120.0);
    assert!(tempo.confidence >= 1.0 - 1e-9);
    assert_eq!(tempo.downbeats.len(), 11, "ten bars and the one the tail runs into");
    let music = analysis.music.as_ref().unwrap();
    assert_eq!(music.grid.whole_bars(asset.duration), 10);
    assert_eq!(music.bar_chroma.len(), 10);
    // ... which is what fit_music needs: it plans against the score's own grid (here the
    // 8-bar drop repeats its two chords, so a phrase can be left out).
    let fit = project.plan_music_fit(clip.id, Some(14.0)).unwrap();
    assert_eq!(fit.bars, 10, "{fit:?}");
    assert!(project.plan_music_fit(clip.id, None).is_err(), "no picture to fit to");

    // A second piece goes after the first on the same track, which is not made twice.
    let (_, second) = project
        .place_music(&composed("/m/b.wav", &score(100.0, 4)), None, None, None)
        .unwrap();
    assert!((second.timeline_start - clip.timeline_end()).abs() < 1e-9);
    let timeline = project.timeline().unwrap();
    assert_eq!(timeline.tracks.iter().filter(|t| t.name == MUSIC_TRACK).count(), 1);
}

#[test]
fn the_same_score_is_the_same_asset() {
    let project = Project::open_in_memory().unwrap();
    let s = score(120.0, 4);
    let (a, _) = project.place_music(&composed("/m/a.wav", &s), None, None, None).unwrap();
    let (b, _) = project.place_music(&composed("/m/a.wav", &s), None, None, None).unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(project.list_assets().unwrap().len(), 1);
}

#[test]
fn music_wants_an_audio_track_and_a_composed_asset() {
    let project = Project::open_in_memory().unwrap();
    let v = video_asset(&project);
    let video_track = project.add_clip_to_timeline(v.id, None, 0.0, 5.0, Some(0.0)).unwrap();
    let tl = project.timeline().unwrap();
    let (ti, _) = tl.locate(video_track.id).unwrap();
    let e = project
        .place_music(&composed("/m/a.wav", &score(120.0, 4)), Some(tl.tracks[ti].id), None, None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("audio track"), "{e}");
    assert!(project
        .place_music(&asset("/x.wav", 5.0, vec![stream(StreamKind::Audio)]), None, None, None)
        .is_err());
    assert!(project
        .place_music(&composed("/m/a.wav", &score(120.0, 4)), Some(Uuid::new_v4()), None, None)
        .is_err());
}

#[test]
fn an_agents_music_is_staged_but_its_asset_lands_at_once() {
    let mut project = Project::open_in_memory().unwrap();
    project.set_actor(EditSource::Agent);
    let (asset, _) = project
        .place_music(&composed("/m/a.wav", &score(120.0, 4)), None, None, None)
        .unwrap();
    // The asset and its analysis are in the library; the placement waits in the proposal.
    assert!(project.get_analysis(asset.id).unwrap().is_some());
    assert!(project.timeline().unwrap().tracks.iter().all(|t| t.name != MUSIC_TRACK));
    assert!(project
        .working_timeline()
        .unwrap()
        .tracks
        .iter()
        .any(|t| t.name == MUSIC_TRACK));
}

#[test]
fn replacing_a_clips_music_keeps_its_place_gain_and_fades() {
    let project = Project::open_in_memory().unwrap();
    let old = score(120.0, 8);
    let (_, clip) = project
        .place_music(&composed("/m/a.wav", &old), None, Some(5.0), None)
        .unwrap();
    project.set_volume(clip.id, 0.6).unwrap();
    let before = project.history().unwrap().len();

    // The edited score is longer and at another tempo: another file, another asset.
    let edited = score(100.0, 12);
    let new_asset = composed("/m/b.wav", &edited);
    let (stored, replaced) = project.place_music(&new_asset, None, None, Some(clip.id)).unwrap();

    assert_eq!(replaced.id, clip.id, "the same clip");
    assert_eq!(replaced.asset_id, stored.id);
    assert_ne!(stored.id, clip.asset_id);
    let now = project.timeline().unwrap().clip(clip.id).cloned().unwrap();
    assert_eq!(now.timeline_start, 5.0);
    assert!((now.volume - 0.6).abs() < 1e-6);
    assert_eq!(now.source_in, 0.0);
    assert!(
        (now.duration() - stored.duration).abs() < 1e-9,
        "the clip is the new render's length"
    );
    assert_eq!(project.history().unwrap().len(), before + 1, "one revision");
    assert_eq!(project.history().unwrap().last().unwrap().label, "Recompose music");
    // The new asset's analysis is its own score's.
    let tempo = project.get_analysis(stored.id).unwrap().unwrap().tempo.unwrap();
    assert_eq!(tempo.bpm, 100.0);
    assert_eq!(project.timeline().unwrap().tracks.iter().flat_map(|t| &t.clips).count(), 1);
}

#[test]
fn only_music_can_be_replaced_and_not_into_the_next_clip() {
    let project = Project::open_in_memory().unwrap();
    let (_, clip) = project
        .place_music(&composed("/m/a.wav", &score(120.0, 4)), None, Some(0.0), None)
        .unwrap();
    // Another clip right after it on the track.
    let other = asset("/other.wav", 4.0, vec![stream(StreamKind::Audio)]);
    project.insert_asset(&other).unwrap();
    let tl = project.timeline().unwrap();
    let track = tl.tracks.iter().find(|t| t.name == MUSIC_TRACK).unwrap().id;
    let next = project
        .add_clip_to_timeline(other.id, Some(track), 0.0, 4.0, Some(clip.timeline_end()))
        .unwrap();

    // A longer render would run into it.
    let longer = composed("/m/long.wav", &score(120.0, 30));
    let e = project
        .place_music(&longer, None, None, Some(clip.id))
        .unwrap_err()
        .to_string();
    assert!(
        e.to_lowercase().contains("overlap") || e.contains("another clip") || e.contains("on top"),
        "{e}"
    );
    // The failed edit changed nothing.
    assert_eq!(project.timeline().unwrap().clip(clip.id).unwrap().asset_id, clip.asset_id);

    // A clip of ordinary media is not music to recompose.
    let e = project
        .place_music(&composed("/m/c.wav", &score(110.0, 4)), None, None, Some(next.id))
        .unwrap_err()
        .to_string();
    assert!(e.contains("not composed music"), "{e}");
    assert!(project
        .place_music(&composed("/m/c.wav", &score(110.0, 4)), None, None, Some(Uuid::new_v4()))
        .is_err());
}

/// A project with a video track of three 4 s shots (cuts at 4 and 8 s) and no music.
fn picture() -> (Project, Uuid) {
    let project = Project::open_in_memory().unwrap();
    let v = video_asset(&project);
    let mut track = None;
    for k in 0..3 {
        let c = project
            .add_clip_to_timeline(v.id, track, 0.0, 4.0, Some(4.0 * k as f64))
            .unwrap();
        track = project
            .timeline()
            .unwrap()
            .tracks
            .iter()
            .find(|t| t.clips.iter().any(|x| x.id == c.id))
            .map(|t| t.id);
    }
    (project, track.unwrap())
}

#[test]
fn a_score_is_fitted_to_the_cuts_of_the_working_timeline() {
    let (project, video) = picture();
    // 2 s bars: the cuts at 4 and 8 s are downbeats already.
    let req = ComposeRequest {
        score: score(120.0, 8),
        track_id: None,
        at: None,
        align: Some(ScoreAlign {
            to: AlignTo::Cuts,
            track_id: None,
            max_tempo_change: None,
        }),
        replace_clip_id: None,
    };
    let plan = project.compose_music_inputs(&req).unwrap();
    assert_eq!(plan.music_start, 0.0);
    assert_eq!(plan.at, None, "the caller's own, resolved when the music is placed");
    assert!(plan.replaces.is_empty());
    let report = plan.align.as_ref().unwrap();
    assert_eq!(report.cuts, 2);
    assert_eq!(report.on_downbeat_after, 2);
    assert_eq!(plan.score.bpm, 120.0);
    let impacts: Vec<f64> = plan
        .score
        .hits
        .iter()
        .filter(|h| h.kind == HitKind::Impact)
        .map(|h| h.at_s)
        .collect();
    assert_eq!(impacts, vec![4.0, 8.0]);

    // Music that starts at 1 s sees the cuts a second earlier in its own time; at 120 BPM they are
    // then 1 s off the bars, so the tempo is nudged (within 6 %) and the hits follow.
    let mut late = req.clone();
    late.at = Some(1.0);
    let plan = project.compose_music_inputs(&late).unwrap();
    assert_eq!(plan.music_start, 1.0);
    assert_eq!(plan.at, Some(1.0));
    assert!(plan.align.as_ref().unwrap().on_beat_after >= plan.align.as_ref().unwrap().on_beat_before);

    // An explicit video track, and the guards.
    let mut explicit = req.clone();
    explicit.align = Some(ScoreAlign {
        to: AlignTo::Cuts,
        track_id: Some(video),
        max_tempo_change: Some(0.0),
    });
    assert_eq!(project.compose_music_inputs(&explicit).unwrap().score.bpm, 120.0);
    let mut bad = req.clone();
    bad.align = Some(ScoreAlign {
        to: AlignTo::Cuts,
        track_id: None,
        max_tempo_change: Some(0.9),
    });
    assert!(project
        .compose_music_inputs(&bad)
        .unwrap_err()
        .to_string()
        .contains("max_tempo_change"));
    let mut markers = req.clone();
    markers.align = Some(ScoreAlign {
        to: AlignTo::Markers,
        track_id: None,
        max_tempo_change: None,
    });
    assert!(project
        .compose_music_inputs(&markers)
        .unwrap_err()
        .to_string()
        .contains("no markers"));
    let mut no_at = req.clone();
    no_at.at = Some(-1.0);
    assert!(project.compose_music_inputs(&no_at).is_err());
    let mut invalid = req;
    invalid.score.sections[0].chords = vec!["Hm".into()];
    assert!(project
        .compose_music_inputs(&invalid)
        .unwrap_err()
        .to_string()
        .contains("sections[0]"));
}

#[test]
fn the_plan_starts_where_the_music_will_be_placed() {
    let (project, _) = picture();
    // The M track exists and holds 6 s: new music starts after it unless told where.
    let s = score(120.0, 1);
    let (_, clip) = project.place_music(&composed("/m/a.wav", &s), None, Some(0.0), None).unwrap();
    let req = ComposeRequest {
        score: s,
        track_id: None,
        at: None,
        align: None,
        replace_clip_id: None,
    };
    let plan = project.compose_music_inputs(&req).unwrap();
    assert!((plan.music_start - clip.timeline_end()).abs() < 1e-9);
    // Replacing a clip starts at that clip.
    let mut replace = req;
    replace.replace_clip_id = Some(clip.id);
    assert_eq!(
        project.compose_music_inputs(&replace).unwrap().music_start,
        clip.timeline_start
    );
}

#[test]
fn an_asset_remembers_its_score_across_a_save_and_a_reopen() {
    let project = Project::open_in_memory().unwrap();
    let s = score(96.0, 4);
    let a = composed("/m/a.wav", &s);
    project.insert_asset(&a).unwrap();
    let path = std::env::temp_dir().join(format!("kerf-music-{}.kerf", Uuid::new_v4()));
    project.save_as(&path).unwrap();
    let Some(loaded) = Project::open(&path)
        .ok()
        .and_then(|p| p.list_assets().ok())
        .and_then(|a| a.into_iter().next())
    else {
        panic!("the reopened project lists the asset");
    };
    std::fs::remove_file(&path).ok();
    assert_eq!(loaded.music_score, Some(s));
    // An asset that was not composed has none, and its JSON omits the key.
    let plain = asset("/x.wav", 3.0, vec![stream(StreamKind::Audio)]);
    assert!(!serde_json::to_string(&plain).unwrap().contains("music_score"));
}

fn audio_project() -> (Project, Asset) {
    let project = Project::open_in_memory().unwrap();
    let a = composed("/m/a.wav", &score(120.0, 6));
    project.insert_asset(&a).unwrap();
    (project, a)
}

fn req_asset(id: Uuid) -> BandBalanceRequest {
    BandBalanceRequest {
        asset_id: Some(id),
        ..BandBalanceRequest::default()
    }
}

#[test]
fn a_composed_assets_balance_is_measured_per_section() {
    let (project, a) = audio_project();
    let plan = project.band_balance_inputs(&req_asset(a.id)).unwrap();
    assert_eq!(plan.target, "asset");
    assert_eq!(plan.windows_from, "score");
    let labels: Vec<_> = plan.windows.iter().map(|w| w.label.clone().unwrap()).collect();
    assert_eq!(labels, vec!["intro", "drop"]);
    assert_eq!((plan.windows[0].start, plan.windows[0].end), (0.0, 4.0));
    assert_eq!((plan.windows[1].start, plan.windows[1].end), (4.0, 16.0));
    assert_eq!(plan.origin, 0.0);
    assert!((plan.span - a.duration).abs() < 1e-9);
}

#[test]
fn explicit_sections_win_and_an_unanalysed_asset_is_cut_in_eight() {
    let (project, a) = audio_project();
    let mut req = req_asset(a.id);
    req.sections = Some(vec![BandWindow {
        start: 1.0,
        end: 5.0,
        label: Some("mine".into()),
    }]);
    let plan = project.band_balance_inputs(&req).unwrap();
    assert_eq!(plan.windows_from, "sections");
    assert_eq!(plan.windows[0].label.as_deref(), Some("mine"));

    let plain = asset("/p.wav", 16.0, vec![stream(StreamKind::Audio)]);
    project.insert_asset(&plain).unwrap();
    let plan = project.band_balance_inputs(&req_asset(plain.id)).unwrap();
    assert_eq!(plan.windows_from, "equal");
    assert_eq!(plan.windows.len(), 8);

    // With a bar grid analysed, the windows are its phrases.
    let mut analysis = AssetAnalysis {
        asset_id: plain.id,
        ..AssetAnalysis::default()
    };
    analysis.music = Some(score(120.0, 6).music_analysis(16.0).unwrap());
    analysis.ran = vec![AnalysisKind::Rhythm];
    project.set_analysis(&analysis).unwrap();
    let plan = project.band_balance_inputs(&req_asset(plain.id)).unwrap();
    assert_eq!(plan.windows_from, "phrases");
    assert_eq!(plan.windows[0].label.as_deref(), Some("bars 1–4"));
}

#[test]
fn a_clip_is_measured_over_its_source_range_at_its_gain() {
    let (project, a) = audio_project();
    let tl_asset = project.insert_or_get_asset(&a).unwrap();
    let clip = project.add_clip_to_timeline(tl_asset.id, None, 4.0, 10.0, Some(0.0)).unwrap();
    project.set_volume(clip.id, 0.5).unwrap();
    let plan = project
        .band_balance_inputs(&BandBalanceRequest {
            clip_id: Some(clip.id),
            ..BandBalanceRequest::default()
        })
        .unwrap();
    assert_eq!(plan.target, "clip");
    assert_eq!((plan.origin, plan.span), (4.0, 6.0));
    match &plan.source {
        BandSource::File { start, secs, gain, .. } => {
            assert_eq!((*start, *secs), (4.0, 6.0));
            assert!((*gain - 0.5).abs() < 1e-6);
        }
        _ => panic!("a clip is measured from its file"),
    }
    // The score's sections, cut to the clip's source range: the drop from 4 s to 10 s.
    assert_eq!(plan.windows.len(), 1);
    assert_eq!(plan.windows[0].label.as_deref(), Some("drop"));
    assert_eq!((plan.windows[0].start, plan.windows[0].end), (4.0, 10.0));
}

#[test]
fn a_request_names_one_target_and_a_real_one() {
    let (project, a) = audio_project();
    assert!(project.band_balance_inputs(&BandBalanceRequest::default()).is_err());
    let both = BandBalanceRequest {
        asset_id: Some(a.id),
        range: Some(TimeRange { start: 0.0, end: 5.0 }),
        ..BandBalanceRequest::default()
    };
    assert!(project
        .band_balance_inputs(&both)
        .unwrap_err()
        .to_string()
        .contains("exactly one"));
    assert!(project.band_balance_inputs(&req_asset(Uuid::new_v4())).is_err());
    // A range on a timeline with no cut.
    let range = BandBalanceRequest {
        range: Some(TimeRange { start: 0.0, end: 5.0 }),
        ..BandBalanceRequest::default()
    };
    assert!(project.band_balance_inputs(&range).is_err());
    let video = asset("/v.mp4", 10.0, vec![stream(StreamKind::Video)]);
    project.insert_asset(&video).unwrap();
    assert!(project
        .band_balance_inputs(&req_asset(video.id))
        .unwrap_err()
        .to_string()
        .contains("no audio"));
}

#[test]
fn a_range_of_the_mix_is_cut_in_eight_in_timeline_seconds() {
    let (project, a) = audio_project();
    project.add_clip_to_timeline(a.id, None, 0.0, 16.0, Some(0.0)).unwrap();
    let plan = project
        .band_balance_inputs(&BandBalanceRequest {
            range: Some(TimeRange { start: 4.0, end: 12.0 }),
            ..BandBalanceRequest::default()
        })
        .unwrap();
    assert_eq!(plan.target, "mix");
    assert_eq!((plan.origin, plan.span), (4.0, 8.0));
    assert_eq!(plan.windows.len(), 8);
    assert_eq!(plan.windows[0].start, 4.0);
    assert!(matches!(plan.source, BandSource::Mix { .. }));
    // Past the end of the cut is refused rather than measured as something else.
    let e = project
        .band_balance_inputs(&BandBalanceRequest {
            range: Some(TimeRange { start: 99.0, end: 120.0 }),
            ..BandBalanceRequest::default()
        })
        .unwrap_err()
        .to_string();
    assert!(e.contains("starts at"), "{e}");
}

/// The whole path, for real: render a score, land it, measure it by sections, compose it again.
/// Downloads the SoundFont on first use.
#[test]
#[ignore = "downloads the SoundFont, renders audio and drives the ffmpeg binary"]
fn a_composed_score_is_rendered_placed_and_measured_by_section() {
    let project = Project::open_in_memory().unwrap();
    let mut s = score(120.0, 6);
    s.style = MusicStyle::TropicalHouse;
    let asset = Project::synthesize_music(&s, &mut |_, _, _| {}, &|| false).unwrap();
    assert_eq!(asset.music_score.as_ref(), Some(&s));
    assert!(asset.has_audio());
    assert!(
        (asset.duration - s.duration_s()).abs() < 0.05,
        "{} vs {}",
        asset.duration,
        s.duration_s()
    );
    assert!(asset.name.starts_with("Music: Tropical house"));

    let (asset, clip) = project.place_music(&asset, None, None, None).unwrap();
    let balance = project.band_balance(&req_asset(asset.id)).unwrap();
    assert_eq!(balance.windows_from, "score");
    assert_eq!(balance.windows.len(), 2);
    let (intro, drop) = (&balance.windows[0], &balance.windows[1]);
    assert_eq!((intro.label.as_deref(), drop.label.as_deref()), (Some("intro"), Some("drop")));
    // The drop has the beat and the bass; the intro is a pad: the drop is the louder and carries more low end.
    assert!(drop.loudness_lufs.unwrap() > intro.loudness_lufs.unwrap() + 3.0);
    let low = |w: &crate::engine::WindowBalance| w.bands[0].share_pct + w.bands[1].share_pct;
    assert!(low(drop) > low(intro));
    assert!(balance.overall.rms_dbfs.unwrap() < -8.0);

    // Composing it again with the score edited swaps the clip's audio, and the mix measures too.
    let mut edited = s;
    edited.bpm = 100.0;
    edited.style = MusicStyle::Lofi;
    let again = Project::synthesize_music(&edited, &mut |_, _, _| {}, &|| false).unwrap();
    let (new_asset, replaced) = project.place_music(&again, None, None, Some(clip.id)).unwrap();
    assert_eq!(replaced.id, clip.id);
    assert_ne!(new_asset.id, asset.id);
    let mix = project
        .band_balance(&BandBalanceRequest {
            range: Some(TimeRange { start: 0.0, end: 10.0 }),
            ..BandBalanceRequest::default()
        })
        .unwrap();
    assert_eq!(mix.target, "mix");
    assert!(mix.overall.rms_dbfs.is_some());
    // The same score is the same file: a cache hit, the same asset.
    let cached = Project::synthesize_music(&edited, &mut |_, _, _| {}, &|| false).unwrap();
    assert_eq!(cached.path, again.path);
}

/// A composed bed of `bars` drop bars after a 2-bar intro at 120 BPM, placed at `at` and cut by
/// `fit_music` to about `target` seconds into several segments (the 16-bar drop repeats its two
/// chords, so whole phrases can be left out). The bed's clips, in time order, and its end.
fn fitted_bed(project: &Project, path: &str, at: f64, target: f64) -> (Vec<Uuid>, f64) {
    let (_, clip) = project
        .place_music(&composed(path, &score(120.0, 16)), None, Some(at), None)
        .unwrap();
    let report = project.fit_music(clip.id, Some(target), false).unwrap();
    assert!(report.clips.len() >= 2, "a bed of several segments: {report:?}");
    let timeline = project.timeline().unwrap();
    let track = timeline.tracks.iter().find(|t| t.name == MUSIC_TRACK).unwrap();
    (report.clips, track.end())
}

/// An unrelated clip right after the bed on its track.
fn follower_after(project: &Project, end: f64) -> Uuid {
    let other = asset("/other.wav", 4.0, vec![stream(StreamKind::Audio)]);
    project.insert_asset(&other).unwrap();
    let track = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.name == MUSIC_TRACK)
        .unwrap()
        .id;
    project
        .add_clip_to_timeline(other.id, Some(track), 0.0, 4.0, Some(end))
        .unwrap()
        .id
}

fn music_clips(project: &Project) -> Vec<Clip> {
    let tl = project.timeline().unwrap();
    tl.tracks.iter().find(|t| t.name == MUSIC_TRACK).unwrap().clips.clone()
}

#[test]
fn recomposing_replaces_the_whole_fitted_bed_not_one_segment() {
    let project = Project::open_in_memory().unwrap();
    let (bed, end) = fitted_bed(&project, "/m/a.wav", 3.0, 22.0);
    let start = project.timeline().unwrap().clip(bed[0]).unwrap().timeline_start;
    project.set_volume(bed[1], 0.7).unwrap();
    let old_asset = project.timeline().unwrap().clip(bed[0]).unwrap().asset_id;
    let before = project.history().unwrap().len();
    assert!(end - start < 36.0, "fit_music left phrases out");

    // The score is edited (shorter, another tempo); the clip named is the second segment.
    let edited = score(100.0, 6);
    let plan = project
        .compose_music_inputs(&ComposeRequest {
            score: edited.clone(),
            track_id: None,
            at: None,
            align: None,
            replace_clip_id: Some(bed[1]),
        })
        .unwrap();
    assert_eq!(plan.replaces, bed, "the whole bed, in time order");
    assert_eq!(plan.music_start, start, "from the bed's first clip, not the named one");
    assert_eq!(plan.at, None);
    let (new_asset, clip) = project
        .place_music(&composed("/m/b.wav", &edited), None, None, Some(bed[1]))
        .unwrap();

    assert_eq!(clip.id, bed[1], "the named clip's id survives");
    let clips = music_clips(&project);
    assert_eq!(clips.len(), 1, "one clip of the new render, no segment of the old: {clips:?}");
    assert_eq!(clips[0].asset_id, new_asset.id);
    assert_ne!(new_asset.id, old_asset);
    assert!((clips[0].timeline_start - start).abs() < 1e-9, "at the earliest start");
    assert!((clips[0].duration() - new_asset.duration).abs() < 1e-9);
    assert_eq!((clips[0].source_in, clips[0].speed), (0.0, 1.0));
    assert!(
        (clips[0].volume - 1.0).abs() < 1e-6,
        "the first clip's gain (the second was turned down)"
    );
    assert_eq!(project.history().unwrap().len(), before + 1, "one revision");
    assert_eq!(project.history().unwrap().last().unwrap().label, "Recompose music");
}

#[test]
fn recomposing_a_bed_ripples_the_clips_after_it_when_ripple_is_on_and_not_otherwise() {
    // Ripple on: a shorter piece pulls the next clip in, a longer one pushes it out.
    for (bars, longer) in [(6u32, false), (30, true)] {
        let project = Project::open_in_memory().unwrap();
        let (bed, end) = fitted_bed(&project, "/m/a.wav", 0.0, 22.0);
        let next = follower_after(&project, end);
        let edited = score(120.0, bars);
        let new_asset = composed("/m/b.wav", &edited);
        let (stored, clip) = project
            .with_ripple(Some(true), |p| p.place_music(&new_asset, None, None, Some(bed[0])))
            .unwrap();
        let tl = project.timeline().unwrap();
        let moved = tl.clip(next).unwrap();
        assert!(
            (moved.timeline_start - clip.timeline_end()).abs() < 0.05,
            "ripple on, longer {longer}: the next clip starts at {} and the piece ends at {}",
            moved.timeline_start,
            clip.timeline_end()
        );
        assert_eq!(music_clips(&project).len(), 2, "the new bed and the follower");
        assert_eq!(clip.asset_id, stored.id);
    }

    // Ripple off, a shorter piece: the next clip stays and a gap is left.
    let project = Project::open_in_memory().unwrap();
    let (bed, end) = fitted_bed(&project, "/m/a.wav", 0.0, 22.0);
    let next = follower_after(&project, end);
    let (_, clip) = project
        .with_ripple(Some(false), |p| {
            p.place_music(&composed("/m/b.wav", &score(120.0, 6)), None, None, Some(bed[0]))
        })
        .unwrap();
    assert!(clip.timeline_end() < end);
    assert!((project.timeline().unwrap().clip(next).unwrap().timeline_start - end).abs() < 1e-9);

    // Ripple off, a longer piece: it would run into the next clip, so nothing changes at all.
    let project = Project::open_in_memory().unwrap();
    let (bed, end) = fitted_bed(&project, "/m/a.wav", 0.0, 22.0);
    let next = follower_after(&project, end);
    let before = music_clips(&project);
    let e = project
        .with_ripple(Some(false), |p| {
            p.place_music(&composed("/m/long.wav", &score(120.0, 30)), None, None, Some(bed[0]))
        })
        .unwrap_err()
        .to_string();
    assert!(
        e.to_lowercase().contains("overlap") || e.contains("another clip") || e.contains("on top"),
        "{e}"
    );
    let after = music_clips(&project);
    assert_eq!(
        after.iter().map(|c| c.id).collect::<Vec<_>>(),
        before.iter().map(|c| c.id).collect::<Vec<_>>()
    );
    assert!(after
        .iter()
        .all(|c| c.asset_id == before.iter().find(|b| b.id == c.id).unwrap().asset_id));
    assert!((project.timeline().unwrap().clip(next).unwrap().timeline_start - end).abs() < 1e-9);
}

#[test]
fn a_locked_track_or_a_bad_target_is_refused_before_anything_renders_or_is_stored() {
    let project = Project::open_in_memory().unwrap();
    let (bed, _) = fitted_bed(&project, "/m/a.wav", 0.0, 22.0);
    let track = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.name == MUSIC_TRACK)
        .unwrap()
        .id;
    let req = |replace: Option<Uuid>| ComposeRequest {
        score: score(110.0, 4),
        track_id: None,
        at: None,
        align: None,
        replace_clip_id: replace,
    };
    let assets_before = project.list_assets().unwrap().len();
    project.set_track_locked(track, true).unwrap();
    // The plan says so, for a new clip and for a recompose, before a minute of rendering.
    for replace in [None, Some(bed[0])] {
        let e = project.compose_music_inputs(&req(replace)).unwrap_err().to_string();
        assert!(e.contains("is locked"), "{e}");
    }
    // ... and so does placing, with nothing stored.
    let fresh = composed("/m/c.wav", &score(110.0, 4));
    for replace in [None, Some(bed[0])] {
        let e = project.place_music(&fresh, None, None, replace).unwrap_err().to_string();
        assert!(e.contains("is locked"), "{e}");
    }
    assert_eq!(project.list_assets().unwrap().len(), assets_before);
    project.set_track_locked(track, false).unwrap();

    // A clip that is not there, and a score that does not resolve: nothing stored either.
    assert!(project.compose_music_inputs(&req(Some(Uuid::new_v4()))).is_err());
    assert!(project.place_music(&fresh, None, None, Some(Uuid::new_v4())).is_err());
    let mut broken = score(110.0, 4);
    broken.sections[0].chords = vec!["Hm".into()];
    let e = project
        .place_music(&composed("/m/bad.wav", &broken), None, None, None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("sections[0]"), "{e}");
    assert_eq!(project.list_assets().unwrap().len(), assets_before, "no asset, no analysis");
}

#[test]
fn a_start_over_other_clips_is_refused_before_the_render_unless_ripple_pushes() {
    let project = Project::open_in_memory().unwrap();
    // 2 + 8 bars at 120 BPM: 20 s of music on M, then a second piece after it.
    let (_, first) = project
        .place_music(&composed("/m/a.wav", &score(120.0, 8)), None, Some(0.0), None)
        .unwrap();
    let req = |at: Option<f64>| ComposeRequest {
        score: score(120.0, 4),
        track_id: None,
        at,
        align: None,
        replace_clip_id: None,
    };

    // Inside the first piece: refused with edit_timeline's words, ripple or not (ripple never
    // splits a clip to make room).
    for ripple in [false, true] {
        let e = project
            .with_ripple(Some(ripple), |p| p.compose_music_inputs(&req(Some(5.0))))
            .unwrap_err()
            .to_string();
        assert!(e.contains("would overlap another clip on M"), "ripple {ripple}: {e}");
    }
    // On the first piece's start: off, it collides; on, it pushes the piece along.
    let e = project
        .with_ripple(Some(false), |p| p.compose_music_inputs(&req(Some(0.0))))
        .unwrap_err()
        .to_string();
    assert!(e.contains("would overlap another clip on M"), "{e}");
    project
        .with_ripple(Some(true), |p| p.compose_music_inputs(&req(Some(0.0))))
        .unwrap();
    // The project's own flag counts the same as an override.
    project.set_ripple_mode(true).unwrap();
    project.compose_music_inputs(&req(Some(0.0))).unwrap();
    project.set_ripple_mode(false).unwrap();
    assert!(project.compose_music_inputs(&req(Some(0.0))).is_err());

    // Free space, the end of the track and "after what it holds" all fit.
    project.compose_music_inputs(&req(Some(first.timeline_end()))).unwrap();
    project.compose_music_inputs(&req(Some(first.timeline_end() + 3.0))).unwrap();
    let plan = project.compose_music_inputs(&req(None)).unwrap();
    assert_eq!(plan.at, None);
    // The rehearsal changed nothing: no track, clip, asset or revision.
    assert_eq!(project.list_assets().unwrap().len(), 1);
    assert_eq!(music_clips(&project).len(), 1);
}

#[test]
fn a_tempo_nudge_that_lengthens_the_piece_is_checked_at_its_aligned_length() {
    // Cuts at 4 and 8 s. At 122 BPM the 6-bar piece lasts 13.30 s; aligned to 120 BPM, where the
    // cuts are downbeats, it lasts 13.50 s.
    let (project, _) = picture();
    let tail = composed("/m/tail.wav", &score(120.0, 1));
    project.place_music(&tail, None, Some(13.4), None).unwrap();
    let req = |align: bool| ComposeRequest {
        score: score(122.0, 4),
        track_id: None,
        at: Some(0.0),
        align: align.then_some(ScoreAlign {
            to: AlignTo::Cuts,
            track_id: None,
            max_tempo_change: None,
        }),
        replace_clip_id: None,
    };
    let off = |align: bool| {
        project
            .with_ripple(Some(false), |p| p.compose_music_inputs(&req(align)))
            .map_err(|e| e.to_string())
    };
    let plain = off(false).unwrap();
    assert!(plain.score.duration_s() < 13.4);
    let e = off(true).unwrap_err();
    assert!(e.contains("would overlap another clip on M"), "{e}");
}

#[test]
fn recomposing_a_longer_piece_is_refused_up_front_without_ripple_and_pushes_with_it() {
    let project = Project::open_in_memory().unwrap();
    let (bed, end) = fitted_bed(&project, "/m/a.wav", 0.0, 22.0);
    follower_after(&project, end);
    let req = |bars: u32| ComposeRequest {
        score: score(120.0, bars),
        track_id: None,
        at: None,
        align: None,
        replace_clip_id: Some(bed[0]),
    };
    let off = |bars: u32| {
        project
            .with_ripple(Some(false), |p| p.compose_music_inputs(&req(bars)))
            .map_err(|e| e.to_string())
    };
    let on = |bars: u32| {
        project
            .with_ripple(Some(true), |p| p.compose_music_inputs(&req(bars)))
            .map_err(|e| e.to_string())
    };
    let e = off(30).unwrap_err();
    assert!(e.contains("would overlap another clip on M"), "{e}");
    on(30).unwrap();
    // A shorter one fits either way.
    off(6).unwrap();
    on(6).unwrap();
}

#[test]
fn a_refused_placement_stores_neither_the_asset_nor_its_analysis() {
    let project = Project::open_in_memory().unwrap();
    let (bed, end) = fitted_bed(&project, "/m/a.wav", 0.0, 22.0);
    follower_after(&project, end);
    let assets = project.list_assets().unwrap().len();
    let history = project.history().unwrap().len();

    // Too long for the gap behind the bed, ripple off: the edit is refused ...
    let long = composed("/m/long.wav", &score(120.0, 30));
    let e = project
        .with_ripple(Some(false), |p| p.place_music(&long, None, None, Some(bed[0])))
        .unwrap_err()
        .to_string();
    assert!(e.contains("would overlap another clip"), "{e}");
    // ... and leaves nothing of the new render in the library.
    assert_eq!(project.list_assets().unwrap().len(), assets);
    assert!(project.asset_by_path("/m/long.wav").unwrap().is_none());
    assert!(project.get_analysis(long.id).unwrap().is_none(), "no orphan analysis either");
    assert_eq!(project.history().unwrap().len(), history);

    // An asset that was already there stays when a later placement of it is refused.
    let (known, _) = project
        .place_music(&composed("/m/known.wav", &score(120.0, 2)), None, None, None)
        .unwrap();
    let again = composed("/m/known.wav", &score(120.0, 2));
    let track = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.name == MUSIC_TRACK)
        .unwrap()
        .id;
    let e = project
        .with_ripple(Some(false), |p| p.place_music(&again, Some(track), Some(1.0), None))
        .unwrap_err()
        .to_string();
    assert!(e.contains("would overlap another clip"), "{e}");
    assert_eq!(project.asset_by_path("/m/known.wav").unwrap().unwrap().id, known.id);
    assert!(project.get_analysis(known.id).unwrap().is_some());

    // The same for an agent, whose placement is staged: nothing lands from a refused one.
    let mut agent = Project::open_in_memory().unwrap();
    agent.set_actor(EditSource::Agent);
    let (_, held) = agent
        .place_music(&composed("/m/a.wav", &score(120.0, 8)), None, Some(0.0), None)
        .unwrap();
    let late = composed("/m/late.wav", &score(120.0, 2));
    let e = agent
        .with_ripple(Some(false), |p| {
            let m = agent
                .working_timeline()
                .unwrap()
                .tracks
                .iter()
                .find(|t| t.name == MUSIC_TRACK)
                .unwrap()
                .id;
            p.place_music(&late, Some(m), Some(held.timeline_start + 1.0), None)
        })
        .unwrap_err()
        .to_string();
    assert!(e.contains("would overlap another clip"), "{e}");
    assert!(agent.asset_by_path("/m/late.wav").unwrap().is_none());
    assert!(agent.get_analysis(late.id).unwrap().is_none());
}

#[test]
fn a_linked_bed_is_not_replaced() {
    let project = Project::open_in_memory().unwrap();
    let (_, clip) = project
        .place_music(&composed("/m/a.wav", &score(120.0, 4)), None, Some(0.0), None)
        .unwrap();
    let req = ComposeRequest {
        score: score(110.0, 4),
        track_id: None,
        at: None,
        align: None,
        replace_clip_id: Some(clip.id),
    };
    // Link the music to a picture clip.
    let v = video_asset(&project);
    let picture = project.add_clip_to_timeline(v.id, None, 0.0, 5.0, Some(0.0)).unwrap();
    project.link_clips(&[clip.id, picture.id]).unwrap();
    let e = project.compose_music_inputs(&req).unwrap_err().to_string();
    assert!(e.contains("unlink the music"), "{e}");
}

#[test]
fn a_misspelled_field_is_an_error_not_a_default() {
    // `enrgy` for `energy`, `at` for `at_s`: each would be silently dropped without the check.
    let section = r#"{"kind":"drop","bars":4,"chords":["C"],"enrgy":0.9}"#;
    let score_json = |section: &str| format!(r#"{{"bpm":100,"key":"C","style":"pop","sections":[{section}]}}"#);
    let e = serde_json::from_str::<MusicScore>(&score_json(section))
        .unwrap_err()
        .to_string();
    assert!(e.contains("unknown field `enrgy`"), "{e}");
    let e = serde_json::from_str::<MusicScore>(
        r#"{"bpm":100,"key":"C","style":"pop","sections":[{"kind":"drop","bars":4,"chords":["C"]}],"hits":[{"at":3.0,"kind":"impact"}]}"#,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("unknown field `at`"), "{e}");
    let e = serde_json::from_str::<MusicScore>(
        r#"{"bpm":100,"key":"C","style":"pop","sections":[{"kind":"drop","bars":4,"chords":["C"]}],"instruments":{"drum":10}}"#,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("unknown field `drum`"), "{e}");
    let e = serde_json::from_str::<MusicScore>(
        r#"{"bpm":100,"key":"C","style":"pop","sections":[{"kind":"drop","bars":4,"chords":["C"]}],"tempo":90}"#,
    )
    .unwrap_err()
    .to_string();
    assert!(e.contains("unknown field `tempo`"), "{e}");
    // The right spelling is accepted.
    serde_json::from_str::<MusicScore>(&score_json(r#"{"kind":"drop","bars":4,"chords":["C"],"energy":0.9}"#)).unwrap();

    // The request, its align and the band-balance request and sections.
    let score_ok = score_json(r#"{"kind":"drop","bars":4,"chords":["C"]}"#);
    let e = serde_json::from_str::<ComposeRequest>(&format!(r#"{{"score":{score_ok},"replace_clip":"x"}}"#))
        .unwrap_err()
        .to_string();
    assert!(e.contains("unknown field `replace_clip`"), "{e}");
    let e = serde_json::from_str::<ComposeRequest>(&format!(r#"{{"score":{score_ok},"align":{{"to":"cuts","max_tempo":0.1}}}}"#))
        .unwrap_err()
        .to_string();
    assert!(e.contains("unknown field `max_tempo`"), "{e}");
    let e = serde_json::from_str::<BandBalanceRequest>(r#"{"sectons":[]}"#)
        .unwrap_err()
        .to_string();
    assert!(e.contains("unknown field `sectons`"), "{e}");
    let e = serde_json::from_str::<BandBalanceRequest>(r#"{"sections":[{"start":0,"end":3,"lable":"a"}]}"#)
        .unwrap_err()
        .to_string();
    assert!(e.contains("unknown field `lable`"), "{e}");
    serde_json::from_str::<BandBalanceRequest>(r#"{"sections":[{"start":0,"end":3,"label":"a"}]}"#).unwrap();
}
