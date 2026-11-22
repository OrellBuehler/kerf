//! The measured size estimate held to real full exports, in depth (`#[ignore]`d: they drive the real `ffmpeg`, and run only on the build they were tuned on).
//!
//! `measure_export_size` extrapolates from a few one-second sample encodes, so what has to be shown is that its
//! band holds the *whole* file when the footage changes under it. Each case here is a cut of three
//! sections of different complexity (the soft, plain and grainy pictures of `export_size_rendered`: a still
//! card, an ordinary shot, a noisy one), in each order, exported in full with the same options and compared to
//! what was measured first; a burst the windows miss; shapes that differ in fit. Every row prints what the measurement said, how wide,
//! how long it took and how many windows it got through; the table in `.claude/docs/engine-cli.md` is that output.
//!
//! CI does not run this suite (`--skip export_size_measure_calibration`) and each test starts at `calibrated_here` (FFmpeg 9, or
//! `KERF_SIZE_CALIBRATION=any`). The wall-clock bounds here (a budget and its grace) are for the machine the numbers came from.
//!
//! `KERF_FFMPEG=<pinned ffmpeg> KERF_FFPROBE=<pinned ffprobe> cargo test -p kerf-core --no-default-features \
//!  -- --ignored export_size_measure_calibration --test-threads=1`

#![allow(clippy::print_stderr)] // what each case measured, for whoever runs the ignored ones

use std::time::{Duration, Instant};

use super::export_size_measure::{plan_windows, planned_windows, Span, WINDOW_SECS};
use super::export_size_measure_rendered::{compare, cut_of, finish, sections, Row};
use super::export_size_rendered::{calibrated_here, crf_case, sound_source, source, Class, Scratch};
use super::*;
use crate::engine::test_support::{audio_track, make_clip, timeline_of};
use crate::platform::{PlatformTarget, SizeLimit};

/// Constant quality on a minute of footage that changes: every codec the dialog offers, three orders, 360p.
#[test]
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn measured_bands_hold_for_footage_that_changes() {
    if !calibrated_here("measured_bands_hold_for_footage_that_changes") {
        return;
    }
    let scratch = Scratch::new("measured-crf");
    let dir = &scratch.0;
    let mut rows = Vec::new();
    let cases: Vec<(&str, ExportOptions)> = vec![
        ("x264 CRF 23", crf_case("libx264", Container::Mp4, 23, Some("medium"))),
        ("x264 CRF 18", crf_case("libx264", Container::Mp4, 18, Some("medium"))),
        ("x264 CRF 28", crf_case("libx264", Container::Mp4, 28, Some("medium"))),
        ("x265 CRF 23", crf_case("libx265", Container::Mp4, 23, Some("medium"))),
        ("svt-av1 CRF 30", crf_case("libsvtav1", Container::Mkv, 30, Some("8"))),
        ("vp9 CRF 31", crf_case("libvpx-vp9", Container::Webm, 31, Some("4"))),
    ];
    for (order, assets) in sections(dir, (640, 360), 20.0, false) {
        let tl = cut_of(&assets);
        for (what, opts) in &cases {
            rows.push(compare(dir, &format!("{what} {order}"), &tl, &assets, opts));
        }
    }
    finish(&rows);
}

/// ProRes, GIF, lossless and sound beside the picture, on the same changing footage.
#[test]
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn measured_bands_hold_for_the_other_encoders() {
    if !calibrated_here("measured_bands_hold_for_the_other_encoders") {
        return;
    }
    let scratch = Scratch::new("measured-other");
    let dir = &scratch.0;
    let mut rows = Vec::new();
    let prores = ExportOptions {
        container: Container::Mov,
        video_codec: Some("prores_ks".into()),
        prores_profile: Some(3),
        include_audio: false,
        ..ExportOptions::default()
    };
    let gif = ExportOptions {
        container: Container::Gif,
        video_codec: Some("gif".into()),
        resolution: Some((480, 270)),
        fps: Some(15.0),
        include_audio: false,
        ..ExportOptions::default()
    };
    let lossless = ExportOptions {
        rate_control: RateControl::Lossless,
        ..crf_case("libx264", Container::Mp4, 0, Some("medium"))
    };
    let svt_lossless = ExportOptions {
        rate_control: RateControl::Lossless,
        ..crf_case("libsvtav1", Container::Mkv, 0, Some("8"))
    };
    for (order, assets) in sections(dir, (640, 360), 20.0, false) {
        let tl = cut_of(&assets);
        for (what, opts) in [
            ("ProRes HQ", &prores),
            ("GIF 480x270 15 fps", &gif),
            ("x264 lossless", &lossless),
            ("svt-av1 lossless", &svt_lossless),
        ] {
            rows.push(compare(dir, &format!("{what} {order}"), &tl, &assets, opts));
        }
    }
    // The sound is measured with the picture: AAC beside x264.
    for (order, assets) in sections(dir, (640, 360), 20.0, true) {
        let tl = cut_of(&assets);
        let opts = ExportOptions {
            audio_codec: Some("aac".into()),
            audio_bitrate: Some("128k".into()),
            include_audio: true,
            ..crf_case("libx264", Container::Mp4, 23, Some("medium"))
        };
        rows.push(compare(dir, &format!("x264 CRF 23 + aac 128k {order}"), &tl, &assets, &opts));
    }
    finish(&rows);
}

