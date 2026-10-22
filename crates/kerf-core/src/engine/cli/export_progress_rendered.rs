//! An export's progress through the real `ffmpeg` (`#[ignore]`d: they drive the binary).
//!
//! The reported symptom was a range export (`range {120, 151.5}`, mp4, x264 slow CRF 17, AAC 320k,
//! faststart, a loudness target) whose `job_status` read progress 0.0 / "rendering" for most of two
//! minutes while the output file stayed empty. Measured on FFmpeg 4.4.2: ffmpeg prints **no**
//! `-progress` block while it opens the footage and decodes up to the range's start (the first one
//! comes with the first packet), and a static loudness normalization meters the mix before that;
//! both used to be 0.0 with no words (the MCP `export` dropped the one it had). Here the cut is a
//! range starting deep into a source with a single keyframe, so ffmpeg has to decode from the head
//! of the file to reach it.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored export_progress_rendered`

use std::path::{Path, PathBuf};

use super::*;
use crate::engine::test_support::{audio_stream, make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded};
use crate::model::TimeRange;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-progress-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// `secs` of a moving test picture with a tone, one keyframe for the whole of it.
fn long_gop_source(dir: &Path, secs: f64) -> Asset {
    let path = dir.join("long-gop.mp4");
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=s=640x360:r=30:d={secs}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=f=440:sample_rate=48000:d={secs}"))
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "99999",
            "-keyint_min",
            "99999",
            "-x264-params",
            "scenecut=0",
            "-pix_fmt",
            "yuv420p",
            "-ac",
            "2",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(&path)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());
    let mut asset = test_asset(vec![video_stream(640, 360, 30.0), audio_stream(48_000, 2)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = secs;
    asset
}

fn reported(tl: &Timeline, asset: &Asset, out: &Path, opts: &ExportOptions) -> Vec<ExportProgress> {
    let mut seen = Vec::new();
    let status =
        render_with_progress(tl, std::slice::from_ref(asset), out, opts, &mut |p| seen.push(p), &|| false).expect("export");
    assert_eq!(status, RenderStatus::Completed);
    seen
}

/// The user's export, scaled down: a range not at 0, x264 slow CRF 17, AAC 320k, faststart, a
/// static loudness target. The metering pass walks the mix, ffmpeg says it is starting before it
/// has written anything, the render rises to the end of the bar, and the bar never goes back.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_range_export_reads_as_metering_then_starting_then_rendering() {
    let dir = scratch("range");
    let asset = long_gop_source(&dir, 60.0);
    let tl = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, 60.0, 0.0)])]);
    let opts = ExportOptions {
        container: Container::Mp4,
        video_codec: Some("libx264".into()),
        crf: Some(17),
        preset: Some("slow".into()),
        audio_codec: Some("aac".into()),
        audio_bitrate: Some("320k".into()),
        faststart: true,
        loudness: Some(LoudnessPreset::Youtube),
        loudness_static: true,
        range: Some(TimeRange { start: 40.0, end: 52.0 }),
        ..ExportOptions::default()
    };
    let out = dir.join("range.mp4");
    let seen = reported(&tl, &asset, &out, &opts);

    let working: Vec<&ExportProgress> = seen.iter().filter(|p| p.waiting.is_none()).collect();
    let mut order: Vec<ExportStage> = working.iter().filter_map(|p| p.stage).collect();
    order.dedup();
    // Metering walks the mix, then ffmpeg starts, then it renders; the muxer's close may be
    // told last. Never the other way round.
    assert_eq!(
        &order[..3],
        [ExportStage::Metering, ExportStage::Starting, ExportStage::Rendering],
        "{order:?}"
    );
    assert!(order[3..].iter().all(|s| *s == ExportStage::Finishing), "{order:?}");

    let fractions: Vec<f64> = seen.iter().map(|p| p.fraction).collect();
    assert!(
        fractions.windows(2).all(|w| w[0] <= w[1] + 1e-9),
        "the bar went backwards: {fractions:?}"
    );
    // The metering pass stays inside its share of the bar; the render starts after it.
    let share = metering_share(true, true);
    assert!(working
        .iter()
        .filter(|p| p.stage == Some(ExportStage::Metering))
        .all(|p| p.fraction <= share + 1e-9));
    assert!(working
        .iter()
        .filter(|p| p.stage != Some(ExportStage::Metering))
        .all(|p| p.fraction >= share - 1e-9));
    // Nothing reaches the end of the bar before ffmpeg has exited; the render did get near it.
    let last = *fractions.last().unwrap();
    assert!(last < 1.0 && last > 0.9, "{last}");
    // Elapsed time is the whole export's: it never restarts at the render.
    let elapsed: Vec<f64> = seen.iter().map(|p| p.elapsed_secs).collect();
    assert!(elapsed.windows(2).all(|w| w[0] <= w[1] + 1e-9), "{elapsed:?}");
    assert!(out.metadata().unwrap().len() > 0);
}

