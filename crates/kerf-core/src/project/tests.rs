use super::*;
use crate::captions_import::{parse_captions, CaptionFormat, CaptionImportRequest, ImportSummary, MAX_IMPORTED_CAPTIONS};
use crate::engine::test_support::{remove_proxy, ProxyGuard};
use crate::model::SvgPresetParams;
use crate::model::{ClipMove, DiffKind, Fit};

#[test]
fn sample_project_has_assets_and_timeline() {
    let project = Project::sample().unwrap();
    let assets = project.list_assets().unwrap();
    assert_eq!(assets.len(), 2);

    let timeline = project.timeline().unwrap();
    let total_clips: usize = timeline.tracks.iter().map(|t| t.clips.len()).sum();
    assert!(total_clips >= 3);
}

#[test]
fn a_sheet_of_the_cut_covers_the_whole_cut_unless_told_and_clamps_into_it() {
    let timeline = Project::sample().unwrap().timeline().unwrap();
    let d = timeline.duration();
    assert!(d > 0.0);
    assert_eq!(Project::timeline_sheet_window(&timeline, None, None), (0.0, d));
    assert_eq!(
        Project::timeline_sheet_window(&timeline, Some(-3.0), Some(d + 10.0)),
        (0.0, d)
    );
    assert_eq!(Project::timeline_sheet_window(&timeline, Some(d + 5.0), None), (d, d));
    assert_eq!(Project::timeline_sheet_window(&timeline, Some(2.0), Some(1.0)), (2.0, 2.0));
    let times = Project::timeline_sheet_times(&timeline, None, None, 4, 2);
    assert_eq!(times.len(), 8);
    assert_eq!(times[0], 0.0);
    assert!((times[1] - d / 8.0).abs() < 1e-9);
    assert!(times.iter().all(|&t| t < d), "{times:?}");
    assert!(Project::timeline_sheet_times(&timeline, Some(1.0), Some(1.0), 2, 2)
        .iter()
        .all(|&t| t == 1.0));
    let mut empty = timeline;
    empty.tracks.clear();
    let refused = Project::composite_timeline_sheet_cancellable(&empty, &[], None, None, 4, 4, 240, 5, &|| false);
    assert!(matches!(refused, Err(Error::InvalidArgument(_))), "{refused:?}");
}

#[test]
fn a_grade_is_normalized_stored_and_diffed() {
    use crate::model::{ClipLut, CurvePoint, Curves};
    let project = Project::sample().unwrap();
    let before = project.timeline().unwrap();
    let clip = before.tracks[0].clips[0].id;
    let grade = Grade {
        curves: Some(Curves {
            master: vec![CurvePoint { x: 1.0, y: 0.0 }, CurvePoint { x: 0.0, y: 1.0 }],
            red: vec![CurvePoint { x: 0.0, y: 0.0 }, CurvePoint { x: 1.0, y: 1.0 }],
            ..Curves::default()
        }),
        wheels: None,
        lut: Some(ClipLut {
            path: "relative.cube".into(),
            intensity: 1.0,
        }),
    };
    assert!(
        project.set_grade(clip, grade.clone()).is_err(),
        "a relative LUT path is refused"
    );
    let out = project.set_grade(clip, Grade { lut: None, ..grade }).unwrap();
    let curves = out.grade.curves.unwrap();
    assert_eq!(curves.master[0], CurvePoint { x: 0.0, y: 1.0 }, "sorted");
    assert!(curves.red.is_empty(), "the identity curve is dropped");
    let after = project.timeline().unwrap();
    let diff = before.diff(&after);
    assert!(
        diff.entries
            .iter()
            .any(|e| e.detail.as_deref().is_some_and(|d| d.contains("curves set"))),
        "{diff:?}"
    );
    let cleared = project.set_grade(clip, Grade::default()).unwrap();
    assert!(cleared.grade.is_empty());
}

#[test]
fn referenced_files_name_every_source_and_every_lut_the_history_holds() {
    use crate::model::ClipLut;
    // An absolute path on this platform (a LUT path must be one).
    let abs = |p: &str| {
        if cfg!(windows) {
            format!("C:\\{}", p.replace('/', "\\"))
        } else {
            format!("/{p}")
        }
    };
    let mut project = Project::sample().unwrap();
    let clip = project.timeline().unwrap().tracks[0].clips[0].id;
    let lut = |path: &str| Grade {
        lut: Some(ClipLut {
            path: abs(path),
            intensity: 1.0,
        }),
        ..Grade::default()
    };
    project.set_grade(clip, lut("luts/old.cube")).unwrap();
    project.set_grade(clip, lut("luts/new.cube")).unwrap();
    let mut stitched = project.list_assets().unwrap()[0].clone();
    stitched.id = Uuid::new_v4();
    stitched.path = abs("cache/stitched/pair.mp4");
    stitched.source_paths = vec![abs("caps/VID_00_000.insv"), abs("caps/VID_10_000.insv")];
    project.insert_or_get_asset(&stitched).unwrap();
    // An agent's proposal and its undo step hold LUTs the cut does not.
    project.set_agent_review(true);
    project.set_actor(EditSource::Agent);
    project.set_grade(clip, lut("luts/staged-first.cube")).unwrap();
    project.set_grade(clip, lut("luts/staged-then.cube")).unwrap();
    assert!(project.staged().unwrap().is_some_and(|s| s.undo_depth > 0));
    let files = project.referenced_files().unwrap();
    for asset in project.list_assets().unwrap() {
        assert!(files.media.contains(&asset.path), "an asset's file");
    }
    for capture in &stitched.source_paths {
        assert!(files.media.contains(capture), "a capture file");
    }
    for (path, why) in [
        ("luts/new.cube", "the cut"),
        ("luts/old.cube", "an undo brings it back"),
        ("luts/staged-then.cube", "the proposal"),
        ("luts/staged-first.cube", "the proposal's undo step"),
    ] {
        assert!(files.luts.contains(&abs(path)), "{why}");
    }
    assert!(files.media.iter().all(|m| !m.ends_with(".cube")));
}

#[test]
fn platform_check_reads_the_cut_the_export_would_render() {
    let project = Project::sample().unwrap();
    // Cut for a vertical feed: the frame the checks compare against is the
    // project's delivery format, not the footage's shape.
    project
        .set_delivery_format(Some(crate::model::Delivery {
            width: 1080,
            height: 1920,
            fit: Fit::Cover,
            fps: None,
        }))
        .unwrap();
    let summary = project.cut_summary(None).unwrap();
    assert_eq!((summary.width, summary.height), (1080, 1920));
    assert!(summary.duration > 0.0);
    assert!(summary.has_audio, "the sample has audio-bearing clips");

    let report = project.platform_report(None, None).unwrap();
    assert_eq!((report.cut.width, report.cut.height), (1080, 1920));
    assert_eq!(report.cut.duration, summary.duration);
    let checks = report.targets;
    assert_eq!(checks.len(), crate::platform::TARGETS.len());
    let reels = checks.iter().find(|c| c.target == "reels").unwrap();
    assert!(reels.ok, "a short vertical cut is publishable: {:?}", reels.issues);
    // The same cut is the wrong shape for a landscape player, and says so.
    let youtube = checks.iter().find(|c| c.target == "youtube").unwrap();
    assert!(youtube
        .issues
        .iter()
        .any(|i| i.severity == crate::platform::Severity::Warning && i.message.contains("letterboxed")));
}

#[test]
fn review_cut_reads_the_working_cut_and_its_analysis() {
    let project = Project::sample().unwrap();
    let timeline = project.working_timeline().unwrap();
    let clip = timeline.tracks.iter().flat_map(|t| t.clips.iter()).next().unwrap().clone();
    project
        .set_analysis(&AssetAnalysis {
            asset_id: clip.asset_id,
            scene_changes: vec![1.0],
            ..AssetAnalysis::default()
        })
        .unwrap();
    let inputs = project.review_inputs().unwrap();
    assert_eq!(inputs.timeline.duration(), timeline.duration());
    assert!(inputs.analyses.contains_key(&clip.asset_id));
    let review = project.review_cut(None, &["flash_frame".to_string()]).unwrap();
    assert_eq!(review.checked, vec![crate::review::Check::FlashFrame]);
    assert!(matches!(
        project.review_cut(None, &["flash".to_string()]),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn importing_the_same_media_twice_reuses_the_asset() {
    // Both halves of an Insta360 pair stitch to one cached file and arrive
    // with the same path — that must be one asset, not two.
    let project = Project::open_in_memory().unwrap();
    let first = project
        .insert_or_get_asset(&asset_with("/cache/stitched.mp4", vec![vid_stream(false)]))
        .unwrap();
    let second = project
        .insert_or_get_asset(&asset_with("/cache/stitched.mp4", vec![vid_stream(false)]))
        .unwrap();
    assert_eq!(first.id, second.id, "the second import resolves to the first asset");
    assert_eq!(project.list_assets().unwrap().len(), 1);
}

#[test]
fn a_waveform_range_needs_an_asset_with_audio() {
    let project = Project::open_in_memory().unwrap();
    // Nothing here exists on disk: both refusals must come before any
    // ffmpeg is spawned, and say what is wrong.
    let silent = project
        .insert_or_get_asset(&asset_with("/nowhere/silent.mp4", vec![vid_stream(false)]))
        .unwrap();
    let err = project.waveform_range(silent.id, 0.0, 1.0, 100).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
    assert!(err.to_string().contains("no audio stream"), "{err}");
    let still = project
        .insert_or_get_asset(&asset_with("/nowhere/still.png", vec![vid_stream(true)]))
        .unwrap();
    assert!(matches!(
        project.waveform_range(still.id, 0.0, 1.0, 100),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        project.waveform_range(Uuid::new_v4(), 0.0, 1.0, 100),
        Err(Error::AssetNotFound(_))
    ));
}

/// `cargo test -p kerf-core --no-default-features -- --ignored waveform_range_reads`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn waveform_range_reads_an_imported_assets_peaks_without_the_project_lock() {
    use crate::engine::test_support::StatusBounded;
    let ffmpeg = std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
    let dir = std::env::temp_dir().join(format!("kerf-wave-range-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("tone.wav");
    // Left: a 0.5 sine; right: silence. Two seconds.
    let made = std::process::Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg("aevalsrc='0.5*sin(2*PI*440*t)|0':s=48000:d=2:c=stereo")
        .args(["-c:a", "pcm_f32le"])
        .arg(&wav)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());

    let project = Project::open_in_memory().unwrap();
    let asset = project.import_asset(&wav).unwrap();
    let range = project.waveform_range(asset.id, 0.0, 2.0, 40).unwrap();
    assert_eq!(
        (range.channels, range.buckets, range.min.len(), range.max.len()),
        (2, 40, 2, 2)
    );
    assert!(range.max[0].iter().all(|v| (0.45..=0.51).contains(v)), "{:?}", range.max[0]);
    assert!(range.min[1].iter().chain(&range.max[1]).all(|v| v.abs() < 1e-4));
    assert!((range.duration - 2.0).abs() < 0.01);

    // The lock-free static a surface calls after dropping the project lock
    // reads the same answer (from the memo this time).
    let again = Project::decode_waveform_range(&asset, 0.0, 2.0, 40).unwrap();
    assert_eq!(again, range);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stitched_asset_provenance_survives_a_save_and_reopen() {
    let project = Project::open_in_memory().unwrap();
    let mut asset = asset_with("/cache/stitched.mp4", vec![vid_stream(false)]);
    asset.source_paths = vec!["/dcim/VID_1_2_00_3.mp4".into(), "/dcim/VID_1_2_10_3.mp4".into()];
    project.insert_asset(&asset).unwrap();

    let dir = std::env::temp_dir().join(format!("kerf-stitch-provenance-{}.kerf", Uuid::new_v4()));
    project.save_as(&dir).unwrap();
    let reopened = Project::open(&dir).unwrap();
    let loaded = reopened.require_asset(asset.id).unwrap();
    assert_eq!(loaded.source_paths, asset.source_paths);
    let _ = std::fs::remove_file(&dir);
}

fn voiceover_asset(path: &str) -> Asset {
    let mut asset = asset_with(path, vec![aud_stream()]);
    asset.duration = 4.0;
    asset.voiceover = Some(Voiceover {
        text: "Welcome to the shop. We open at nine.".into(),
        voice: "af_heart".into(),
        speed: 1.0,
        segments: vec![
            TranscriptSegment {
                start: 0.0,
                end: 1.6,
                text: "Welcome to the shop.".into(),
                ..Default::default()
            },
            TranscriptSegment {
                start: 1.85,
                end: 4.0,
                text: "We open at nine.".into(),
                ..Default::default()
            },
        ],
    });
    asset
}

#[test]
fn a_voiceover_lands_on_its_own_track_and_captions_from_its_script() {
    let project = Project::open_in_memory().unwrap();
    let (asset, clip) = project
        .place_voiceover(&voiceover_asset("/vo/a.wav"), None, Some(2.0))
        .unwrap();
    let timeline = project.timeline().unwrap();
    let vo = timeline.tracks.iter().find(|t| t.name == VOICEOVER_TRACK).expect("VO track");
    assert_eq!(vo.kind, StreamKind::Audio);
    assert_eq!(vo.clips.len(), 1);
    assert_eq!((clip.timeline_start, clip.duration()), (2.0, 4.0));
    // The script is the transcript: no analysis pass, no speech model.
    assert_eq!(project.get_analysis(asset.id).unwrap().unwrap().transcript.len(), 2);

    let captions = project.generate_captions(CaptionOptions::default()).unwrap();
    assert!(!captions.is_empty());
    assert!(
        (captions[0].start - 2.0).abs() < 1e-9,
        "captions follow the clip onto the timeline"
    );
    assert!(captions.iter().all(|c| c.end <= 6.0 + 1e-9));

    // A second voiceover joins the same lane, after the first.
    let (_, second) = project.place_voiceover(&voiceover_asset("/vo/b.wav"), None, None).unwrap();
    let timeline = project.timeline().unwrap();
    assert_eq!(timeline.tracks.iter().filter(|t| t.name == VOICEOVER_TRACK).count(), 1);
    assert_eq!(second.timeline_start, 6.0);
}

#[test]
fn the_cut_transcript_reads_the_working_cut_and_names_what_is_untranscribed() {
    let project = Project::open_in_memory().unwrap();
    let (vo, _) = project
        .place_voiceover(&voiceover_asset("/vo/a.wav"), None, Some(2.0))
        .unwrap();
    let music = asset_with("/music.wav", vec![aud_stream()]);
    project.insert_asset(&music).unwrap();
    project.add_clip_to_timeline(music.id, None, 0.0, 3.0, Some(0.0)).unwrap();
    let cut = project.cut_transcript().unwrap();
    let lines: Vec<(&str, f64)> = cut
        .lines
        .iter()
        .map(|l| (l.text.as_str(), (l.start * 1000.0).round() / 1000.0))
        .collect();
    assert_eq!(lines, [("Welcome to the shop.", 2.0), ("We open at nine.", 3.85)]);
    assert!(cut.lines.iter().all(|l| l.asset_id == vo.id));
    assert_eq!(cut.untranscribed.len(), 1);
    assert_eq!((cut.untranscribed[0].asset_id, cut.untranscribed[0].clips), (music.id, 1));
}

#[test]
fn a_synthesized_sound_effect_is_not_listed_as_untranscribed() {
    let project = Project::open_in_memory().unwrap();
    let music = asset_with("/music.wav", vec![aud_stream()]);
    let mut effect = asset_with("/fx/whoosh.wav", vec![aud_stream()]);
    effect.sound_effect = Some(crate::model::SfxSpec::new(crate::model::SfxKind::Whoosh));
    project.insert_asset(&music).unwrap();
    project.insert_asset(&effect).unwrap();
    project.add_clip_to_timeline(music.id, None, 0.0, 3.0, Some(0.0)).unwrap();
    project.add_clip_to_timeline(effect.id, None, 0.0, 1.0, Some(3.0)).unwrap();
    let cut = project.cut_transcript().unwrap();
    assert_eq!(
        cut.untranscribed.iter().map(|u| u.asset_id).collect::<Vec<_>>(),
        [music.id],
        "an effect has no speech to transcribe"
    );
}

#[test]
fn a_voiceover_refuses_a_video_track() {
    let project = Project::open_in_memory().unwrap();
    let v1 = project.timeline().unwrap().first_track_of(StreamKind::Video).unwrap();
    assert!(project
        .place_voiceover(&voiceover_asset("/vo/a.wav"), Some(v1), None)
        .is_err());
}

#[test]
fn voiceover_provenance_survives_a_save_and_reopen() {
    let project = Project::open_in_memory().unwrap();
    let asset = voiceover_asset("/vo/a.wav");
    project.insert_asset(&asset).unwrap();
    let path = std::env::temp_dir().join(format!("kerf-voiceover-{}.kerf", Uuid::new_v4()));
    project.save_as(&path).unwrap();
    let loaded = Project::open(&path).unwrap().require_asset(asset.id).unwrap();
    let voiceover = loaded.voiceover.expect("voiceover survives");
    assert_eq!(voiceover.voice, "af_heart");
    assert_eq!(voiceover.segments.len(), 2);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_older_file_gains_the_voiceover_column() {
    let path = std::env::temp_dir().join(format!("kerf-pre-voiceover-{}.kerf", Uuid::new_v4()));
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE assets (id TEXT PRIMARY KEY, path TEXT NOT NULL, name TEXT NOT NULL,
                 duration REAL NOT NULL, streams TEXT NOT NULL, imported_at TEXT NOT NULL, source_paths TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO assets VALUES (?1, '/m.mp4', 'm', 1.0, '[]', ?2, NULL)",
            params![Uuid::new_v4().to_string(), Utc::now().to_rfc3339()],
        )
        .unwrap();
    }
    let project = Project::open(&path).unwrap();
    let assets = project.list_assets().unwrap();
    assert_eq!(assets.len(), 1);
    assert!(assets[0].voiceover.is_none());
    let backup = schema_backup_path(&path, 0);
    assert!(backup.exists(), "the pre-upgrade file is kept beside it");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&backup);
}

fn asset_with(path: &str, streams: Vec<StreamInfo>) -> Asset {
    Asset {
        id: Uuid::new_v4(),
        path: path.into(),
        name: "x".into(),
        duration: 10.0,
        streams,
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    }
}

fn vid_stream(image: bool) -> StreamInfo {
    StreamInfo {
        index: 0,
        kind: StreamKind::Video,
        codec: if image { "png".into() } else { "h264".into() },
        width: Some(1920),
        height: Some(1080),
        fps: Some(30.0),
        sample_rate: None,
        channels: None,
        image,
        projection: None,
        rotation: 0,
        color_transfer: None,
        color_primaries: None,
        pix_fmt: None,
        color_space: None,
        color_range: None,
        lead: 0.0,
        time_base: None,
    }
}

fn aud_stream() -> StreamInfo {
    StreamInfo {
        index: 0,
        kind: StreamKind::Audio,
        codec: "aac".into(),
        width: None,
        height: None,
        fps: None,
        sample_rate: Some(48_000),
        channels: Some(2),
        image: false,
        projection: None,
        rotation: 0,
        color_transfer: None,
        color_primaries: None,
        pix_fmt: None,
        color_space: None,
        color_range: None,
        lead: 0.0,
        time_base: None,
    }
}

#[test]
fn snap_to_beats_lands_the_video_cuts_on_the_music_grid() {
    let project = Project::open_in_memory().unwrap();
    let video = project
        .insert_or_get_asset(&asset_with("/beat-video.mp4", vec![vid_stream(false)]))
        .unwrap();
    let music = project
        .insert_or_get_asset(&asset_with("/beat-music.wav", vec![aud_stream()]))
        .unwrap();
    project
        .set_analysis(&AssetAnalysis {
            asset_id: music.id,
            tempo: Some(crate::model::Tempo {
                downbeats: Vec::new(),
                bpm: 120.0,
                beats: (0..=20).map(|i| i as f64 * 0.5).collect(),
                confidence: 0.8,
            }),
            ..Default::default()
        })
        .unwrap();
    project.add_asset_audio(music.id).unwrap();
    project.cut_clip(video.id, 0.0, 1.1).unwrap();
    project.cut_clip(video.id, 2.0, 3.4).unwrap();

    let moved = project.snap_to_beats(None, None).unwrap();
    assert!(moved > 0, "cuts off the grid should have moved");

    let timeline = project.timeline().unwrap();
    let cuts: Vec<f64> = timeline
        .tracks
        .iter()
        .filter(|t| t.kind == StreamKind::Video)
        .flat_map(|t| t.clips.iter().map(Clip::timeline_end))
        .collect();
    assert_eq!(cuts, vec![1.0, 2.5]);

    // The music track is the grid, not a target — it keeps its full length.
    let audio = timeline.tracks.iter().find(|t| t.kind == StreamKind::Audio).unwrap();
    assert_eq!(audio.clips[0].duration(), 10.0);
}

fn fit_music_project() -> (Project, Uuid) {
    let project = Project::open_in_memory().unwrap();
    let mut music = asset_with("/fit-music.wav", vec![aud_stream()]);
    music.duration = 34.5;
    let music = project.insert_or_get_asset(&music).unwrap();
    let prog = [0, 9, 5, 7, 0, 9, 2, 4];
    let bar_chroma: Vec<[f32; 12]> = (0..16)
        .map(|k| {
            let mut c = [0.0_f32; 12];
            c[prog[k % 8]] = 1.0;
            c
        })
        .collect();
    let phrases = crate::engine::music::phrase_matches(&bar_chroma, crate::engine::music::SPLICE_SIMILARITY);
    project
        .set_analysis(&AssetAnalysis {
            asset_id: music.id,
            music: Some(crate::model::MusicAnalysis {
                grid: crate::model::BeatGrid {
                    period_s: 0.5,
                    phase_s: 0.0,
                    downbeat_offset: 2,
                    beats_per_bar: 4,
                },
                duration: 34.5,
                bar_chroma,
                phrases,
            }),
            ..Default::default()
        })
        .unwrap();
    let mut long = asset_with("/fit-video.mp4", vec![vid_stream(false)]);
    long.duration = 60.0;
    let long = project.insert_or_get_asset(&long).unwrap();
    project.cut_clip(long.id, 0.0, 50.0).unwrap();
    project.add_asset_audio(music.id).unwrap();
    let timeline = project.timeline().unwrap();
    let clip = timeline.tracks.iter().find(|t| t.kind == StreamKind::Audio).unwrap().clips[0].id;
    (project, clip)
}

#[test]
fn fit_music_fills_the_picture_with_whole_phrases() {
    let (project, clip) = fit_music_project();
    let plan = project.plan_music_fit(clip, None).unwrap();
    assert_eq!(plan.target, 50.0);
    assert_eq!(plan.bars, 24, "{plan:?}");
    assert!(plan.remainder < 0.0, "24 bars run half a second over");
    assert!(project.timeline().unwrap().clip(clip).is_some(), "planning changes nothing");

    let report = project.fit_music(clip, None, true).unwrap();
    assert!(report.faded);
    let timeline = project.timeline().unwrap();
    let audio = timeline.tracks.iter().find(|t| t.kind == StreamKind::Audio).unwrap();
    assert_eq!(audio.clips.len(), report.clips.len());
    assert_eq!(audio.clips.len(), 2);
    assert!((audio.end() - 50.0).abs() < 1e-6, "{}", audio.end());
    assert!(audio.clips[1].transition_in.is_some());
    assert_eq!(audio.clips[1].fade_out, crate::model::FIT_FADE_S);
    assert!(timeline.clip(clip).is_none(), "the original clip is replaced");

    let err = project.fit_music(report.clips[0], Some(-1.0), false).unwrap_err().to_string();
    assert!(err.contains("positive"), "{err}");
}

#[test]
fn an_agents_duck_mode_stages_and_applies() {
    let mut project = Project::open_in_memory().unwrap();
    project.set_actor(EditSource::Agent);
    project.set_agent_review(false);
    project.begin_staging(None, None, None).unwrap();
    let master = project.set_master_duck(Some(-90.0)).unwrap();
    assert_eq!(master.duck_depth_db, Some(crate::model::DUCK_MIN_DEPTH_DB));
    let staged = project.staged().unwrap().expect("a proposal");
    assert_eq!(
        staged.diff.entries.len(),
        1,
        "the review lists the duck change: {:?}",
        staged.diff
    );
    let applied = project.apply_staged(false).unwrap();
    assert_eq!(applied.master.duck_depth_db, Some(crate::model::DUCK_MIN_DEPTH_DB));
    assert!(project.set_master_duck(Some(f64::NAN)).is_err());
    project.begin_staging(None, None, None).unwrap();
    project.set_master_duck(None).unwrap();
    assert_eq!(project.apply_staged(false).unwrap().master.duck_depth_db, None);
}

#[test]
fn stems_land_under_the_clip_and_silence_its_mix() {
    let project = Project::open_in_memory().unwrap();
    let mut song = asset_with("/song.wav", vec![aud_stream()]);
    song.duration = 30.0;
    let song = project.insert_or_get_asset(&song).unwrap();
    project.add_asset_audio(song.id).unwrap();
    let clip = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.kind == StreamKind::Audio)
        .unwrap()
        .clips[0]
        .clone();
    let stems: Vec<Asset> = engine::STEM_NAMES
        .iter()
        .map(|n| {
            let mut a = asset_with(&format!("/stems/{n}.flac"), vec![aud_stream()]);
            a.duration = 30.0;
            a.name = format!("song · {n}");
            a
        })
        .collect();
    let other = project
        .insert_or_get_asset(&asset_with("/other.wav", vec![aud_stream()]))
        .unwrap();
    assert!(
        project.place_stems(other.id, &stems, Some(clip.id)).is_err(),
        "a clip of another asset"
    );

    let placed = project.place_stems(song.id, &stems, Some(clip.id)).unwrap();
    assert_eq!(placed.assets.len(), 4);
    assert_eq!(placed.clips.len(), 4);
    let timeline = project.timeline().unwrap();
    let names: Vec<&str> = timeline.tracks.iter().map(|t| t.name.as_str()).collect();
    for n in ["Drums", "Bass", "Other", "Vocals"] {
        assert!(names.contains(&n), "{names:?}");
    }
    for c in &placed.clips {
        assert_eq!(
            (c.timeline_start, c.source_in, c.source_out),
            (clip.timeline_start, clip.source_in, clip.source_out)
        );
    }
    assert!(!timeline.clip(clip.id).unwrap().enabled, "the mix is not heard twice");
    // Importing again reuses the stored stems.
    let again = project.place_stems(song.id, &stems, None).unwrap();
    assert_eq!(
        again.assets.iter().map(|a| a.id).collect::<Vec<_>>(),
        placed.assets.iter().map(|a| a.id).collect::<Vec<_>>()
    );
    assert!(again.clips.is_empty());
}

#[test]
fn fit_music_needs_a_bar_grid() {
    let project = Project::open_in_memory().unwrap();
    let music = project
        .insert_or_get_asset(&asset_with("/plain.wav", vec![aud_stream()]))
        .unwrap();
    project.add_asset_audio(music.id).unwrap();
    let clip = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.kind == StreamKind::Audio)
        .unwrap()
        .clips[0]
        .id;
    let err = project.fit_music(clip, Some(20.0), false).unwrap_err().to_string();
    assert!(err.contains("no bar grid"), "{err}");
}

#[test]
fn snap_to_beats_without_a_grid_says_what_is_missing() {
    let project = Project::open_in_memory().unwrap();
    let video = project
        .insert_or_get_asset(&asset_with("/no-music.mp4", vec![vid_stream(false)]))
        .unwrap();
    project.cut_clip(video.id, 0.0, 1.1).unwrap();
    let err = project.snap_to_beats(None, None).unwrap_err().to_string();
    assert!(err.contains("no beat grid"), "got: {err}");
}

#[test]
fn preview_source_falls_back_to_original_without_a_proxy() {
    // A video asset with no generated proxy decodes from the original, so a
    // preview never breaks or blocks on a proxy that hasn't landed yet.
    let asset = asset_with("/no-such-kerf-source.mp4", vec![vid_stream(false)]);
    assert_eq!(Project::preview_source(&asset), PathBuf::from(&asset.path));
}

#[test]
fn a_stored_transcript_answers_speakers_and_fillers() {
    use crate::model::Word;
    let project = Project::open_in_memory().unwrap();
    let asset = project
        .insert_or_get_asset(&asset_with("/kerf-speakers.mp4", vec![vid_stream(false)]))
        .unwrap();
    assert!(project.transcript(asset.id).is_err());
    let word = |text: &str, start: f64, end: f64, sp: &str| Word {
        text: text.into(),
        start,
        end,
        speaker: Some(sp.into()),
        ..Word::default()
    };
    let line = |words: Vec<Word>, sp: &str| TranscriptSegment {
        start: words[0].start,
        end: words[words.len() - 1].end,
        text: words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "),
        words,
        speaker: Some(sp.into()),
    };
    let plain = vec![TranscriptSegment {
        start: 0.0,
        end: 2.4,
        text: "Um, hello hi".into(),
        ..TranscriptSegment::default()
    }];
    project
        .merge_analysis(&AssetAnalysis {
            asset_id: asset.id,
            transcript: plain.clone(),
            ran: vec![crate::model::AnalysisKind::Transcript],
            ..AssetAnalysis::default()
        })
        .unwrap();
    let refined = vec![
        line(vec![word("Um,", 0.0, 0.3, "S1"), word("hello", 0.4, 0.9, "S1")], "S1"),
        line(vec![word("hi", 2.0, 2.4, "S2")], "S2"),
    ];
    project.store_transcript(asset.id, &plain, refined.clone()).unwrap();
    // A refinement of a transcript that has since been replaced is refused.
    let stale = project.store_transcript(asset.id, &plain, plain.clone()).unwrap_err();
    assert!(stale.to_string().contains("changed"), "{stale}");
    assert_eq!(project.transcript(asset.id).unwrap(), refined);
    let s2 = project.speaker_ranges(asset.id, Some("S2"), 0.5).unwrap();
    assert_eq!(s2.len(), 1);
    assert_eq!((s2[0].ranges[0].start, s2[0].ranges[0].end), (2.0, 2.4));
    assert!(project
        .speaker_ranges(asset.id, Some("S9"), 0.5)
        .unwrap_err()
        .to_string()
        .contains("S1, S2"));
    let fillers = project.filler_words(asset.id, &[]).unwrap();
    assert_eq!(fillers.len(), 1);
    assert_eq!((fillers[0].start, fillers[0].end), (0.0, 0.3));
    let analysis = project.get_analysis(asset.id).unwrap().unwrap();
    assert!(analysis.ran.contains(&crate::model::AnalysisKind::Transcript));

    let silent = project
        .insert_or_get_asset(&asset_with("/kerf-silent.mp4", vec![vid_stream(false)]))
        .unwrap();
    let spoken = project
        .insert_or_get_asset(&asset_with("/kerf-untranscribed.mov", vec![aud_stream()]))
        .unwrap();
    let clip = project.cut_clip(asset.id, 0.2, 1.0).unwrap();
    let search = |text: &str, ids: Option<&[Uuid]>| {
        let inputs = project.transcript_search_inputs(ids).unwrap();
        let query = crate::transcript::TranscriptQuery {
            query: text.into(),
            ..Default::default()
        };
        crate::transcript::search_transcripts(inputs, &query).unwrap()
    };
    let found = search("UM hello", None);
    assert_eq!((found.total, found.searched_assets), (1, 1));
    assert_eq!(found.assets_without_transcript.asset_ids, vec![spoken.id]);
    let hit = &found.hits[0];
    assert_eq!((hit.start, hit.end, hit.speaker.as_deref()), (0.0, 0.9, Some("S1")));
    assert_eq!(hit.in_cut.len(), 1);
    assert_eq!(hit.in_cut[0].clip_id, clip.id);
    assert!(hit.in_cut[0].partial);
    assert!(search("hi", None).hits[0].in_cut.is_empty());
    let named = search("hi", Some(&[silent.id]));
    assert_eq!((named.total, named.assets_without_transcript.count), (0, 1));
    assert!(matches!(
        project.transcript_search_inputs(Some(&[Uuid::new_v4()])),
        Err(Error::AssetNotFound(_))
    ));
}

/// 50 assets × one hour of speech (≈ 9,600 timed words each), timed: what is read under the lock
/// and the search after it. Run it with
/// `cargo test --release -p kerf-core --no-default-features a_large_library -- --ignored --nocapture`.
#[test]
#[ignore]
#[allow(clippy::print_stderr)]
fn a_large_library_searches_its_transcripts_quickly() {
    use crate::model::Word;
    use crate::transcript::{search_transcripts, MatchMode, TranscriptQuery};
    let vocab = [
        "so", "we", "went", "out", "to", "the", "lake", "and", "then", "it", "started", "raining", "really", "hard", "café",
        "don't",
    ];
    let project = Project::open_in_memory().unwrap();
    for a in 0..50 {
        let asset = project
            .insert_or_get_asset(&Asset {
                duration: 3600.0,
                ..asset_with(&format!("/kerf-hour-{a}.mov"), vec![aud_stream()])
            })
            .unwrap();
        let transcript = (0..1200)
            .map(|l| {
                let start = l as f64 * 3.0;
                let words: Vec<Word> = (0..8)
                    .map(|w| Word {
                        text: vocab[(a * 7 + l * 3 + w) % vocab.len()].into(),
                        start: start + w as f64 * 0.35,
                        end: start + w as f64 * 0.35 + 0.3,
                        confidence: Some(0.9),
                        speaker: Some(format!("S{}", l % 2 + 1)),
                        ..Word::default()
                    })
                    .collect();
                TranscriptSegment {
                    start,
                    end: start + 2.8,
                    text: words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "),
                    words,
                    speaker: Some(format!("S{}", l % 2 + 1)),
                }
            })
            .collect();
        project
            .merge_analysis(&AssetAnalysis {
                asset_id: asset.id,
                transcript,
                ran: vec![crate::model::AnalysisKind::Transcript],
                ..AssetAnalysis::default()
            })
            .unwrap();
        project.cut_clip(asset.id, 60.0, 120.0).unwrap();
    }
    let mut report = Vec::new();
    for (text, mode) in [
        ("the lake", MatchMode::Phrase),
        ("raining lake", MatchMode::AllWords),
        ("the", MatchMode::AnyWord),
    ] {
        let t = std::time::Instant::now();
        let inputs = project.transcript_search_inputs(None).unwrap();
        let locked = t.elapsed();
        let bytes: usize = inputs.cached.values().map(String::len).sum();
        let query = TranscriptQuery {
            query: text.into(),
            mode,
            ..TranscriptQuery::default()
        };
        let t = std::time::Instant::now();
        let found = search_transcripts(inputs, &query).unwrap();
        assert_eq!(found.searched_assets, 50);
        report.push(format!(
            "{mode:?}: {} MB read under the lock in {locked:?}, searched in {:?} ({} hits)",
            bytes / 1_000_000,
            t.elapsed(),
            found.total
        ));
    }
    eprintln!("{}", report.join("\n"));
}

#[test]
fn merging_an_analysis_patch_keeps_the_kinds_it_did_not_run() {
    use crate::model::AnalysisKind;
    let project = Project::open_in_memory().unwrap();
    let asset = project
        .insert_or_get_asset(&asset_with("/kerf-merge-analysis.mp4", vec![vid_stream(false)]))
        .unwrap();
    // Nothing cached yet: a patch that ran nothing caches nothing.
    let empty = AssetAnalysis {
        asset_id: asset.id,
        ..AssetAnalysis::default()
    };
    project.merge_analysis(&empty).unwrap();
    assert!(project.get_analysis(asset.id).unwrap().is_none());

    let silence = AssetAnalysis {
        asset_id: asset.id,
        silence_segments: vec![TimeRange { start: 1.0, end: 2.0 }],
        ran: vec![AnalysisKind::Silence],
        ..AssetAnalysis::default()
    };
    project.merge_analysis(&silence).unwrap();
    let scenes = AssetAnalysis {
        asset_id: asset.id,
        scene_changes: vec![4.0],
        ran: vec![AnalysisKind::Scenes],
        ..AssetAnalysis::default()
    };
    let merged = project.merge_analysis(&scenes).unwrap();
    assert_eq!(merged.done_kinds(), [AnalysisKind::Silence, AnalysisKind::Scenes]);
    let stored = project.get_analysis(asset.id).unwrap().unwrap();
    assert_eq!(stored.silence_segments.len(), 1, "the first patch survived the second");
    assert_eq!(stored.scene_changes, [4.0]);
    let status = project.analysis_status(asset.id).unwrap();
    assert_eq!(status.kinds.len(), 5);
    assert_eq!(status.kinds[0].state, crate::analysis::AnalysisState::Done);
    assert_eq!(project.analysis_statuses().unwrap().len(), 1);
}

#[test]
fn the_preview_source_setting_decides_whether_a_ready_proxy_is_used() {
    use crate::proxy::PreviewSource;
    let asset = asset_with("/kerf-source-setting.mp4", vec![vid_stream(false)]);
    let ready = || Some(PathBuf::from("/cache/proxy.mp4"));
    let original = PathBuf::from(&asset.path);
    // Auto and Proxy only both decode a ready proxy; Original never asks for one.
    assert_eq!(
        resolve_preview_source(&asset, PreviewSource::Auto, ready),
        PathBuf::from("/cache/proxy.mp4")
    );
    assert_eq!(
        resolve_preview_source(&asset, PreviewSource::ProxyOnly, ready),
        PathBuf::from("/cache/proxy.mp4")
    );
    assert_eq!(
        resolve_preview_source(&asset, PreviewSource::Original, || panic!("a proxy was looked up")),
        original
    );
    // No proxy yet: the original, which is what Proxy only then refuses (`proxy_waits`).
    assert_eq!(resolve_preview_source(&asset, PreviewSource::Auto, || None), original);
    // A still never has one.
    let still = asset_with("/kerf-source-setting.png", vec![vid_stream(true)]);
    assert_eq!(
        resolve_preview_source(&still, PreviewSource::Auto, || panic!("a still has no proxy")),
        PathBuf::from(&still.path)
    );
}

#[test]
fn an_analysis_read_takes_a_ready_proxy_only_where_it_stands_in_for_the_file() {
    use crate::proxy::PreviewSource;
    let asset = asset_with("/kerf-analysis-proxy.mp4", vec![vid_stream(false)]);
    let proxy = PathBuf::from("/cache/proxy.mp4");
    let ready = || Some(proxy.clone());
    let take = |mode, faithful: bool| resolve_analysis_proxy(&asset, mode, ready, || faithful);
    // Auto and Proxy only read a ready, faithful proxy; Always original never looks for one.
    assert_eq!(take(PreviewSource::Auto, true), Some(proxy.clone()));
    assert_eq!(take(PreviewSource::ProxyOnly, true), Some(proxy.clone()));
    assert_eq!(take(PreviewSource::Original, true), None);
    // A proxy that does not stand in for the file (an HDR source's scores, a transport
    // stream's times) is left alone.
    assert_eq!(take(PreviewSource::Auto, false), None);
    // Never waited for: no proxy yet reads the original, and the fidelity is not even probed.
    assert_eq!(
        resolve_analysis_proxy(&asset, PreviewSource::Auto, || None, || panic!("probed a file with no proxy")),
        None
    );
    // A still, or a file with no picture, has none.
    let still = asset_with("/kerf-analysis-proxy.png", vec![vid_stream(true)]);
    let audio = asset_with("/kerf-analysis-proxy.wav", vec![aud_stream()]);
    for asset in [still, audio] {
        assert_eq!(
            resolve_analysis_proxy(&asset, PreviewSource::Auto, || panic!("no proxy exists"), || true),
            None
        );
    }
}

#[test]
fn a_deleted_proxy_is_remembered_by_the_project_and_forgiven_by_a_rebuild() {
    let project = Project::open_in_memory().unwrap();
    let a = project
        .insert_or_get_asset(&asset_with("/kerf-declined-a.mp4", vec![vid_stream(false)]))
        .unwrap();
    let b = project
        .insert_or_get_asset(&asset_with("/kerf-declined-b.mp4", vec![vid_stream(false)]))
        .unwrap();
    assert!(!project.proxy_declined(a.id).unwrap());
    project.set_proxy_declined(a.id, true).unwrap();
    project.set_proxy_declined(a.id, true).unwrap();
    assert!(project.proxy_declined(a.id).unwrap());
    assert!(!project.proxy_declined(b.id).unwrap(), "only the asset that was deleted");
    let inputs = project.proxy_inputs().unwrap();
    assert_eq!(inputs.iter().filter(|i| i.declined).count(), 1);
    assert!(inputs.iter().any(|i| i.asset.id == a.id && i.declined));
    project.set_proxy_declined(a.id, false).unwrap();
    assert!(!project.proxy_declined(a.id).unwrap());
    assert!(project.proxy_inputs().unwrap().iter().all(|i| !i.declined));
}

#[test]
fn proxy_only_waits_on_the_clips_in_range_that_have_no_proxy_and_nothing_else() {
    use crate::proxy::{PreviewSource, ProxyPhase};
    let project = Project::open_in_memory().unwrap();
    let early = project
        .insert_or_get_asset(&asset_with("/kerf-wait-early.mp4", vec![vid_stream(false)]))
        .unwrap();
    let mut late = asset_with("/kerf-wait-late.mp4", vec![vid_stream(false)]);
    late.name = "late.mp4".into();
    let late = project.insert_or_get_asset(&late).unwrap();
    project.cut_clip(early.id, 0.0, 2.0).unwrap();
    project.cut_clip(late.id, 0.0, 2.0).unwrap();
    // Auto and Original never wait: they fall back to the original.
    for source in [PreviewSource::Auto, PreviewSource::Original] {
        assert!(project.proxy_waits_under(source, 0.0, None).unwrap().is_empty());
    }
    // Proxy only waits on every clip in the range whose proxy is not there.
    let all = project.proxy_waits_under(PreviewSource::ProxyOnly, 0.0, None).unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|w| w.status.state == ProxyPhase::Missing));
    // A frame at 3 s is the second clip only (the first ends at 2 s).
    let at = project.proxy_waits_under(PreviewSource::ProxyOnly, 3.0, Some(3.0)).unwrap();
    assert_eq!(at.len(), 1);
    assert_eq!(at[0].asset_id, late.id);
    assert!(at[0].message().contains("late.mp4"), "{}", at[0].message());
    // Nothing under the playhead, nothing to wait for.
    assert!(project
        .proxy_waits_under(PreviewSource::ProxyOnly, 9.0, Some(9.0))
        .unwrap()
        .is_empty());

    // The same answers from a timeline and inputs held outside the project (a snapshot).
    let timeline = project.working_timeline().unwrap();
    let inputs: HashMap<Uuid, crate::proxy::ProxyInput> =
        project.proxy_inputs().unwrap().into_iter().map(|i| (i.asset.id, i)).collect();
    let held = |id: Uuid| inputs.get(&id).cloned().ok_or(Error::AssetNotFound(id));
    for (source, from, to) in [
        (PreviewSource::Auto, 0.0, None),
        (PreviewSource::ProxyOnly, 0.0, None),
        (PreviewSource::ProxyOnly, 3.0, Some(3.0)),
        (PreviewSource::ProxyOnly, 9.0, Some(9.0)),
    ] {
        let live = project.proxy_waits_under(source, from, to).unwrap();
        let kept = Project::proxy_waits_in(&timeline, source, from, to, &held).unwrap();
        assert_eq!(live, kept, "{source:?} {from} {to:?}");
    }
}