/// Footage that does not change gets a narrow band; a cut short enough to encode whole an even narrower one.
#[test]
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn measured_bands_are_narrow_for_even_footage_and_short_cuts() {
    if !calibrated_here("measured_bands_are_narrow_for_even_footage_and_short_cuts") {
        return;
    }
    let scratch = Scratch::new("measured-even");
    let dir = &scratch.0;
    let mut rows = Vec::new();
    let opts = crf_case("libx264", Container::Mp4, 23, Some("medium"));
    for class in [Class::Soft, Class::Plain, Class::Grainy] {
        let asset = source(dir, class, (640, 360), 30.0, 40.0, false);
        let tl = cut_of(std::slice::from_ref(&asset));
        let what = format!("x264 CRF 23, 40 s of {}", class.name());
        let row = compare(dir, &what, &tl, std::slice::from_ref(&asset), &opts);
        assert!(
            row.width() < 1.9,
            "{}: even footage should be narrow, x{:.2}",
            row.what,
            row.width()
        );
        rows.push(row);
        let short = source(dir, class, (640, 360), 30.0, 4.0, false);
        let tl = cut_of(std::slice::from_ref(&short));
        let what = format!("x264 CRF 23, 4 s of {} (whole)", class.name());
        let row = compare(dir, &what, &tl, std::slice::from_ref(&short), &opts);
        assert!(
            row.width() < 1.4,
            "{}: a whole encode should be narrower still, x{:.2}",
            row.what,
            row.width()
        );
        rows.push(row);
    }
    finish(&rows);
}

/// 1080p: what the measurement costs, and that its band holds, on a cut that changes.
#[test]
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn measured_bands_hold_at_1080p_and_say_how_long_they_took() {
    if !calibrated_here("measured_bands_hold_at_1080p_and_say_how_long_they_took") {
        return;
    }
    let scratch = Scratch::new("measured-1080p");
    let dir = &scratch.0;
    let mut rows = Vec::new();
    let (_, assets) = sections(dir, (1920, 1080), 10.0, false).remove(0);
    let tl = cut_of(&assets);
    for (what, opts) in [
        (
            "x264 CRF 20 (Web 1080p)",
            crf_case("libx264", Container::Mp4, 20, Some("medium")),
        ),
        ("x265 CRF 23", crf_case("libx265", Container::Mp4, 23, Some("medium"))),
        ("svt-av1 CRF 30", crf_case("libsvtav1", Container::Mkv, 30, Some("8"))),
    ] {
        rows.push(compare(
            dir,
            &format!("1080p {what}, 30 s soft>plain>grainy"),
            &tl,
            &assets,
            &opts,
        ));
    }
    finish(&rows);
}