/// A cut with nothing to meter: the bar is the render's alone, from the first tick, which says
/// ffmpeg is starting.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn an_export_that_meters_nothing_starts_its_bar_at_the_render() {
    let dir = scratch("plain");
    let asset = long_gop_source(&dir, 6.0);
    let tl = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, 6.0, 0.0)])]);
    let opts = ExportOptions {
        video_codec: Some("libx264".into()),
        preset: Some("ultrafast".into()),
        ..ExportOptions::default()
    };
    let seen = reported(&tl, &asset, &dir.join("plain.mp4"), &opts);
    assert!(seen.iter().all(|p| p.stage != Some(ExportStage::Metering)), "{seen:?}");
    // (Another test's ffmpeg may hold the slot for a moment: that is a `waiting` tick first.)
    let working: Vec<&ExportProgress> = seen.iter().filter(|p| p.waiting.is_none()).collect();
    assert_eq!(working[0].stage, Some(ExportStage::Starting));
    assert_eq!(working[0].fraction, 0.0);
    // (An export this short can be over inside one half-second block: the last block is told too.)
    assert!(working.iter().any(|p| p.fraction > 0.0), "{working:?}");
    assert!(working.iter().all(|p| p.fraction < 1.0), "{working:?}");
}

/// The export measured: the read-back is the last of the same job's bar, ends it at 1.0 and hands
/// back what the file turned out to be.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_measured_export_ends_on_the_read_back_and_reports_the_file() {
    let dir = scratch("measured");
    let asset = long_gop_source(&dir, 8.0);
    let tl = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, 8.0, 0.0)])]);
    let opts = ExportOptions {
        video_codec: Some("libx264".into()),
        preset: Some("ultrafast".into()),
        audio_codec: Some("aac".into()),
        loudness: Some(LoudnessPreset::Youtube),
        loudness_static: true,
        range: Some(TimeRange { start: 2.0, end: 7.0 }),
        ..ExportOptions::default()
    };
    let out = dir.join("measured.mp4");
    let mut seen = Vec::new();
    let (status, report) = render_measured(&tl, std::slice::from_ref(&asset), &out, &opts, &mut |p| seen.push(p), &|| {
        false
    })
    .expect("export");
    assert_eq!(status, RenderStatus::Completed);
    let report = report.expect("a completed export is measured");
    assert_eq!(report.problem, None, "{report:?}");
    assert_eq!(report.path, out.to_string_lossy());
    assert!(
        (report.duration_secs.unwrap() - 5.0).abs() < 0.2,
        "{:?}",
        report.duration_secs
    );
    assert!(report.video.is_some() && report.audio.is_some());
    let integrated = report.loudness.unwrap().integrated_lufs.unwrap();
    assert!((integrated + 14.0).abs() < 1.0, "{integrated}");

    let mut order: Vec<ExportStage> = seen.iter().filter_map(|p| p.stage).collect();
    order.dedup();
    assert_eq!(order.last(), Some(&ExportStage::Measuring), "{order:?}");
    let fractions: Vec<f64> = seen.iter().map(|p| p.fraction).collect();
    assert!(fractions.windows(2).all(|w| w[0] <= w[1] + 1e-9), "{fractions:?}");
    assert!((fractions.last().unwrap() - 1.0).abs() < 1e-9, "{fractions:?}");
    // The read-back owns the end of the bar and nothing before it.
    assert!(seen
        .iter()
        .filter(|p| p.stage == Some(ExportStage::Measuring))
        .all(|p| p.fraction >= 1.0 - VERIFY_SHARE - 1e-9));
    assert!(seen
        .iter()
        .filter(|p| p.stage != Some(ExportStage::Measuring))
        .all(|p| p.fraction <= 1.0 - VERIFY_SHARE + 1e-9));
}

/// Every file of a multi-format export is read back, at its own frame, in the order asked.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn measured_variants_report_each_file_at_its_own_frame() {
    let dir = scratch("variants");
    let asset = long_gop_source(&dir, 4.0);
    let tl = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, 4.0, 0.0)])]);
    let opts = ExportOptions {
        video_codec: Some("libx264".into()),
        preset: Some("ultrafast".into()),
        audio_codec: Some("aac".into()),
        ..ExportOptions::default()
    };
    let base = dir.join("cut.mp4");
    let variants = vec![
        ExportVariant::beside(&base, Delivery::new(90, 160, Fit::Cover)),
        ExportVariant::beside(&base, Delivery::new(160, 160, Fit::Cover)),
    ];
    for together in [true, false] {
        let mut seen: Vec<VariantProgress> = Vec::new();
        let (run, reports) = render_variants_measured(
            &tl,
            std::slice::from_ref(&asset),
            &variants,
            &opts,
            together,
            &mut |p| seen.push(p),
            &|| false,
        )
        .expect("export");
        assert_eq!(run.status, RenderStatus::Completed);
        assert_eq!(run.together, together);
        let frames: Vec<(Option<u32>, Option<u32>)> = reports
            .iter()
            .map(|r| r.video.as_ref().map(|v| (v.width, v.height)).unwrap_or_default())
            .collect();
        assert_eq!(frames, [(Some(90), Some(160)), (Some(160), Some(160))], "together={together}");
        assert!(reports.iter().all(|r| r.problem.is_none() && r.loudness.is_some()));
        assert_eq!(reports[1].path, variants[1].output.to_string_lossy());
        let fractions: Vec<f64> = seen.iter().map(|p| p.fraction).collect();
        assert!(
            fractions.windows(2).all(|w| w[0] <= w[1] + 1e-9),
            "together={together}: {fractions:?}"
        );
        assert!((fractions.last().unwrap() - 1.0).abs() < 1e-9, "{fractions:?}");
        assert_eq!(seen.last().and_then(|p| p.stage), Some(ExportStage::Measuring));
    }
}