#[test]
fn the_assets_the_proxy_queue_ranks_as_used_are_those_with_a_clip_on_a_video_track() {
    let project = Project::open_in_memory().unwrap();
    let used = project
        .insert_or_get_asset(&asset_with("/kerf-rank-used.mp4", vec![vid_stream(false)]))
        .unwrap();
    project
        .insert_or_get_asset(&asset_with("/kerf-rank-unused.mp4", vec![vid_stream(false)]))
        .unwrap();
    assert!(project.proxy_used_assets().unwrap().is_empty());
    project.cut_clip(used.id, 0.0, 2.0).unwrap();
    assert_eq!(
        project.proxy_used_assets().unwrap(),
        std::collections::HashSet::from([used.id])
    );
}

#[test]
fn preview_source_skips_proxy_for_stills_and_audio_only() {
    let image = asset_with("/still.png", vec![vid_stream(true)]);
    let audio = asset_with("/voice.wav", vec![aud_stream()]);
    assert_eq!(Project::preview_source(&image), PathBuf::from(&image.path));
    assert_eq!(Project::preview_source(&audio), PathBuf::from(&audio.path));
}

#[test]
fn preview_source_uses_proxy_once_one_exists() {
    // A unique per-process source path keeps the deterministic proxy path
    // distinct across concurrent test runs (no shared-file race).
    let path = format!("/kerf-test-proxy-source-{}.mp4", std::process::id());
    let asset = asset_with(&path, vec![vid_stream(false)]);
    let width = crate::engine::proxy_width(asset.projection());
    let Some(proxy) = crate::engine::proxy_path(Path::new(&asset.path), width) else {
        return; // no cache dir on this platform — nothing to resolve to
    };
    if let Some(dir) = proxy.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(&proxy, b"stub").unwrap();
    let resolved = Project::preview_source(&asset);
    remove_proxy(&proxy);
    assert_eq!(resolved, proxy);
}

#[test]
fn a_filmstrip_needs_an_asset_with_video() {
    let project = Project::open_in_memory().unwrap();
    // Nothing here exists on disk: the refusal must come before any ffmpeg is
    // spawned, and say what is wrong.
    let voice = project
        .insert_or_get_asset(&asset_with("/nowhere/voice.wav", vec![aud_stream()]))
        .unwrap();
    let err = project.filmstrip(voice.id).unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err:?}");
    assert!(err.to_string().contains("no video stream"), "{err}");
    assert!(matches!(project.filmstrip(Uuid::new_v4()), Err(Error::AssetNotFound(_))));
}

#[test]
fn a_filmstrip_decodes_the_proxy_only_once_it_exists() {
    let project = Project::open_in_memory().unwrap();
    // A unique per-process source path keeps the deterministic proxy path
    // distinct across concurrent test runs (no shared-file race).
    let path = format!("/kerf-test-filmstrip-source-{}.mp4", std::process::id());
    let asset = project
        .insert_or_get_asset(&asset_with(&path, vec![vid_stream(false)]))
        .unwrap();
    assert_eq!(
        Project::filmstrip_proxy(&asset),
        None,
        "no proxy yet: the original is decoded"
    );

    let width = crate::engine::proxy_width(asset.projection());
    let Some(proxy_file) = crate::engine::proxy_path(Path::new(&asset.path), width) else {
        return; // no cache dir on this platform — nothing to resolve to
    };
    if let Some(dir) = proxy_file.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(&proxy_file, b"stub").unwrap();
    let resolved = Project::filmstrip_proxy(&asset);
    remove_proxy(&proxy_file);
    assert_eq!(resolved, Some(proxy_file), "the ready proxy is preferred");

    // A still never has a proxy, so it resolves to the original.
    let still_path = format!("/kerf-test-filmstrip-still-{}.png", std::process::id());
    let still = project
        .insert_or_get_asset(&asset_with(&still_path, vec![vid_stream(true)]))
        .unwrap();
    assert_eq!(Project::filmstrip_proxy(&still), None);
}