/// Several shapes: one picture at two sizes shares a measurement and the rest are each measured on their own, bars and blur
/// fill included; every file is held to its own real export.
#[test]
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn measured_deliveries_hold_for_each_shape() {
    if !calibrated_here("measured_deliveries_hold_for_each_shape") {
        return;
    }
    let scratch = Scratch::new("measured-deliveries");
    let dir = &scratch.0;
    let (_, assets) = sections(dir, (640, 360), 20.0, false).remove(0);
    let tl = cut_of(&assets);
    let opts = crf_case("libx264", Container::Mp4, 23, Some("medium"));
    let shapes = [
        Delivery::new(360, 640, Fit::Cover),
        // The same picture, smaller: scaled from the first.
        Delivery::new(270, 480, Fit::Cover),
        Delivery::new(360, 360, Fit::Cover),
        Delivery::new(640, 360, Fit::Contain),
        // The same pixels as the first, contained with bars, and filled with a blurred copy: other pictures, each measured.
        Delivery::new(360, 640, Fit::Contain),
        Delivery::new(360, 640, Fit::Blur),
    ];
    let started = Instant::now();
    let measured =
        measure_export_size(&tl, &assets, &opts, &shapes, DEFAULT_MEASURE_BUDGET, &mut |_| {}, &|| false).expect("measure");
    let took = started.elapsed();
    let mut rows = Vec::new();
    for (file, shape) in measured.files.iter().zip(shapes) {
        let out = dir.join(format!("shape-{}x{}-{:?}.mp4", shape.width, shape.height, shape.fit));
        let per = ExportOptions {
            resolution: Some((shape.width, shape.height)),
            fit: shape.fit,
            ..opts.clone()
        };
        let status = render_with_progress(&tl.for_delivery(shape), &assets, &out, &per, &mut |_| {}, &|| false).expect("export");
        assert_eq!(status, RenderStatus::Completed);
        let real = std::fs::metadata(&out).unwrap().len();
        let _ = std::fs::remove_file(&out);
        rows.push(Row {
            what: format!(
                "{}x{} {:?} ({:?}, all six in {:.1} s)",
                shape.width,
                shape.height,
                shape.fit,
                file.estimate.basis,
                took.as_secs_f64()
            ),
            real,
            estimate: file.estimate.clone(),
            took,
            windows: 0,
            planned: 0,
        });
    }
    // Only the smaller of the two covers is scaled; the bars and the blur are measured, not scaled from the cover.
    let notes: Vec<&str> = measured.files.iter().map(|f| f.estimate.note.as_str()).collect();
    assert_eq!(
        notes.iter().map(|n| n.contains("Scaled from")).collect::<Vec<_>>(),
        [false, true, false, false, false, false],
        "{notes:?}"
    );
    assert!(measured.total.is_some());
    // A contained picture is a different file from the covered one of the same pixels: the real sizes say why they are not scaled.
    eprintln!(
        "cover {} B, bars {} B, blur {} B of the same 360x640 pixels",
        rows[0].real, rows[4].real, rows[5].real
    );
    finish(&rows);
}

