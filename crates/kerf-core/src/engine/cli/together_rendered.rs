//! Every delivery frame in one run, through the real `ffmpeg` (`#[ignore]`d: they drive the
//! binary, on 6.1 and 9.0 alike).
//!
//! The one run has to write the files the variants rendered one after another write. Both are
//! rendered losslessly (x264 `-crf 0`, FLAC in Matroska) from a cut of two clips with sound, a
//! clip framed for 9:16 and an animated title (each frame's own picture inputs, after the
//! shared ones), and every decoded frame and audio packet is compared through `framemd5`. A
//! cancelled run removes every file it touched and leaves the ones it never did.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored together_rendered`

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::*;
use crate::engine::test_support::{audio_stream, make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded};
use crate::model::{Clip, Framing, SvgOverlay};
use crate::svg::raster::with_pinned_dir;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-together-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// `secs` of a moving test picture at 320x180 / 30 fps with a tone at `hz`, lossless.
fn source(dir: &Path, name: &str, pattern: &str, hz: u32, secs: f64) -> Asset {
    let path = dir.join(name);
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("{pattern}=s=320x180:r=30:d={secs}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=f={hz}:sample_rate=48000:d={secs}"))
        .args([
            "-c:v",
            "libx264",
            "-crf",
            "0",
            "-g",
            "15",
            "-pix_fmt",
            "yuv420p",
            "-ac",
            "2",
            "-c:a",
            "flac",
            "-shortest",
        ])
        .arg(&path)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());
    let mut asset = test_asset(vec![video_stream(320, 180, 30.0), audio_stream(48_000, 2)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = secs;
    asset
}

/// Two clips with sound, the first framed for 9:16, under a title that slides in.
fn cut(dir: &Path) -> (Timeline, Vec<Asset>) {
    let a = source(dir, "a.mkv", "testsrc2", 440, 3.0);
    let b = source(dir, "b.mkv", "smptebars", 660, 3.0);
    let mut first = make_clip(a.id, 0.5, 2.5, 0.0);
    first.framings.push(Framing {
        aspect_w: 9,
        aspect_h: 16,
        crop_left: 0.1,
        crop_right: 0.5,
        crop_top: 0.0,
        crop_bottom: 0.0,
    });
    let second = make_clip(b.id, 1.0, 2.5, 2.0);
    let mut timeline = timeline_of(vec![video_track(vec![first, second])]);
    timeline.svg_overlays.push(SvgOverlay {
        id: uuid::Uuid::new_v4(),
        name: String::new(),
        svg: "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 36\"><rect y=\"12\" width=\"12\" height=\"12\" \
              fill=\"#ff2040\"><animate attributeName=\"x\" from=\"0\" to=\"52\" dur=\"2s\" fill=\"freeze\"/></rect></svg>"
            .into(),
        start: 0.5,
        end: 3.0,
        x: 0.1,
        y: 0.1,
        width: 0.5,
        height: 0.5,
        opacity: 1.0,
        offset: 0.0,
        preset: None,
        template: None,
    });
    (timeline, vec![a, b])
}

fn variants(base: &Path) -> Vec<ExportVariant> {
    vec![
        ExportVariant::beside(base, Delivery::new(90, 160, Fit::Cover)),
        ExportVariant::beside(base, Delivery::new(160, 160, Fit::Cover)),
        ExportVariant::beside(base, Delivery::new(192, 108, Fit::Contain)),
    ]
}

fn lossless() -> ExportOptions {
    ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        audio_codec: Some("flac".into()),
        ..ExportOptions::default()
    }
}