/// `cargo test -p kerf-core --no-default-features -- --ignored filmstrip_of_an_imported`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn filmstrip_of_an_imported_asset_is_built_without_the_project_lock_and_then_cached() {
    use crate::engine::test_support::StatusBounded;
    let ffmpeg = std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
    let dir = std::env::temp_dir().join(format!("kerf-filmstrip-project-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let clip = dir.join("clip.mp4");
    let made = std::process::Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=640x360:rate=25:duration=6")
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&clip)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());

    let project = Project::open_in_memory().unwrap();
    let asset = project.import_asset(&clip).unwrap();
    let strip = project.filmstrip(asset.id).unwrap();
    // 6 s at 0.5 s; 640x360 is 16:9 -> 170x96, 12 of them on one sheet.
    assert_eq!(
        (
            strip.interval,
            strip.frames,
            strip.frame_width,
            strip.frame_height,
            strip.sheets.len()
        ),
        (0.5, 12, 170, 96, 1)
    );
    assert!(strip.sheets[0].jpeg.starts_with(&[0xFF, 0xD8]), "a JPEG");

    // The surface's shape: resolve the asset under the lock, decode with it
    // released. The second ask is the memoized strip, not a second decode.
    let resolved = project.require_asset(asset.id).unwrap();
    let again = Project::decode_filmstrip(&resolved).unwrap();
    assert!(std::sync::Arc::ptr_eq(&strip, &again));
    // The strip went to the real cache under a path unique to this run.
    if let Some(entry) = crate::engine::cached_filmstrip_dir(&resolved) {
        assert!(entry.join("manifest.json").is_file(), "{entry:?}");
        let _ = std::fs::remove_dir_all(entry);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn trim_with_timeline_start_keeps_the_right_edge_put() {
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
    project.insert_asset(&asset).unwrap();

    // 4s clip at t=2 (source 3..7); a left-edge trim tightens the source
    // in-point and moves the start in one edit, so the end stays at t=6.
    let clip = project.add_clip_to_timeline(asset.id, None, 3.0, 7.0, Some(2.0)).unwrap();
    let trimmed = project.trim(clip.id, Some(4.0), None, Some(3.0)).unwrap();
    assert!((trimmed.timeline_start - 3.0).abs() < 1e-9);
    assert!((trimmed.timeline_end() - 6.0).abs() < 1e-9);

    let history = project.history().unwrap();
    assert_eq!(history.last().unwrap().label, "Trim clip");
}

#[test]
fn cut_clip_range_splits_and_ripples() {
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
    project.insert_asset(&asset).unwrap();
    // Two 10s clips back to back; cut source 4..6 out of the first.
    let a = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
    let b = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
    let pieces = project.cut_clip_range(a.id, 4.0, 6.0).unwrap();
    assert_eq!(pieces.len(), 2);
    assert!((pieces[0].source_out - 4.0).abs() < 1e-9);
    assert!((pieces[1].source_in - 6.0).abs() < 1e-9);
    assert!((pieces[1].timeline_start - 4.0).abs() < 1e-9);
    // The following clip rippled left by the removed 2 seconds.
    let timeline = project.timeline().unwrap();
    let moved = timeline.clip(b.id).unwrap();
    assert!((moved.timeline_start - 8.0).abs() < 1e-9, "{}", moved.timeline_start);
}

#[test]
fn mute_solo_lock_and_clip_enable_persist_and_gate_the_render() {
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
    project.insert_asset(&asset).unwrap();
    let clip = project.cut_clip(asset.id, 0.0, 5.0).unwrap();
    let tl = project.timeline().unwrap();
    let vid = tl.tracks.iter().find(|t| t.kind == StreamKind::Video).unwrap().id;

    // Each flag round-trips through the JSON blob independently.
    assert!(project.set_track_muted(vid, true).unwrap().muted);
    assert!(project.set_track_solo(vid, true).unwrap().solo);
    assert!(project.set_track_locked(vid, true).unwrap().locked);
    let saved = project.timeline().unwrap();
    let t = saved.track(vid).unwrap();
    assert!(t.muted && t.solo && t.locked);

    // Muted wins over soloed, so nothing reaches the graph.
    assert!(saved.for_render().track(vid).unwrap().clips.is_empty());

    // Unmuting brings it back; locking is an editing guard, not a render gate.
    project.set_track_muted(vid, false).unwrap();
    let saved = project.timeline().unwrap();
    assert_eq!(saved.for_render().track(vid).unwrap().clips.len(), 1);

    // Disabling the clip drops it while leaving it on the timeline — once its track is
    // unlocked: whether a clip plays is the lock's to guard.
    assert!(project.set_clip_enabled(clip.id, false).is_err());
    project.set_track_locked(vid, false).unwrap();
    assert!(!project.set_clip_enabled(clip.id, false).unwrap().enabled);
    let saved = project.timeline().unwrap();
    assert_eq!(saved.track(vid).unwrap().clips.len(), 1, "still on the timeline");
    assert!(saved.for_render().track(vid).unwrap().clips.is_empty(), "but not rendered");

    // Every one of those was a labelled, revertible edit.
    let labels: Vec<_> = project.history().unwrap().iter().map(|r| r.label.clone()).collect();
    for want in ["Mute track", "Solo track", "Lock track", "Unmute track", "Disable clip"] {
        assert!(labels.contains(&want.to_string()), "missing {want} in {labels:?}");
    }
}

#[test]
fn markers_stay_sorted_and_round_trip() {
    let project = Project::open_in_memory().unwrap();
    // Added out of order; the store keeps them sorted so the UI never re-sorts.
    project.add_marker(9.0, "late".into(), None).unwrap();
    let mid = project.add_marker(4.0, "middle".into(), Some("#ff0000".into())).unwrap();
    project.add_marker(1.0, "early".into(), None).unwrap();
    let names: Vec<_> = project.timeline().unwrap().markers.iter().map(|m| m.name.clone()).collect();
    assert_eq!(names, ["early", "middle", "late"]);

    // Moving one re-sorts, renaming sticks, and an empty color clears it.
    let moved = project
        .update_marker(mid.id, Some(12.0), Some("moved".into()), Some(String::new()))
        .unwrap();
    assert!(moved.color.is_none());
    let names: Vec<_> = project.timeline().unwrap().markers.iter().map(|m| m.name.clone()).collect();
    assert_eq!(names, ["early", "late", "moved"]);

    project.remove_marker(mid.id).unwrap();
    assert_eq!(project.timeline().unwrap().markers.len(), 2);
    assert!(project.remove_marker(mid.id).is_err(), "removing twice must fail");
    assert!(project.add_marker(-1.0, "bad".into(), None).is_err());
}

#[test]
fn duplicate_clips_preserves_everything_and_relative_offsets() {
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
    project.insert_asset(&asset).unwrap();
    // Two clips, 2s apart, the first carrying non-default properties.
    let a = project.add_clip_to_timeline(asset.id, None, 0.0, 2.0, Some(0.0)).unwrap();
    let b = project.add_clip_to_timeline(asset.id, None, 5.0, 6.0, Some(4.0)).unwrap();
    project.set_volume(a.id, 0.25).unwrap();
    project.set_speed(a.id, 2.0).unwrap();
    project
        .set_video_effects(a.id, vec![VideoEffect::Blur { sigma: 3.0 }])
        .unwrap();

    let copies = project.duplicate_clips(&[a.id, b.id], 20.0).unwrap();
    assert_eq!(copies.len(), 2);
    // The earliest lands on `at`, and the gap between them survives.
    assert!((copies[0].timeline_start - 20.0).abs() < 1e-9);
    assert!((copies[1].timeline_start - 24.0).abs() < 1e-9);
    // Fresh identities, but everything else carried over — which is exactly
    // what add_clip_to_timeline cannot do.
    assert_ne!(copies[0].id, a.id);
    assert!((copies[0].volume - 0.25).abs() < 1e-6);
    assert!((copies[0].speed - 2.0).abs() < 1e-9);
    assert_eq!(copies[0].effects.len(), 1);
    assert_eq!(project.timeline().unwrap().tracks[0].clips.len(), 4);

    // Overlapping an existing clip is rejected outright, leaving nothing behind.
    let before = project.timeline().unwrap().tracks[0].clips.len();
    assert!(project.duplicate_clips(&[a.id], 20.5).is_err());
    assert_eq!(project.timeline().unwrap().tracks[0].clips.len(), before, "no partial paste");

    assert!(project.duplicate_clips(&[], 0.0).is_err());
    assert!(project.duplicate_clips(&[Uuid::new_v4()], 30.0).is_err());
}

/// The point of `insert_clips` taking values rather than ids: cut-then-paste,
/// where the source clip no longer exists by the time the paste happens.
#[test]
fn insert_clips_pastes_clips_whose_sources_are_already_gone() {
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
    project.insert_asset(&asset).unwrap();
    let clip = project.add_clip_to_timeline(asset.id, None, 0.0, 3.0, Some(0.0)).unwrap();
    project.set_volume(clip.id, 0.5).unwrap();

    // Snapshot it the way a clipboard would, then cut it.
    let tl = project.timeline().unwrap();
    let track_id = tl.tracks[0].id;
    let snapshot = tl.clip(clip.id).unwrap().clone();
    project.remove(clip.id).unwrap();
    assert!(project.timeline().unwrap().tracks[0].clips.is_empty());

    // Pasting still works, and the copy is a new identity carrying the edits.
    let pasted = project.insert_clips(&[(track_id, snapshot)], 10.0).unwrap();
    assert_eq!(pasted.len(), 1);
    assert_ne!(pasted[0].id, clip.id);
    assert!((pasted[0].timeline_start - 10.0).abs() < 1e-9);
    assert!((pasted[0].volume - 0.5).abs() < 1e-6);

    // Pasting the same clipboard again is fine — each insert re-ids.
    let again = project.insert_clips(&[(track_id, pasted[0].clone())], 20.0).unwrap();
    assert_ne!(again[0].id, pasted[0].id);
    assert_eq!(project.timeline().unwrap().tracks[0].clips.len(), 2);

    assert!(project.insert_clips(&[(Uuid::new_v4(), pasted[0].clone())], 30.0).is_err());
}

#[test]
fn split_and_remove_roundtrip() {
    let project = Project::open_in_memory().unwrap();
    let asset = Asset {
        id: Uuid::new_v4(),
        path: "/x.mp4".into(),
        name: "x.mp4".into(),
        duration: 10.0,
        streams: vec![StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(1280),
            height: Some(720),
            fps: Some(25.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: None,
            color_space: None,
            color_range: None,
            lead: 0.0,
            time_base: None,
        }],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    project.insert_asset(&asset).unwrap();

    let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
    let (left, right) = project.split_at(clip.id, 4.0).unwrap();
    assert!((left.duration() - 4.0).abs() < 1e-9);
    assert!((right.duration() - 6.0).abs() < 1e-9);

    project.remove(right.id).unwrap();
    assert!(project.timeline().unwrap().clip(right.id).is_none());
}

#[test]
fn text_overlay_add_update_remove_roundtrip() {
    let project = Project::open_in_memory().unwrap();
    let o = project.add_overlay("Hello".into(), 1.0, 4.0).unwrap();
    assert_eq!(project.timeline().unwrap().overlays.len(), 1);
    let updated = project
        .update_overlay(
            o.id,
            Some("Hi".into()),
            None,
            Some(5.0),
            None,
            None,
            None,
            None,
            Some("black@0.5".into()),
            Some("Arial".into()),
            Some(true),
        )
        .unwrap();
    assert_eq!(updated.text, "Hi");
    assert!((updated.end - 5.0).abs() < 1e-9);
    assert_eq!(updated.bg.as_deref(), Some("black@0.5"));
    assert_eq!(updated.font.as_deref(), Some("Arial"));
    assert!(updated.bold);
    // An empty bg / font string clears it back to the default.
    let cleared = project
        .update_overlay(
            o.id,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(String::new()),
            Some(String::new()),
            None,
        )
        .unwrap();
    assert!(cleared.bg.is_none());
    assert!(cleared.font.is_none());
    project.remove_overlay(o.id).unwrap();
    assert!(project.timeline().unwrap().overlays.is_empty());
    assert!(project.remove_overlay(o.id).is_err());
}

fn svg_fixture(dur: &str) -> String {
    format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 50\"><rect width=\"10\" height=\"10\"><animate attributeName=\"x\" from=\"0\" to=\"90\" dur=\"{dur}\" fill=\"freeze\"/></rect></svg>"
        )
}

fn svg_layout() -> SvgLayout {
    SvgLayout::default()
}

#[test]
fn an_animated_title_starts_and_ends_within_a_day() {
    let project = Project::open_in_memory().unwrap();
    let add =
        |start: f64, end: Option<f64>| project.add_svg_overlay(SvgSource::Custom(svg_fixture("1s")), start, end, svg_layout());
    for (start, end) in [
        (1e300, None),
        (86_401.0, None),
        (0.0, Some(86_400.5)),
        (0.0, Some(1e300)),
        (86_400.0, Some(86_401.0)),
        (f64::INFINITY, None),
    ] {
        assert!(
            matches!(add(start, end), Err(Error::InvalidArgument(_))),
            "{start} .. {end:?}"
        );
    }
    assert!(project.timeline().unwrap().svg_overlays.is_empty(), "nothing was written");
    let last = add(86_399.0, Some(86_400.0)).unwrap().overlay;
    assert_eq!((last.start, last.end), (86_399.0, 86_400.0));
    // An update is held to the same, and a refused one writes nothing.
    let before = project.history().unwrap().len();
    for patch in [
        SvgOverlayPatch {
            end: Some(86_400.5),
            ..Default::default()
        },
        SvgOverlayPatch {
            start: Some(1e300),
            end: Some(2e300),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            project.update_svg_overlay(last.id, patch),
            Err(Error::InvalidArgument(_))
        ));
    }
    assert_eq!(project.history().unwrap().len(), before);
    assert!(matches!(
        SvgOverlayPatch {
            end: Some(1e18),
            ..Default::default()
        }
        .check_cheap(),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn svg_overlay_add_update_remove_roundtrip() {
    let project = Project::open_in_memory().unwrap();
    let added = project
        .add_svg_overlay(SvgSource::Custom(svg_fixture("3s")), 1.0, None, svg_layout())
        .unwrap();
    assert!(added.warnings.is_empty(), "{:?}", added.warnings);
    let o = added.overlay;
    assert_eq!((o.start, o.end), (1.0, 4.0), "the end is when the animation has finished");
    assert_eq!(
        (o.x, o.y, o.width, o.height, o.opacity, o.offset),
        (0.0, 0.0, 1.0, 1.0, 1.0, 0.0)
    );
    assert!(o.preset.is_none() && o.name.is_empty());
    assert_eq!(
        project.timeline().unwrap().svg_overlays,
        vec![o.clone()],
        "stored with the timeline"
    );
    assert_eq!(project.history().unwrap().last().unwrap().label, "Add animated title");

    let updated = project
        .update_svg_overlay(
            o.id,
            SvgOverlayPatch {
                name: Some("  Logo  ".into()),
                start: Some(2.0),
                end: Some(6.5),
                x: Some(0.1),
                y: Some(0.2),
                width: Some(0.5),
                height: Some(0.25),
                opacity: Some(0.8),
                ..Default::default()
            },
        )
        .unwrap()
        .overlay;
    assert_eq!(updated.name, "Logo");
    assert_eq!((updated.start, updated.end), (2.0, 6.5));
    assert_eq!(
        (updated.x, updated.y, updated.width, updated.height, updated.opacity),
        (0.1, 0.2, 0.5, 0.25, 0.8)
    );
    assert_eq!(updated.svg, o.svg, "the artwork is untouched by a retime");
    assert_eq!(project.timeline().unwrap().svg_overlay(o.id), Some(&updated));

    let revisions = project.history().unwrap().len();
    for bad in [
        SvgOverlayPatch {
            end: Some(1.0),
            ..Default::default()
        },
        SvgOverlayPatch {
            opacity: Some(2.0),
            ..Default::default()
        },
        SvgOverlayPatch {
            width: Some(0.0),
            ..Default::default()
        },
        SvgOverlayPatch {
            x: Some(f64::NAN),
            ..Default::default()
        },
        SvgOverlayPatch {
            start: Some(f64::INFINITY),
            ..Default::default()
        },
        SvgOverlayPatch {
            svg: Some("<nope/>".into()),
            ..Default::default()
        },
        SvgOverlayPatch {
            preset_params: Some(SvgPresetParams::new("x")),
            ..Default::default()
        },
        SvgOverlayPatch {
            svg: Some(svg_fixture("1s")),
            preset_params: Some(SvgPresetParams::new("x")),
            ..Default::default()
        },
    ] {
        assert!(
            matches!(project.update_svg_overlay(o.id, bad.clone()), Err(Error::InvalidArgument(_))),
            "{bad:?}"
        );
    }
    assert_eq!(
        project.history().unwrap().len(),
        revisions,
        "a refused edit writes no revision"
    );
    assert_eq!(project.timeline().unwrap().svg_overlay(o.id), Some(&updated));

    let swapped = project
        .update_svg_overlay(
            o.id,
            SvgOverlayPatch {
                svg: Some(format!("{}<script>x</script>", svg_fixture("1s").trim_end_matches("</svg>")) + "</svg>"),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(!swapped.overlay.svg.contains("script") && swapped.overlay.svg.contains("dur=\"1s\""));
    assert!(
        swapped.warnings.iter().any(|w| w.contains("script")),
        "{:?}",
        swapped.warnings
    );

    project.remove_svg_overlay(o.id).unwrap();
    assert!(project.timeline().unwrap().svg_overlays.is_empty());
    assert!(matches!(project.remove_svg_overlay(o.id), Err(Error::SvgOverlayNotFound(id)) if id == o.id));
    assert!(matches!(
        project.update_svg_overlay(o.id, SvgOverlayPatch::default()),
        Err(Error::SvgOverlayNotFound(_))
    ));
}

#[test]
fn a_checked_source_adds_and_updates_what_the_plain_methods_do() {
    let direct = Project::open_in_memory().unwrap();
    let split = Project::open_in_memory().unwrap();
    let layout = SvgLayout {
        name: Some(" Logo ".into()),
        x: Some(0.25),
        ..SvgLayout::default()
    };
    let a = direct
        .add_svg_overlay(SvgSource::Custom(svg_fixture("3s")), 1.0, None, layout.clone())
        .unwrap();
    // The check is the part that parses; what takes the project's lock after it does not.
    let checked = SvgSource::Custom(svg_fixture("3s")).check().unwrap();
    assert!(matches!(&checked, SvgChecked::Custom(doc) if doc.natural_duration() == Some(3.0)));
    let b = split.add_svg_overlay_parsed(checked, 1.0, None, layout).unwrap();
    let same = |x: &SvgOverlay, y: &SvgOverlay| SvgOverlay { id: y.id, ..x.clone() } == *y;
    assert!(same(&a.overlay, &b.overlay), "{:?} vs {:?}", a.overlay, b.overlay);
    assert_eq!(a.warnings, b.warnings);
    assert_eq!(split.history().unwrap().last().unwrap().label, "Add animated title");

    // Bad sources are refused by the check, with the lock not yet taken.
    assert!(matches!(
        SvgSource::Custom("<nope/>".into()).check(),
        Err(Error::InvalidArgument(_))
    ));
    let mut preset = SvgPreset {
        kind: crate::model::SvgPresetKind::FadeUp,
        params: SvgPresetParams::new("Hi"),
    };
    assert!(matches!(SvgSource::Preset(preset.clone()).check(), Ok(SvgChecked::Preset(_))));
    preset.params.color = "not a colour".into();
    assert!(matches!(SvgSource::Preset(preset).check(), Err(Error::InvalidArgument(_))));

    // An update: the patch is checked first, and its answer is the artwork.
    let id = b.overlay.id;
    let patch = SvgOverlayPatch {
        svg: Some(svg_fixture("1s")),
        end: Some(9.0),
        ..Default::default()
    };
    let doc = patch.check().unwrap();
    assert!(doc.as_ref().is_some_and(|d| d.natural_duration() == Some(1.0)));
    let updated = split.update_svg_overlay_parsed(id, patch.clone(), doc).unwrap();
    let plain = direct.update_svg_overlay(a.overlay.id, patch).unwrap();
    assert!(same(&plain.overlay, &updated.overlay));
    assert!(updated.overlay.svg.contains("dur=\"1s\""));
    // A patch with no artwork has nothing to check; a bad one is refused before the lock.
    assert!(SvgOverlayPatch::default().check().unwrap().is_none());
    for bad in [
        SvgOverlayPatch {
            svg: Some("<nope/>".into()),
            ..Default::default()
        },
        SvgOverlayPatch {
            svg: Some(svg_fixture("1s")),
            preset_params: Some(SvgPresetParams::new("x")),
            ..Default::default()
        },
        SvgOverlayPatch {
            start: Some(f64::NAN),
            ..Default::default()
        },
        SvgOverlayPatch {
            preset_params: Some(SvgPresetParams {
                accent: "x".repeat(100),
                ..SvgPresetParams::new("x")
            }),
            ..Default::default()
        },
    ] {
        assert!(matches!(bad.check(), Err(Error::InvalidArgument(_))), "{bad:?}");
        assert!(matches!(
            split.update_svg_overlay_parsed(id, bad, None),
            Err(Error::InvalidArgument(_))
        ));
    }
    // With a doc in hand the cheap checks still run, and the patch's own svg is not parsed again.
    let with_doc = SvgOverlayPatch {
        svg: Some("<not even xml".into()),
        ..Default::default()
    };
    let doc = SvgDoc::parse(&svg_fixture("2s")).unwrap();
    let swapped = split.update_svg_overlay_parsed(id, with_doc, Some(doc)).unwrap();
    assert!(swapped.overlay.svg.contains("dur=\"2s\""));
    assert!(split
        .update_svg_overlay_parsed(
            id,
            SvgOverlayPatch {
                svg: Some(svg_fixture("1s")),
                preset_params: Some(SvgPresetParams::new("x")),
                ..Default::default()
            },
            Some(SvgDoc::parse(&svg_fixture("1s")).unwrap())
        )
        .is_err());
}

#[test]
fn svg_overlay_end_defaults_to_the_natural_duration_held_to_two_to_thirty_seconds() {
    let project = Project::open_in_memory().unwrap();
    let still = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"><rect width=\"5\" height=\"5\"/></svg>";
    let looping = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"><rect><animate attributeName=\"x\" to=\"5\" dur=\"1s\" repeatCount=\"indefinite\"/></rect></svg>";
    for (src, want) in [
        (svg_fixture("3.5s"), 3.5),
        (svg_fixture("0.5s"), 2.0),
        (svg_fixture("100s"), 30.0),
        (still.to_string(), 4.0),
        (looping.to_string(), 4.0),
    ] {
        let o = project
            .add_svg_overlay(SvgSource::Custom(src), 10.0, None, svg_layout())
            .unwrap()
            .overlay;
        assert!((o.end - 10.0 - want).abs() < 1e-9, "{} for {want}", o.end - 10.0);
    }
    let explicit = project
        .add_svg_overlay(SvgSource::Custom(svg_fixture("3s")), 0.0, Some(9.0), svg_layout())
        .unwrap()
        .overlay;
    assert_eq!(explicit.end, 9.0);
    for (start, end) in [(1.0, Some(1.0)), (1.0, Some(0.5)), (f64::NAN, None), (0.0, Some(f64::NAN))] {
        assert!(
            matches!(
                project.add_svg_overlay(SvgSource::Custom(svg_fixture("3s")), start, end, svg_layout()),
                Err(Error::InvalidArgument(_))
            ),
            "{start} {end:?}"
        );
    }
    let early = project
        .add_svg_overlay(SvgSource::Custom(svg_fixture("3s")), -5.0, None, svg_layout())
        .unwrap();
    assert_eq!(early.overlay.start, 0.0, "a negative start is clamped like a text overlay's");
}

#[test]
fn svg_overlay_refuses_what_cannot_be_drawn_and_writes_nothing() {
    let project = Project::open_in_memory().unwrap();
    let revisions = project.history().unwrap().len();
    for src in [
        "",
        "not xml",
        "<html/>",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        "<svg viewBox=\"0 0 1 1\"/>",
    ] {
        let r = project.add_svg_overlay(SvgSource::Custom(src.into()), 0.0, None, svg_layout());
        assert!(matches!(r, Err(Error::InvalidArgument(_))), "{src:?}");
    }
    for layout in [
        SvgLayout {
            width: Some(-1.0),
            ..Default::default()
        },
        SvgLayout {
            opacity: Some(1.5),
            ..Default::default()
        },
        SvgLayout {
            x: Some(f64::INFINITY),
            ..Default::default()
        },
        SvgLayout {
            height: Some(f64::NAN),
            ..Default::default()
        },
    ] {
        let r = project.add_svg_overlay(SvgSource::Custom(svg_fixture("1s")), 0.0, None, layout);
        assert!(matches!(r, Err(Error::InvalidArgument(_))));
    }
    assert!(project.timeline().unwrap().svg_overlays.is_empty());
    assert_eq!(project.history().unwrap().len(), revisions);
}

#[test]
fn svg_overlay_count_is_capped() {
    let project = Project::open_in_memory().unwrap();
    for _ in 0..MAX_SVG_OVERLAYS {
        project
            .add_svg_overlay(SvgSource::Custom(svg_fixture("1s")), 0.0, None, svg_layout())
            .unwrap();
    }
    let err = project
        .add_svg_overlay(SvgSource::Custom(svg_fixture("1s")), 0.0, None, svg_layout())
        .unwrap_err();
    assert!(matches!(&err, Error::InvalidArgument(m) if m.contains("50")), "{err}");
    assert_eq!(project.timeline().unwrap().svg_overlays.len(), MAX_SVG_OVERLAYS);
}

#[test]
fn preset_titles_follow_their_length_params_and_frame() {
    use crate::model::SvgPresetKind;
    let project = Project::open_in_memory().unwrap();
    let preset = SvgPreset {
        kind: SvgPresetKind::FadeUp,
        params: SvgPresetParams::new("Hello"),
    };
    let added = project
        .add_svg_overlay(SvgSource::Preset(preset.clone()), 0.0, None, svg_layout())
        .unwrap();
    let o = added.overlay;
    assert!(added.warnings.is_empty());
    assert_eq!((o.start, o.end), (0.0, 4.0));
    assert_eq!(o.preset.as_ref(), Some(&preset));
    assert!(
        o.svg.contains("viewBox=\"0 0 1920 1080\"") && o.svg.contains(">Hello</text>"),
        "{}",
        o.svg
    );
    assert!(
        o.svg.contains("begin=\"3.6s\""),
        "the exit is timed against the span: {}",
        o.svg
    );

    let longer = project
        .update_svg_overlay(
            o.id,
            SvgOverlayPatch {
                end: Some(8.0),
                ..Default::default()
            },
        )
        .unwrap()
        .overlay;
    assert!(
        longer.svg.contains("begin=\"7.6s\"") && !longer.svg.contains("begin=\"3.6s\""),
        "{}",
        longer.svg
    );
    let moved = project
        .update_svg_overlay(
            o.id,
            SvgOverlayPatch {
                start: Some(5.0),
                end: Some(13.0),
                ..Default::default()
            },
        )
        .unwrap()
        .overlay;
    assert_eq!(moved.svg, longer.svg, "a move that keeps the length keeps the artwork");

    let reworded = project
        .update_svg_overlay(
            o.id,
            SvgOverlayPatch {
                preset_params: Some(SvgPresetParams {
                    subtitle: Some("World".into()),
                    ..SvgPresetParams::new("Bye")
                }),
                ..Default::default()
            },
        )
        .unwrap()
        .overlay;
    assert!(reworded.svg.contains(">Bye</text>") && reworded.svg.contains(">World</text>") && !reworded.svg.contains("Hello"));
    assert_eq!(reworded.preset.as_ref().unwrap().params.text, "Bye");

    project
        .set_delivery_format(Some(Delivery::new(1080, 1920, crate::engine::Fit::Cover)))
        .unwrap();
    let vertical = project.timeline().unwrap().svg_overlay(o.id).unwrap().clone();
    assert!(vertical.svg.contains("viewBox=\"0 0 1080 1920\""), "{}", vertical.svg);
    project.set_delivery_format(None).unwrap();
    assert_eq!(project.timeline().unwrap().svg_overlay(o.id).unwrap().svg, reworded.svg);

    project
        .set_delivery_format(Some(Delivery::new(1080, 1080, crate::engine::Fit::Cover)))
        .unwrap();
    let square = project
        .add_svg_overlay(SvgSource::Preset(preset), 0.0, None, svg_layout())
        .unwrap()
        .overlay;
    assert!(
        square.svg.contains("viewBox=\"0 0 1080 1080\""),
        "a new preset is drawn for the delivery frame"
    );

    let custom = project
        .update_svg_overlay(
            o.id,
            SvgOverlayPatch {
                svg: Some(svg_fixture("2s")),
                ..Default::default()
            },
        )
        .unwrap()
        .overlay;
    assert!(custom.preset.is_none(), "replacing the artwork converts it to custom");
    assert!(matches!(
        project.update_svg_overlay(
            o.id,
            SvgOverlayPatch {
                preset_params: Some(SvgPresetParams::new("x")),
                ..Default::default()
            }
        ),
        Err(Error::InvalidArgument(_))
    ));
    project
        .set_delivery_format(Some(Delivery::new(1080, 1920, crate::engine::Fit::Cover)))
        .unwrap();
    assert_eq!(
        project.timeline().unwrap().svg_overlay(o.id).unwrap().svg,
        custom.svg,
        "custom artwork is never re-made"
    );
}

#[test]
fn switching_a_preset_kind_is_one_edit_that_keeps_the_title_in_place() {
    use crate::model::SvgPresetKind;
    let project = Project::open_in_memory().unwrap();
    let add = |text: &str| {
        let layout = SvgLayout {
            name: Some(format!("name {text}")),
            x: Some(0.1),
            ..SvgLayout::default()
        };
        project
            .add_svg_overlay(
                SvgSource::Preset(SvgPreset {
                    kind: SvgPresetKind::FadeUp,
                    params: SvgPresetParams::new(text),
                }),
                2.0,
                Some(7.0),
                layout,
            )
            .unwrap()
            .overlay
    };
    let (first, second) = (add("A"), add("B"));
    let revisions = project.history().unwrap().len();

    let switched = project
        .update_svg_overlay(
            first.id,
            SvgOverlayPatch {
                preset_kind: Some(SvgPresetKind::LowerThird),
                ..Default::default()
            },
        )
        .unwrap()
        .overlay;
    assert_eq!(project.history().unwrap().len(), revisions + 1, "one edit");
    assert_eq!(project.history().unwrap().last().unwrap().label, "Update animated title");
    assert_eq!(
        switched.preset.as_ref().map(|p| (p.kind, p.params.text.as_str())),
        Some((SvgPresetKind::LowerThird, "A")),
        "the kind changes, the params stay"
    );
    assert_eq!(
        (switched.id, switched.name.as_str(), switched.start, switched.end, switched.x),
        (first.id, "name A", 2.0, 7.0, 0.1)
    );
    let frame = project.timeline().unwrap().preset_frame();
    assert_eq!(
        switched.svg,
        crate::svg::render_preset(SvgPresetKind::LowerThird, &SvgPresetParams::new("A"), 5.0, frame),
        "the artwork is re-made for the new kind and the same span"
    );
    assert_ne!(switched.svg, first.svg);
    let order: Vec<_> = project.timeline().unwrap().svg_overlays.iter().map(|o| o.id).collect();
    assert_eq!(order, [first.id, second.id], "the draw order is kept");

    // The kind and the params together are one edit as well.
    let both = project
        .update_svg_overlay(
            first.id,
            SvgOverlayPatch {
                preset_kind: Some(SvgPresetKind::WordPop),
                preset_params: Some(SvgPresetParams::new("C")),
                ..Default::default()
            },
        )
        .unwrap()
        .overlay;
    assert_eq!(
        both.preset.as_ref().map(|p| (p.kind, p.params.text.as_str())),
        Some((SvgPresetKind::WordPop, "C"))
    );
    assert_eq!(project.history().unwrap().len(), revisions + 2);

    // Refused: a custom title has no kind, and artwork of your own is not a kind.
    let custom = project
        .add_svg_overlay(SvgSource::Custom(svg_fixture("1s")), 0.0, None, svg_layout())
        .unwrap()
        .overlay;
    let revisions = project.history().unwrap().len();
    let on_custom = SvgOverlayPatch {
        preset_kind: Some(SvgPresetKind::Typewriter),
        ..Default::default()
    };
    assert!(matches!(
        project.update_svg_overlay(custom.id, on_custom),
        Err(Error::InvalidArgument(m)) if m.contains("custom")
    ));
    let with_svg = SvgOverlayPatch {
        svg: Some(svg_fixture("1s")),
        preset_kind: Some(SvgPresetKind::Typewriter),
        ..Default::default()
    };
    assert!(matches!(with_svg.check(), Err(Error::InvalidArgument(_))));
    assert!(matches!(
        project.update_svg_overlay_parsed(first.id, with_svg, Some(SvgDoc::parse(&svg_fixture("1s")).unwrap())),
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(project.history().unwrap().len(), revisions, "a refused switch writes nothing");
    assert_eq!(project.timeline().unwrap().svg_overlay(first.id), Some(&both));
    assert_eq!(project.timeline().unwrap().svg_overlay(custom.id), Some(&custom));
}

#[test]
fn preset_params_are_checked_on_add_and_update() {
    use crate::model::SvgPresetKind;
    let project = Project::open_in_memory().unwrap();
    let bad = |params: SvgPresetParams| {
        SvgSource::Preset(SvgPreset {
            kind: SvgPresetKind::Typewriter,
            params,
        })
    };
    for params in [
        SvgPresetParams::new("  "),
        SvgPresetParams {
            color: "nope".into(),
            ..SvgPresetParams::new("x")
        },
        SvgPresetParams::new("x".repeat(201)),
    ] {
        assert!(matches!(
            project.add_svg_overlay(bad(params), 0.0, None, svg_layout()),
            Err(Error::InvalidArgument(_))
        ));
    }
    let o = project
        .add_svg_overlay(bad(SvgPresetParams::new("ok")), 0.0, None, svg_layout())
        .unwrap()
        .overlay;
    let r = project.update_svg_overlay(
        o.id,
        SvgOverlayPatch {
            preset_params: Some(SvgPresetParams::new("")),
            ..Default::default()
        },
    );
    assert!(matches!(r, Err(Error::InvalidArgument(_))));
    assert!(project.timeline().unwrap().svg_overlays.len() == 1);
}

#[test]
fn svg_overlay_edits_stage_for_an_agent() {
    let project = agent_project();
    let revisions = project.history().unwrap().len();
    project.begin_staging(None, None, None).unwrap();
    let added = project
        .add_svg_overlay(
            SvgSource::Custom(svg_fixture("3s")),
            1.0,
            Some(5.0),
            SvgLayout {
                name: Some("Logo".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .overlay;
    assert!(
        project.timeline().unwrap().svg_overlays.is_empty(),
        "the live cut has not moved"
    );
    assert_eq!(
        project.working_timeline().unwrap().svg_overlays.len(),
        1,
        "the agent sees its work"
    );
    let staged = project.staged().unwrap().unwrap();
    let entry = staged
        .diff
        .entries
        .iter()
        .find(|e| e.kind == DiffKind::OverlayAdded)
        .expect("the diff names it");
    assert_eq!(entry.overlay_id, Some(added.id));
    assert_eq!(entry.summary, "Added animated title “Logo” at 0:01.0–0:05.0");

    project
        .update_svg_overlay(
            added.id,
            SvgOverlayPatch {
                end: Some(6.0),
                opacity: Some(0.5),
                ..Default::default()
            },
        )
        .unwrap();
    let entries = project.staged().unwrap().unwrap().diff.entries;
    assert_eq!(
        entries.iter().filter(|e| e.overlay_id == Some(added.id)).count(),
        1,
        "still one entry for it"
    );
    assert_eq!(project.history().unwrap().len(), revisions);

    let landed = project.apply_staged(false).unwrap();
    assert_eq!(landed.svg_overlays.len(), 1);
    assert_eq!((landed.svg_overlays[0].end, landed.svg_overlays[0].opacity), (6.0, 0.5));
    assert_eq!(project.timeline().unwrap().svg_overlays, landed.svg_overlays);
    let history = project.history().unwrap();
    assert_eq!(history.len(), revisions + 1);
    assert_eq!(history.last().unwrap().source, EditSource::Agent);
}

#[test]
fn an_animated_title_is_not_text_for_the_platform_check() {
    let project = Project::open_in_memory().unwrap();
    project
        .add_svg_overlay(SvgSource::Custom(svg_fixture("3s")), 0.0, None, svg_layout())
        .unwrap();
    let summary = project.cut_summary_in(Some((1920, 1080)), None).unwrap();
    assert!(!summary.has_text, "has_text stays about drawtext overlays");
    project.add_overlay("Title".into(), 0.0, 2.0).unwrap();
    assert!(project.cut_summary_in(Some((1920, 1080)), None).unwrap().has_text);
}

#[test]
fn svg_overlays_survive_undo_redo_and_a_save_and_reload() {
    let project = Project::open_in_memory().unwrap();
    let o = project
        .add_svg_overlay(SvgSource::Custom(svg_fixture("3s")), 0.0, None, svg_layout())
        .unwrap()
        .overlay;
    project.remove_svg_overlay(o.id).unwrap();
    project.undo().unwrap();
    assert_eq!(project.timeline().unwrap().svg_overlays, vec![o.clone()]);
    project.undo().unwrap();
    assert!(project.timeline().unwrap().svg_overlays.is_empty());
    project.redo().unwrap();
    assert_eq!(project.timeline().unwrap().svg_overlays, vec![o]);

    let dir = std::env::temp_dir().join(format!("kerf-svg-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("svg.kerf");
    let file = Project::open(&path).unwrap();
    let saved = file
        .add_svg_overlay(SvgSource::Custom(svg_fixture("3s")), 0.5, None, svg_layout())
        .unwrap()
        .overlay;
    drop(file);
    let reopened = Project::open(&path).unwrap();
    assert_eq!(reopened.timeline().unwrap().svg_overlays, vec![saved]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn keyframes_add_pins_pose_and_clear_resets() {
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
    project.insert_asset(&asset).unwrap();
    let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
    // Pin the current (static) scale at t=0, then animate to 1.5 at t=4.
    project
        .set_transform(clip.id, Some(1.2), None, None, None, None, None, None, None, None)
        .unwrap();
    let pinned = project.add_keyframe(clip.id, 0.0, None, None, None, None, None).unwrap();
    assert_eq!(pinned.keyframes.len(), 1);
    assert!((pinned.keyframes[0].scale - 1.2).abs() < 1e-9); // captured the static pose
    let animated = project.add_keyframe(clip.id, 4.0, Some(1.5), None, None, None, None).unwrap();
    assert_eq!(animated.keyframes.len(), 2);
    assert!(animated.is_animated());
    // Re-adding at the same time replaces (no duplicate).
    let replaced = project.add_keyframe(clip.id, 0.0, Some(1.0), None, None, None, None).unwrap();
    assert_eq!(replaced.keyframes.len(), 2);
    assert!(!project.clear_keyframes(clip.id).unwrap().is_animated());
}

#[test]
fn a_keys_easing_is_set_by_time_kept_when_the_key_is_rekeyed_and_checked() {
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
    project.insert_asset(&asset).unwrap();
    let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
    project.add_keyframe(clip.id, 0.0, Some(1.0), None, None, None, None).unwrap();
    project.add_keyframe(clip.id, 4.0, Some(2.0), None, None, None, None).unwrap();
    // Found within a millisecond of its time.
    let eased = project.set_keyframe_easing(clip.id, 0.0004, Easing::EaseInOut).unwrap();
    assert_eq!(eased.keyframes[0].easing, Easing::EaseInOut);
    assert!(
        (eased.transform_at(1.0).scale - 1.25).abs() > 0.05,
        "the curve is not the line"
    );
    // Moving the pose at that moment keeps how it leaves.
    let rekeyed = project.add_keyframe(clip.id, 0.0, Some(1.1), None, None, None, None).unwrap();
    assert_eq!(rekeyed.keyframes[0].easing, Easing::EaseInOut);
    // No key there, a control point out of range: refused, nothing written.
    let before = project.history().unwrap().len();
    assert!(matches!(
        project.set_keyframe_easing(clip.id, 2.0, Easing::Hold),
        Err(Error::InvalidArgument(_))
    ));
    let wild = Easing::Bezier {
        x1: 0.2,
        y1: 1.4,
        x2: 0.8,
        y2: 0.5,
    };
    assert!(matches!(
        project.set_keyframe_easing(clip.id, 0.0, wild),
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(project.history().unwrap().len(), before);
}

#[test]
fn a_key_added_inside_a_hold_keeps_holding_and_inside_a_curve_keeps_the_curve() {
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
    project.insert_asset(&asset).unwrap();
    let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
    project.add_keyframe(clip.id, 0.0, Some(1.0), None, None, None, None).unwrap();
    project.add_keyframe(clip.id, 4.0, Some(3.0), None, None, None, None).unwrap();
    project.set_keyframe_easing(clip.id, 0.0, Easing::Hold).unwrap();
    // Pinning the present pose in the middle of the hold changes nothing about it.
    let held = project.add_keyframe(clip.id, 2.0, None, None, None, None, None).unwrap();
    assert_eq!(held.keyframes.len(), 3);
    assert_eq!(held.keyframes[1].easing, Easing::Hold);
    assert_eq!(held.transform_at(3.9).scale, 1.0);
    assert_eq!(held.transform_at(4.0).scale, 3.0);
    // The same in a curve: the motion stays where it was (to the polyline's resolution).
    project.clear_keyframes(clip.id).unwrap();
    project.add_keyframe(clip.id, 0.0, Some(1.0), None, None, None, None).unwrap();
    project.add_keyframe(clip.id, 4.0, Some(3.0), None, None, None, None).unwrap();
    let eased = project.set_keyframe_easing(clip.id, 0.0, Easing::EaseInOut).unwrap();
    let split = project.add_keyframe(clip.id, 1.0, None, None, None, None, None).unwrap();
    assert!(matches!(split.keyframes[0].easing, Easing::Bezier { .. }));
    for i in 0..40 {
        let t = (f64::from(i) + 0.5) * 0.1;
        let (a, b) = (split.transform_at(t).scale, eased.transform_at(t).scale);
        assert!((a - b).abs() < 0.01, "at {t}: now {a}, was {b}");
    }
}

fn channel_project() -> (Project, Clip) {
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
    project.insert_asset(&asset).unwrap();
    let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
    (project, clip)
}

fn pk(time: f64, value: f64) -> PropertyKey {
    PropertyKey::new(time, value)
}

#[test]
fn a_pose_keys_what_is_keyed_and_sets_the_rest_in_one_revision() {
    let (project, clip) = channel_project();
    project
        .set_property_keyframes(clip.id, Property::PosX, vec![pk(0.0, 0.0), pk(4.0, 0.4)])
        .unwrap();
    let before = project.history().unwrap().len();
    let patch = TransformPatch {
        pos_x: Some(-0.25),
        pos_y: Some(0.1),
        crop_right: Some(0.2),
        ..TransformPatch::default()
    };
    let posed = project.set_transform_at(clip.id, 2.0, patch).unwrap();
    assert_eq!(
        project.history().unwrap().len(),
        before + 1,
        "one revision for the whole pose"
    );
    assert_eq!(project.history().unwrap().last().unwrap().label, "Set transform");
    assert_eq!(posed.property_at(Property::PosX, 2.0), -0.25);
    assert_eq!((posed.transform.pos_y, posed.transform.crop_right), (0.1, 0.2));
    assert!(posed.keyframes.is_empty());

    // Refused before anything is written: a bad time, a bad number, a crop of everything.
    for (time, patch) in [
        (-1.0, TransformPatch::default()),
        (f64::NAN, TransformPatch::default()),
        (
            0.0,
            TransformPatch {
                scale: Some(0.0),
                ..TransformPatch::default()
            },
        ),
        (
            0.0,
            TransformPatch {
                crop_left: Some(0.9),
                ..TransformPatch::default()
            },
        ),
    ] {
        assert!(project.set_transform_at(clip.id, time, patch).is_err(), "{time} {patch:?}");
    }
    assert_eq!(project.history().unwrap().len(), before + 1);
}

#[test]
fn a_property_gets_keys_of_its_own_and_loses_them_by_setting_none() {
    let (project, clip) = channel_project();
    let edited = project
        .set_property_keyframes(
            clip.id,
            Property::Brightness,
            vec![
                pk(4.0, 0.5),
                PropertyKey {
                    easing: Easing::Hold,
                    ..pk(1.0, -0.5)
                },
            ],
        )
        .unwrap();
    // Sorted by time; the sample is the curve.
    let keys = edited.property_keys(Property::Brightness);
    assert_eq!((keys[0].time, keys[0].easing), (1.0, Easing::Hold));
    assert_eq!(edited.color_at(2.0).brightness, -0.5);
    assert!(edited.color_animated() && !edited.is_animated());
    assert_eq!(project.history().unwrap().last().unwrap().label, "Set brightness keyframes");
    let off = project.set_property_keyframes(clip.id, Property::Brightness, vec![]).unwrap();
    assert!(off.channels.is_empty() && !off.color_animated());

    // One edit each, validated against the range the static setter takes.
    let before = project.history().unwrap().len();
    for (prop, key) in [
        (Property::Opacity, pk(0.0, 1.5)),
        (Property::Scale, pk(0.0, 0.0)),
        (Property::Gamma, pk(0.0, 0.05)),
        (Property::Volume, pk(0.0, -1.0)),
        (Property::Volume, pk(0.0, 40.0)),
        (Property::PosX, pk(0.0, f64::NAN)),
        (Property::PosX, pk(-1.0, 0.0)),
        (
            Property::PosX,
            PropertyKey {
                easing: Easing::Bezier {
                    x1: 0.5,
                    y1: 1.5,
                    x2: 0.5,
                    y2: 0.5,
                },
                ..pk(0.0, 0.0)
            },
        ),
    ] {
        assert!(
            matches!(
                project.set_property_keyframes(clip.id, prop, vec![key]),
                Err(Error::InvalidArgument(_))
            ),
            "{prop:?} {key:?}"
        );
    }
    let too_many: Vec<PropertyKey> = (0..=MAX_CHANNEL_KEYS).map(|i| pk(i as f64, 1.0)).collect();
    assert!(project.set_property_keyframes(clip.id, Property::Volume, too_many).is_err());
    assert!(matches!(
        project.set_property_keyframes(Uuid::new_v4(), Property::Volume, vec![]),
        Err(Error::ClipNotFound(_))
    ));
    assert_eq!(project.history().unwrap().len(), before, "a refused edit records nothing");
}

#[test]
fn several_numbers_are_rekeyed_in_one_edit() {
    let (project, clip) = channel_project();
    project
        .add_keyframe(clip.id, 0.0, Some(1.0), None, None, None, Some(1.0))
        .unwrap();
    project
        .add_keyframe(clip.id, 4.0, Some(2.0), None, None, None, Some(0.0))
        .unwrap();
    let before = project.history().unwrap().len();
    let edited = project
        .set_property_tracks(
            clip.id,
            vec![
                PropertyTrack {
                    prop: Property::Scale,
                    keys: vec![pk(1.0, 1.0), pk(5.0, 2.0)],
                },
                PropertyTrack {
                    prop: Property::Volume,
                    keys: vec![pk(0.0, 1.0), pk(2.0, 0.5)],
                },
            ],
        )
        .unwrap();
    let history = project.history().unwrap();
    assert_eq!(history.len(), before + 1);
    assert_eq!(history.last().unwrap().label, "Edit keyframes");
    assert_eq!(edited.property_keys(Property::Scale)[0].time, 1.0);
    assert_eq!(edited.property_keys(Property::Opacity).len(), 2, "the rest keep the bundle");
    assert!(edited.volume_animated());

    for tracks in [
        vec![],
        vec![
            PropertyTrack {
                prop: Property::Gamma,
                keys: vec![],
            },
            PropertyTrack {
                prop: Property::Gamma,
                keys: vec![pk(0.0, 1.0)],
            },
        ],
        vec![PropertyTrack {
            prop: Property::Opacity,
            keys: vec![pk(0.0, 2.0)],
        }],
    ] {
        assert!(matches!(
            project.set_property_tracks(clip.id, tracks),
            Err(Error::InvalidArgument(_))
        ));
    }
    assert_eq!(project.history().unwrap().len(), before + 1, "a refused edit records nothing");
}

#[test]
fn a_mask_is_keyed_only_on_a_clip_with_one_and_clearing_it_clears_its_keys() {
    let (project, clip) = channel_project();
    let keys = vec![pk(0.0, 0.2), pk(2.0, 0.8)];
    let before = project.history().unwrap().len();
    for refused in [
        project.set_property_keyframes(clip.id, Property::MaskX, keys.clone()),
        project.set_property_tracks(
            clip.id,
            vec![PropertyTrack {
                prop: Property::MaskWidth,
                keys: keys.clone(),
            }],
        ),
    ] {
        assert!(
            matches!(&refused, Err(Error::InvalidArgument(m)) if m.contains("no mask")),
            "{refused:?}"
        );
    }
    assert_eq!(project.history().unwrap().len(), before, "a refused edit records nothing");
    // The crop needs nothing of the clip.
    project
        .set_property_keyframes(clip.id, Property::CropZoom, vec![pk(0.0, 1.0), pk(1.0, 2.0)])
        .unwrap();
    project.set_mask(clip.id, Some(Mask::default())).unwrap();
    let keyed = project.set_property_keyframes(clip.id, Property::MaskX, keys).unwrap();
    assert!(keyed.mask_animated() && keyed.crop_animated());
    // Copying every keyed number onto a clip with no mask leaves the mask's behind; naming
    // one is still refused.
    let other = project.cut_clip(clip.asset_id, 0.0, 5.0).unwrap();
    let copied = project.copy_keyframes(clip.id, other.id, &[], 0.0).unwrap();
    assert!(copied.crop_animated() && !copied.is_keyed(Property::MaskX));
    assert!(project.copy_keyframes(clip.id, other.id, &[Property::MaskX], 0.0).is_err());
    let cleared = project.set_mask(clip.id, None).unwrap();
    assert!(!cleared.is_keyed(Property::MaskX) && cleared.crop_animated());
}

/// A proposal that only holds a bundle-driven number static changes the render, so it is a
/// change: it used to diff as empty and `apply_staged` discarded it without a revision.
#[test]
fn an_agent_holding_a_bundle_driven_number_static_is_a_proposal_and_it_lands() {
    let (mut project, clip) = channel_project();
    project
        .add_keyframe(clip.id, 0.0, Some(1.0), None, None, None, Some(1.0))
        .unwrap();
    project
        .add_keyframe(clip.id, 4.0, Some(2.0), None, None, None, Some(0.0))
        .unwrap();
    let revisions = project.history().unwrap().len();
    project.set_actor(EditSource::Agent);
    project.set_agent_review(false);
    project.begin_staging(None, None, None).unwrap();
    project.set_property_keyframes(clip.id, Property::Opacity, vec![]).unwrap();

    let staged = project.staged().unwrap().expect("a proposal");
    assert!(!staged.diff.is_empty(), "{:?}", staged.diff);
    let text = staged
        .diff
        .entries
        .iter()
        .map(|e| format!("{} {}", e.summary, e.detail.as_deref().unwrap_or("")))
        .collect::<String>();
    assert!(text.contains("opacity keyframes 2 → 0"), "{text}");
    // The user's cut has not moved; applying lands one agent revision and the opacity is static.
    assert!(project.timeline().unwrap().tracks[0].clips[0].channels.is_empty());
    let applied = project.apply_staged(false).unwrap();
    let landed = &applied.tracks[0].clips[0];
    assert_eq!(landed.transform_at(2.0).opacity, 1.0, "held at the static value");
    assert!(
        (landed.transform_at(2.0).scale - 1.5).abs() < 1e-12,
        "the rest still animates"
    );
    let history = project.history().unwrap();
    assert_eq!(history.len(), revisions + 1);
    assert_eq!(history.last().unwrap().source, EditSource::Agent);
}

#[test]
fn set_keyframes_clears_the_bundle_only_and_drops_a_track_that_was_only_holding_it_off() {
    let (project, clip) = channel_project();
    let key = |time: f64, scale: f64| Keyframe {
        time,
        scale,
        pos_x: 0.0,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        easing: Easing::Linear,
    };
    project.set_keyframes(clip.id, vec![key(0.0, 1.0), key(4.0, 2.0)]).unwrap();
    // The opacity held static against the bundle; the scale taken over with keys of its own.
    project.set_property_keyframes(clip.id, Property::Opacity, vec![]).unwrap();
    project
        .set_property_keyframes(clip.id, Property::Scale, vec![pk(0.0, 1.0), pk(4.0, 3.0)])
        .unwrap();
    assert_eq!(project.timeline().unwrap().tracks[0].clips[0].channels.len(), 2);
    let c = project.set_keyframes(clip.id, vec![]).unwrap();
    assert!(c.keyframes.is_empty());
    // The scale's own keys survive an emptied bundle (the clip is still animated) ...
    assert!(c.is_animated() && c.is_keyed(Property::Scale));
    assert_eq!(c.transform_at(2.0).scale, 2.0);
    // ... and the empty opacity track, which had nothing left to hold off, is gone.
    assert_eq!(c.channels.len(), 1, "{:?}", c.channels);
    assert_eq!(c.channels[0].prop, Property::Scale);
    // `clear_keyframes` is the one that makes the whole transform static.
    let c = project.clear_keyframes(clip.id).unwrap();
    assert!(!c.is_animated() && c.channels.is_empty());
}

#[test]
fn keying_one_number_off_the_bundle_leaves_the_others_and_the_old_ops_keep_working() {
    let (project, clip) = channel_project();
    project
        .add_keyframe(clip.id, 0.0, Some(1.0), None, None, None, Some(1.0))
        .unwrap();
    project
        .add_keyframe(clip.id, 4.0, Some(2.0), None, None, None, Some(0.0))
        .unwrap();
    // Take the opacity off the bundle, with a key in the middle of its own.
    let c = project
        .set_property_keyframes(clip.id, Property::Opacity, vec![pk(0.0, 1.0), pk(2.0, 0.25), pk(4.0, 1.0)])
        .unwrap();
    assert_eq!(c.keyframes.len(), 2, "the bundle is as it was");
    assert!((c.transform_at(2.0).opacity - 0.25).abs() < 1e-12 && (c.transform_at(2.0).scale - 1.5).abs() < 1e-12);
    // `add_keyframe` puts an asked-for opacity into the track that drives it.
    let c = project.add_keyframe(clip.id, 3.0, None, None, None, None, Some(0.9)).unwrap();
    assert_eq!(c.keyframes.len(), 3);
    assert!((c.transform_at(3.0).opacity - 0.9).abs() < 1e-12);
    assert_eq!(c.channel(Property::Opacity).unwrap().keys.len(), 4);
    // The easing of the transform key at 0 s reaches the bundle *and* the track.
    let c = project.set_keyframe_easing(clip.id, 0.0, Easing::Hold).unwrap();
    assert_eq!(c.keyframes[0].easing, Easing::Hold);
    assert_eq!(c.property_keys(Property::Opacity)[0].easing, Easing::Hold);
    assert_eq!(c.transform_at(1.9).opacity, 1.0);
    // One number's segment alone.
    let c = project
        .set_property_easing(clip.id, Property::Scale, 0.0, Easing::EaseIn)
        .unwrap();
    assert_eq!(
        c.keyframes[0].easing,
        Easing::Hold,
        "the bundle keeps its other numbers' shapes"
    );
    assert_eq!(c.property_keys(Property::Scale)[0].easing, Easing::EaseIn);
    assert!(project
        .set_property_easing(clip.id, Property::Volume, 0.0, Easing::Hold)
        .is_err());
    assert!(project.set_keyframe_easing(clip.id, 8.0, Easing::Hold).is_err());
    // Clearing the transform animation takes the tracks of its numbers too, and only those.
    project
        .set_property_keyframes(clip.id, Property::Brightness, vec![pk(0.0, 0.1), pk(1.0, 0.2)])
        .unwrap();
    let c = project.clear_keyframes(clip.id).unwrap();
    assert!(!c.is_animated() && c.keyframes.is_empty());
    assert_eq!(c.channels.len(), 1);
    assert_eq!(c.channels[0].prop, Property::Brightness);
}

#[test]
fn copying_keyframes_moves_an_animation_to_another_clip() {
    let (project, from) = channel_project();
    let to = project
        .add_clip_to_timeline(from.asset_id, None, 0.0, 6.0, Some(20.0))
        .unwrap();
    project
        .set_property_keyframes(from.id, Property::Volume, vec![pk(1.0, 0.0), pk(3.0, 2.0)])
        .unwrap();
    project.add_keyframe(from.id, 0.0, Some(1.0), None, None, None, None).unwrap();
    project.add_keyframe(from.id, 2.0, Some(2.0), None, None, None, None).unwrap();
    project
        .set_property_keyframes(to.id, Property::Volume, vec![pk(0.0, 1.0), pk(5.0, 1.0)])
        .unwrap();
    project.set_volume(to.id, 0.5).unwrap();

    // The volume only, half a second later: it replaces the destination's own.
    let copied = project.copy_keyframes(from.id, to.id, &[Property::Volume], 0.5).unwrap();
    let keys = copied.property_keys(Property::Volume);
    assert_eq!(
        keys.iter().map(|k| (k.time, k.value)).collect::<Vec<_>>(),
        [(1.5, 0.0), (3.5, 2.0)]
    );
    assert!(!copied.is_animated(), "only the volume was asked for");
    assert_eq!(project.history().unwrap().last().unwrap().label, "Copy keyframes");
    // Every keyed number, the bundle's included (as the tracks of its numbers), earlier.
    let all = project.copy_keyframes(from.id, to.id, &[], -1.0).unwrap();
    assert!(all.is_animated() && all.is_keyed(Property::Scale) && all.is_keyed(Property::Volume));
    assert_eq!(
        all.property_keys(Property::Volume)[0].time,
        0.0,
        "the head before the destination's start is cut"
    );
    assert!(
        (all.property_at(Property::Volume, 0.0) - 0.0).abs() < 1e-9
            && (all.property_at(Property::Volume, 1.0) - 1.0).abs() < 1e-9
    );
    assert!((all.property_at(Property::Scale, 0.5) - from_clip_scale_at(&project, from.id, 1.5)).abs() < 1e-9);
    // Refusals, none of which write.
    let before = project.history().unwrap().len();
    assert!(project.copy_keyframes(from.id, from.id, &[], 0.0).is_err());
    assert!(project.copy_keyframes(from.id, to.id, &[Property::Gamma], 0.0).is_err());
    assert!(project.copy_keyframes(to.id, from.id, &[Property::Temperature], 0.0).is_err());
    assert!(project.copy_keyframes(from.id, to.id, &[], f64::NAN).is_err());
    assert!(project.copy_keyframes(from.id, Uuid::new_v4(), &[], 0.0).is_err());
    assert!(project.copy_keyframes(Uuid::new_v4(), to.id, &[], 0.0).is_err());
    assert_eq!(project.history().unwrap().len(), before);
    // Nothing keyed in the source is nothing to copy.
    let bare = project
        .add_clip_to_timeline(from.asset_id, None, 0.0, 2.0, Some(40.0))
        .unwrap();
    assert!(project.copy_keyframes(bare.id, to.id, &[], 0.0).is_err());
}

fn from_clip_scale_at(project: &Project, id: Uuid, local: f64) -> f64 {
    project.timeline().unwrap().clip(id).unwrap().transform_at(local).scale
}

#[test]
fn set_keyframes_refuses_a_bezier_that_overshoots_like_set_keyframe_easing_does() {
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/x.mp4", vec![vid_stream(false)]);
    project.insert_asset(&asset).unwrap();
    let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
    let key = |time: f64, easing: Easing| Keyframe {
        easing,
        ..Keyframe::from_transform(time, &Default::default())
    };
    let fine = Easing::Bezier {
        x1: 0.2,
        y1: 0.9,
        x2: 0.3,
        y2: 1.0,
    };
    assert!(project
        .set_keyframes(clip.id, vec![key(0.0, fine), key(2.0, Easing::Linear)])
        .is_ok());
    let before = project.history().unwrap().len();
    for wild in [
        Easing::Bezier {
            x1: 0.2,
            y1: 1.4,
            x2: 0.8,
            y2: 0.5,
        },
        Easing::Bezier {
            x1: -0.1,
            y1: 0.0,
            x2: 0.5,
            y2: 1.0,
        },
        Easing::Bezier {
            x1: 0.2,
            y1: 0.0,
            x2: f64::NAN,
            y2: 1.0,
        },
    ] {
        assert!(matches!(
            project.set_keyframes(clip.id, vec![key(0.0, wild), key(2.0, Easing::Linear)]),
            Err(Error::InvalidArgument(_))
        ));
    }
    assert_eq!(project.history().unwrap().len(), before);
    assert_eq!(project.timeline().unwrap().tracks[0].clips[0].keyframes[0].easing, fine);
}

#[test]
fn a_360_clip_reframes_by_default_and_a_flat_one_never_does() {
    let project = Project::open_in_memory().unwrap();
    let mut v = vid_stream(false);
    v.width = Some(5760);
    v.height = Some(2880);
    v.projection = Some(Projection::DualFisheye);
    let sphere = asset_with("/VID_001.insv", vec![v]);
    let flat = asset_with("/plain.mp4", vec![vid_stream(false)]);
    project.insert_asset(&sphere).unwrap();
    project.insert_asset(&flat).unwrap();

    // Landing a 360 clip on the timeline points a camera at it, so it
    // previews as ordinary footage rather than two fisheye circles.
    let clip = project.cut_clip(sphere.id, 0.0, 10.0).unwrap();
    let rf = clip.reframe.expect("a 360 clip reframes on arrival");
    assert_eq!(rf.input, Projection::DualFisheye);
    assert_eq!(rf.output, Projection::Flat);

    assert!(
        project.cut_clip(flat.id, 0.0, 10.0).unwrap().reframe.is_none(),
        "ordinary footage is never reprojected"
    );
}

#[test]
fn marking_an_asset_360_makes_later_clips_reframe() {
    // Footage kerf can't identify (no spherical metadata, no telltale
    // geometry) probes flat; marking the asset is the escape hatch, and it
    // has to stick to the asset so every later cut picks it up.
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/mystery.mp4", vec![vid_stream(false)]);
    project.insert_asset(&asset).unwrap();
    assert!(project.cut_clip(asset.id, 0.0, 5.0).unwrap().reframe.is_none());

    let updated = project.set_asset_projection(asset.id, Some(Projection::Equirect)).unwrap();
    assert_eq!(updated.projection(), Some(Projection::Equirect));
    let clip = project.cut_clip(asset.id, 0.0, 5.0).unwrap();
    assert_eq!(clip.reframe.expect("marked asset reframes").input, Projection::Equirect);
    // Persisted on the asset, not just on the returned copy.
    assert_eq!(
        project.require_asset(asset.id).unwrap().projection(),
        Some(Projection::Equirect)
    );

    // And it can be taken back off again.
    project.set_asset_projection(asset.id, None).unwrap();
    assert_eq!(project.require_asset(asset.id).unwrap().projection(), None);
}

#[test]
fn asset_projection_rejects_flat_and_audio_only() {
    let project = Project::open_in_memory().unwrap();
    let video = asset_with("/v.mp4", vec![vid_stream(false)]);
    let audio = asset_with("/a.wav", vec![aud_stream()]);
    project.insert_asset(&video).unwrap();
    project.insert_asset(&audio).unwrap();
    assert!(project.set_asset_projection(video.id, Some(Projection::Flat)).is_err());
    assert!(project.set_asset_projection(audio.id, Some(Projection::Equirect)).is_err());
}

#[test]
fn reframe_ops_aim_the_camera_and_pin_its_pose() {
    let project = Project::open_in_memory().unwrap();
    let mut v = vid_stream(false);
    v.projection = Some(Projection::Equirect);
    let asset = asset_with("/360.mp4", vec![v]);
    project.insert_asset(&asset).unwrap();
    let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();

    // `None` leaves a channel alone, so yaw can be nudged on its own.
    let aimed = project
        .set_reframe(clip.id, Some(85.0), Some(-10.0), None, None, None, None, None)
        .unwrap();
    let rf = aimed.reframe.as_ref().unwrap();
    assert_eq!((rf.yaw, rf.pitch, rf.fov), (85.0, -10.0, 100.0));

    // A fresh keyframe captures the pose that is already there.
    let pinned = project.add_reframe_keyframe(clip.id, 0.0, None, None, None, None).unwrap();
    let kfs = &pinned.reframe.as_ref().unwrap().keyframes;
    assert_eq!(kfs.len(), 1);
    assert_eq!((kfs[0].yaw, kfs[0].pitch), (85.0, -10.0));

    let panned = project
        .add_reframe_keyframe(clip.id, 4.0, Some(-85.0), None, None, None)
        .unwrap();
    assert!(panned.reframe.as_ref().unwrap().is_animated());
    // Re-adding at the same time replaces rather than duplicates.
    let replaced = project
        .add_reframe_keyframe(clip.id, 0.0, Some(0.0), None, None, None)
        .unwrap();
    assert_eq!(replaced.reframe.as_ref().unwrap().keyframes.len(), 2);

    // Out-of-range field of view is refused up front, since v360 would
    // reject it at render time.
    assert!(project
        .set_reframe(clip.id, None, None, None, Some(0.0), None, None, None)
        .is_err());
    assert!(project.clear_reframe(clip.id).unwrap().reframe.is_none());
}

#[test]
fn reframing_flat_footage_needs_an_explicit_projection() {
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/plain.mp4", vec![vid_stream(false)]);
    project.insert_asset(&asset).unwrap();
    let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();

    assert!(
        project
            .set_reframe(clip.id, Some(30.0), None, None, None, None, None, None)
            .is_err(),
        "detection can miss, but silently reprojecting flat video is worse"
    );
    // …and the escape hatch when detection did miss.
    let forced = project
        .set_reframe(clip.id, Some(30.0), None, None, None, None, Some(Projection::Equirect), None)
        .unwrap();
    assert_eq!(forced.reframe.unwrap().input, Projection::Equirect);
}

#[test]
fn video_and_audio_effects_persist_and_validate() {
    let project = Project::open_in_memory().unwrap();
    let asset = asset_with("/x.mp4", vec![vid_stream(false), aud_stream()]);
    project.insert_asset(&asset).unwrap();
    let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
    let updated = project
        .set_video_effects(clip.id, vec![VideoEffect::Blur { sigma: 5.0 }, VideoEffect::Grayscale])
        .unwrap();
    assert_eq!(updated.effects.len(), 2);
    // An out-of-range chroma key is rejected.
    assert!(project
        .set_video_effects(
            clip.id,
            vec![VideoEffect::ChromaKey {
                color: "green".into(),
                similarity: 2.0,
                blend: 0.0
            }]
        )
        .is_err());
    let a = project
        .set_audio_effects(clip.id, vec![AudioEffect::Highpass { hz: 80.0 }])
        .unwrap();
    assert_eq!(a.audio.len(), 1);
    assert!(project
        .set_audio_effects(clip.id, vec![AudioEffect::Highpass { hz: -1.0 }])
        .is_err());
}

#[test]
fn split_maps_the_timeline_point_through_speed() {
    let project = Project::open_in_memory().unwrap();
    let asset = Asset {
        id: Uuid::new_v4(),
        path: "/x.mp4".into(),
        name: "x.mp4".into(),
        duration: 10.0,
        streams: vec![StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(1280),
            height: Some(720),
            fps: Some(25.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: None,
            color_space: None,
            color_range: None,
            lead: 0.0,
            time_base: None,
        }],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    project.insert_asset(&asset).unwrap();

    let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
    project.set_speed(clip.id, 2.0).unwrap(); // 10s of source over 5s of timeline
                                              // Split at timeline t=2.0 → 4.0s into the source (2.0 * 2x).
    let (left, right) = project.split_at(clip.id, 2.0).unwrap();
    assert!((left.source_out - 4.0).abs() < 1e-9, "left out: {}", left.source_out);
    assert!((right.source_in - 4.0).abs() < 1e-9, "right in: {}", right.source_in);
    assert!((left.duration() - 2.0).abs() < 1e-9, "left dur: {}", left.duration());
    assert!((right.duration() - 3.0).abs() < 1e-9, "right dur: {}", right.duration());
    // Gapless: the two halves still sum to the original timeline duration.
    assert!((left.duration() + right.duration() - 5.0).abs() < 1e-9);
    assert!((right.timeline_start - 2.0).abs() < 1e-9);
    assert_eq!(left.speed, 2.0);
    assert_eq!(right.speed, 2.0);
}

#[test]
fn set_fade_persists_and_validates() {
    let project = Project::open_in_memory().unwrap();
    let asset = Asset {
        id: Uuid::new_v4(),
        path: "/x.mp4".into(),
        name: "x.mp4".into(),
        duration: 10.0,
        streams: vec![StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(1280),
            height: Some(720),
            fps: Some(25.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: None,
            color_space: None,
            color_range: None,
            lead: 0.0,
            time_base: None,
        }],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    project.insert_asset(&asset).unwrap();
    let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
    assert_eq!(clip.fade_in, 0.0);

    // Setting only fade_in leaves fade_out untouched.
    let faded = project.set_fade(clip.id, Some(0.5), None).unwrap();
    assert_eq!(faded.fade_in, 0.5);
    assert_eq!(faded.fade_out, 0.0);

    let faded = project.set_fade(clip.id, None, Some(1.0)).unwrap();
    assert_eq!(faded.fade_in, 0.5);
    assert_eq!(faded.fade_out, 1.0);

    // It persists to the stored timeline.
    let stored = project.timeline().unwrap().clip(clip.id).unwrap().clone();
    assert_eq!(stored.fade_in, 0.5);
    assert_eq!(stored.fade_out, 1.0);

    // Negative fades are rejected.
    assert!(matches!(
        project.set_fade(clip.id, Some(-1.0), None),
        Err(Error::InvalidArgument(_))
    ));
}

/// [`project_with_video_asset`] whose footage has sound too, for a test that puts it on an audio track.
fn project_with_av_asset() -> (Project, Uuid) {
    let project = Project::open_in_memory().unwrap();
    let mut asset = asset_with("/av.mp4", vec![vid_stream(false), aud_stream()]);
    asset.duration = 60.0;
    let id = project.insert_or_get_asset(&asset).unwrap().id;
    (project, id)
}

fn project_with_video_asset() -> (Project, Uuid) {
    let project = Project::open_in_memory().unwrap();
    let asset = Asset {
        id: Uuid::new_v4(),
        path: "/x.mp4".into(),
        name: "x.mp4".into(),
        duration: 60.0,
        streams: vec![StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(1920),
            height: Some(1080),
            fps: Some(30.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: None,
            color_space: None,
            color_range: None,
            lead: 0.0,
            time_base: None,
        }],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    let id = asset.id;
    project.insert_asset(&asset).unwrap();
    (project, id)
}

#[test]
fn move_clip_repositions_and_rejects_overlap() {
    let (project, asset) = project_with_video_asset();
    let a = project.cut_clip(asset, 0.0, 5.0).unwrap(); // [0,5)
    let b = project.cut_clip(asset, 0.0, 5.0).unwrap(); // appended [5,10)

    // Free move into the open space well after b.
    let moved = project.move_clip(a.id, 20.0, None).unwrap();
    assert!((moved.timeline_start - 20.0).abs() < 1e-9);
    // The track is re-sorted by start (b first now).
    let tl = project.timeline().unwrap();
    let starts: Vec<f64> = tl.tracks[0].clips.iter().map(|c| c.timeline_start).collect();
    assert_eq!(starts, vec![5.0, 20.0]);

    // Dropping a back on top of b overlaps -> rejected.
    assert!(matches!(project.move_clip(a.id, 6.0, None), Err(Error::InvalidArgument(_))));
    assert_eq!(b.timeline_start, 5.0);

    // A negative start clamps to 0.
    let moved = project.move_clip(a.id, -3.0, None).unwrap();
    assert_eq!(moved.timeline_start, 0.0);
}

#[test]
fn move_clip_across_tracks_same_kind_only() {
    let (project, asset) = project_with_video_asset();
    let clip = project.cut_clip(asset, 0.0, 5.0).unwrap();
    let v2 = project.add_track(StreamKind::Video, None).unwrap();
    let a1 = project.timeline().unwrap().first_track_of(StreamKind::Audio).unwrap();

    // Lift the clip onto the second video track (B-roll lane).
    project.move_clip(clip.id, 0.0, Some(v2.id)).unwrap();
    let tl = project.timeline().unwrap();
    assert!(tl.track(v2.id).unwrap().clips.iter().any(|c| c.id == clip.id));

    // Moving a video clip onto an audio track is rejected.
    assert!(matches!(
        project.move_clip(clip.id, 0.0, Some(a1)),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn ripple_delete_closes_the_gap() {
    let (project, asset) = project_with_video_asset();
    let a = project.cut_clip(asset, 0.0, 5.0).unwrap(); // [0,5)
    let b = project.cut_clip(asset, 0.0, 5.0).unwrap(); // [5,10)
    project.cut_clip(asset, 0.0, 5.0).unwrap(); // [10,15)

    project.ripple_delete(a.id).unwrap();
    let tl = project.timeline().unwrap();
    let starts: Vec<f64> = tl.tracks[0].clips.iter().map(|c| c.timeline_start).collect();
    // b and the third clip each shift left by 5s, closing the gap.
    assert_eq!(starts, vec![0.0, 5.0]);
    assert!(tl.clip(b.id).is_some());
}

#[test]
fn add_and_remove_track() {
    let (project, _asset) = project_with_video_asset();
    let before = project.timeline().unwrap().tracks.len(); // V1 + A1

    let v2 = project.add_track(StreamKind::Video, None).unwrap();
    assert_eq!(v2.name, "V2");
    let tl = project.timeline().unwrap();
    assert_eq!(tl.tracks.len(), before + 1);
    // Video tracks stay grouped above audio tracks.
    let kinds: Vec<StreamKind> = tl.tracks.iter().map(|t| t.kind).collect();
    assert_eq!(kinds, vec![StreamKind::Video, StreamKind::Video, StreamKind::Audio]);

    project.remove_track(v2.id).unwrap();
    assert_eq!(project.timeline().unwrap().tracks.len(), before);
}

#[test]
fn history_undo_redo_revert() {
    let project = Project::open_in_memory().unwrap();
    let asset = Asset {
        id: Uuid::new_v4(),
        path: "/x.mp4".into(),
        name: "x.mp4".into(),
        duration: 10.0,
        streams: vec![StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(1280),
            height: Some(720),
            fps: Some(25.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: None,
            color_space: None,
            color_range: None,
            lead: 0.0,
            time_base: None,
        }],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    project.insert_asset(&asset).unwrap();

    let clipped = |p: &Project| -> usize { p.timeline().unwrap().tracks.iter().map(|t| t.clips.len()).sum() };

    // Baseline (seq 0) is the only revision; nothing to undo yet.
    assert!(!project.can_undo().unwrap());
    assert_eq!(project.history().unwrap().len(), 1);

    let clip = project.cut_clip(asset.id, 0.0, 10.0).unwrap();
    project.split_at(clip.id, 4.0).unwrap();
    assert_eq!(clipped(&project), 2);
    assert_eq!(project.history().unwrap().len(), 3); // baseline + add + split

    // Undo the split, then the add.
    project.undo().unwrap();
    assert_eq!(clipped(&project), 1);
    assert!(project.can_redo().unwrap());

    // Redo the split back.
    project.redo().unwrap();
    assert_eq!(clipped(&project), 2);

    // Revert all the way to the empty baseline.
    project.revert_to(0).unwrap();
    assert_eq!(clipped(&project), 0);
    assert!(project.history().unwrap().iter().find(|r| r.seq == 0).unwrap().current);

    // A new edit from a non-tip head truncates the redo branch.
    project.cut_clip(asset.id, 0.0, 5.0).unwrap();
    assert_eq!(clipped(&project), 1);
    assert_eq!(project.history().unwrap().len(), 2); // baseline + the new edit
    assert!(!project.can_redo().unwrap());
}

#[test]
fn edits_are_attributed_to_actor() {
    let mut project = Project::open_in_memory().unwrap();
    let asset = Asset {
        id: Uuid::new_v4(),
        path: "/x.mp4".into(),
        name: "x.mp4".into(),
        duration: 10.0,
        streams: vec![StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".into(),
            width: Some(1280),
            height: Some(720),
            fps: Some(25.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: None,
            color_space: None,
            color_range: None,
            lead: 0.0,
            time_base: None,
        }],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    project.insert_asset(&asset).unwrap();

    project.set_actor(crate::model::EditSource::Agent);

    project.set_agent_review(false);
    project.cut_clip(asset.id, 0.0, 5.0).unwrap();
    let latest = project.history().unwrap().pop().unwrap();
    assert_eq!(latest.source, crate::model::EditSource::Agent);
}

#[test]
fn task_queue_lifecycle() {
    let project = Project::open_in_memory().unwrap();
    assert!(project.list_tasks().unwrap().is_empty());

    let queued = project.add_task("trim the intro").unwrap();
    assert_eq!(queued.status, TaskStatus::Queued);

    let claimed = project.claim_next_task(None).unwrap().unwrap();
    assert_eq!(claimed.id, queued.id);
    assert_eq!(claimed.status, TaskStatus::Working);
    // The queue is now empty, so there is nothing left to claim.
    assert!(project.claim_next_task(None).unwrap().is_none());

    let ready = project.complete_task(queued.id, Some("done".to_string())).unwrap();
    assert_eq!(ready.status, TaskStatus::Ready);
    assert_eq!(ready.result.as_deref(), Some("done"));

    let resolved = project.resolve_task(queued.id).unwrap();
    assert_eq!(resolved.status, TaskStatus::Done);
    // resolve leaves the agent's summary intact.
    assert_eq!(resolved.result.as_deref(), Some("done"));

    project.remove_task(queued.id).unwrap();
    assert!(project.list_tasks().unwrap().is_empty());
    assert!(matches!(project.require_task(queued.id), Err(Error::TaskNotFound(_))));
}

#[test]
fn sample_project_seeds_tasks() {
    let project = Project::sample().unwrap();
    let tasks = project.list_tasks().unwrap();
    assert_eq!(tasks.len(), 3);
    assert!(tasks.iter().any(|t| t.status == TaskStatus::Done));
    assert!(tasks.iter().any(|t| t.status == TaskStatus::Queued));
}

#[test]
fn the_delivery_format_persists_and_clears() {
    let project = Project::sample().unwrap();
    assert!(project.timeline().unwrap().format.is_none(), "projects start at source shape");

    let tl = project
        .set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover)))
        .unwrap();
    assert_eq!(tl.format, Some(Delivery::new(1080, 1920, Fit::Cover)));

    // The timeline is stored as one JSON blob, so a reload is the real test.
    let path = std::env::temp_dir().join(format!("kerf-delivery-{}.kerf", Uuid::new_v4()));
    project.save_as(&path).unwrap();
    let reopened = Project::open(&path).unwrap();
    assert_eq!(
        reopened.timeline().unwrap().format,
        Some(Delivery::new(1080, 1920, Fit::Cover))
    );
    drop(reopened);
    let _ = std::fs::remove_file(&path);

    // And clearing it goes back to following the footage.
    assert!(project.set_delivery_format(None).unwrap().format.is_none());
    // Both edits are in the log, so the frame change is undoable like any other.
    let history = project.history().unwrap();
    assert!(history.iter().any(|r| r.label == "Deliver 1080x1920"), "{history:?}");
}

#[test]
fn the_delivery_frame_rate_is_set_checked_persisted_and_undoable() {
    let project = Project::sample().unwrap();
    let at = |fps: Option<f64>| Delivery::new(1080, 1920, Fit::Cover).with_fps(fps).unwrap();

    // A rate is part of the frame; the short NTSC decimals are made exact on the way in.
    let tl = project
        .set_delivery_format(Some(Delivery {
            fps: Some(29.97),
            ..at(None)
        }))
        .unwrap();
    assert_eq!(tl.format.unwrap().fps, Some(30_000.0 / 1001.0));
    let tl = project.set_delivery_format(Some(at(Some(50.0)))).unwrap();
    assert_eq!(tl.format, Some(at(Some(50.0))));
    let history = project.history().unwrap();
    assert!(
        history.iter().any(|r| r.label == "Deliver 1080x1920 at 50 fps"),
        "{history:?}"
    );

    // Not a rate: refused, and the frame is as it was.
    for bad in [0.0, -25.0, 0.5, 241.0, f64::NAN, f64::INFINITY] {
        let err = project
            .set_delivery_format(Some(Delivery {
                fps: Some(bad),
                ..at(None)
            }))
            .unwrap_err();
        assert!(err.to_string().contains("frame rate"), "{bad}: {err}");
    }
    assert_eq!(project.timeline().unwrap().format, Some(at(Some(50.0))));

    // It survives a reload, and a frame set without one clears it: the rate belongs to the frame.
    let path = std::env::temp_dir().join(format!("kerf-delivery-fps-{}.kerf", Uuid::new_v4()));
    project.save_as(&path).unwrap();
    let reopened = Project::open(&path).unwrap();
    assert_eq!(reopened.timeline().unwrap().format, Some(at(Some(50.0))));
    drop(reopened);
    let _ = std::fs::remove_file(&path);
    assert_eq!(project.set_delivery_format(Some(at(None))).unwrap().format, Some(at(None)));
    assert!(project.set_delivery_format(None).unwrap().format.is_none());

    // The change is a revision like any other, so undo puts the rate back.
    project.undo().unwrap();
    project.undo().unwrap();
    assert_eq!(project.timeline().unwrap().format, Some(at(Some(50.0))));
}

#[test]
fn save_as_snapshots_and_reopens_with_state() {
    let project = Project::sample().unwrap();
    assert!(project.path().is_none(), "in-memory project has no path");
    let assets = project.list_assets().unwrap().len();
    let tracks = project.timeline().unwrap().tracks.len();
    let tasks = project.list_tasks().unwrap().len();

    let path = std::env::temp_dir().join(format!("kerf-save-as-{}.kerf", Uuid::new_v4()));
    project.save_as(&path).unwrap();

    // Reopening the snapshot is file-backed and preserves the full state.
    let reopened = Project::open(&path).unwrap();
    assert_eq!(reopened.path(), Some(path.as_path()));
    assert_eq!(reopened.list_assets().unwrap().len(), assets);
    assert_eq!(reopened.timeline().unwrap().tracks.len(), tracks);
    assert_eq!(reopened.list_tasks().unwrap().len(), tasks);

    // save_as overwrites an existing file (the dialog confirms the overwrite).
    // Drop the open connection first: on Windows an open handle locks the file,
    // so the overwrite's remove_file would fail with "used by another process".
    drop(reopened);
    Project::sample().unwrap().save_as(&path).unwrap();

    std::fs::remove_file(&path).ok();
}

/// A sample project with the agent driving, which is the only actor whose
/// edits stage.
fn agent_project() -> Project {
    let mut project = Project::sample().unwrap();
    project.set_actor(EditSource::Agent);
    project.set_agent_review(false);
    project
}

fn first_clip(project: &Project) -> Clip {
    project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .flat_map(|t| t.clips.clone())
        .next()
        .unwrap()
}

#[test]
fn staged_agent_edits_leave_the_live_timeline_alone_until_applied() {
    let project = agent_project();
    let clip = first_clip(&project);
    let before = project.timeline().unwrap();
    let revisions_before = project.history().unwrap().len();

    project.begin_staging(None, None, None).unwrap();
    project.set_volume(clip.id, 0.4).unwrap();
    project.remove(clip.id).unwrap();

    // The cut the user is looking at has not moved.
    let live = project.timeline().unwrap();
    assert_eq!(live.duration(), before.duration());
    assert!(live.clip(clip.id).is_some());
    assert_eq!(project.history().unwrap().len(), revisions_before);
    // …but the agent sees its own work.
    assert!(project.working_timeline().unwrap().clip(clip.id).is_none());

    let staged = project.staged().unwrap().unwrap();
    assert!(!staged.stale);
    assert_eq!(staged.edits, vec!["Set volume".to_string(), "Remove clip".to_string()]);
    assert!(staged.diff.entries.iter().any(|e| e.kind == DiffKind::ClipRemoved));

    let applied = project.apply_staged(false).unwrap();
    assert!(applied.clip(clip.id).is_none());
    assert!(project.timeline().unwrap().clip(clip.id).is_none());
    assert!(project.staged().unwrap().is_none());

    // Two edits, one revision: the user accepted a proposal, not a replay.
    let history = project.history().unwrap();
    assert_eq!(history.len(), revisions_before + 1);
    let latest = history.last().unwrap();
    assert_eq!(latest.source, EditSource::Agent);
    assert!(latest.label.starts_with("Agent edit ("), "{}", latest.label);
}

#[test]
fn discarding_a_proposal_leaves_no_trace() {
    let project = agent_project();
    let clip = first_clip(&project);
    let revisions_before = project.history().unwrap().len();

    project.begin_staging(None, Some("tighten the intro"), None).unwrap();
    project.remove(clip.id).unwrap();
    project.discard_staged().unwrap();

    assert!(project.staged().unwrap().is_none());
    assert!(project.timeline().unwrap().clip(clip.id).is_some());
    assert_eq!(project.history().unwrap().len(), revisions_before);
    assert!(matches!(project.discard_staged(), Err(Error::NoStagedEdit)));
}

#[test]
fn closing_a_staging_session_moves_the_plan_epoch_though_sqlite_empties_the_table_without_a_hook() {
    use crate::plan_epoch::bumps_here;
    let project = agent_project();
    let clip = first_clip(&project);

    project.begin_staging(None, None, None).unwrap();
    project.remove(clip.id).unwrap();
    let before = bumps_here();
    project.discard_staged().unwrap();
    assert!(bumps_here() > before, "discarding a proposal");

    // A session that staged nothing just closes: no revision, no timeline written.
    project.begin_staging(None, None, None).unwrap();
    let before = bumps_here();
    project.apply_staged(false).unwrap();
    assert!(bumps_here() > before, "closing an empty proposal");
    assert!(project.staged().unwrap().is_none());
}

#[test]
fn undo_redo_and_a_row_replaced_by_its_key_each_move_the_plan_epoch() {
    use crate::plan_epoch::bumps_here;
    let project = Project::sample().unwrap();
    let clip = first_clip(&project);
    project.set_volume(clip.id, 0.4).unwrap();

    let before = bumps_here();
    project.undo().unwrap();
    assert!(bumps_here() > before, "undo");
    let before = bumps_here();
    project.redo().unwrap();
    assert!(bumps_here() > before, "redo");

    project.set_meta("epoch-key", "one").unwrap();
    let before = bumps_here();
    project.set_meta("epoch-key", "two").unwrap();
    assert!(bumps_here() > before, "INSERT OR REPLACE over an existing row");
    assert_eq!(project.meta("epoch-key").unwrap().as_deref(), Some("two"));
}

#[test]
fn a_project_replaced_by_another_moves_the_plan_epoch_after_the_swap() {
    use crate::plan_epoch::bumps_here;
    let mut shared = Project::open_in_memory().unwrap();
    let opened = Project::open_in_memory().unwrap();
    let before = bumps_here();
    shared.replace_with(opened).close();
    assert!(bumps_here() > before);
}

#[test]
fn a_user_edit_underneath_makes_the_proposal_stale() {
    let mut project = agent_project();
    let clips: Vec<Clip> = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .flat_map(|t| t.clips.clone())
        .collect();

    project.begin_staging(None, None, None).unwrap();
    project.set_volume(clips[0].id, 0.2).unwrap();

    // The user keeps cutting while the agent works.
    project.set_actor(EditSource::User);
    project.set_volume(clips[1].id, 0.9).unwrap();
    project.set_actor(EditSource::Agent);
    project.set_agent_review(false);

    let staged = project.staged().unwrap().unwrap();
    assert!(staged.stale, "the live timeline moved on since the proposal branched");
    assert!(matches!(project.apply_staged(false), Err(Error::StagedEditStale)));

    // Forcing it is the explicit "replace that newer cut" choice.
    let applied = project.apply_staged(true).unwrap();
    assert_eq!(applied.clip(clips[0].id).unwrap().volume, 0.2);
    assert_eq!(applied.clip(clips[1].id).unwrap().volume, 1.0);
}

#[test]
fn staging_refuses_to_nest_and_history_refuses_to_move_under_it() {
    let project = agent_project();
    project.begin_staging(None, None, None).unwrap();
    assert!(matches!(
        project.begin_staging(None, None, None),
        Err(Error::StagedEditPending)
    ));
    // Undo would walk the live history out from under the proposal.
    assert!(project.undo().is_err());
    project.discard_staged().unwrap();
    assert!(project.begin_staging(None, None, None).is_ok());
}

#[test]
fn applying_a_proposal_that_changed_nothing_adds_no_revision() {
    let project = agent_project();
    let revisions_before = project.history().unwrap().len();
    project.begin_staging(None, None, None).unwrap();
    project.apply_staged(false).unwrap();
    assert_eq!(project.history().unwrap().len(), revisions_before);
    assert!(project.staged().unwrap().is_none());
}

// ---- the master bus ----------------------------------------------------

#[test]
fn the_master_fader_clamps_and_each_move_is_one_revision() {
    let project = Project::sample().unwrap();
    let before = project.history().unwrap().len();

    let master = project.set_master_volume(0.5).unwrap();
    assert_eq!(master.volume, 0.5);
    assert_eq!(project.timeline().unwrap().master.volume, 0.5);
    let history = project.history().unwrap();
    assert_eq!(history.len(), before + 1);
    assert_eq!(history.last().unwrap().label, "Set master level");

    assert_eq!(
        project.set_master_volume(99.0).unwrap().volume,
        crate::model::MASTER_MAX_VOLUME
    );
    assert_eq!(project.set_master_volume(-2.0).unwrap().volume, 0.0);
    // Not a number is refused, not clamped into something: and changes nothing.
    let head = project.history().unwrap().len();
    assert!(matches!(project.set_master_volume(f64::NAN), Err(Error::InvalidArgument(_))));
    assert!(matches!(
        project.set_master_volume(f64::INFINITY),
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(project.history().unwrap().len(), head);

    // Back at unity the saved timeline carries no master at all again.
    project.set_master_volume(1.0).unwrap();
    assert!(!project.timeline_json().unwrap().contains("master"));
}

#[test]
fn the_master_limiter_remembers_its_ceiling() {
    let project = Project::sample().unwrap();
    let on = project.set_master_limiter(true, None).unwrap();
    assert!(on.limiter);
    assert_eq!(on.ceiling_db, -1.5, "the default ceiling when none was ever chosen");

    let moved = project.set_master_limiter(true, Some(-3.5)).unwrap();
    assert_eq!(moved.ceiling_db, -3.5);
    // Off keeps the ceiling, and turning it back on without one returns to it.
    let off = project.set_master_limiter(false, None).unwrap();
    assert!(!off.limiter);
    assert_eq!(off.ceiling_db, -3.5);
    assert_eq!(project.set_master_limiter(true, None).unwrap().ceiling_db, -3.5);

    // The ends: below the filter's floor, above full scale.
    assert_eq!(
        project.set_master_limiter(true, Some(-90.0)).unwrap().ceiling_db,
        crate::model::MASTER_MIN_CEILING_DB
    );
    assert_eq!(project.set_master_limiter(true, Some(6.0)).unwrap().ceiling_db, 0.0);
    assert!(matches!(
        project.set_master_limiter(true, Some(f64::NAN)),
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(project.history().unwrap().last().unwrap().label, "Set master limiter");
}

#[test]
fn the_true_peak_switch_is_one_revision_and_survives_the_limiter_going_off() {
    let project = Project::sample().unwrap();
    let on = project.set_master_true_peak(true).unwrap();
    assert!(on.true_peak && !on.limiter);
    assert_eq!(project.history().unwrap().last().unwrap().label, "Limit true peaks");
    project.set_master_limiter(true, None).unwrap();
    let off = project.set_master_limiter(false, None).unwrap();
    assert!(off.true_peak, "kept while the limiter is off");
    let json = serde_json::to_string(&project.timeline().unwrap()).unwrap();
    assert!(json.contains("\"true_peak\":true"), "{json}");
    assert!(!project.set_master_true_peak(false).unwrap().true_peak);
    let json = serde_json::to_string(&project.timeline().unwrap()).unwrap();
    assert!(!json.contains("true_peak"), "off is not written: {json}");
}

#[test]
fn an_agents_master_edit_stages_and_is_not_thrown_away_as_empty() {
    let project = agent_project();
    let revisions_before = project.history().unwrap().len();
    project.begin_staging(None, None, None).unwrap();
    project.set_master_volume(0.6).unwrap();
    project.set_master_limiter(true, Some(-2.0)).unwrap();

    // The cut the user is looking at has not moved; the agent's view has.
    assert!(project.timeline().unwrap().master.is_default());
    let proposed = project.working_timeline().unwrap().master;
    assert_eq!((proposed.volume, proposed.limiter, proposed.ceiling_db), (0.6, true, -2.0));
    assert_eq!(project.history().unwrap().len(), revisions_before);

    // And it reads as a change to review — an empty diff would be discarded.
    let staged = project.staged().unwrap().unwrap();
    assert_eq!(staged.diff.entries.len(), 1, "{:?}", staged.diff);
    assert_eq!(staged.diff.entries[0].kind, DiffKind::MasterChanged);

    let applied = project.apply_staged(false).unwrap();
    assert_eq!(applied.master.volume, 0.6);
    assert!(project.timeline().unwrap().master.limiter);
    assert_eq!(project.history().unwrap().len(), revisions_before + 1);

    // The true-peak switch alone is a change too, even with the limiter it shapes off.
    let project = agent_project();
    let revisions_before = project.history().unwrap().len();
    project.begin_staging(None, None, None).unwrap();
    project.set_master_true_peak(true).unwrap();
    assert!(!project.timeline().unwrap().master.true_peak);
    let staged = project.staged().unwrap().unwrap();
    assert_eq!(staged.diff.entries.len(), 1, "{:?}", staged.diff);
    assert_eq!(staged.diff.entries[0].kind, DiffKind::MasterChanged);
    assert!(project.apply_staged(false).unwrap().master.true_peak);
    assert!(project.timeline().unwrap().master.true_peak);
    assert_eq!(project.history().unwrap().len(), revisions_before + 1);
}

#[test]
fn a_levels_range_past_the_end_of_the_cut_is_refused_not_widened() {
    let project = Project::sample().unwrap();
    let (timeline, assets) = project.levels_inputs().unwrap();
    let end = timeline.for_render().duration();
    let past = Project::measure_levels(
        &timeline,
        &assets,
        Some(TimeRange {
            start: end + 5.0,
            end: end + 9.0,
        }),
        false,
        &|| false,
    );
    match past {
        Err(Error::InvalidArgument(why)) => assert!(why.contains("only"), "{why}"),
        other => panic!("expected the caller's mistake, got {other:?}"),
    }
}

#[test]
fn the_levels_of_a_cut_with_no_audio_say_so_without_running_anything() {
    // No clips, so there is no graph to meter and ffmpeg is never launched.
    let project = Project::open_in_memory().unwrap();
    let levels = project.levels(None, false).unwrap();
    assert!(levels.master.is_none());
    assert!(levels.tracks.is_empty());
    assert_eq!(levels.notes, ["The cut has no audio to measure."]);
    assert_eq!(levels.target_lufs, -14.0);
    // A range that is not a span is the caller's mistake.
    let bad = project.levels(Some(TimeRange { start: 4.0, end: 2.0 }), false);
    assert!(
        matches!(bad, Err(Error::InvalidArgument(_))),
        "a reversed range must be refused"
    );
}

#[test]
fn claiming_a_task_stages_its_work_and_resolving_applies_it() {
    let mut project = Project::open_in_memory().unwrap();
    let task = project.add_task("tighten the intro").unwrap();

    project.set_actor(EditSource::Agent);

    let claimed = project.claim_next_task(None).unwrap().unwrap();
    let staged = project.staged().unwrap().unwrap();
    assert_eq!(staged.task_id, Some(claimed.id));

    project.add_track(StreamKind::Video, Some("B-roll".to_string())).unwrap();
    assert_eq!(project.timeline().unwrap().tracks.len(), 2, "live cut untouched");
    assert_eq!(project.working_timeline().unwrap().tracks.len(), 3);

    project
        .complete_task(task.id, Some("added a B-roll track".to_string()))
        .unwrap();
    // Accepting the task is accepting its edits — and the user's to do.
    project.set_actor(EditSource::User);
    project.resolve_task(task.id).unwrap();
    assert_eq!(project.timeline().unwrap().tracks.len(), 3);
    assert!(project.staged().unwrap().is_none());
}

#[test]
fn dismissing_a_task_throws_its_staged_edits_away() {
    let mut project = Project::open_in_memory().unwrap();
    let task = project.add_task("tighten the intro").unwrap();
    project.set_actor(EditSource::Agent);
    project.claim_next_task(None).unwrap().unwrap();
    project.add_track(StreamKind::Video, Some("B-roll".to_string())).unwrap();

    project.remove_task(task.id).unwrap();
    assert!(project.staged().unwrap().is_none());
    assert_eq!(project.timeline().unwrap().tracks.len(), 2);
}

#[test]
fn claiming_a_second_task_refuses_while_the_first_is_still_staged() {
    let mut project = Project::open_in_memory().unwrap();
    let first = project.add_task("tighten the intro").unwrap();
    let second = project.add_task("caption the cut").unwrap();
    project.set_actor(EditSource::Agent);

    let claimed = project.claim_next_task(None).unwrap().unwrap();
    assert_eq!(claimed.id, first.id);

    // The second task must not fold its edits into the first task's
    // session — it stays queued rather than sharing another task's proposal.
    assert!(matches!(project.claim_next_task(None), Err(Error::StagedEditPending)));
    assert_eq!(project.require_task(second.id).unwrap().status, TaskStatus::Queued);
    let staged = project.staged().unwrap().unwrap();
    assert_eq!(staged.task_id, Some(first.id));
}

#[test]
fn failing_a_task_discards_its_staged_edits_and_frees_the_queue() {
    let mut project = Project::open_in_memory().unwrap();
    let first = project.add_task("tighten the intro").unwrap();
    let second = project.add_task("caption the cut").unwrap();
    project.set_actor(EditSource::Agent);

    let claimed = project.claim_next_task(None).unwrap().unwrap();
    assert_eq!(claimed.id, first.id);
    project.add_track(StreamKind::Video, Some("B-roll".to_string())).unwrap();

    project.fail_task(first.id, "could not find a good cut").unwrap();
    assert!(project.staged().unwrap().is_none(), "the orphaned session is cleaned up");
    assert_eq!(project.timeline().unwrap().tracks.len(), 2, "its staged edit never landed");

    // The queue is not wedged: the next claim opens its own session.
    let claimed = project.claim_next_task(None).unwrap().unwrap();
    assert_eq!(claimed.id, second.id);
    let staged = project.staged().unwrap().unwrap();
    assert_eq!(staged.task_id, Some(second.id));
}

#[test]
fn claim_next_task_clears_a_session_orphaned_by_an_already_terminal_task() {
    // A session left behind by a task that somehow went terminal without
    // going through `fail_task`/`remove_task` (or one from a build predating
    // that cleanup) must not wedge the queue forever.
    let mut project = Project::open_in_memory().unwrap();
    let first = project.add_task("tighten the intro").unwrap();
    let second = project.add_task("caption the cut").unwrap();
    project.set_actor(EditSource::Agent);
    project.begin_staging(Some(first.id), None, None).unwrap();
    project.complete_task(first.id, Some("done".to_string())).unwrap();
    // Accepting is the user's while review is on.
    project.set_actor(EditSource::User);
    project.resolve_task(first.id).unwrap();
    // `resolve_task` already applies/clears its own session; stage a fresh
    // one directly against the now-`Done` task to simulate the orphan.
    project.set_actor(EditSource::Agent);
    project.begin_staging(Some(first.id), None, None).unwrap();

    let claimed = project.claim_next_task(None).unwrap().unwrap();
    assert_eq!(claimed.id, second.id);
    let staged = project.staged().unwrap().unwrap();
    assert_eq!(staged.task_id, Some(second.id));
}

#[test]
fn a_recycled_seq_from_undo_then_edit_is_still_caught_as_stale() {
    let mut project = Project::sample().unwrap();
    let clips: Vec<Clip> = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .flat_map(|t| t.clips.clone())
        .collect();

    // Advance the head once so there is somewhere for undo to land.
    project.set_volume(clips[0].id, 0.5).unwrap();
    let base_seq = project.history().unwrap().last().unwrap().seq;

    project.set_actor(EditSource::Agent);

    project.set_agent_review(false);
    project.begin_staging(None, None, None).unwrap();
    project.set_volume(clips[0].id, 0.3).unwrap();
    assert_eq!(project.staged().unwrap().unwrap().base_seq, base_seq);

    // The user undoes underneath the proposal, then makes an unrelated
    // edit — `record_revision` prunes the undone row and reinserts a
    // *different* one at the same seq the proposal was staged from.
    project.set_actor(EditSource::User);
    project.undo().unwrap();
    project.set_volume(clips[1].id, 0.9).unwrap();
    project.set_actor(EditSource::Agent);
    project.set_agent_review(false);

    assert_eq!(
        project.history().unwrap().last().unwrap().seq,
        base_seq,
        "the seq was recycled"
    );
    assert!(project.staged().unwrap().unwrap().stale);
    assert!(matches!(project.apply_staged(false), Err(Error::StagedEditStale)));

    // Forcing it still works, same as any other staleness.
    let applied = project.apply_staged(true).unwrap();
    assert_eq!(applied.clip(clips[0].id).unwrap().volume, 0.3);
    assert_eq!(applied.clip(clips[1].id).unwrap().volume, 1.0);
}

#[test]
fn a_revision_explains_what_it_changed() {
    let project = Project::sample().unwrap();
    let clip = first_clip(&project);
    project.set_volume(clip.id, 0.25).unwrap();

    let seq = project.history().unwrap().last().unwrap().seq;
    let diff = project.revision_diff(seq).unwrap();
    assert_eq!(diff.entries.len(), 1);
    assert_eq!(diff.entries[0].kind, DiffKind::ClipChanged);
    assert!(
        diff.entries[0].detail.as_deref().unwrap().contains("volume 100% → 25%"),
        "{:?}",
        diff.entries[0].detail
    );
    // The baseline revision changed nothing by definition.
    assert!(project.revision_diff(0).unwrap().is_empty());
}

// ---- smart crop ---------------------------------------------------------

#[test]
fn smart_crop_has_nothing_to_do_when_the_footage_is_already_the_frame() {
    let project = Project::sample().unwrap();
    // The sample is 1920x1080 with no explicit delivery frame, so the frame
    // is derived from the footage and every shot already fills it.
    let err = project.smart_crop_inputs(None).unwrap_err().to_string();
    assert!(err.contains("already"), "{err}");
}

#[test]
fn smart_crop_plans_every_video_clip_for_a_vertical_delivery() {
    let project = Project::sample().unwrap();
    project
        .set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover)))
        .unwrap();
    let plan = project.smart_crop_inputs(None).unwrap();
    assert!((plan.target_aspect - 1080.0 / 1920.0).abs() < 1e-9);
    let video_clips: usize = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .filter(|t| t.kind == StreamKind::Video)
        .map(|t| t.clips.len())
        .sum();
    assert_eq!(plan.jobs.len(), video_clips);
    // Each job points at real media over the clip's own source window.
    for job in &plan.jobs {
        assert!(job.path.to_string_lossy().ends_with(".mp4"));
        assert!(job.end > job.start);
        // Both sample sources are 16:9 — 1080p and 4K.
        assert!((job.width as f64 / job.height as f64 - 16.0 / 9.0).abs() < 1e-6);
    }
}

#[test]
fn smart_crop_can_be_narrowed_to_one_clip() {
    let project = Project::sample().unwrap();
    project
        .set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover)))
        .unwrap();
    let clip = first_video_clip(&project);
    let plan = project.smart_crop_inputs(Some(clip)).unwrap();
    assert_eq!(plan.jobs.len(), 1);
    assert_eq!(plan.jobs[0].clip_id, clip);
}

#[test]
fn smart_crop_skips_a_locked_track_but_not_a_clip_named_on_one() {
    let project = Project::sample().unwrap();
    project
        .set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover)))
        .unwrap();
    let clip = first_video_clip(&project);
    let track = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.clips.iter().any(|c| c.id == clip))
        .unwrap()
        .id;
    project.set_track_locked(track, true).unwrap();
    // The bulk pass leaves the locked track alone...
    let bulk = project.smart_crop_inputs(None);
    assert!(!bulk.map(|p| p.jobs.iter().any(|j| j.clip_id == clip)).unwrap_or(false));
    // ...but naming one of its clips is an explicit instruction.
    assert_eq!(project.smart_crop_inputs(Some(clip)).unwrap().jobs.len(), 1);
}

#[test]
fn applying_crops_is_one_undoable_edit_that_only_counts_real_changes() {
    let project = Project::sample().unwrap();
    project
        .set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover)))
        .unwrap();
    let clip = first_video_clip(&project);
    let crop = CropFrame {
        left: 0.1,
        right: 0.5836,
        top: 0.0,
        bottom: 0.0,
        offset: -0.6,
    };
    let before = project.history().unwrap().len();
    assert_eq!(project.apply_smart_crops(&[(clip, crop)]).unwrap(), 1);
    let t = clip_transform(&project, clip);
    assert!((t.crop_left - 0.1).abs() < 1e-9 && (t.crop_right - 0.5836).abs() < 1e-9);
    assert_eq!(project.history().unwrap().len(), before + 1);
    assert_eq!(project.history().unwrap().last().unwrap().label, "Smart crop");

    // Re-running the same pass changes nothing, and leaves no revision behind.
    assert_eq!(project.apply_smart_crops(&[(clip, crop)]).unwrap(), 0);
    assert_eq!(project.history().unwrap().len(), before + 1);

    // The whole pass undoes in one step.
    project.undo().unwrap();
    assert_eq!(clip_transform(&project, clip).crop_left, 0.0);
}

#[test]
fn applying_no_crops_writes_no_revision() {
    let project = Project::sample().unwrap();
    let before = project.history().unwrap().len();
    assert_eq!(project.apply_smart_crops(&[]).unwrap(), 0);
    assert_eq!(project.history().unwrap().len(), before);
}

#[test]
fn framing_plans_every_shape_but_the_projects_own() {
    let project = Project::sample().unwrap();
    project
        .set_delivery_format(Some(Delivery::new(1080, 1920, Fit::Cover)))
        .unwrap();
    let plan = project
        .framing_inputs(&[
            Delivery::new(1080, 1920, Fit::Cover),
            Delivery::new(1080, 1080, Fit::Cover),
            Delivery::new(720, 1280, Fit::Cover),
            Delivery::new(1920, 1080, Fit::Contain),
        ])
        .unwrap();
    // 9:16 is the project frame (and 720x1280 the same shape); the rest dedupe.
    assert_eq!(plan.ratios, vec![(1, 1), (16, 9)]);
    let video_clips: usize = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .filter(|t| t.kind == StreamKind::Video)
        .map(|t| t.clips.len())
        .sum();
    assert_eq!(plan.jobs.len(), video_clips);

    // Only the project's own shape asked for: nothing to frame, not an error.
    let none = project.framing_inputs(&[Delivery::new(1080, 1920, Fit::Cover)]).unwrap();
    assert!(none.ratios.is_empty() && none.jobs.is_empty());
}

#[test]
fn framings_land_as_one_labelled_revision_and_only_when_they_change() {
    let project = Project::sample().unwrap();
    let clip = first_video_clip(&project);
    let crop = CropFrame {
        left: 0.3,
        right: 0.2625,
        top: 0.0,
        bottom: 0.0,
        offset: 0.2,
    };
    let framings = vec![
        (clip, Framing::new((1, 1), &crop)),
        (clip, Framing::new((4, 5), &CropFrame::default())),
    ];
    let before = project.history().unwrap().len();
    assert_eq!(project.apply_framings(&framings).unwrap(), 1, "one clip changed");
    let history = project.history().unwrap();
    assert_eq!(history.len(), before + 1);
    assert_eq!(history.last().unwrap().label, "Frame for 1:1, 4:5");
    let timeline = project.timeline().unwrap();
    let (ti, ci) = timeline.locate(clip).unwrap();
    let framed = &timeline.tracks[ti].clips[ci];
    assert_eq!(framed.framing_for((1, 1)).unwrap().crop_left, 0.3);
    assert_eq!(framed.framing_for((4, 5)).unwrap().crop_left, 0.0);
    // The project frame's own crop is untouched.
    assert!(!framed.transform.has_crop());

    // Same framings again: no change, no revision.
    assert_eq!(project.apply_framings(&framings).unwrap(), 0);
    assert_eq!(project.history().unwrap().len(), before + 1);

    // The whole pass undoes in one step.
    project.undo().unwrap();
    let timeline = project.timeline().unwrap();
    let (ti, ci) = timeline.locate(clip).unwrap();
    assert!(timeline.tracks[ti].clips[ci].framings.is_empty());
}

fn first_video_clip(project: &Project) -> Uuid {
    project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .filter(|t| t.kind == StreamKind::Video)
        .flat_map(|t| t.clips.iter())
        .next()
        .unwrap()
        .id
}

fn clip_transform(project: &Project, clip_id: Uuid) -> crate::model::Transform {
    let timeline = project.timeline().unwrap();
    let (ti, ci) = timeline.locate(clip_id).unwrap();
    timeline.tracks[ti].clips[ci].transform
}

#[test]
fn captions_survive_the_cut_that_silence_removal_makes() {
    let project = Project::sample().unwrap();
    let asset = project.list_assets().unwrap()[0].id;
    // The sample's transcript is two lines over 0..12.5s with a silence at
    // 12.5..14.0; put the asset on the timeline at a non-zero position and
    // trimmed, which is what makes source time and timeline time disagree.
    let timeline = project.timeline().unwrap();
    let track = timeline.tracks[0].id;
    for t in &timeline.tracks {
        for clip in &t.clips {
            project.remove(clip.id).unwrap();
        }
    }
    project
        .add_clip_to_timeline(asset, Some(track), 5.5, 12.5, Some(2.0))
        .unwrap();

    let created = project.generate_captions(CaptionOptions::default()).unwrap();
    assert!(!created.is_empty());
    // Every caption sits inside the clip's timeline span (2.0 .. 9.0), not
    // at the transcript's own 5.5 .. 12.5.
    for o in &created {
        assert!(o.start >= 2.0 - 1e-6 && o.end <= 9.0 + 1e-6, "{o:?} is in source time");
        assert!(o.generated);
    }
    // The line spoken before the in-point was trimmed away, so it gets none.
    assert!(
        !created.iter().any(|o| o.text.contains("Welcome")),
        "captioned words that are not in the cut: {created:?}"
    );

    // Regenerating replaces rather than stacks, and leaves a hand-made
    // title alone.
    let title = project.add_overlay("Chapter one".to_string(), 0.0, 1.5).unwrap();
    let again = project.generate_captions(CaptionOptions::default()).unwrap();
    let overlays = project.timeline().unwrap().overlays;
    assert_eq!(overlays.iter().filter(|o| o.generated).count(), again.len());
    assert!(overlays.iter().any(|o| o.id == title.id), "the typed title was thrown away");

    // Clearing takes only the generated ones.
    let cleared = project.clear_captions().unwrap();
    assert_eq!(cleared, again.len());
    let overlays = project.timeline().unwrap().overlays;
    assert_eq!(overlays.len(), 1);
    assert_eq!(overlays[0].id, title.id);
}

#[test]
fn captions_need_something_transcribed_on_the_timeline() {
    let project = Project::open_in_memory().unwrap();
    let err = project.generate_captions(CaptionOptions::default()).unwrap_err();
    assert!(err.to_string().contains("run analysis first"), "{err}");
}

const SRT: &str = "1\n00:00:01,000 --> 00:00:03,000\nHello there\n\n2\n00:00:04,000 --> 00:00:06,000\nGeneral Kenobi\n";

/// A project whose timeline is one `len`-second clip of its first asset,
/// cut from `source_in`, sitting at `at`.
fn project_with_one_clip(source_in: f64, len: f64, at: f64) -> (Project, Uuid) {
    let project = Project::sample().unwrap();
    let asset = project.list_assets().unwrap()[0].id;
    let timeline = project.timeline().unwrap();
    let track = timeline.tracks[0].id;
    for t in &timeline.tracks {
        for clip in &t.clips {
            project.remove(clip.id).unwrap();
        }
    }
    project
        .add_clip_to_timeline(asset, Some(track), source_in, source_in + len, Some(at))
        .unwrap();
    (project, asset)
}

fn captions_on(project: &Project) -> Vec<TextOverlay> {
    project
        .working_timeline()
        .unwrap()
        .overlays
        .into_iter()
        .filter(|o| o.generated)
        .collect()
}

/// What the surfaces do, in the order they do it: parse with no project in
/// hand, then place under the project.
fn import_with(project: &Project, text: &str, format: Option<CaptionFormat>, req: CaptionImportRequest) -> Result<ImportSummary> {
    let file = parse_captions(text, format)?;
    project.import_captions(&file, req)
}

fn import_text(project: &Project, text: &str, base: CaptionTimeBase, options: CaptionOptions) -> Result<ImportSummary> {
    import_with(
        project,
        text,
        None,
        CaptionImportRequest {
            base,
            options,
            offset: 0.0,
        },
    )
}

fn import_srt(project: &Project) -> Result<ImportSummary> {
    import_text(project, SRT, CaptionTimeBase::Timeline, CaptionOptions::default())
}

#[test]
fn importing_a_subtitle_file_is_one_revision_of_generated_captions() {
    let (project, _) = project_with_one_clip(0.0, 20.0, 0.0);
    let title = project.add_overlay("Chapter one".to_string(), 0.0, 1.5).unwrap();
    let revisions = project.history().unwrap().len();

    let summary = import_srt(&project).unwrap();
    assert_eq!(
        summary,
        ImportSummary {
            format: CaptionFormat::Srt,
            cues: 2,
            placed: 2,
            captions: 2,
            skipped_lines: 0,
            dropped_outside: 0,
            dropped_short: 0,
            dropped_overlap: 0,
            replaced: 0,
        }
    );
    let history = project.history().unwrap();
    assert_eq!(history.len(), revisions + 1, "one revision, however many captions");
    let last = history.last().unwrap();
    assert_eq!((last.label.as_str(), last.source), ("Import captions", EditSource::User));

    let captions = captions_on(&project);
    assert_eq!(
        captions.iter().map(|o| (o.text.as_str(), o.start, o.end)).collect::<Vec<_>>(),
        [("Hello there", 1.0, 3.0), ("General Kenobi", 4.0, 6.0)]
    );
    assert!(
        project.timeline().unwrap().overlays.iter().any(|o| o.id == title.id),
        "the typed title is untouched"
    );

    // Importing again replaces the set rather than stacking a second one…
    let again = import_text(
        &project,
        SRT,
        CaptionTimeBase::Timeline,
        CaptionOptions::styled(CaptionStyle::WordPunch),
    )
    .unwrap();
    assert_eq!(again.replaced, 2);
    assert_eq!(captions_on(&project).len(), again.captions);
    assert!(captions_on(&project).iter().all(|o| o.bold), "in the style asked for");
    // …and Clear takes imported captions exactly as it takes generated ones.
    assert_eq!(project.clear_captions().unwrap(), again.captions);
    let overlays = project.timeline().unwrap().overlays;
    assert_eq!(overlays.len(), 1);
    assert_eq!(overlays[0].id, title.id);
}

#[test]
fn a_later_generate_captions_replaces_an_imported_set() {
    // The documented interaction: captions are one lane, so whichever set was
    // written last is the set.
    let (project, _) = project_with_one_clip(5.5, 7.0, 2.0);
    import_srt(&project).unwrap();
    assert!(captions_on(&project).iter().any(|o| o.text == "Hello there"));
    let generated = project.generate_captions(CaptionOptions::default()).unwrap();
    let now = captions_on(&project);
    assert_eq!(now.len(), generated.len());
    assert!(!now.iter().any(|o| o.text == "Hello there"), "the imported set was replaced");
}

#[test]
fn ass_cues_in_source_time_follow_the_cut() {
    // The asset's 5.5..12.5 sits at 2.0: source time 6.0 is timeline 2.5.
    let (project, asset) = project_with_one_clip(5.5, 7.0, 2.0);
    let ass = "[Script Info]\nTitle: x\n\n[Events]\n\
                   Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
                   Dialogue: 0,0:00:06.00,0:00:08.00,Default,,0,0,0,,Kept\n\
                   Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Trimmed away\n\
                   Dialogue: 0,0:00:30.00,0:00:32.00,Default,,0,0,0,,Past the footage\n\
                   Dialogue: 0,broken\n";
    let summary = import_text(&project, ass, CaptionTimeBase::Source(asset), CaptionOptions::default()).unwrap();
    assert_eq!(summary.format, CaptionFormat::Ass);
    assert_eq!(
        (summary.cues, summary.placed, summary.dropped_outside, summary.skipped_lines),
        (3, 1, 2, 1)
    );
    let captions = captions_on(&project);
    assert_eq!(captions.len(), 1);
    assert_eq!(captions[0].text, "Kept");
    assert!(
        (captions[0].start - 2.5).abs() < 1e-9 && (captions[0].end - 4.5).abs() < 1e-9,
        "{:?}",
        captions[0]
    );
}

#[test]
fn an_offset_moves_every_cue_before_it_is_placed() {
    // A broadcast file whose clock starts at one hour.
    let (project, _) = project_with_one_clip(0.0, 20.0, 0.0);
    let broadcast = "1\n01:00:01,000 --> 01:00:03,000\nHello there\n\n2\n01:00:04,000 --> 01:00:06,000\nGeneral Kenobi\n";
    let req = |offset| CaptionImportRequest {
        offset,
        ..CaptionImportRequest::default()
    };
    let err = import_with(&project, broadcast, None, req(0.0)).unwrap_err();
    assert!(err.to_string().contains("fall inside the cut"), "{err}");
    let summary = import_with(&project, broadcast, None, req(-3600.0)).unwrap();
    assert_eq!((summary.placed, summary.dropped_outside), (2, 0));
    let captions = captions_on(&project);
    assert_eq!(
        captions.iter().map(|o| (o.start, o.end)).collect::<Vec<_>>(),
        [(1.0, 3.0), (4.0, 6.0)]
    );
    // A positive offset shifts the other way, and part of a cue pushed below
    // zero is clipped, the rest dropped.
    let early = "1\n00:00:00,000 --> 00:00:04,000\nStarts at zero\n\n2\n00:00:01,000 --> 00:00:02,000\nBefore it\n";
    let summary = import_with(&project, early, None, req(-2.0)).unwrap();
    assert_eq!((summary.placed, summary.dropped_outside), (1, 1), "{summary:?}");
    assert_eq!(captions_on(&project)[0].start, 0.0);
    // Not a number of seconds, or an absurd one: refused, not applied.
    for bad in [f64::NAN, f64::INFINITY, 1e9, -1e9] {
        let err = import_with(&project, SRT, None, req(bad)).unwrap_err();
        assert!(err.to_string().contains("offset"), "{bad}: {err}");
    }
}

#[test]
fn a_cue_too_short_to_read_is_counted_on_its_own() {
    let (project, _) = project_with_one_clip(0.0, 10.0, 0.0);
    let text = "1\n00:00:01,000 --> 00:00:03,000\nReadable\n\n2\n00:00:04,000 --> 00:00:04,050\nA blink\n\n3\n00:09:00,000 --> 00:09:03,000\nWay past the end\n";
    let summary = import_text(&project, text, CaptionTimeBase::Timeline, CaptionOptions::default()).unwrap();
    assert_eq!(
        (
            summary.cues,
            summary.placed,
            summary.dropped_outside,
            summary.dropped_short,
            summary.dropped_overlap
        ),
        (3, 1, 1, 1, 0)
    );
    assert_eq!(
        summary.cues,
        summary.placed + summary.dropped_outside + summary.dropped_short + summary.dropped_overlap
    );
    // When *nothing* could be shown the error says why, not "outside the cut".
    let err = import_srt_text(&project, "1\n00:00:04,000 --> 00:00:04,050\nA blink\n").unwrap_err();
    assert!(err.to_string().contains("too short to read"), "{err}");
}

fn import_srt_text(project: &Project, text: &str) -> Result<ImportSummary> {
    import_text(project, text, CaptionTimeBase::Timeline, CaptionOptions::default())
}

#[test]
fn an_import_that_would_write_too_many_captions_is_refused_whole() {
    // 9,000 five-word cues in word punch: 45,000 one-word captions. The cut is
    // empty, so the cues are not clipped to anything — which is exactly when
    // an unbounded import used to write all of them.
    let project = Project::open_in_memory().unwrap();
    let text: String = (0..9_000)
        .map(|i| {
            let (a, b) = (i as f64 * 1.5, i as f64 * 1.5 + 1.0);
            let ts = |t: f64| {
                format!(
                    "{:02}:{:02}:{:02},000",
                    (t / 3600.0) as u32,
                    (t as u32 / 60) % 60,
                    t as u32 % 60
                )
            };
            format!("{i}\n{} --> {}\nalpha bravo charlie delta echo\n\n", ts(a), ts(b))
        })
        .collect();
    let revisions = project.history().unwrap().len();
    let err = import_text(
        &project,
        &text,
        CaptionTimeBase::Timeline,
        CaptionOptions::styled(CaptionStyle::WordPunch),
    )
    .unwrap_err();
    assert!(err.to_string().contains("at most 20000 at a time"), "{err}");
    assert!(project.timeline().unwrap().overlays.is_empty());
    assert_eq!(project.history().unwrap().len(), revisions);
    // The same file in the lines look is a few captions per cue and lands.
    let summary = import_text(&project, &text, CaptionTimeBase::Timeline, CaptionOptions::default()).unwrap();
    assert!(
        summary.captions <= MAX_IMPORTED_CAPTIONS && summary.placed > 8_000,
        "{summary:?}"
    );
}

#[test]
fn cues_on_an_empty_timeline_are_bounded_by_the_day_not_by_the_file() {
    let project = Project::open_in_memory().unwrap();
    let text = "1\n00:00:01,000 --> 00:00:03,000\nToday\n\n2\n30:00:00,000 --> 30:00:03,000\nA day and a bit on\n";
    let summary = import_text(&project, text, CaptionTimeBase::Timeline, CaptionOptions::default()).unwrap();
    assert_eq!((summary.placed, summary.dropped_outside), (1, 1), "{summary:?}");
}

#[test]
fn an_import_that_cannot_land_writes_nothing_and_keeps_the_captions_it_would_replace() {
    let (project, asset) = project_with_one_clip(0.0, 10.0, 0.0);
    import_srt(&project).unwrap();
    let before = project.timeline().unwrap();
    let revisions = project.history().unwrap().len();
    let refused = |result: Result<ImportSummary>, needle: &str| {
        let err = result.unwrap_err();
        assert!(err.to_string().contains(needle), "{err}");
    };
    let at_timeline = |text: &str| import_text(&project, text, CaptionTimeBase::Timeline, CaptionOptions::default());
    let at_source = |text: &str, asset| import_text(&project, text, CaptionTimeBase::Source(asset), CaptionOptions::default());

    // Nothing to read.
    refused(at_timeline("not a subtitle file"), "no captions found");
    // Every cue after the cut ends.
    refused(
        at_timeline("1\n00:05:00,000 --> 00:05:02,000\nToo late\n"),
        "fall inside the cut",
    );
    // An asset that is not in the project, and one that is but not in the cut.
    let missing = Uuid::new_v4();
    assert!(matches!(at_source(SRT, missing), Err(Error::AssetNotFound(id)) if id == missing));
    let unused = project
        .list_assets()
        .unwrap()
        .into_iter()
        .map(|a| a.id)
        .find(|id| *id != asset)
        .expect("a second sample asset");
    refused(at_source(SRT, unused), "not used by any clip");
    // Source time the clips do not show.
    refused(
        at_source("1\n00:09:00,000 --> 00:09:02,000\nElsewhere\n", asset),
        "land on footage",
    );
    // An ASS file with an unusable Format line.
    refused(
        at_timeline("[Events]\nFormat: Layer, Text\nDialogue: 0,hi"),
        "Start, End and Text",
    );

    assert_eq!(project.history().unwrap().len(), revisions, "a refused import is not an edit");
    assert_eq!(
        serde_json::to_string(&project.timeline().unwrap()).unwrap(),
        serde_json::to_string(&before).unwrap()
    );
}

#[test]
fn a_named_format_overrides_the_guess() {
    let (project, _) = project_with_one_clip(0.0, 10.0, 0.0);
    let err = import_with(&project, SRT, Some(CaptionFormat::Ass), CaptionImportRequest::default()).unwrap_err();
    assert!(err.to_string().contains("no captions found in the ASS"), "{err}");
}

#[test]
fn an_agents_import_is_staged_and_lands_as_one_agent_revision() {
    let (mut project, _) = project_with_one_clip(0.0, 10.0, 0.0);
    project.set_actor(EditSource::Agent);
    project.set_agent_review(false);
    let revisions = project.history().unwrap().len();
    project.begin_staging(None, None, None).unwrap();

    let summary = import_srt(&project).unwrap();
    assert_eq!(summary.captions, 2);

    // The cut the user is looking at has not moved; the agent's own view has.
    assert!(project.timeline().unwrap().overlays.is_empty());
    assert_eq!(captions_on(&project).len(), 2, "the agent sees its proposal");
    let staged = project.staged().unwrap().expect("a proposal");
    assert_eq!(staged.edits, ["Import captions"]);
    assert!(staged.diff.entries.iter().any(|e| e.kind == DiffKind::OverlayAdded));
    assert_eq!(project.history().unwrap().len(), revisions);

    project.apply_staged(false).unwrap();
    assert_eq!(project.timeline().unwrap().overlays.len(), 2);
    let history = project.history().unwrap();
    assert_eq!(history.len(), revisions + 1);
    assert_eq!(history.last().unwrap().source, EditSource::Agent);
}

/// A phone's HLG footage reaches the preview two ways — straight from the
/// original (tone-mapped in the graph) until its proxy lands, then from the
/// proxy (tone-mapped when it was encoded). Both must convert exactly once,
/// so the same moment looks the same either side of the swap.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored hdr_asset_previews`
#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn hdr_asset_previews_through_a_tonemapped_proxy_exactly_once() {
    use crate::engine::test_support::StatusBounded;
    let ffmpeg = std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
    let ffprobe = std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string());
    let dir = std::env::temp_dir().join(format!("kerf-hdr-proxy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let media = dir.join("hlg.mp4");
    let sdr = dir.join("sdr.mp4");
    let tagged = std::process::Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=640x360:rate=30:duration=2,format=yuv420p")
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .args([
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
        ])
        .arg(&sdr)
        .status_bounded();
    assert!(tagged.unwrap().success());
    // FFmpeg 9 wants the card's colourspace stated in the graph, FFmpeg 4
    // wants it on a file: one of the two starts `zscale` from a known place.
    let convert = |input: &[&str]| {
        std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(input)
            .args([
                "-vf",
                "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt2020:t=arib-std-b67:m=bt2020nc:r=tv,format=yuv420p10le",
            ])
            .args(["-c:v", "libx265", "-x265-params", "log-level=error"])
            .args([
                "-color_primaries",
                "bt2020",
                "-color_trc",
                "arib-std-b67",
                "-colorspace",
                "bt2020nc",
            ])
            .arg(&media)
            .status_bounded()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    let card = "testsrc2=size=640x360:rate=30:duration=2,format=yuv420p,\
                    setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv";
    let made = convert(&["-f", "lavfi", "-i", card]) || convert(&["-i", sdr.to_str().unwrap()]);
    if !made {
        eprintln!("skipped: this ffmpeg cannot make the HLG test clip");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let rgb = |jpeg: &[u8]| -> Vec<u8> {
        let file = dir.join("frame.jpg");
        std::fs::write(&file, jpeg).unwrap();
        let out = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&file)
            .args(["-pix_fmt", "rgb24", "-f", "rawvideo", "pipe:1"])
            .output()
            .unwrap();
        out.stdout
    };

    let project = Project::open_in_memory().unwrap();
    let asset = project.import_asset(&media).unwrap();
    assert_eq!(asset.hdr(), Some(crate::model::Hdr::Hlg));
    project.add_clip_to_timeline(asset.id, None, 0.0, 2.0, Some(0.0)).unwrap();
    let (timeline, originals) = (project.timeline().unwrap(), project.list_assets().unwrap());
    let before = project.preview_assets().unwrap();
    assert_eq!(before[0].path, asset.path, "no proxy yet, so the original is decoded");
    assert!(before[0].hdr().is_some(), "and the graph tone-maps it");

    let from_original = rgb(&Project::composite_timeline_frame(&timeline, &originals, 1.0, 640, 2).unwrap());

    let proxy = engine::generate_proxy(&media, engine::proxy_width(asset.projection())).unwrap();
    let tags = std::process::Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=color_transfer,pix_fmt",
            "-of",
            "csv=p=0",
        ])
        .arg(&proxy)
        .output()
        .unwrap();
    let tags = String::from_utf8_lossy(&tags.stdout).trim().to_string();
    let after = project.preview_assets().unwrap();
    let (timeline, _) = project.timeline_frame_inputs().unwrap();
    let from_proxy = rgb(&Project::composite_timeline_frame(&timeline, &after, 1.0, 640, 2).unwrap());
    remove_proxy(&proxy);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(tags, "yuv420p,bt709", "the proxy is SDR");
    assert_eq!(after[0].path, proxy.to_string_lossy());
    assert!(after[0].hdr().is_none(), "so the graph leaves it alone");
    assert_eq!(from_original.len(), from_proxy.len());
    let mad = from_original
        .iter()
        .zip(&from_proxy)
        .map(|(a, b)| (*a as f64 - *b as f64).abs())
        .sum::<f64>()
        / from_original.len() as f64;
    assert!(
        mad < 8.0,
        "proxy and original previews differ by {mad:.1} levels: converted twice or not at all"
    );
}

/// Deletes a scratch directory when dropped, pass or fail.
struct RemoveDirOnDrop(PathBuf);

impl Drop for RemoveDirOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Six seconds of 30 fps video whose frames number themselves (bit k of the
/// frame number is the k-th eighth of the picture's width, white for 1), the
/// video starting `lead` seconds after the audio. `None` when this ffmpeg
/// cannot make it.
fn late_barcode_source(dir: &Path, lead: f64) -> Option<PathBuf> {
    use crate::engine::test_support::StatusBounded;
    let ffmpeg = std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
    let run = |args: &[&str]| {
        std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .status_bounded()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    let bars = "color=c=black:s=640x360:r=30:d=6,format=yuv420p,\
                    geq=lum='if(bitand(trunc(N/pow(2,trunc(X*8/W))),1),235,16)':cb=128:cr=128";
    let (video, audio, media) = (dir.join("v.mp4"), dir.join("a.mp4"), dir.join(format!("late-{lead}.mp4")));
    let delay = lead.to_string();
    let made = run(&[
        "-f",
        "lavfi",
        "-i",
        bars,
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-g",
        "30",
        "-pix_fmt",
        "yuv420p",
        video.to_str().unwrap(),
    ]) && run(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:duration=8",
        "-c:a",
        "aac",
        audio.to_str().unwrap(),
    ]) && run(&[
        "-itsoffset",
        &delay,
        "-i",
        video.to_str().unwrap(),
        "-i",
        audio.to_str().unwrap(),
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c",
        "copy",
        media.to_str().unwrap(),
    ]);
    made.then_some(media)
}

/// Which numbered frame of [`late_barcode_source`] a JPEG shows.
fn barcode_of(dir: &Path, jpeg: &[u8]) -> u32 {
    let ffmpeg = std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
    let file = dir.join("frame.jpg");
    std::fs::write(&file, jpeg).unwrap();
    let out = std::process::Command::new(&ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&file)
        .args(["-vf", "scale=640:360,format=gray", "-f", "rawvideo", "pipe:1"])
        .output()
        .unwrap();
    (0..8)
        .filter(|k| out.stdout[180 * 640 + (2 * k + 1) * 640 / 16] > 125)
        .map(|k| 1u32 << k)
        .sum()
}

/// A source whose video starts after its audio (a camera that opens its mic
/// first). Its preview decodes the proxy and its export decodes the original,
/// and a clip cut from the middle must show the same frame in both: an input
/// `-ss` is relative to the *container's* start, and an FFmpeg-9 proxy of such
/// a file used to start its container at the video, so every cut landed a few
/// frames deeper in the preview than in the render.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored late_starting_source`
#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn late_starting_source_previews_the_frame_the_export_cuts() {
    let dir = std::env::temp_dir().join(format!("kerf-late-preview-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _scratch = RemoveDirOnDrop(dir.clone());
    let Some(media) = late_barcode_source(&dir, 0.064) else {
        eprintln!("skipped: this ffmpeg cannot make the late-starting test clip");
        return;
    };

    let project = Project::open_in_memory().unwrap();
    let asset = project.import_asset(&media).unwrap();
    // Two cuts from the middle of the source, one on the frame grid and one off it.
    project.add_clip_to_timeline(asset.id, None, 0.5, 2.0, Some(0.0)).unwrap();
    project.add_clip_to_timeline(asset.id, None, 1.234, 3.0, Some(2.0)).unwrap();
    let (timeline, originals) = (project.timeline().unwrap(), project.list_assets().unwrap());
    let proxy = engine::generate_proxy(&media, engine::proxy_width(asset.projection())).unwrap();
    let _cleanup = ProxyGuard(proxy.clone());
    let (_, previews) = project.timeline_frame_inputs().unwrap();
    assert_eq!(previews[0].path, proxy.to_string_lossy(), "the preview decodes the proxy");

    let mut mismatches = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for t in [0.0, 0.1, 0.25, 0.5, 1.0, 1.4, 2.0, 2.05, 2.3, 3.0, 3.7] {
        let render = barcode_of(
            &dir,
            &Project::composite_timeline_frame(&timeline, &originals, t, 640, 2).unwrap(),
        );
        let preview = barcode_of(
            &dir,
            &Project::composite_timeline_frame(&timeline, &previews, t, 640, 2).unwrap(),
        );
        seen.insert(render);
        if render != preview {
            mismatches.push((t, render, preview));
        }
    }
    assert!(seen.len() > 5, "the clips never moved: {seen:?}");
    assert!(
        mismatches.is_empty(),
        "(time, export's frame, preview's frame) differ: {mismatches:?}"
    );
}

/// The same source, played from the very head of a clip cut at `source_in` 0 —
/// which reads the proxy from its start with no `-ss`, where every other cut
/// seeks. A padded proxy opens with a clone of the first frame that the original
/// has no frame for, and the clip came out `lead` seconds of held frame late
/// (with a 0.5 s lead: frame 15 at a second in where the export shows 30) —
/// while a cut 2 ms in, which seeks past the clone, was exact. The cliff is at
/// the most common clip there is, so both sides of it are pinned: the streamed
/// frames of the proxy, as the player shows them, against the original's.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored head_clip_of_a_late`
#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn head_clip_of_a_late_starting_source_streams_the_frames_the_export_renders() {
    let dir = std::env::temp_dir().join(format!("kerf-late-head-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _scratch = RemoveDirOnDrop(dir.clone());
    let Some(media) = late_barcode_source(&dir, 0.5) else {
        eprintln!("skipped: this ffmpeg cannot make the late-starting test clip");
        return;
    };
    let project = Project::open_in_memory().unwrap();
    let asset = project.import_asset(&media).unwrap();
    let proxy = engine::generate_proxy(&media, engine::proxy_width(asset.projection())).unwrap();
    let _cleanup = ProxyGuard(proxy.clone());

    // A frame a clip shows, 40 frames from its start — past the second mark, where the
    // held-frame slip is a whole `lead` of them.
    for source_in in [0.0, 0.0005, 0.002, 0.5] {
        let clip = project
            .add_clip_to_timeline(asset.id, None, source_in, 4.0, Some(0.0))
            .unwrap();
        let originals = project.list_assets().unwrap();
        let (timeline, previews) = project.timeline_frame_inputs().unwrap();
        assert_eq!(previews[0].path, proxy.to_string_lossy(), "the preview decodes the proxy");
        let stream = |assets: &[Asset]| -> Vec<u32> {
            let mut frames = Vec::new();
            engine::stream_preview(&timeline, assets, 0.0, 30.0, &mut |f| {
                frames.push(barcode_of(&dir, &f.jpeg));
                frames.len() < 40
            })
            .unwrap();
            frames
        };
        let (export, preview) = (stream(&originals), stream(&previews));
        assert!(
            export.len() == 40 && export[39] > export[0],
            "source_in {source_in}: {export:?}"
        );
        assert_eq!(
            preview, export,
            "source_in {source_in}: the preview's frames differ from the export's"
        );
        project.remove_clips(&[clip.id]).unwrap();
    }
}

// ---- ripple mode and multi-clip edits ---------------------------------------

/// V1 holding `a [0,5)  b [6,10)  c [12,15)` — a gap of 1s and one of 2s.
fn gapped_project() -> (Project, Uuid, [Clip; 3]) {
    let (project, asset) = project_with_video_asset();
    let a = project.add_clip_to_timeline(asset, None, 0.0, 5.0, Some(0.0)).unwrap();
    let b = project.add_clip_to_timeline(asset, None, 0.0, 4.0, Some(6.0)).unwrap();
    let c = project.add_clip_to_timeline(asset, None, 0.0, 3.0, Some(12.0)).unwrap();
    (project, asset, [a, b, c])
}

/// Where each clip on the first track starts, in lane order.
fn v1_starts(project: &Project) -> Vec<f64> {
    project.timeline().unwrap().tracks[0]
        .clips
        .iter()
        .map(|c| c.timeline_start)
        .collect()
}

#[test]
fn ripple_mode_is_off_by_default_persists_with_the_file_and_is_not_an_edit() {
    let path = std::env::temp_dir().join(format!("kerf-ripple-{}.kerf", Uuid::new_v4()));
    let project = Project::create(&path).unwrap();
    assert!(!project.ripple_mode().unwrap());
    let revisions = project.history().unwrap().len();

    project.set_ripple_mode(true).unwrap();
    assert!(project.ripple_mode().unwrap() && project.ripple_active().unwrap());
    assert_eq!(project.history().unwrap().len(), revisions, "a setting, not an edit");
    drop(project);

    let reopened = Project::open(&path).unwrap();
    assert!(reopened.ripple_mode().unwrap(), "it travels with the project");
    reopened.set_ripple_mode(false).unwrap();
    assert!(!reopened.ripple_mode().unwrap());
    drop(reopened);
    std::fs::remove_file(&path).ok();
}

#[test]
fn with_the_mode_off_a_trim_leaves_the_later_clips_where_they_were() {
    let (project, _, [a, ..]) = gapped_project();
    project.trim(a.id, None, Some(3.0), None).unwrap();
    assert_eq!(v1_starts(&project), vec![0.0, 6.0, 12.0]);
}

#[test]
fn a_ripple_trim_carries_the_track_along_as_one_revision() {
    let (project, _, [a, ..]) = gapped_project();
    project.set_ripple_mode(true).unwrap();
    let revisions = project.history().unwrap().len();

    project.trim(a.id, None, Some(3.0), None).unwrap(); // 5s -> 3s
    assert_eq!(v1_starts(&project), vec![0.0, 4.0, 10.0], "gaps of 1s and 2s kept");
    assert_eq!(project.history().unwrap().len(), revisions + 1);

    project.undo().unwrap();
    assert_eq!(v1_starts(&project), vec![0.0, 6.0, 12.0], "undo is one step too");
}

#[test]
fn a_ripple_left_trim_keeps_the_start_and_reports_the_clip_as_it_ended_up() {
    let (project, _, [_, b, _]) = gapped_project();
    project.set_ripple_mode(true).unwrap();

    // What the GUI sends for a left-edge drag of one second: the in-point
    // moves and so does the start, to hold the right edge at 10.0.
    let trimmed = project.trim(b.id, Some(1.0), None, Some(7.0)).unwrap();
    assert_eq!(trimmed.timeline_start, 6.0, "the clip stays where it started");
    assert_eq!(trimmed.duration(), 3.0);
    assert_eq!(v1_starts(&project), vec![0.0, 6.0, 11.0], "and the rest follows it in");
}

#[test]
fn ripple_mode_follows_speed_changes_deletes_and_inserts() {
    let (project, asset, [_, b, c]) = gapped_project();
    project.set_ripple_mode(true).unwrap();

    project.set_speed(b.id, 2.0).unwrap(); // 4s -> 2s
    assert_eq!(v1_starts(&project), vec![0.0, 6.0, 10.0]);

    project.remove(b.id).unwrap(); // closes b's 2s
    assert_eq!(v1_starts(&project), vec![0.0, 8.0]);

    // Appending moves nothing; dropping onto `c` pushes it by the new length.
    project.cut_clip(asset, 0.0, 2.0).unwrap();
    assert_eq!(v1_starts(&project)[1], 8.0);
    project.add_clip_to_timeline(asset, None, 0.0, 2.0, Some(8.0)).unwrap();
    let tl = project.timeline().unwrap();
    assert_eq!(tl.clip(c.id).unwrap().timeline_start, 10.0);
}

#[test]
fn ripple_mode_leaves_ops_that_close_the_gap_themselves_alone() {
    // The same cut, edited with the mode off and on, must come out the same.
    let run = |ripple: bool| {
        let (project, _, [a, b, _]) = gapped_project();
        project.set_ripple_mode(ripple).unwrap();
        project.ripple_delete(b.id).unwrap();
        let after_delete = v1_starts(&project);
        project.cut_clip_range(a.id, 1.0, 2.0).unwrap();
        (after_delete, v1_starts(&project))
    };
    let (plain_delete, plain_cut) = run(false);
    assert_eq!(plain_delete, vec![0.0, 8.0]);
    assert_eq!(run(true), (plain_delete, plain_cut));
}

#[test]
fn a_move_never_ripples() {
    let (project, _, [a, ..]) = gapped_project();
    project.set_ripple_mode(true).unwrap();
    project.move_clip(a.id, 0.5, None).unwrap();
    assert_eq!(v1_starts(&project), vec![0.5, 6.0, 12.0]);
}

#[test]
fn the_beat_snap_comes_out_the_same_in_ripple_mode() {
    let run = |ripple: bool| {
        let project = Project::open_in_memory().unwrap();
        project.set_ripple_mode(ripple).unwrap();
        let video = project
            .insert_or_get_asset(&asset_with("/beat-video.mp4", vec![vid_stream(false)]))
            .unwrap();
        let music = project
            .insert_or_get_asset(&asset_with("/beat-music.wav", vec![aud_stream()]))
            .unwrap();
        project
            .set_analysis(&AssetAnalysis {
                asset_id: music.id,
                tempo: Some(crate::model::Tempo {
                    downbeats: Vec::new(),
                    bpm: 120.0,
                    beats: (0..=20).map(|i| i as f64 * 0.5).collect(),
                    confidence: 0.8,
                }),
                ..Default::default()
            })
            .unwrap();
        project.add_asset_audio(music.id).unwrap();
        project.cut_clip(video.id, 0.0, 1.1).unwrap();
        project.cut_clip(video.id, 2.0, 2.9).unwrap();
        project.cut_clip(video.id, 4.0, 5.0).unwrap();
        project.snap_to_beats(None, None).unwrap();
        project.timeline().unwrap().tracks[0]
            .clips
            .iter()
            .map(|c| (c.timeline_start, c.timeline_end()))
            .collect::<Vec<_>>()
    };
    let plain = run(false);
    assert_eq!(plain, vec![(0.0, 1.0), (1.0, 2.0), (2.0, 3.0)]);
    assert_eq!(run(true), plain);
}

#[test]
fn with_ripple_overrides_the_flag_for_the_call_and_nothing_else() {
    let (project, _, [a, b, _]) = gapped_project();
    assert!(!project.ripple_active().unwrap());

    // Forced on over a project that is off.
    project.with_ripple(Some(true), |p| {
        assert!(p.ripple_active().unwrap());
        p.trim(a.id, None, Some(4.0), None).unwrap();
    });
    assert_eq!(v1_starts(&project), vec![0.0, 5.0, 11.0]);
    assert!(!project.ripple_mode().unwrap(), "the project's flag was never touched");
    assert!(!project.ripple_active().unwrap(), "and the override is gone");

    // Forced off over a project that is on; `None` inherits; nesting restores.
    project.set_ripple_mode(true).unwrap();
    project.with_ripple(Some(false), |p| {
        p.trim(a.id, None, Some(3.0), None).unwrap();
        assert!(!p.ripple_active().unwrap());
        p.with_ripple(None, |inner| assert!(!inner.ripple_active().unwrap(), "inherits the outer"));
        p.with_ripple(Some(true), |inner| assert!(inner.ripple_active().unwrap()));
        assert!(!p.ripple_active().unwrap(), "inner override undone");
    });
    assert_eq!(v1_starts(&project), vec![0.0, 5.0, 11.0], "a 1s trim that did not ripple");
    project.with_ripple(None, |p| p.trim(b.id, None, Some(3.0), None).unwrap());
    assert_eq!(v1_starts(&project), vec![0.0, 5.0, 10.0], "None follows the project's flag");
    assert!(project.ripple_active().unwrap());
}

#[test]
fn a_staged_agent_edit_ripples_in_the_proposal_and_not_the_live_cut() {
    let (mut project, _, [a, ..]) = gapped_project();
    project.set_ripple_mode(true).unwrap();
    project.set_actor(EditSource::Agent);
    project.set_agent_review(false);
    let revisions = project.history().unwrap().len();

    project.begin_staging(None, None, None).unwrap();
    project.trim(a.id, None, Some(3.0), None).unwrap();

    let live: Vec<f64> = v1_starts(&project);
    assert_eq!(live, vec![0.0, 6.0, 12.0], "the cut the user is looking at has not moved");
    let staged = project.working_timeline().unwrap();
    let proposed: Vec<f64> = staged.tracks[0].clips.iter().map(|c| c.timeline_start).collect();
    assert_eq!(proposed, vec![0.0, 4.0, 10.0]);

    let diff = project.staged().unwrap().unwrap().diff;
    let kinds: Vec<DiffKind> = diff.entries.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds.iter().filter(|k| **k == DiffKind::ClipMoved).count(),
        2,
        "the review card shows the clips that followed: {kinds:?}"
    );
    assert!(kinds.contains(&DiffKind::ClipRetrimmed));

    project.apply_staged(false).unwrap();
    assert_eq!(v1_starts(&project), vec![0.0, 4.0, 10.0]);
    assert_eq!(project.history().unwrap().len(), revisions + 1);
}

#[test]
fn ripple_mode_never_moves_a_locked_track() {
    let (project, _, [a, ..]) = gapped_project();
    let v1 = project.timeline().unwrap().tracks[0].id;
    project.set_track_locked(v1, true).unwrap();
    project.set_ripple_mode(true).unwrap();
    // A trim on it is refused outright, so nothing on it moves.
    assert!(matches!(project.trim(a.id, None, Some(3.0), None), Err(Error::InvalidArgument(why)) if why.contains("locked")));
    assert_eq!(v1_starts(&project), vec![0.0, 6.0, 12.0]);
}

#[test]
fn move_clips_is_one_revision_and_keeps_the_group_together() {
    let (project, _, [a, b, c]) = gapped_project();
    let revisions = project.history().unwrap().len();

    let moved = project
        .move_clips(&[
            ClipMove {
                clip_id: b.id,
                timeline_start: 7.0,
                track_id: None,
            },
            ClipMove {
                clip_id: c.id,
                timeline_start: 13.0,
                track_id: None,
            },
        ])
        .unwrap();
    assert_eq!(moved.iter().map(|c| c.timeline_start).collect::<Vec<_>>(), vec![7.0, 13.0]);
    assert_eq!(v1_starts(&project), vec![0.0, 7.0, 13.0]);
    assert!(project.timeline().unwrap().clip(a.id).is_some());

    let history = project.history().unwrap();
    assert_eq!(history.len(), revisions + 1);
    assert_eq!(history.last().unwrap().label, "Move 2 clips");
}

#[test]
fn a_refused_group_move_leaves_the_timeline_and_history_alone() {
    let (project, _, [a, b, _]) = gapped_project();
    let revisions = project.history().unwrap().len();
    let before = serde_json::to_string(&project.timeline().unwrap()).unwrap();

    // `b` would be fine; `a` lands on `b`'s spot.
    let result = project.move_clips(&[
        ClipMove {
            clip_id: b.id,
            timeline_start: 20.0,
            track_id: None,
        },
        ClipMove {
            clip_id: a.id,
            timeline_start: 12.5,
            track_id: None,
        },
    ]);
    assert!(matches!(result, Err(Error::InvalidArgument(_))), "{result:?}");
    assert_eq!(project.history().unwrap().len(), revisions);
    assert_eq!(serde_json::to_string(&project.timeline().unwrap()).unwrap(), before);
}

#[test]
fn an_agents_group_move_is_staged_like_any_other_edit() {
    let (mut project, _, [a, ..]) = gapped_project();
    project.set_actor(EditSource::Agent);
    project.set_agent_review(false);
    project.begin_staging(None, None, None).unwrap();
    project
        .move_clips(&[ClipMove {
            clip_id: a.id,
            timeline_start: 0.5,
            track_id: None,
        }])
        .unwrap();
    assert_eq!(v1_starts(&project)[0], 0.0, "the live cut is untouched");
    assert_eq!(project.working_timeline().unwrap().clip(a.id).unwrap().timeline_start, 0.5);
    assert_eq!(project.staged().unwrap().unwrap().edits, vec!["Move clip".to_string()]);
}

#[test]
fn remove_clips_is_one_revision_and_all_or_nothing() {
    let (project, _, [a, b, c]) = gapped_project();
    let revisions = project.history().unwrap().len();

    // One bad id and nothing is removed.
    assert!(matches!(
        project.remove_clips(&[a.id, Uuid::new_v4()]),
        Err(Error::ClipNotFound(_))
    ));
    assert_eq!(project.history().unwrap().len(), revisions);
    assert_eq!(v1_starts(&project).len(), 3);

    assert_eq!(project.remove_clips(&[a.id, c.id, a.id]).unwrap(), 2);
    let tl = project.timeline().unwrap();
    assert_eq!(tl.tracks[0].clips.len(), 1);
    assert_eq!(tl.tracks[0].clips[0].id, b.id);
    assert_eq!(tl.tracks[0].clips[0].timeline_start, 6.0, "a plain delete leaves the gap");
    let history = project.history().unwrap();
    assert_eq!(history.len(), revisions + 1);
    assert_eq!(history.last().unwrap().label, "Remove 2 clips");
}

#[test]
fn remove_clips_under_ripple_closes_every_track_behind_it() {
    let (project, asset, [a, b, c]) = gapped_project();
    let v2 = project.add_track(StreamKind::Video, None).unwrap();
    let on_v2 = project.add_clip_to_timeline(asset, Some(v2.id), 0.0, 2.0, Some(1.0)).unwrap();
    let later_on_v2 = project.add_clip_to_timeline(asset, Some(v2.id), 0.0, 2.0, Some(5.0)).unwrap();

    // Forced on for the call: a multi-select *ripple* delete.
    let n = project
        .with_ripple(Some(true), |p| p.remove_clips(&[a.id, on_v2.id]))
        .unwrap();
    assert_eq!(n, 2);
    let tl = project.timeline().unwrap();
    assert_eq!(tl.clip(b.id).unwrap().timeline_start, 1.0, "b: 6 - 5");
    assert_eq!(tl.clip(c.id).unwrap().timeline_start, 7.0);
    assert_eq!(
        tl.clip(later_on_v2.id).unwrap().timeline_start,
        3.0,
        "V2 closed by its own 2s"
    );
    assert_eq!(project.history().unwrap().last().unwrap().label, "Ripple delete 2 clips");
    assert!(!project.ripple_mode().unwrap());
}

#[test]
fn a_ripple_remove_of_one_clip_is_a_ripple_delete() {
    let remove = |ripple_delete: bool| {
        let (project, _, [_, b, _]) = gapped_project();
        if ripple_delete {
            project.ripple_delete(b.id).unwrap();
        } else {
            project.with_ripple(Some(true), |p| p.remove(b.id)).unwrap();
        }
        serde_json::to_value(&project.timeline().unwrap().tracks[0].clips)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["timeline_start"].as_f64().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(remove(true), remove(false));
}

#[test]
fn group_edits_refuse_a_locked_track() {
    let (project, _, [a, b, _]) = gapped_project();
    let v1 = project.timeline().unwrap().tracks[0].id;
    project.set_track_locked(v1, true).unwrap();
    let revisions = project.history().unwrap().len();

    let moved = project.move_clips(&[ClipMove {
        clip_id: a.id,
        timeline_start: 0.5,
        track_id: None,
    }]);
    assert!(matches!(moved, Err(Error::InvalidArgument(why)) if why.contains("locked")));
    assert!(matches!(
        project.remove_clips(&[b.id]),
        Err(Error::InvalidArgument(why)) if why.contains("locked")
    ));
    assert_eq!(project.history().unwrap().len(), revisions);
}

/// Every single-clip op that changes what is on a track, where, how long or whether it plays,
/// as `(name, op)`, each given the middle clip `b`, the last `c` and V1's id.
#[allow(clippy::type_complexity)]
fn layout_ops() -> Vec<(&'static str, Box<dyn Fn(&Project, &Clip, &Clip, Uuid) -> Result<()>>)> {
    vec![
        (
            "split",
            Box::new(|p, b, _, _| p.split_at(b.id, b.timeline_start + 1.0).map(drop)),
        ),
        (
            "trim",
            Box::new(|p, b, _, _| p.trim(b.id, None, Some(b.source_out - 0.5), None).map(drop)),
        ),
        (
            "trim and move",
            Box::new(|p, _, c, _| p.trim(c.id, None, None, Some(c.timeline_start + 0.25)).map(drop)),
        ),
        (
            "cut range",
            Box::new(|p, b, _, _| p.cut_clip_range(b.id, b.source_in + 1.0, b.source_in + 2.0).map(drop)),
        ),
        (
            "move",
            Box::new(|p, _, c, _| p.move_clip(c.id, c.timeline_start + 0.25, None).map(drop)),
        ),
        ("reorder", Box::new(|p, b, _, v1| p.reorder(v1, b.id, 2))),
        (
            "split and remove",
            Box::new(|p, b, _, _| p.split_remove(b.id, b.timeline_start + 1.0, SplitSide::Left).map(drop)),
        ),
        ("slip", Box::new(|p, b, _, _| p.slip_clip(b.id, 0.5).map(drop))),
        ("ripple delete", Box::new(|p, b, _, _| p.ripple_delete(b.id))),
        ("remove", Box::new(|p, b, _, _| p.remove(b.id))),
        ("speed", Box::new(|p, b, _, _| p.set_speed(b.id, 2.0).map(drop))),
        ("disable", Box::new(|p, b, _, _| p.set_clip_enabled(b.id, false).map(drop))),
        ("duplicate", Box::new(|p, b, _, _| p.duplicate_clips(&[b.id], 30.0).map(drop))),
    ]
}

#[test]
fn single_clip_layout_edits_refuse_a_locked_track_as_the_group_edits_do() {
    for (name, op) in layout_ops() {
        let (project, _, [_, b, c]) = abutting_project();
        let v1 = project.timeline().unwrap().tracks[0].id;
        project.set_track_locked(v1, true).unwrap();
        let before = serde_json::to_value(project.timeline().unwrap()).unwrap();
        let revisions = project.history().unwrap().len();
        let refused = op(&project, &b, &c, v1);
        assert!(
            matches!(&refused, Err(Error::InvalidArgument(why)) if why == "track V1 is locked"),
            "{name} is refused on the locked track"
        );
        assert_eq!(
            serde_json::to_value(project.timeline().unwrap()).unwrap(),
            before,
            "{name} changed the cut"
        );
        assert_eq!(project.history().unwrap().len(), revisions, "{name} wrote a revision");

        // Unlocked, the same op goes through: the refusal was the lock.
        project.set_track_locked(v1, false).unwrap();
        op(&project, &b, &c, v1).unwrap_or_else(|e| panic!("{name} unlocked: {e}"));
    }
}

#[test]
fn a_locked_clip_keeps_its_look_and_sound_editable() {
    let (project, _, [_, b, _]) = abutting_project();
    let v1 = project.timeline().unwrap().tracks[0].id;
    project.set_track_locked(v1, true).unwrap();
    project.set_volume(b.id, 0.5).unwrap();
    project.set_fade(b.id, Some(0.5), None).unwrap();
    project.set_mask(b.id, None).unwrap();
    project.set_audio_effects(b.id, Vec::new()).unwrap();
    let after = project.timeline().unwrap().clip(b.id).unwrap().clone();
    assert_eq!(
        (after.volume, after.fade_in, after.timeline_start),
        (0.5, 0.5, b.timeline_start)
    );
}

#[test]
fn nothing_is_put_on_a_locked_track() {
    let (project, asset, [a, _, c]) = abutting_project();
    let v1 = project.timeline().unwrap().tracks[0].id;
    let v2 = project.add_track(StreamKind::Video, None).unwrap().id;
    project.set_track_locked(v2, true).unwrap();
    // Moved onto it, or added to it by name.
    let moved = project.move_clip(c.id, 40.0, Some(v2));
    assert!(
        matches!(&moved, Err(Error::InvalidArgument(why)) if why == "track V2 is locked"),
        "a move onto a locked track is refused"
    );
    let added = project.add_clip_to_timeline(asset, Some(v2), 0.0, 1.0, Some(50.0));
    assert!(
        matches!(&added, Err(Error::InvalidArgument(why)) if why == "track V2 is locked"),
        "an add to a named locked track is refused"
    );
    let pasted = project.insert_clips(&[(v2, a)], 50.0);
    assert!(
        matches!(&pasted, Err(Error::InvalidArgument(why)) if why == "track V2 is locked"),
        "a paste onto a locked track is refused"
    );

    // Named no track, a clip goes to the first unlocked one of its kind — and with every one of
    // them locked, nowhere.
    project.set_track_locked(v2, false).unwrap();
    project.set_track_locked(v1, true).unwrap();
    let placed = project.add_clip_to_timeline(asset, None, 0.0, 1.0, Some(50.0)).unwrap();
    assert!(project
        .timeline()
        .unwrap()
        .track(v2)
        .unwrap()
        .clips
        .iter()
        .any(|x| x.id == placed.id));
    project.set_track_locked(v2, true).unwrap();
    let nowhere = project.add_clip_to_timeline(asset, None, 0.0, 1.0, Some(60.0));
    assert!(
        matches!(&nowhere, Err(Error::InvalidArgument(why)) if why.contains("every video track is locked")),
        "with every video track locked nothing is added"
    );
    assert!(matches!(project.concatenate(&[asset]), Err(Error::InvalidArgument(_))));
}

#[test]
fn an_agent_proposal_honours_a_lock_the_user_set_while_it_was_open() {
    let (mut project, _, [a, b, c]) = abutting_project();
    let v1 = project.timeline().unwrap().tracks[0].id;
    project.set_actor(EditSource::Agent);
    project.set_volume(a.id, 0.5).unwrap();
    assert!(project.staged().unwrap().is_some());

    project.set_actor(EditSource::User);
    project.set_track_locked(v1, true).unwrap();

    project.set_actor(EditSource::Agent);
    for (name, op) in layout_ops() {
        let refused = op(&project, &b, &c, v1);
        assert!(
            matches!(&refused, Err(Error::InvalidArgument(why)) if why.contains("locked")),
            "{name} is refused on the locked track"
        );
    }
    // A look is still the agent's to propose, and the user's lock is not recorded as part of
    // the proposal.
    project.set_volume(b.id, 0.25).unwrap();
    assert!(!project.staged_timeline().unwrap().unwrap().track(v1).unwrap().locked);
}

#[test]
fn the_agent_sees_the_lock_it_is_refused_by_and_a_refusal_leaves_its_proposal_alone() {
    let (mut project, _, [a, b, _]) = abutting_project();
    let v1 = project.timeline().unwrap().tracks[0].id;
    project.set_actor(EditSource::Agent);
    project.set_volume(a.id, 0.5).unwrap();
    project.set_actor(EditSource::User);
    project.set_track_locked(v1, true).unwrap();

    project.set_actor(EditSource::Agent);
    // What get_timeline_state and every other agent read see: the lock, as the user set it.
    assert!(project.working_timeline().unwrap().track(v1).unwrap().locked);
    // The stored proposal is not rewritten to say so.
    assert!(!project.staged_timeline().unwrap().unwrap().track(v1).unwrap().locked);

    let before = serde_json::to_value(project.staged_timeline().unwrap().unwrap()).unwrap();
    let depth = project.staged().unwrap().unwrap().undo_depth;
    assert!(project.split_at(b.id, b.timeline_start + 1.0).is_err());
    assert_eq!(
        serde_json::to_value(project.staged_timeline().unwrap().unwrap()).unwrap(),
        before
    );
    assert_eq!(project.staged().unwrap().unwrap().undo_depth, depth);
}

#[test]
fn a_lock_the_proposal_inherited_follows_the_user_and_one_it_added_stays() {
    let (mut project, _, [a, b, _]) = abutting_project();
    let v1 = project.timeline().unwrap().tracks[0].id;
    project.set_track_locked(v1, true).unwrap();
    // The proposal branches off a cut with V1 locked…
    project.set_actor(EditSource::Agent);
    project.set_volume(a.id, 0.5).unwrap();
    let v2 = project.add_track(StreamKind::Video, None).unwrap().id;
    project.set_track_locked(v2, true).unwrap();
    // …and the user lifts that lock while it is open.
    project.set_actor(EditSource::User);
    project.set_track_locked(v1, false).unwrap();

    project.set_actor(EditSource::Agent);
    let seen = project.working_timeline().unwrap();
    assert!(!seen.track(v1).unwrap().locked, "the inherited lock follows the user");
    assert!(seen.track(v2).unwrap().locked, "the lock the agent made itself stays");
    project.split_at(b.id, b.timeline_start + 1.0).unwrap();
    assert!(
        project.set_track_locked(v2, false).is_err(),
        "and an agent still cannot lift it"
    );

    // Forced in over the user's newer work, the user's unlock is not undone.
    project.set_actor(EditSource::User);
    let landed = project.apply_staged(true).unwrap();
    assert!(!landed.track(v1).unwrap().locked);
    assert!(landed.track(v2).unwrap().locked);
    let live = project.timeline().unwrap();
    assert!(!live.track(v1).unwrap().locked && live.track(v2).unwrap().locked);
    assert_eq!(live.tracks[0].clips.len(), 4, "the split landed");
}

#[test]
fn an_agent_may_lock_a_track_but_only_the_user_unlocks_one() {
    for review in [true, false] {
        let (mut project, _, _) = abutting_project();
        project.set_agent_review(review);
        let v1 = project.timeline().unwrap().tracks[0].id;
        project.set_actor(EditSource::User);
        project.set_track_locked(v1, true).unwrap();

        project.set_actor(EditSource::Agent);
        let refused = project.set_track_locked(v1, false);
        assert!(
            matches!(&refused, Err(Error::InvalidArgument(why)) if why == "track V1 is locked, and an agent cannot unlock a track — ask the user to"),
            "review {review}: the agent may not unlock"
        );
        assert!(project.timeline().unwrap().track(v1).unwrap().locked, "review {review}");

        // Locking is the agent's to do; so is "unlocking" a track that is not locked.
        let v2 = project.add_track(StreamKind::Video, None).unwrap().id;
        project.set_track_locked(v2, true).unwrap();
        project.set_actor(EditSource::User);
        project.set_track_locked(v1, false).unwrap();
    }
}

#[test]
fn a_paste_does_not_put_a_clip_on_a_track_it_cannot_play_on() {
    let project = Project::open_in_memory().unwrap();
    let music = project
        .insert_or_get_asset(&asset_with("/kind/music.wav", vec![aud_stream()]))
        .unwrap();
    let silent = project
        .insert_or_get_asset(&asset_with("/kind/silent.mp4", vec![vid_stream(false)]))
        .unwrap();
    let both = project
        .insert_or_get_asset(&asset_with("/kind/both.mp4", vec![vid_stream(false), aud_stream()]))
        .unwrap();
    let sound = project.add_asset_audio(music.id).unwrap();
    let picture = project.cut_clip(silent.id, 0.0, 1.0).unwrap();
    let footage = project.cut_clip(both.id, 0.0, 1.0).unwrap();
    let timeline = project.timeline().unwrap();
    let v1 = timeline.first_track_of(StreamKind::Video).unwrap();
    let a1 = timeline.first_track_of(StreamKind::Audio).unwrap();
    let revisions = project.history().unwrap().len();

    let on_video = project.insert_clips(&[(v1, sound)], 50.0);
    assert!(
        matches!(&on_video, Err(Error::InvalidArgument(why)) if why.starts_with("a clip with no picture cannot go on video track V1")),
        "sound alone is refused on a video track"
    );
    let on_audio = project.insert_clips(&[(a1, picture)], 50.0);
    assert!(
        matches!(&on_audio, Err(Error::InvalidArgument(why)) if why.starts_with("a clip with no sound cannot go on audio track A1")),
        "a picture with no sound is refused on an audio track"
    );
    assert_eq!(project.history().unwrap().len(), revisions, "nothing was written");
    // Footage with both goes on either: a detached sound is a clip of the picture's own asset.
    project.insert_clips(&[(a1, footage.clone())], 50.0).unwrap();
    project.insert_clips(&[(v1, footage)], 50.0).unwrap();
}

#[test]
fn an_agent_relocking_a_track_the_user_unlocked_is_told_what_holds() {
    let (mut project, _, [a, ..]) = abutting_project();
    let v1 = project.timeline().unwrap().tracks[0].id;
    project.set_track_locked(v1, true).unwrap();
    project.set_actor(EditSource::Agent);
    project.set_volume(a.id, 0.5).unwrap();
    project.set_actor(EditSource::User);
    project.set_track_locked(v1, false).unwrap();

    project.set_actor(EditSource::Agent);
    // The lock it inherited follows the user, so its own "lock" does not hold — and the answer says so.
    let answered = project.set_track_locked(v1, true).unwrap();
    assert!(!answered.locked);
    assert!(!project.working_timeline().unwrap().track(v1).unwrap().locked);
    // A track the base did not lock does take its lock.
    let v2 = project.add_track(StreamKind::Video, None).unwrap().id;
    assert!(project.set_track_locked(v2, true).unwrap().locked);
}

#[test]
fn a_clip_is_added_only_to_a_track_it_can_play_on() {
    let project = Project::open_in_memory().unwrap();
    let music = project
        .insert_or_get_asset(&asset_with("/kind/music.wav", vec![aud_stream()]))
        .unwrap();
    let silent = project
        .insert_or_get_asset(&asset_with("/kind/silent.mp4", vec![vid_stream(false)]))
        .unwrap();
    let both = project
        .insert_or_get_asset(&asset_with("/kind/both.mp4", vec![vid_stream(false), aud_stream()]))
        .unwrap();
    let v1 = project.timeline().unwrap().first_track_of(StreamKind::Video).unwrap();
    let a1 = project.add_track(StreamKind::Audio, None).unwrap().id;
    let on_video = project.add_clip_to_timeline(music.id, Some(v1), 0.0, 1.0, Some(0.0));
    assert!(
        matches!(&on_video, Err(Error::InvalidArgument(why)) if why == "a clip with no picture cannot go on video track V1 — add it to a track of its kind"),
        "sound alone is refused on a video track"
    );
    let on_audio = project.add_clip_to_timeline(silent.id, Some(a1), 0.0, 1.0, Some(0.0));
    assert!(
        matches!(&on_audio, Err(Error::InvalidArgument(why)) if why.starts_with("a clip with no sound cannot go on audio track")),
        "a picture with no sound is refused on an audio track"
    );
    project.add_clip_to_timeline(both.id, Some(a1), 0.0, 1.0, Some(0.0)).unwrap();
    project.add_clip_to_timeline(both.id, Some(v1), 0.0, 1.0, Some(0.0)).unwrap();
}

#[test]
fn a_duplicate_stays_on_its_own_track_whatever_its_kind() {
    let project = Project::open_in_memory().unwrap();
    let music = project
        .insert_or_get_asset(&asset_with("/kind/music.wav", vec![aud_stream()]))
        .unwrap();
    // A sound on a video track, as an older project may hold one.
    let mut stray = Clip::for_asset(&music, 0.0, 1.0, 0.0);
    stray.id = Uuid::new_v4();
    let id = stray.id;
    project
        .edit_timeline("plant", |t| {
            t.tracks[0].clips.push(stray);
            Ok(())
        })
        .unwrap();
    let copies = project.duplicate_clips(&[id], 5.0).unwrap();
    let v1 = &project.timeline().unwrap().tracks[0];
    assert!(v1.clips.iter().any(|c| c.id == copies[0].id));
}

#[test]
fn an_agent_may_not_remove_a_locked_track_and_the_user_may() {
    let (mut project, _, _) = abutting_project();
    project.set_agent_review(false);
    let v2 = project.add_track(StreamKind::Video, None).unwrap().id;
    project.set_track_locked(v2, true).unwrap();
    project.set_actor(EditSource::Agent);
    let refused = project.remove_track(v2);
    assert!(
        matches!(&refused, Err(Error::InvalidArgument(why)) if why == "track V2 is locked — only the user can remove it"),
        "an agent may not remove a locked track"
    );
    project.set_actor(EditSource::User);
    project.remove_track(v2).unwrap();
    assert!(project.timeline().unwrap().track(v2).is_none());
}

#[test]
fn a_voiceover_does_not_land_on_a_locked_lane_and_leaves_no_asset_behind() {
    let project = Project::open_in_memory().unwrap();
    let a2 = project.add_track(StreamKind::Audio, None).unwrap().id;
    project.set_track_locked(a2, true).unwrap();
    let named = project.place_voiceover(&voiceover_asset("/vo/a.wav"), Some(a2), None);
    assert!(
        matches!(&named, Err(Error::InvalidArgument(why)) if why.contains("is locked")),
        "a named locked track is refused"
    );
    assert!(
        project.list_assets().unwrap().is_empty(),
        "a refused voiceover stores nothing"
    );

    // The voiceover lane, once the user has locked it.
    project.place_voiceover(&voiceover_asset("/vo/b.wav"), None, None).unwrap();
    let vo = project
        .timeline()
        .unwrap()
        .tracks
        .iter()
        .find(|t| t.name == VOICEOVER_TRACK)
        .unwrap()
        .id;
    project.set_track_locked(vo, true).unwrap();
    let lane = project.place_voiceover(&voiceover_asset("/vo/c.wav"), None, None);
    assert!(
        matches!(&lane, Err(Error::InvalidArgument(why)) if why == "track VO is locked"),
        "the locked voiceover lane is refused"
    );
    assert_eq!(project.list_assets().unwrap().len(), 1);
}

#[test]
fn the_beat_snap_refuses_a_named_locked_track() {
    let project = Project::open_in_memory().unwrap();
    let video = project
        .insert_or_get_asset(&asset_with("/beat-video.mp4", vec![vid_stream(false)]))
        .unwrap();
    let music = project
        .insert_or_get_asset(&asset_with("/beat-music.wav", vec![aud_stream()]))
        .unwrap();
    project
        .set_analysis(&AssetAnalysis {
            asset_id: music.id,
            tempo: Some(crate::model::Tempo {
                downbeats: Vec::new(),
                bpm: 120.0,
                beats: (0..=20).map(|i| i as f64 * 0.5).collect(),
                confidence: 0.8,
            }),
            ..Default::default()
        })
        .unwrap();
    project.add_asset_audio(music.id).unwrap();
    project.cut_clip(video.id, 0.0, 1.1).unwrap();
    project.cut_clip(video.id, 2.0, 3.4).unwrap();
    let v1 = project.timeline().unwrap().first_track_of(StreamKind::Video).unwrap();
    project.set_track_locked(v1, true).unwrap();
    let named = project.snap_to_beats(Some(v1), None);
    assert!(
        matches!(&named, Err(Error::InvalidArgument(why)) if why == "track V1 is locked"),
        "a named locked track is refused"
    );
    // Named no track, it aligns the unlocked ones: none here.
    assert_eq!(project.snap_to_beats(None, None).unwrap(), 0);
}

// ---- edit modes: roll, slip, slide, split-and-remove -------------------------

/// V1 holding `a [0,4)` src 10..14, `b [4,8)` src 20..24 and `c [8,12)` src
/// 30..34, all abutting, of a 60 s asset.
fn abutting_project() -> (Project, Uuid, [Clip; 3]) {
    let (project, asset) = project_with_video_asset();
    let a = project.add_clip_to_timeline(asset, None, 10.0, 14.0, Some(0.0)).unwrap();
    let b = project.add_clip_to_timeline(asset, None, 20.0, 24.0, Some(4.0)).unwrap();
    let c = project.add_clip_to_timeline(asset, None, 30.0, 34.0, Some(8.0)).unwrap();
    (project, asset, [a, b, c])
}

/// Each clip of V1 as `(start, source_in, source_out)`, in lane order.
fn v1_layout(project: &Project) -> Vec<(f64, f64, f64)> {
    project.timeline().unwrap().tracks[0]
        .clips
        .iter()
        .map(|c| (c.timeline_start, c.source_in, c.source_out))
        .collect()
}

#[test]
fn roll_slip_and_slide_are_one_labelled_revision_each() {
    let (project, _, [a, b, c]) = abutting_project();
    let revisions = project.history().unwrap().len();

    let rolled = project.roll_edit(a.id, b.id, 1.0).unwrap();
    assert_eq!((rolled.requested, rolled.applied, rolled.clamped), (1.0, 1.0, false));
    assert_eq!(rolled.clips.iter().map(|x| x.id).collect::<Vec<_>>(), vec![a.id, b.id]);
    assert_eq!(
        v1_layout(&project),
        vec![(0.0, 10.0, 15.0), (5.0, 21.0, 24.0), (8.0, 30.0, 34.0)]
    );

    let slipped = project.slip_clip(c.id, -2.0).unwrap();
    assert_eq!(slipped.applied, -2.0);
    assert_eq!(v1_layout(&project)[2], (8.0, 28.0, 32.0));

    // `b` is now [5,8) and touches both neighbours.
    let slid = project.slide_clip(b.id, 1.0).unwrap();
    assert_eq!(slid.clips.iter().map(|x| x.id).collect::<Vec<_>>(), vec![a.id, b.id, c.id]);
    assert_eq!(
        v1_layout(&project),
        vec![(0.0, 10.0, 16.0), (6.0, 21.0, 24.0), (9.0, 29.0, 32.0)]
    );

    let history = project.history().unwrap();
    assert_eq!(history.len(), revisions + 3, "one revision per op");
    let labels: Vec<_> = history[revisions..].iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["Roll edit", "Slip clip", "Slide clip"]);
}

#[test]
fn the_three_exact_edit_modes_never_ripple_whatever_the_mode() {
    let run = |ripple: bool| {
        let (project, _, [a, b, c]) = abutting_project();
        project.set_ripple_mode(ripple).unwrap();
        project.roll_edit(a.id, b.id, -1.5).unwrap();
        project.slip_clip(a.id, 3.0).unwrap();
        // The last clip slid later carries the end of the track with it — the
        // one place a ripple of the following clips could have been mistaken.
        project.slide_clip(c.id, 2.0).unwrap();
        project.slide_clip(b.id, -0.5).unwrap();
        v1_layout(&project)
    };
    assert_eq!(run(true), run(false));
}

#[test]
fn a_refused_edit_mode_records_no_revision() {
    let (project, _, [a, b, c]) = abutting_project();
    let revisions = project.history().unwrap().len();
    let before = serde_json::to_string(&project.timeline().unwrap()).unwrap();

    assert!(
        matches!(project.roll_edit(a.id, c.id, 1.0), Err(Error::InvalidArgument(_))),
        "not adjacent"
    );
    assert!(matches!(
        project.roll_edit(a.id, Uuid::new_v4(), 1.0),
        Err(Error::ClipNotFound(_))
    ));
    assert!(matches!(project.slip_clip(a.id, 0.0), Err(Error::InvalidArgument(_))));
    // `b` starts 20 s into a 60 s asset; a slip later that clamps to nothing is no edit.
    project.slip_clip(b.id, 100.0).unwrap();
    let revisions = revisions + 1;
    assert!(matches!(project.slip_clip(b.id, 1.0), Err(Error::InvalidArgument(why)) if why.contains("no footage left")));
    assert!(matches!(
        project.split_remove(a.id, 50.0, SplitSide::Left),
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(project.history().unwrap().len(), revisions);
    assert_ne!(
        serde_json::to_string(&project.timeline().unwrap()).unwrap(),
        before,
        "only the one slip landed"
    );
}

#[test]
fn a_locked_track_refuses_every_edit_mode() {
    let (project, _, [a, b, _]) = abutting_project();
    let v1 = project.timeline().unwrap().tracks[0].id;
    project.set_track_locked(v1, true).unwrap();
    let revisions = project.history().unwrap().len();
    let why = |r: Result<()>| match r {
        Err(Error::InvalidArgument(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert!(why(project.roll_edit(a.id, b.id, 1.0).map(|_| ())).contains("locked"));
    assert!(why(project.slip_clip(a.id, 1.0).map(|_| ())).contains("locked"));
    assert!(why(project.slide_clip(b.id, 1.0).map(|_| ())).contains("locked"));
    assert!(why(project.split_remove(a.id, 2.0, SplitSide::Left).map(|_| ())).contains("locked"));
    assert_eq!(project.history().unwrap().len(), revisions);
}

#[test]
fn edit_modes_clamp_a_still_to_nothing_but_the_neighbours() {
    let (project, asset) = project_with_video_asset();
    let mut image = project.require_asset(asset).unwrap();
    image.id = Uuid::new_v4();
    image.path = "/x.png".into();
    image.name = "x.png".into();
    image.duration = 5.0;
    image.streams[0].image = true;
    project.insert_asset(&image).unwrap();
    let limits = project.source_limits().unwrap();
    assert_eq!(limits[&asset], 60.0);
    assert!(limits[&image.id].is_infinite());

    let video = project.add_clip_to_timeline(asset, None, 10.0, 15.0, Some(0.0)).unwrap();
    let still = project.add_clip_to_timeline(image.id, None, 0.0, 5.0, Some(5.0)).unwrap();
    assert!(matches!(project.slip_clip(still.id, 1.0), Err(Error::InvalidArgument(why)) if why.contains("still")));
    // Rolled earlier the still grows, its window staying at 0.
    let out = project.roll_edit(video.id, still.id, -2.0).unwrap();
    assert_eq!(out.applied, -2.0);
    assert_eq!(v1_layout(&project)[1], (3.0, 0.0, 7.0));
}

#[test]
fn an_agents_edit_modes_are_staged_like_any_other_edit() {
    let (mut project, _, [a, b, c]) = abutting_project();
    let live = v1_layout(&project);
    project.set_actor(EditSource::Agent);
    project.set_agent_review(false);
    project.begin_staging(None, None, None).unwrap();
    project.roll_edit(a.id, b.id, 1.0).unwrap();
    project.slip_clip(c.id, 1.0).unwrap();
    project.slide_clip(b.id, 0.5).unwrap();
    project.split_remove(c.id, 10.0, SplitSide::Right).unwrap();

    assert_eq!(v1_layout(&project), live, "the live cut is untouched");
    let staged = project.staged().unwrap().unwrap();
    assert_eq!(
        staged.edits,
        ["Roll edit", "Slip clip", "Slide clip", "Split and remove right"]
    );
    let proposal = project.working_timeline().unwrap();
    assert_eq!(
        proposal.clip(c.id).unwrap().source_in,
        31.5,
        "slipped by 1, then the slide trimmed half a second off its head"
    );
    assert!(!staged.diff.is_empty(), "the review card has something to show");
}

#[test]
fn a_group_split_remove_is_one_revision_and_undoes_in_one_step() {
    let (project, asset) = project_with_av_asset();
    let tracks = project.timeline().unwrap().tracks;
    let (v1, a1) = (tracks[0].id, tracks[1].id);
    let v = project.add_clip_to_timeline(asset, Some(v1), 10.0, 16.0, Some(0.0)).unwrap();
    let a = project.add_clip_to_timeline(asset, Some(a1), 10.0, 16.0, Some(1.0)).unwrap();
    let revisions = project.history().unwrap().len();

    let cuts = [ClipCut { clip_id: v.id, at: 3.0 }, ClipCut { clip_id: a.id, at: 3.0 }];
    let kept = project.split_remove_clips(&cuts, SplitSide::Left).unwrap();
    assert_eq!(kept.iter().map(|c| c.id).collect::<Vec<_>>(), vec![v.id, a.id]);
    let tl = project.timeline().unwrap();
    assert_eq!(
        (tl.clip(v.id).unwrap().timeline_start, tl.clip(a.id).unwrap().timeline_start),
        (3.0, 3.0)
    );
    let history = project.history().unwrap();
    assert_eq!(history.len(), revisions + 1, "one revision for both tracks");
    assert_eq!(history.last().unwrap().label, "Split and remove left (2 clips)");

    let undone = project.undo().unwrap();
    assert_eq!(
        (
            undone.clip(v.id).unwrap().timeline_start,
            undone.clip(a.id).unwrap().timeline_start
        ),
        (0.0, 1.0)
    );

    // A single cut keeps the single label, and is the same code path.
    project.split_remove_clips(&cuts[..1], SplitSide::Right).unwrap();
    assert_eq!(project.history().unwrap().last().unwrap().label, "Split and remove right");
}

#[test]
fn a_refused_group_split_remove_records_nothing_and_ripple_is_per_track() {
    let (project, asset) = project_with_av_asset();
    let tracks = project.timeline().unwrap().tracks;
    let (v1, a1) = (tracks[0].id, tracks[1].id);
    let v = project.add_clip_to_timeline(asset, Some(v1), 10.0, 16.0, Some(0.0)).unwrap();
    let w = project.add_clip_to_timeline(asset, Some(v1), 0.0, 2.0, Some(8.0)).unwrap();
    let a = project.add_clip_to_timeline(asset, Some(a1), 10.0, 16.0, Some(1.0)).unwrap();
    let x = project.add_clip_to_timeline(asset, Some(a1), 0.0, 2.0, Some(9.0)).unwrap();
    let revisions = project.history().unwrap().len();

    let bad = [ClipCut { clip_id: v.id, at: 3.0 }, ClipCut { clip_id: a.id, at: 50.0 }];
    assert!(matches!(
        project.split_remove_clips(&bad, SplitSide::Left),
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(project.history().unwrap().len(), revisions);
    assert_eq!(project.timeline().unwrap().clip(v.id).unwrap().timeline_start, 0.0);

    // Ripple on: each lane closes by what it lost — V1 by 2 s, A1 by 4 s.
    project.set_ripple_mode(true).unwrap();
    let cuts = [ClipCut { clip_id: v.id, at: 2.0 }, ClipCut { clip_id: a.id, at: 5.0 }];
    let kept = project.split_remove_clips(&cuts, SplitSide::Left).unwrap();
    assert_eq!(
        kept.iter().map(|c| c.timeline_start).collect::<Vec<_>>(),
        vec![0.0, 1.0],
        "handed back as they ended up"
    );
    let tl = project.timeline().unwrap();
    assert_eq!(
        (tl.clip(w.id).unwrap().timeline_start, tl.clip(x.id).unwrap().timeline_start),
        (6.0, 5.0)
    );
}

#[test]
fn an_agents_group_split_remove_is_staged() {
    let (mut project, asset) = project_with_av_asset();
    let tracks = project.timeline().unwrap().tracks;
    let v = project
        .add_clip_to_timeline(asset, Some(tracks[0].id), 10.0, 16.0, Some(0.0))
        .unwrap();
    let a = project
        .add_clip_to_timeline(asset, Some(tracks[1].id), 10.0, 16.0, Some(0.0))
        .unwrap();
    project.set_actor(EditSource::Agent);
    project.set_agent_review(false);
    project.begin_staging(None, None, None).unwrap();
    let cuts = [ClipCut { clip_id: v.id, at: 2.0 }, ClipCut { clip_id: a.id, at: 2.0 }];
    project.split_remove_clips(&cuts, SplitSide::Right).unwrap();
    assert_eq!(
        project.timeline().unwrap().clip(v.id).unwrap().duration(),
        6.0,
        "the live cut is untouched"
    );
    assert_eq!(project.working_timeline().unwrap().clip(v.id).unwrap().duration(), 2.0);
    assert_eq!(project.staged().unwrap().unwrap().edits, ["Split and remove right (2 clips)"]);
}

#[test]
fn a_staged_slip_alone_still_shows_up_in_the_review_diff() {
    // A slip changes no timing at all, only which footage shows: a diff that
    // missed it would read as an empty proposal and `apply_staged` would drop it.
    let (mut project, _, [a, ..]) = abutting_project();
    project.set_actor(EditSource::Agent);
    project.set_agent_review(false);
    project.begin_staging(None, None, None).unwrap();
    project.slip_clip(a.id, 2.0).unwrap();
    let staged = project.staged().unwrap().unwrap();
    assert_eq!(staged.diff.entries.len(), 1);
    assert!(
        staged.diff.summary().contains("Slipped clip on V1"),
        "{}",
        staged.diff.summary()
    );
}

#[test]
fn split_remove_follows_ripple_mode_like_a_trim_and_keeps_the_clips_identity() {
    let revisions = |project: &Project| project.history().unwrap().len();

    // Off: a plain split-and-delete — the gap stays where the half was.
    let (project, _, [a, b, c]) = abutting_project();
    let before = revisions(&project);
    let kept = project.split_remove(b.id, 6.0, SplitSide::Right).unwrap();
    assert_eq!(kept.id, b.id);
    assert_eq!(
        v1_layout(&project),
        vec![(0.0, 10.0, 14.0), (4.0, 20.0, 22.0), (8.0, 30.0, 34.0)]
    );
    assert_eq!(revisions(&project), before + 1);
    assert_eq!(project.history().unwrap().last().unwrap().label, "Split and remove right");
    let _ = (a, c);

    let (project, _, [_, b, _]) = abutting_project();
    let kept = project.split_remove(b.id, 6.0, SplitSide::Left).unwrap();
    assert_eq!((kept.timeline_start, kept.source_in), (6.0, 22.0));
    assert_eq!(
        v1_layout(&project),
        vec![(0.0, 10.0, 14.0), (6.0, 22.0, 24.0), (8.0, 30.0, 34.0)]
    );
    assert_eq!(project.history().unwrap().last().unwrap().label, "Split and remove left");

    // On: the track closes up, and a left removal keeps the clip's own start.
    let (project, _, [_, b, _]) = abutting_project();
    project.set_ripple_mode(true).unwrap();
    project.split_remove(b.id, 6.0, SplitSide::Right).unwrap();
    assert_eq!(
        v1_layout(&project),
        vec![(0.0, 10.0, 14.0), (4.0, 20.0, 22.0), (6.0, 30.0, 34.0)]
    );

    let (project, _, [_, b, _]) = abutting_project();
    project.set_ripple_mode(true).unwrap();
    let kept = project.split_remove(b.id, 6.0, SplitSide::Left).unwrap();
    assert_eq!(
        kept.timeline_start, 4.0,
        "handed back as it ended up, not as the edit alone made it"
    );
    assert_eq!(
        v1_layout(&project),
        vec![(0.0, 10.0, 14.0), (4.0, 22.0, 24.0), (6.0, 30.0, 34.0)]
    );

    // A per-call override, as every ripple-following edit takes.
    let (project, _, [_, b, _]) = abutting_project();
    project.set_ripple_mode(true).unwrap();
    project
        .with_ripple(Some(false), |p| p.split_remove(b.id, 6.0, SplitSide::Right))
        .unwrap();
    assert_eq!(v1_layout(&project)[2].0, 8.0);
}
