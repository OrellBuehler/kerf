//! The measured size estimate held to real full exports (`#[ignore]`d: they drive the real `ffmpeg`, the pinned 9.0.2).
//!
//! `measure_export_size` extrapolates from a few one-second sample encodes, so what has to be shown is that its
//! band holds the *whole* file when the footage changes under it. Each case here is a cut of three
//! sections of different complexity (the soft, plain and grainy pictures of `export_size_rendered`: a still
//! card, an ordinary shot, a noisy one), in each order, exported in full with the same options and compared to
//! what was measured first. Every row prints what the measurement said, how wide, how long it took and how many
//! windows it got through; the table in `.claude/docs/engine-cli.md` is that output.
//!
//! `KERF_FFMPEG=<pinned ffmpeg> KERF_FFPROBE=<pinned ffprobe> cargo test -p kerf-core --no-default-features \
//!  -- --ignored export_size_measure_rendered --test-threads=1`

#![allow(clippy::print_stderr)] // what each case measured, for whoever runs the ignored ones

use std::time::{Duration, Instant};

use super::export_size_rendered::{crf_case, sound_source, source, Class, Scratch};
use super::*;
use crate::engine::test_support::{audio_track, make_clip, timeline_of, video_track};

/// One cut of consecutive clips, each of its own asset.
fn cut_of(assets: &[Asset]) -> Timeline {
    let mut at = 0.0;
    let clips = assets
        .iter()
        .map(|a| {
            let clip = make_clip(a.id, 0.0, a.duration, at);
            at += a.duration;
            clip
        })
        .collect();
    timeline_of(vec![video_track(clips)])
}

/// What a case measured against the full export.
struct Row {
    what: String,
    real: u64,
    estimate: SizeEstimate,
    took: Duration,
    windows: usize,
    planned: usize,
}

impl Row {
    fn inside(&self) -> bool {
        self.estimate.low_bytes <= self.real && self.real <= self.estimate.high_bytes
    }

    fn width(&self) -> f64 {
        self.estimate.high_bytes as f64 / self.estimate.low_bytes.max(1) as f64
    }

    fn print(&self) {
        let e = &self.estimate;
        eprintln!(
            "{:<52} real {:>10} in {:>10} .. {:>10}  x{:>5.2}  {:>5.1} s  {}/{} windows  {:?} {}",
            self.what,
            self.real,
            e.low_bytes,
            e.high_bytes,
            self.width(),
            self.took.as_secs_f64(),
            self.windows,
            self.planned,
            e.basis,
            if self.inside() { "ok" } else { "OUTSIDE" }
        );
    }
}

