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
