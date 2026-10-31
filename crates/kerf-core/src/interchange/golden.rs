//! Golden text for the interchange writers: each case's `.otio` and `.edl` are
//! committed under `interchange/golden/` and must come out byte for byte.
//! Re-bless an intended change with
//! `KERF_INTERCHANGE_BLESS=1 cargo test -p kerf-core --no-default-features interchange`
//! and read the `git diff`.

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use uuid::Uuid;

use super::{export, InterchangeFormat, InterchangeOptions, Report};
use crate::engine::test_support::{audio_stream, image_stream, video_stream};
use crate::model::{Asset, Clip, Marker, MarkerKind, StreamKind, TextOverlay, Timeline, Track, Transition, TransitionKind};

fn asset(n: u128, path: &str, duration: f64, image: bool) -> Asset {
    Asset {
        id: Uuid::from_u128(n),
        path: path.to_string(),
        name: path.rsplit(['/', '\\']).next().unwrap_or(path).to_string(),
        duration,
        streams: if image {
            vec![image_stream(1920, 1080)]
        } else {
            vec![video_stream(1920, 1080, 25.0), audio_stream(48_000, 2)]
        },
        imported_at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    }
}

fn clip(n: u128, asset: &Asset, source_in: f64, source_out: f64, at: f64) -> Clip {
    Clip {
        id: Uuid::from_u128(n),
        ..Clip::new(asset.id, source_in, source_out, at)
    }
}

fn track(n: u128, kind: StreamKind, name: &str, clips: Vec<Clip>) -> Track {
    Track {
        id: Uuid::from_u128(n),
        clips,
        ..Track::new(kind, name)
    }
}

fn marker(n: u128, time: f64, name: &str) -> Marker {
    Marker {
        id: Uuid::from_u128(n),
        time,
        name: name.to_string(),
        ..Default::default()
    }
}

/// Two shots cut together, a gap, a still, sound under it all, markers and a title.
fn basic() -> (Timeline, Vec<Asset>, f64) {
    let a = asset(1, "/media/Interview Take 1.mov", 120.0, false);
    let b = asset(2, "/media/b-roll.mp4", 60.0, false);
    let still = asset(3, "C:\\Stills\\logo card.png", 0.0, true);
    let music = asset(4, "/media/music.wav", 300.0, false);
    let mut faded = clip(12, &b, 4.0, 8.0, 5.0);
    faded.fade_out = 0.5;
    faded.transform.scale = 1.2;
    let mut chapter = marker(31, 5.0, "Part two");
    chapter.kind = MarkerKind::Chapter;
    chapter.comment = "the b-roll starts".into();
    chapter.color = Some("#22c55e".into());
    let mut review = marker(32, 1.0, "check focus");
    review.duration = 2.0;
    let tl = Timeline {
        tracks: vec![
            track(
                101,
                StreamKind::Video,
                "V1",
                vec![clip(11, &a, 10.0, 15.0, 0.0), faded, clip(13, &still, 0.0, 3.0, 11.0)],
            ),
            track(102, StreamKind::Audio, "A1", vec![clip(14, &music, 0.0, 14.0, 0.0)]),
        ],
        overlays: vec![TextOverlay {
            id: Uuid::from_u128(41),
            ..TextOverlay::new("Title", 0.0, 2.0)
        }],
        markers: vec![review, chapter],
        ..Timeline::new()
    };
    (tl, vec![a, b, still, music], 25.0)
}