/// Measure, then export in full, `timeline` with `opts`: the estimate, the file's real size, what the measurement cost.
fn compare(dir: &Path, what: &str, timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Row {
    let (mut windows, mut planned) = (0, 0);
    let started = Instant::now();
    let measured = measure_export_size(
        timeline,
        assets,
        opts,
        &[],
        DEFAULT_MEASURE_BUDGET,
        &mut |p| (windows, planned) = (p.windows_done, p.windows_planned),
        &|| false,
    )
    .expect("measure");
    let took = started.elapsed();
    let estimate = measured.files[0].estimate.clone();
    assert!(
        took < DEFAULT_MEASURE_BUDGET + Duration::from_secs(6),
        "{what}: {took:?} is past the budget and a window's grace"
    );
    let out = dir.join(format!(
        "out-{}.{}",
        what.replace([' ', '/', ':', '(', ')', ','], "_"),
        opts.container.ext()
    ));
    let status = render_with_progress(timeline, assets, &out, opts, &mut |_| {}, &|| false).expect("export");
    assert_eq!(status, RenderStatus::Completed, "{what}");
    let real = std::fs::metadata(&out).expect("a file").len();
    let _ = std::fs::remove_file(&out);
    Row {
        what: what.to_string(),
        real,
        estimate,
        took,
        windows,
        planned,
    }
}

fn finish(rows: &[Row]) {
    for row in rows {
        row.print();
    }
    let outside: Vec<String> = rows
        .iter()
        .filter(|r| !r.inside())
        .map(|r| {
            format!(
                "{}: {} outside {}..{}",
                r.what, r.real, r.estimate.low_bytes, r.estimate.high_bytes
            )
        })
        .collect();
    assert!(
        outside.is_empty(),
        "the measured band missed {} of {} files:\n{}",
        outside.len(),
        rows.len(),
        outside.join("\n")
    );
}

/// The three sections in each of the orders that matter: soft first, noisy first, noisy last.
fn sections(dir: &Path, frame: (u32, u32), secs: f64, sound: bool) -> Vec<(&'static str, Vec<Asset>)> {
    let [soft, plain, grainy] = [Class::Soft, Class::Plain, Class::Grainy].map(|c| source(dir, c, frame, 30.0, secs, sound));
    vec![
        ("soft>plain>grainy", vec![soft.clone(), plain.clone(), grainy.clone()]),
        ("grainy>soft>plain", vec![grainy.clone(), soft.clone(), plain.clone()]),
        ("plain>grainy>soft", vec![plain, grainy, soft]),
    ]
}

/// Constant quality on a minute of footage that changes: every codec the dialog offers, three orders, 360p.
#[test]
#[ignore = "needs the pinned ffmpeg binary"]
fn measured_bands_hold_for_footage_that_changes() {
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
#[ignore = "needs the pinned ffmpeg binary"]
fn measured_bands_hold_for_the_other_encoders() {
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
#[ignore = "needs the pinned ffmpeg binary"]
fn measured_bands_are_narrow_for_even_footage_and_short_cuts() {
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
#[ignore = "needs the pinned ffmpeg binary"]
fn measured_bands_hold_at_1080p_and_say_how_long_they_took() {
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

/// Several shapes: the largest is measured, the others scaled; every file is held to its own real export.
#[test]
#[ignore = "needs the pinned ffmpeg binary"]
fn measured_deliveries_hold_for_each_shape() {
    let scratch = Scratch::new("measured-deliveries");
    let dir = &scratch.0;
    let (_, assets) = sections(dir, (640, 360), 20.0, false).remove(0);
    let tl = cut_of(&assets);
    let opts = crf_case("libx264", Container::Mp4, 23, Some("medium"));
    let shapes = [
        Delivery::new(360, 640, Fit::Cover),
        Delivery::new(360, 360, Fit::Cover),
        Delivery::new(640, 360, Fit::Contain),
    ];
    let started = Instant::now();
    let measured =
        measure_export_size(&tl, &assets, &opts, &shapes, DEFAULT_MEASURE_BUDGET, &mut |_| {}, &|| false).expect("measure");
    let took = started.elapsed();
    let mut rows = Vec::new();
    for (file, shape) in measured.files.iter().zip(shapes) {
        let out = dir.join(format!("shape-{}x{}.mp4", shape.width, shape.height));
        let per = ExportOptions {
            resolution: Some((shape.width, shape.height)),
            fit: shape.fit,
            ..opts.clone()
        };
        let status = render_with_progress(&tl.for_delivery(shape), &assets, &out, &per, &mut |_| {}, &|| false).expect("export");
        assert_eq!(status, RenderStatus::Completed);
        let real = std::fs::metadata(&out).unwrap().len();
        let _ = std::fs::remove_file(&out);
        assert_eq!(file.estimate.basis, SizeBasis::Measured);
        rows.push(Row {
            what: format!(
                "{}x{} {:?} (all three in {:.1} s)",
                shape.width,
                shape.height,
                shape.fit,
                took.as_secs_f64()
            ),
            real,
            estimate: file.estimate.clone(),
            took,
            windows: 0,
            planned: 0,
        });
    }
    // The scaled files say so; the measured one does not.
    let notes: Vec<&str> = measured.files.iter().map(|f| f.estimate.note.as_str()).collect();
    assert_eq!(notes.iter().filter(|n| n.contains("Scaled from")).count(), 2, "{notes:?}");
    assert!(measured.total.is_some());
    finish(&rows);
}

/// Sound alone: the lossless and the quality-driven codecs the model can only bound, over silence, a tone and noise in turn.
#[test]
#[ignore = "needs the pinned ffmpeg binary"]
fn measured_bands_hold_for_sound_alone() {
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
#[ignore = "needs the pinned ffmpeg binary"]
fn a_budget_too_short_for_one_window_is_the_model_in_time() {
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