/// A short burst of noise in a shot the windows are spread over: the windows can miss it, and the answer must not claim it cannot happen.
#[test]
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn a_burst_the_windows_miss_is_said_not_promised_away() {
    if !calibrated_here("a_burst_the_windows_miss_is_said_not_promised_away") {
        return;
    }
    let scratch = Scratch::new("measured-burst");
    let dir = &scratch.0;
    // One asset of a minute: calm, then a 3 s burst of noise, calm again. One file, so the planner has no other footage to cover.
    let single = [Span {
        start: 0.0,
        end: 60.0,
        source: uuid::Uuid::new_v4(),
    }];
    let windows = plan_windows(0.0, 60.0, &single, planned_windows(60.0), WINDOW_SECS);
    let mut edges = vec![0.0];
    edges.extend(windows.iter().flat_map(|w| [w.start, w.start + w.len]));
    edges.push(60.0);
    // The middle of the widest stretch between two windows.
    let (gap_from, gap_to) = edges
        .chunks(2)
        .map(|c| (c[0], c[1]))
        .max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)))
        .unwrap();
    let burst = ((gap_from + gap_to) / 2.0 - 1.5, (gap_from + gap_to) / 2.0 + 1.5);
    assert!(
        windows.iter().all(|w| w.start + w.len < burst.0 || w.start > burst.1),
        "a window lands on the burst: {windows:?} {burst:?}"
    );
    let path = dir.join("burst.mp4");
    let calm = |secs: f64| format!("testsrc2=s=640x360:r=30:d={secs}");
    let noisy = |secs: f64| format!("testsrc2=s=640x360:r=30:d={secs},noise=alls=20:allf=t");
    let graph = "[0:v][1:v][2:v]concat=n=3:v=1:a=0[v]";
    let bps = (0.405 * 640.0 * 360.0 * 30.0) as u64;
    let mut args: Vec<String> = Vec::new();
    for input in [calm(burst.0), noisy(burst.1 - burst.0), calm(60.0 - burst.1)] {
        args.extend(["-f", "lavfi", "-i"].map(String::from));
        args.push(input);
    }
    args.extend(
        [
            "-filter_complex",
            graph,
            "-map",
            "[v]",
            "-c:v",
            "libx264",
            "-preset",
            "fast",
            "-crf",
            "18",
        ]
        .map(String::from),
    );
    args.extend([
        "-maxrate".to_string(),
        bps.to_string(),
        "-bufsize".to_string(),
        bps.to_string(),
        "-pix_fmt".to_string(),
        "yuv420p".to_string(),
    ]);
    args.push(path.to_string_lossy().into_owned());
    super::export_size_rendered::run(&args);
    let mut asset = crate::engine::test_support::test_asset(vec![crate::engine::test_support::video_stream(640, 360, 30.0)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 60.0;
    let tl = cut_of(std::slice::from_ref(&asset));
    let opts = crf_case("libx264", Container::Mp4, 23, Some("medium"));
    let row = compare(
        dir,
        "x264 CRF 23, 3 s burst the windows miss",
        &tl,
        std::slice::from_ref(&asset),
        &opts,
    );
    row.print();
    let e = &row.estimate;
    // What the answer may be held to: a burst only adds, so the real size is over the low end; the note says a missed stretch makes it
    // larger; the model's own ceiling (the grainiest footage at this quality) still holds it; and a cap between the estimate and the real
    // size is a warning at most, never "it will not fit".
    assert!(e.low_bytes <= row.real, "{} under {}", e.low_bytes, row.real);
    assert!(
        e.note
            .contains("a busy stretch the samples missed would make the file larger"),
        "{}",
        e.note
    );
    let inputs = export_size_inputs(&tl, std::slice::from_ref(&asset), &opts);
    let model = estimate_size(&inputs, &opts);
    assert!(
        row.real <= model.high_bytes,
        "the model's ceiling holds the burst: {} over {}",
        row.real,
        model.high_bytes
    );
    let bounds = e.bounds().unwrap();
    let cap = SizeLimit {
        up_to_secs: None,
        max_bytes: (e.high_bytes + row.real) / 2,
    };
    let target = PlatformTarget {
        size_limits: std::slice::from_ref(&cap).to_vec().leak(),
        ..*crate::platform::target("youtube").unwrap()
    };
    let cut = crate::platform::CutSummary {
        duration: 60.0,
        width: 640,
        height: 360,
        has_audio: false,
        has_text: true,
    };
    let checked = crate::platform::check_sized(&target, &cut, Some(bounds));
    assert!(
        checked.issues.iter().all(|i| i.severity != crate::platform::Severity::Error),
        "{:?}",
        checked.issues
    );
    eprintln!(
        "burst: real {} {} the measured high end {} (floor {}, model high {})",
        row.real,
        if row.real <= e.high_bytes { "is under" } else { "is OVER" },
        e.high_bytes,
        e.floor_bytes,
        model.high_bytes
    );
}

/// Sound alone: the lossless and the quality-driven codecs the model can only bound, over silence, a tone and noise in turn.
#[test]
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn measured_bands_hold_for_sound_alone() {
    if !calibrated_here("measured_bands_hold_for_sound_alone") {
        return;
    }
    let scratch = Scratch::new("measured-sound");
    let dir = &scratch.0;
    let mut rows = Vec::new();
    let [silence, tone, noise] = ["silence", "tone", "noise"].map(|k| sound_source(dir, k, 20.0));
    for (order, assets) in [
        ("silence>tone>noise", vec![silence.clone(), tone.clone(), noise.clone()]),
        ("noise>silence>tone", vec![noise, silence, tone]),
    ] {
        let mut at = 0.0;
        let clips = assets
            .iter()
            .map(|a| {
                let clip = make_clip(a.id, 0.0, a.duration, at);
                at += a.duration;
                clip
            })
            .collect();
        let tl = timeline_of(vec![audio_track(clips)]);
        for (what, container, codec) in [("flac", Container::Flac, "flac"), ("alac", Container::M4a, "alac")] {
            let opts = ExportOptions {
                container,
                audio_codec: Some(codec.into()),
                ..ExportOptions::default()
            };
            rows.push(compare(dir, &format!("{what} {order}"), &tl, &assets, &opts));
        }
    }
    finish(&rows);
}

/// A time limit too short for any window is the model's answer, in time, with the reason.
#[test]
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn a_budget_too_short_for_one_window_is_the_model_in_time() {
    if !calibrated_here("a_budget_too_short_for_one_window_is_the_model_in_time") {
        return;
    }
    let scratch = Scratch::new("measured-late");
    let dir = &scratch.0;
    let asset = source(dir, Class::Plain, (1920, 1080), 30.0, 30.0, false);
    let tl = cut_of(std::slice::from_ref(&asset));
    // x265 veryslow: a second of 1080p takes far longer than the time given.
    let opts = crf_case("libx265", Container::Mp4, 23, Some("veryslow"));
    let started = Instant::now();
    let got = measure_export_size(
        &tl,
        std::slice::from_ref(&asset),
        &opts,
        &[],
        Duration::from_millis(500),
        &mut |_| {},
        &|| false,
    )
    .expect("measure");
    let took = started.elapsed();
    eprintln!("x265 veryslow 1080p, a 0.5 s budget: {took:?}: {:?}", got.files[0].estimate);
    assert!(took < Duration::from_secs(8), "{took:?}");
    let e = &got.files[0].estimate;
    assert_eq!(e.basis, SizeBasis::Modelled);
    assert!(e.note.contains("time limit"), "{}", e.note);
}
