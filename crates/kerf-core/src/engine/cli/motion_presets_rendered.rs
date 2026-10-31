//! Motion presets (B8a) through the real `ffmpeg` (`#[ignore]`d).
//!
//! A preset only writes keys, so the graph is the channels' own — but a string-level test cannot say
//! that FFmpeg accepts the eased, overshooting, tiny-scale tracks a preset makes, or that the export
//! and the scrubbed still agree on them. For every preset, on a clip that is not at the identity
//! (smaller, off-centre, a little turned and see-through), this renders the export of the first
//! seconds frame by frame and holds every fifth one to the still of the same moment: the whole
//! picture, not a pixel, since a slide leaves the centre pixel on an edge. And it checks that the
//! picture moved: a preset that drew nothing would agree with the still perfectly.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored motion_presets_rendered`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::*;
use crate::clip_timing::ffmpeg_frame_time;
use crate::engine::test_support::{make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded};
use crate::model::{Asset, MotionMode, MotionPreset, Property, Transform};

const CW: u32 = 96;
const CH: u32 = 54;
const FPS: u32 = 30;
const SECONDS: f64 = 2.0;

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-motion-presets-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn picture(dir: &Path) -> Asset {
    let path = dir.join("bars.mp4");
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!(
            "color=c=0x4060a0:s={CW}x{CH}:r={FPS}:d={},drawbox=x=24:y=12:w=48:h=30:color=0xe0c040:t=fill",
            SECONDS + 1.0
        ))
        .args(["-c:v", "libx264", "-crf", "0", "-g", "1", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success(), "make the picture");
    let mut asset = test_asset(vec![video_stream(CW, CH, f64::from(FPS))]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = SECONDS + 1.0;
    asset
}

fn options() -> ExportOptions {
    ExportOptions {
        resolution: Some((CW, CH)),
        fps: Some(f64::from(FPS)),
        ..ExportOptions::default()
    }
}

fn export_frames(timeline: &Timeline, assets: &[Asset], dir: &Path, tag: &str) -> Vec<Vec<u8>> {
    let out = dir.join(format!("{tag}.rgb"));
    let mut args = build_export_args(timeline, assets, "unused.mkv", &options()).unwrap();
    let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
    args.truncate(graph + 1);
    args.extend(
        [
            "-map",
            "[outv]",
            fps_mode_flag(),
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-y",
        ]
        .map(String::from),
    );
    args.push(out.to_string_lossy().into_owned());
    // The export spills a long graph to a script file; a preset's eased keys outgrow Windows' 32 K command line.
    let _script = externalize_filter_complex(&mut args, tag).unwrap();
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::read(&out)
        .unwrap()
        .chunks((CW * CH * 3) as usize)
        .map(<[u8]>::to_vec)
        .collect()
}

fn still_frame(timeline: &Timeline, assets: &[Asset], t: f64) -> Vec<u8> {
    let args = build_still_args(timeline, assets, &options(), t, CW, None, &StillOutput::RgbPipe).unwrap();
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout.len(), (CW * CH * 3) as usize, "a still is one frame");
    run.stdout
}

/// The mean absolute difference of two frames, in levels per channel.
fn apart(a: &[u8], b: &[u8]) -> f64 {
    a.iter().zip(b).map(|(x, y)| f64::from(x.abs_diff(*y))).sum::<f64>() / a.len() as f64
}

#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn every_motion_preset_renders_and_the_export_agrees_with_the_still() {
    let dir = scratch();
    let source = picture(&dir);
    let assets = vec![source.clone()];
    for preset in MotionPreset::ALL {
        let mut clip = make_clip(source.id, 0.0, SECONDS, 0.0);
        clip.transform = Transform {
            scale: 0.8,
            pos_x: 0.05,
            pos_y: -0.04,
            rotation: 8.0,
            opacity: 0.9,
            ..Transform::default()
        };
        clip.apply_motion(preset, MotionMode::Add, None, Some(1.0), None).unwrap();
        let (window_start, window_end) = clip.motion_window(preset, None, Some(1.0)).unwrap();
        let timeline = timeline_of(vec![video_track(vec![clip.clone()])]);
        let frames = export_frames(&timeline, &assets, &dir, preset.as_str());
        assert!(
            frames.len() >= (SECONDS * f64::from(FPS)) as usize - 1,
            "{}: {} frames",
            preset.as_str(),
            frames.len()
        );
        // A turned picture is drawn by the export and the still with different resampling whatever made
        // the key (a plain keyed rotation sits 8 to 13 levels apart on this picture), so it gets that room.
        let tolerance = if preset.properties().contains(&Property::Rotation) {
            14.0
        } else {
            6.0
        };
        let (mut worst, mut compared) = (0.0f64, 0);
        for (k, frame) in frames.iter().enumerate().filter(|(k, _)| k % 5 == 0) {
            let t = ffmpeg_frame_time(k as u64, FPS, 1);
            if t >= SECONDS {
                continue;
            }
            let off = apart(frame, &still_frame(&timeline, &assets, t));
            worst = worst.max(off);
            compared += 1;
            assert!(
                off <= tolerance,
                "{}: frame {k} (t = {t:.3}) is {off:.2} levels from the still of that moment",
                preset.as_str()
            );
        }
        assert!(compared >= 10, "{}: only {compared} frames compared", preset.as_str());
        let rest = &frames[frames.len() - 1];
        let first = &frames[0];
        let in_window = frames
            .iter()
            .enumerate()
            .filter(|(k, _)| (window_start..=window_end).contains(&ffmpeg_frame_time(*k as u64, FPS, 1)))
            .map(|(_, f)| apart(f, first).max(apart(f, rest)))
            .fold(0.0, f64::max);
        assert!(
            in_window > 1.0,
            "{}: the window shows nothing moving ({in_window:.2} levels)",
            preset.as_str()
        );
        eprintln!(
            "{}: at worst {worst:.2} levels from the still, {in_window:.1} levels of motion",
            preset.as_str()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