/// Every decoded frame and audio packet of `path`, hashed (`framemd5`).
fn frames(path: &Path) -> String {
    let run = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-map", "0", "-f", "framemd5", "-"])
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    String::from_utf8_lossy(&run.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
#[ignore = "drives the ffmpeg binary"]
fn one_run_writes_the_files_the_variants_rendered_one_by_one_write() {
    let _gate = crate::engine::cpu::test_lock();
    let dir = scratch("same");
    let (timeline, assets) = cut(&dir);
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        for (tag, opts) in [
            ("whole", lossless()),
            (
                "range",
                ExportOptions {
                    range: Some(crate::model::TimeRange { start: 1.0, end: 3.2 }),
                    ..lossless()
                },
            ),
        ] {
            let one_by_one = variants(&dir.join(format!("{tag}-apart.mkv")));
            let together = variants(&dir.join(format!("{tag}-together.mkv")));
            let mut seen = false;
            let VariantsRun {
                status, completed: done, ..
            } = render_variants(
                &timeline,
                &assets,
                &one_by_one,
                &opts,
                false,
                &mut |p| seen |= p.together,
                &|| false,
            )
            .expect("one by one");
            assert_eq!((status, done, seen), (RenderStatus::Completed, 3, false));
            let VariantsRun {
                status,
                completed: done,
                together: one_run,
            } = render_variants(
                &timeline,
                &assets,
                &together,
                &opts,
                true,
                &mut |p| seen |= p.together,
                &|| false,
            )
            .expect("together");
            assert_eq!(
                (status, done, seen, one_run),
                (RenderStatus::Completed, 3, true, true),
                "{tag}"
            );
            for (a, b) in one_by_one.iter().zip(&together) {
                let (apart, joint) = (frames(&a.output), frames(&b.output));
                assert!(apart.lines().count() > 60, "{tag}: {apart}");
                assert!(
                    apart.lines().any(|l| l.starts_with("1,")),
                    "{tag}: no sound in {}",
                    a.output.display()
                );
                assert_eq!(
                    apart,
                    joint,
                    "{tag}: {} and {} differ",
                    a.output.display(),
                    b.output.display()
                );
            }
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// A keyed 360 reframe: each frame's `v360` is commanded by its own `sendcmd`, so every file pans
/// as it does rendered alone (one shared instance name left all but the first at the static pose).
#[test]
#[ignore = "drives the ffmpeg binary"]
fn a_keyed_reframe_pans_in_every_file() {
    let _gate = crate::engine::cpu::test_lock();
    let dir = scratch("reframe");
    let path = dir.join("sphere.mkv");
    let made = command(&ffmpeg_bin())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=512x256:r=30:d=2",
        ])
        .args(["-c:v", "libx264", "-crf", "0", "-g", "15", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());
    let mut stream = video_stream(512, 256, 30.0);
    stream.projection = Some(crate::model::Projection::Equirect);
    let mut asset = test_asset(vec![stream]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 2.0;
    let mut clip = Clip::for_asset(&asset, 0.0, 2.0, 0.0);
    let key = |time, yaw| crate::model::ReframeKeyframe {
        time,
        yaw,
        pitch: 0.0,
        roll: 0.0,
        fov: 90.0,
    };
    clip.reframe.as_mut().expect("a 360 clip reframes").keyframes = vec![key(0.0, 0.0), key(2.0, 120.0)];
    let timeline = timeline_of(vec![video_track(vec![clip])]);
    let assets = vec![asset];
    let one_by_one = variants(&dir.join("apart.mkv"));
    let together = variants(&dir.join("together.mkv"));
    let opts = lossless();
    render_variants(&timeline, &assets, &one_by_one, &opts, false, &mut |_| {}, &|| false).expect("one by one");
    let mut seen = false;
    render_variants(
        &timeline,
        &assets,
        &together,
        &opts,
        true,
        &mut |p| seen |= p.together,
        &|| false,
    )
    .expect("together");
    assert!(seen, "rendered in one run");
    for (a, b) in one_by_one.iter().zip(&together) {
        let (apart, joint) = (frames(&a.output), frames(&b.output));
        assert!(apart.lines().count() >= 60, "{apart}");
        assert_eq!(apart, joint, "{} and {} differ", a.output.display(), b.output.display());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "drives the ffmpeg binary"]
fn a_cancelled_run_removes_every_file_it_touched_and_spares_the_rest() {
    let _gate = crate::engine::cpu::test_lock();
    let dir = scratch("cancel");
    let (timeline, assets) = cut(&dir);
    let base = dir.join("cut.mp4");
    let variants = variants(&base);
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        // Once ffmpeg has opened every output, a Stop takes all of them: none was finished.
        let going = std::cell::Cell::new(false);
        let VariantsRun {
            status, completed: done, ..
        } = render_variants(
            &timeline,
            &assets,
            &variants,
            &ExportOptions::default(),
            true,
            &mut |p| going.set(going.get() | (p.together && p.fraction > 0.0)),
            &|| going.get(),
        )
        .expect("cancel is a status");
        assert_eq!((status, done), (RenderStatus::Cancelled, 0));
        for v in &variants {
            assert!(!v.output.exists(), "{} was left behind", v.output.display());
        }

        // Stopped while queued behind another job: ffmpeg never ran, and the files already at those
        // paths are not this run's.
        for v in &variants {
            std::fs::write(&v.output, b"earlier").unwrap();
        }
        let (holding, release) = (AtomicBool::new(false), AtomicBool::new(false));
        let waiting = AtomicBool::new(false);
        let VariantsRun { status, .. } = std::thread::scope(|scope| {
            scope.spawn(|| {
                let _slot = crate::engine::cpu::lease_waiting(&mut |_| {}, &|| false).expect("the slot");
                holding.store(true, Ordering::SeqCst);
                while !release.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
            while !holding.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let run = render_variants(
                &timeline,
                &assets,
                &variants,
                &ExportOptions::default(),
                true,
                &mut |p| waiting.store(waiting.load(Ordering::SeqCst) | p.waiting.is_some(), Ordering::SeqCst),
                &|| waiting.load(Ordering::SeqCst),
            );
            release.store(true, Ordering::SeqCst);
            run
        })
        .expect("cancel is a status");
        assert_eq!(status, RenderStatus::Cancelled);
        for v in &variants {
            assert_eq!(std::fs::read(&v.output).unwrap(), b"earlier", "{}", v.output.display());
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
}