/// Dissolves (with and without a predecessor), speed, reverse, a dip, a slide,
/// a disabled clip and a second video track, at 24 fps.
fn transitions() -> (Timeline, Vec<Asset>, f64) {
    let a = asset(1, "/m/shot_a.mov", 100.0, false);
    let b = asset(2, "/m/shot_b.mov", 100.0, false);
    let c = asset(3, "/m/shot_a.mp4", 100.0, false);
    let mut fade_up = clip(11, &a, 1.0, 3.0, 0.0);
    fade_up.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let mut dissolve = clip(12, &b, 20.0, 24.0, 2.0);
    dissolve.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 1.0,
    });
    let mut fast = clip(13, &c, 10.0, 14.0, 6.0);
    fast.speed = 2.0;
    fast.transition_in = Some(Transition {
        kind: TransitionKind::DipToBlack,
        duration: 1.0,
    });
    let mut reversed = clip(14, &a, 50.0, 52.0, 8.0);
    reversed.speed = -1.0;
    reversed.transition_in = Some(Transition {
        kind: TransitionKind::SlideLeft,
        duration: 0.5,
    });
    let mut off = clip(15, &b, 0.0, 1.0, 10.0);
    off.enabled = false;
    let tl = Timeline {
        tracks: vec![
            track(101, StreamKind::Video, "V1", vec![fade_up, dissolve, fast, reversed, off]),
            track(102, StreamKind::Video, "V2", vec![clip(16, &c, 0.0, 1.0, 1.0)]),
        ],
        ..Timeline::new()
    };
    (tl, vec![a, b, c], 24.0)
}

/// 29.97 fps across the first drop-frame minute, every edge on a frame.
fn drop_frame() -> (Timeline, Vec<Asset>, f64) {
    let a = asset(1, "/m/ntsc.mov", 600.0, false);
    let f = |n: f64| n * 1001.0 / 30000.0;
    let tl = Timeline {
        tracks: vec![track(
            101,
            StreamKind::Video,
            "V1",
            vec![
                clip(11, &a, 0.0, f(1800.0), 0.0),
                clip(12, &a, f(3600.0), f(3900.0), f(1800.0)),
            ],
        )],
        ..Timeline::new()
    };
    (tl, vec![a], 30000.0 / 1001.0)
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/interchange/golden")
}

fn check(case: &str, build: fn() -> (Timeline, Vec<Asset>, f64), format: InterchangeFormat) -> Report {
    let (tl, assets, rate) = build();
    let opts = InterchangeOptions {
        rate,
        title: format!("Kerf {case}"),
        video_track: 0,
    };
    let (text, report) = export(format, &tl, &assets, &opts).unwrap();
    let path = golden_dir().join(format!("{case}.{}", format.ext()));
    if std::env::var("KERF_INTERCHANGE_BLESS").as_deref() == Ok("1") {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&path, &text).unwrap();
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (bless with KERF_INTERCHANGE_BLESS=1)", path.display()))
        .replace('\r', "");
    assert_eq!(
        text,
        want,
        "{case}.{} moved (bless with KERF_INTERCHANGE_BLESS=1)",
        format.ext()
    );
    report
}

#[test]
fn interchange_basic_otio_and_edl_are_golden() {
    let otio = check("basic", basic, InterchangeFormat::Otio);
    assert_eq!(
        otio.to_string(),
        "info: clip fades have no OTIO equivalent; they are kept in metadata.kerf\n\
         info: clip transforms and keyframes have no OTIO equivalent; they are kept in metadata.kerf\n\
         info: 1 titles and captions have no OTIO equivalent; they are kept in metadata.kerf\n"
    );
    let edl = check("basic", basic, InterchangeFormat::Edl);
    assert_eq!(
        edl.to_string(),
        "info: audio tracks are not written to the EDL\n\
         info: markers are not written to the EDL\n\
         warning: titles and captions are not carried by an EDL\n\
         warning: the sound of video clips is not written to the EDL (events are picture only, channel V) (x2)\n\
         warning: clip fades are not carried by an EDL\n\
         warning: clip transforms and keyframes are not carried by an EDL\n"
    );
}

#[test]
fn interchange_transitions_otio_and_edl_are_golden() {
    let otio = check("transitions", transitions, InterchangeFormat::Otio);
    assert!(
        otio.to_string()
            .contains("dip to black transitions are written as OTIO custom transitions"),
        "{otio}"
    );
    let edl = check("transitions", transitions, InterchangeFormat::Edl);
    let text = edl.to_string();
    for want in [
        "an EDL holds one video track; 1 other video track(s) were not written",
        "dip to black transitions were written as cuts",
        "slide left transitions were written as dissolves",
        "disabled clips were left out of the EDL",
    ] {
        assert!(text.contains(want), "{want}: {text}");
    }
}

#[test]
fn interchange_drop_frame_edl_is_golden() {
    let edl = check("drop_frame", drop_frame, InterchangeFormat::Edl);
    assert_eq!(edl.entries.len(), 1, "{edl}");
    assert!(edl.to_string().contains("sound of video clips"), "{edl}");
    check("drop_frame", drop_frame, InterchangeFormat::Otio);
}

#[test]
fn an_edl_of_a_missing_track_is_refused_and_reels_stay_unique() {
    let (tl, assets, rate) = transitions();
    let opts = InterchangeOptions {
        rate,
        title: "x".into(),
        video_track: 5,
    };
    assert!(export(InterchangeFormat::Edl, &tl, &assets, &opts).is_err());
    let (text, _) = export(
        InterchangeFormat::Edl,
        &tl,
        &assets,
        &InterchangeOptions { video_track: 0, ..opts },
    )
    .unwrap();
    assert!(text.contains("SHOT_A ") && text.contains("SHOT_A2 "), "{text}");
}

/// No interchange format here can write a rate that changes: the event is written at the clip's average
/// speed — so it still covers the footage the clip plays — and the report says so.
#[test]
fn a_speed_ramp_is_written_at_its_average_speed_and_said_so() {
    use crate::model::SpeedKey;
    let a = asset(1, "/m/shot.mov", 100.0, false);
    let mut ramped = clip(11, &a, 10.0, 16.0, 0.0);
    // 1x easing down to 0.5x over two seconds, then 0.5x: 1.5 + 4.5 source seconds make 11 s.
    ramped.speed_ramp = vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(2.0, 0.5)];
    assert_eq!(ramped.duration(), 11.0);
    let tl = Timeline {
        tracks: vec![track(101, StreamKind::Video, "V1", vec![ramped.clone()])],
        ..Timeline::new()
    };
    let opts = InterchangeOptions {
        rate: 24.0,
        title: "ramp".into(),
        video_track: 0,
    };

    let (edl, report) = export(InterchangeFormat::Edl, &tl, std::slice::from_ref(&a), &opts).unwrap();
    assert!(
        report
            .to_string()
            .contains("warning: speed ramps were written at their average speed"),
        "{report}"
    );
    assert!(edl.contains("00:00:10:00 00:00:16:00 01:00:00:00 01:00:11:00"), "{edl}");
    assert!(edl.contains("M2   ") && edl.contains(" 013.1 "), "{edl}");

    let (otio, report) = export(InterchangeFormat::Otio, &tl, std::slice::from_ref(&a), &opts).unwrap();
    assert!(
        report
            .to_string()
            .contains("warning: speed ramps were written at their average speed"),
        "{report}"
    );
    let doc: serde_json::Value = serde_json::from_str(&otio).unwrap();
    let clip_json = &doc["tracks"]["children"][0]["children"][0];
    let scalar = clip_json["effects"][0]["time_scalar"].as_f64().unwrap();
    assert!((scalar - 6.0 / 11.0).abs() < 1e-9, "{scalar}");
    // The ramp itself is not lost: it travels in metadata.kerf for Kerf to read back.
    assert_eq!(clip_json["metadata"]["kerf"]["speed_ramp"].as_array().unwrap().len(), 2);

    // A reversed clip keeps its direction.
    let mut backwards = ramped;
    backwards.speed = -1.0;
    let tl = Timeline {
        tracks: vec![track(101, StreamKind::Video, "V1", vec![backwards])],
        ..Timeline::new()
    };
    let (otio, _) = export(InterchangeFormat::Otio, &tl, std::slice::from_ref(&a), &opts).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&otio).unwrap();
    let scalar = doc["tracks"]["children"][0]["children"][0]["effects"][0]["time_scalar"]
        .as_f64()
        .unwrap();
    assert!((scalar + 6.0 / 11.0).abs() < 1e-9, "{scalar}");
}

/// Neither format has a blend mode: the cut is written as it is, the report names the mode, and OTIO keeps it in
/// metadata.kerf for Kerf to read back. A clip at normal adds no line.
#[test]
fn a_blend_mode_is_reported_and_kept_in_otio_metadata() {
    use crate::model::BlendMode;
    let a = asset(1, "/m/shot.mov", 100.0, false);
    let plain = clip(11, &a, 0.0, 4.0, 0.0);
    let mut screened = clip(12, &a, 10.0, 14.0, 4.0);
    screened.blend_mode = BlendMode::Screen;
    let tl = Timeline {
        tracks: vec![track(101, StreamKind::Video, "V1", vec![plain.clone(), screened])],
        ..Timeline::new()
    };
    let opts = InterchangeOptions {
        rate: 24.0,
        title: "blend".into(),
        video_track: 0,
    };
    let (_, report) = export(InterchangeFormat::Edl, &tl, std::slice::from_ref(&a), &opts).unwrap();
    let said = report.to_string();
    assert!(
        said.contains("warning: clip blend modes are not carried by an EDL\n"),
        "{said}"
    );
    let (otio, report) = export(InterchangeFormat::Otio, &tl, std::slice::from_ref(&a), &opts).unwrap();
    assert!(
        report.to_string().contains("clip blend modes have no OTIO equivalent"),
        "{report}"
    );
    let doc: serde_json::Value = serde_json::from_str(&otio).unwrap();
    let clips = &doc["tracks"]["children"][0]["children"];
    assert!(clips[0]["metadata"]["kerf"].get("blend_mode").is_none());
    assert_eq!(clips[1]["metadata"]["kerf"]["blend_mode"], "screen");

    let tl = Timeline {
        tracks: vec![track(101, StreamKind::Video, "V1", vec![plain])],
        ..Timeline::new()
    };
    let (_, report) = export(InterchangeFormat::Edl, &tl, std::slice::from_ref(&a), &opts).unwrap();
    assert!(!report.to_string().contains("blend"), "{report}");
}

/// Neither format has live behaviours: the cut is written as it is, the report names them, and OTIO keeps them in
/// metadata.kerf for Kerf to read back. A clip with none adds no line.
#[test]
fn live_behaviors_are_reported_and_kept_in_otio_metadata() {
    use crate::model::Behavior;
    let a = asset(1, "/m/shot.mov", 100.0, false);
    let plain = clip(11, &a, 0.0, 4.0, 0.0);
    let mut lively = clip(12, &a, 10.0, 14.0, 4.0);
    lively.behaviors = vec![Behavior::Shake {
        amount: 0.01,
        speed: 3.0,
        seed: 5,
        phase: 0.0,
    }];
    let tl = Timeline {
        tracks: vec![track(101, StreamKind::Video, "V1", vec![plain.clone(), lively])],
        ..Timeline::new()
    };
    let opts = InterchangeOptions {
        rate: 24.0,
        title: "behaviors".into(),
        video_track: 0,
    };
    let (_, report) = export(InterchangeFormat::Edl, &tl, std::slice::from_ref(&a), &opts).unwrap();
    assert!(
        report
            .to_string()
            .contains("warning: clip behaviours are not carried by an EDL\n"),
        "{report}"
    );
    let (otio, report) = export(InterchangeFormat::Otio, &tl, std::slice::from_ref(&a), &opts).unwrap();
    assert!(
        report.to_string().contains("clip behaviours have no OTIO equivalent"),
        "{report}"
    );
    let doc: serde_json::Value = serde_json::from_str(&otio).unwrap();
    let clips = &doc["tracks"]["children"][0]["children"];
    assert!(clips[0]["metadata"]["kerf"].get("behaviors").is_none());
    assert_eq!(clips[1]["metadata"]["kerf"]["behaviors"][0]["kind"], "shake");
    assert_eq!(clips[1]["metadata"]["kerf"]["behaviors"][0]["seed"], 5);

    let tl = Timeline {
        tracks: vec![track(101, StreamKind::Video, "V1", vec![plain])],
        ..Timeline::new()
    };
    let (_, report) = export(InterchangeFormat::Edl, &tl, std::slice::from_ref(&a), &opts).unwrap();
    assert!(!report.to_string().contains("behaviour"), "{report}");
}
