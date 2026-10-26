//! Stabilization rendered by the real `ffmpeg` (`#[ignore]`d). The source is a still pattern shaken by
//! a moving crop, so a steady picture is one whose middle no longer changes from frame to frame.
//!
//! * The corrections Kerf computes are vid.stab's: its own two passes (`smoothing`, gaussian path) and
//!   Kerf's measurement handed to `vidstabtransform` as absolute corrections give the same frames, from
//!   the first frame and from the middle (a preview that starts at the playhead).
//! * The export steadies the clip forward, reversed and at speed (the correction is per source frame);
//!   `deshake` steadies it where vid.stab is missing.
//! * The still is the export's frame, and a proxy at half the size is moved by half as much.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored stabilize_rendered`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build; the vid.stab cases skip a build without it).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::*;
use crate::engine::test_support::{make_clip, single, test_asset, video_stream, StatusBounded};
use crate::model::Stabilize;
use crate::stabilize::{corrections, transforms_text, MotionAnalysis};

const W: u32 = 560;
const H: u32 = 300;
const SECONDS: f64 = 4.0;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-stabilize-rendered-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A still pattern shaken by a crop that moves a few pixels each frame, 560x300 at 30 fps.
fn shaky(dir: &Path) -> Asset {
    let pattern = dir.join("pattern.png");
    let made = command(&ffmpeg_bin())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=s=640x360:d=1",
        ])
        .args(["-frames:v", "1"])
        .arg(&pattern)
        .status_bounded()
        .unwrap();
    assert!(made.success());
    let path = dir.join("shaky.mp4");
    let made = command(&ffmpeg_bin())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-loop",
            "1",
            "-framerate",
            "30",
            "-i",
        ])
        .arg(&pattern)
        .args(["-t", &format!("{SECONDS}"), "-vf"])
        .arg(format!(
            "crop={W}:{H}:40+8*sin(n*0.7)+3*sin(n*1.9):30+6*cos(n*0.8)+2*cos(n*2.3)"
        ))
        .args(["-c:v", "libx264", "-crf", "8", "-g", "15", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status_bounded()
        .unwrap();
    assert!(made.success());
    let mut asset = test_asset(vec![video_stream(W, H, 30.0)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = SECONDS;
    asset
}

/// Run ffmpeg with `args` (in `dir`), its picture as gray frames of `w` x `h`.
fn gray(args: &[String], dir: &Path, w: u32, h: u32) -> Vec<Vec<u8>> {
    let run = command(&ffmpeg_bin())
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    let n = (w * h) as usize;
    assert!(run.stdout.len() >= n, "{} bytes", run.stdout.len());
    run.stdout.chunks_exact(n).map(<[u8]>::to_vec).collect()
}

fn vf_gray(input: &[&str], vf: &str) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error"].map(String::from).to_vec();
    args.extend(input.iter().map(|s| s.to_string()));
    args.extend(["-vf", vf, "-f", "rawvideo", "-pix_fmt", "gray", "-"].map(String::from));
    args
}

/// The largest and the mean difference of two frames, in levels.
fn diff(a: &[u8], b: &[u8]) -> (u8, f64) {
    let max = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    let sum: u64 = a.iter().zip(b).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    (max, sum as f64 / a.len() as f64)
}

/// How much the middle of the picture changes from frame to frame (mean levels): a shaky still
/// pattern changes a lot, a steady one hardly.
fn jitter(frames: &[Vec<u8>], w: u32, h: u32) -> f64 {
    let middle = |f: &Vec<u8>| -> Vec<u8> {
        (h / 4..h * 3 / 4)
            .flat_map(|y| (w / 4..w * 3 / 4).map(move |x| (y * w + x) as usize))
            .map(|i| f[i])
            .collect()
    };
    let m: Vec<Vec<u8>> = frames.iter().map(middle).collect();
    let steps: Vec<f64> = m.windows(2).skip(2).map(|p| diff(&p[0], &p[1]).1).collect();
    steps.iter().sum::<f64>() / steps.len() as f64
}

fn measure(asset: &Asset) -> MotionAnalysis {
    measure_motion(Path::new(&asset.path), 0.0, SECONDS, 0.0, &mut |_| {}, &|| false).expect("measure the camera")
}

fn has_vidstab() -> bool {
    let ok = vidstab_available();
    if !ok {
        eprintln!("skipped: this ffmpeg has no vid.stab");
    }
    ok
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn kerfs_corrections_move_every_frame_as_vid_stabs_own_two_passes_do() {
    if !has_vidstab() {
        return;
    }
    let dir = scratch("parity");
    let asset = shaky(&dir);
    let analysis = measure(&asset);
    assert_eq!((analysis.width, analysis.height), (W, H));
    assert_eq!(analysis.times.len(), (SECONDS * 30.0) as usize);
    let radius = analysis.radius(0.5);
    let zoom = 6.0;
    // vid.stab's own: detect, then smooth and move.
    let src = asset.path.as_str();
    let detect = vf_gray(&["-i", src], "vidstabdetect=result=ref.trf:shakiness=5:accuracy=15");
    gray(&detect, &dir, W, H);
    let theirs = gray(
        &vf_gray(
            &["-i", src],
            &format!("vidstabtransform=input=ref.trf:smoothing={radius}:optalgo=gauss:optzoom=0:zoom={zoom}:crop=black:interpol=bicubic"),
        ),
        &dir,
        W,
        H,
    );
    // Kerf's: the absolute corrections, from the first frame and from frame 45 on.
    let fixes = corrections(&analysis.motions, radius);
    std::fs::write(dir.join("ours.trf"), transforms_text(&fixes, 1.0, 1.0)).unwrap();
    std::fs::write(dir.join("ours45.trf"), transforms_text(&fixes[45..], 1.0, 1.0)).unwrap();
    let ours_vf = |file: &str| {
        format!("vidstabtransform=input={file}:relative=0:smoothing=0:optzoom=0:zoom={zoom}:crop=black:interpol=bicubic")
    };
    let ours = gray(&vf_gray(&["-i", src], &ours_vf("ours.trf")), &dir, W, H);
    let seek = format!("{}", analysis.between(45));
    let ours45 = gray(&vf_gray(&["-ss", &seek, "-i", src], &ours_vf("ours45.trf")), &dir, W, H);
    assert_eq!(theirs.len(), ours.len());
    assert_eq!(ours45.len(), ours.len() - 45);
    // The corrections are printed to a millionth of a pixel, so a pixel on the edge of the black the
    // frame moved away from may fall on the other side of it: a handful of pixels, never the picture.
    let alike = |a: &[u8], b: &[u8]| {
        let off = a.iter().zip(b).filter(|(x, y)| x.abs_diff(**y) > 2).count();
        (off, diff(a, b).1)
    };
    let few = (W * H / 1000) as usize;
    for (k, (a, b)) in theirs.iter().zip(&ours).enumerate() {
        let (off, mean) = alike(a, b);
        assert!(off <= few && mean < 0.05, "frame {k}: {off} pixels off, {mean:.3} on average");
    }
    for (k, (a, b)) in theirs[45..].iter().zip(&ours45).enumerate() {
        let (off, mean) = alike(a, b);
        assert!(
            off <= few && mean < 0.05,
            "frame {} from the middle: {off} pixels off, {mean:.3} on average",
            k + 45
        );
    }
    // And it is a correction: the frames are not the shaky ones.
    let shaky = gray(&vf_gray(&["-i", src], "null"), &dir, W, H);
    assert!(jitter(&ours, W, H) < jitter(&shaky, W, H) * 0.5);
    let _ = std::fs::remove_dir_all(&dir);
}

fn options() -> ExportOptions {
    ExportOptions {
        resolution: Some((W, H)),
        fps: Some(30.0),
        ..ExportOptions::default()
    }
}

/// The export's picture of `timeline` as gray frames.
fn exported(timeline: &Timeline, assets: &[Asset], dir: &Path) -> Vec<Vec<u8>> {
    let mut args = build_export_args(timeline, assets, "unused.mkv", &options()).unwrap();
    let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
    args.truncate(graph + 1);
    args.extend(["-map", "[outv]", "-f", "rawvideo", "-pix_fmt", "gray", "-"].map(String::from));
    gray(&args, dir, W, H)
}

/// Keep `analysis` as `asset`'s, as an analysis job would, for the closure; forgotten after.
fn with_analysis<R>(asset: &Asset, analysis: &MotionAnalysis, f: impl FnOnce() -> R) -> R {
    struct Forget(uuid::Uuid);
    impl Drop for Forget {
        fn drop(&mut self) {
            crate::stabilize::forget(self.0);
        }
    }
    let _forget = Forget(asset.id);
    crate::stabilize::store(asset.id, (0.0, SECONDS), analysis).unwrap();
    f()
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_stabilized_export_holds_the_shaken_picture_still_forward_reversed_and_at_speed() {
    if !has_vidstab() {
        return;
    }
    let dir = scratch("export");
    let mut asset = shaky(&dir);
    asset.id = uuid::Uuid::new_v4();
    let analysis = measure(&asset);
    let assets = std::slice::from_ref(&asset);
    let steady = Some(Stabilize {
        strength: 1.0,
        zoom: None,
    });
    with_analysis(&asset, &analysis, || {
        for (name, speed) in [("forward", 1.0), ("reversed", -1.0), ("double", 2.0)] {
            let mut clip = make_clip(asset.id, 0.3, 3.7, 0.0);
            clip.speed = speed;
            let shaken = exported(&single(vec![clip.clone()]), assets, &dir);
            clip.stabilize = steady;
            let tl = single(vec![clip]);
            let args = build_export_args(&tl, assets, "unused.mkv", &options()).unwrap().join(" ");
            assert!(args.contains("vidstabtransform=input='"), "{name}: {args}");
            let steadied = exported(&tl, assets, &dir);
            assert_eq!(steadied.len(), shaken.len(), "{name}: the same frames");
            let (before, after) = (jitter(&shaken, W, H), jitter(&steadied, W, H));
            assert!(
                after < before * 0.4,
                "{name}: the middle changes {after:.2} a frame, {before:.2} shaken"
            );
        }
    });
    // Without its analysis the clip plays as shot: the graph is the unstabilized one but for the seek.
    let mut clip = make_clip(asset.id, 0.3, 3.7, 0.0);
    clip.stabilize = steady;
    let args = build_export_args(&single(vec![clip]), assets, "unused.mkv", &options())
        .unwrap()
        .join(" ");
    assert!(!args.contains("vidstab"), "{args}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn deshake_steadies_the_export_where_vid_stab_is_missing() {
    let dir = scratch("deshake");
    let asset = shaky(&dir);
    let assets = std::slice::from_ref(&asset);
    let mut clip = make_clip(asset.id, 0.0, SECONDS, 0.0);
    let shaken = exported(&single(vec![clip.clone()]), assets, &dir);
    clip.stabilize = Some(Stabilize {
        strength: 1.0,
        zoom: None,
    });
    let tl = single(vec![clip]);
    with_vidstab(false, || {
        let args = build_export_args(&tl, assets, "unused.mkv", &options()).unwrap().join(" ");
        assert!(args.contains("deshake=rx="), "{args}");
        let steadied = exported(&tl, assets, &dir);
        assert_eq!(steadied.len(), shaken.len());
        let (before, after) = (jitter(&shaken, W, H), jitter(&steadied, W, H));
        assert!(
            after < before * 0.8,
            "deshake: the middle changes {after:.2} a frame, {before:.2} shaken"
        );
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// The still of `timeline` at `t`, as gray (the luma of its RGB).
fn still(timeline: &Timeline, assets: &[Asset], t: f64, width: u32) -> Vec<u8> {
    let args = build_still_args(timeline, assets, &options(), t, width, None, &StillOutput::RgbPipe).unwrap();
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    run.stdout
        .chunks_exact(3)
        .map(|p| ((u32::from(p[0]) * 77 + u32::from(p[1]) * 150 + u32::from(p[2]) * 29) >> 8) as u8)
        .collect()
}

/// The middle half of a `w` x `h` gray frame.
fn middle(f: &[u8], w: u32, h: u32) -> Vec<u8> {
    (h / 4..h * 3 / 4)
        .flat_map(|y| (w / 4..w * 3 / 4).map(move |x| (y * w + x) as usize))
        .map(|i| f[i])
        .collect()
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn the_still_is_the_exports_frame_and_a_half_size_proxy_is_moved_by_half() {
    if !has_vidstab() {
        return;
    }
    let dir = scratch("still");
    let mut asset = shaky(&dir);
    asset.id = uuid::Uuid::new_v4();
    let analysis = measure(&asset);
    // A proxy at half the size, where a proxy lives (the graph knows one by its name).
    let proxies = dir.join("kerf").join("proxies");
    std::fs::create_dir_all(&proxies).unwrap();
    let proxy_path = proxies.join("0123456789abcdef.mp4");
    let made = command(&ffmpeg_bin())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            &asset.path,
            "-vf",
            "scale=280:150",
        ])
        .args(["-c:v", "libx264", "-crf", "8", "-g", "1", "-pix_fmt", "yuv420p"])
        .arg(&proxy_path)
        .status_bounded()
        .unwrap();
    assert!(made.success());
    let mut proxy = asset.clone();
    proxy.path = proxy_path.to_string_lossy().into_owned();
    let mut clip = make_clip(asset.id, 0.0, SECONDS, 0.0);
    clip.stabilize = Some(Stabilize {
        strength: 1.0,
        zoom: None,
    });
    let tl = single(vec![clip.clone()]);
    let plain = single(vec![make_clip(asset.id, 0.0, SECONDS, 0.0)]);
    with_analysis(&asset, &analysis, || {
        let frames = exported(&tl, std::slice::from_ref(&asset), &dir);
        for k in [10usize, 47, 90] {
            let t = k as f64 / 30.0;
            let s = still(&tl, std::slice::from_ref(&asset), t, W);
            let (_, mean) = diff(&middle(&s, W, H), &middle(&frames[k], W, H));
            assert!(mean < 2.0, "frame {k}: the still is {mean:.2} off the export's frame");
            // The proxy's still, against the export's frame at its size: closer than the shaken one.
            let small = |f: &[u8]| -> Vec<u8> {
                (0..150u32)
                    .flat_map(|y| (0..280u32).map(move |x| (y, x)))
                    .map(|(y, x)| {
                        let at = |dy: u32, dx: u32| u32::from(f[((2 * y + dy) * W + 2 * x + dx) as usize]);
                        ((at(0, 0) + at(0, 1) + at(1, 0) + at(1, 1)) / 4) as u8
                    })
                    .collect()
            };
            let want = middle(&small(&frames[k]), 280, 150);
            let steady = middle(&still(&tl, std::slice::from_ref(&proxy), t, 280), 280, 150);
            let shaken = middle(&still(&plain, std::slice::from_ref(&proxy), t, 280), 280, 150);
            let (_, steady_off) = diff(&steady, &want);
            let (_, shaken_off) = diff(&shaken, &want);
            assert!(
                steady_off < 6.0 && steady_off < shaken_off * 0.6,
                "frame {k}: the proxy's still is {steady_off:.2} off the export's frame, the shaken one {shaken_off:.2}"
            );
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn an_analysis_job_measures_keeps_and_is_found_again() {
    if !has_vidstab() {
        return;
    }
    let dir = scratch("job");
    let mut asset = shaky(&dir);
    asset.id = uuid::Uuid::new_v4();
    let job = crate::project::StabilizeJob {
        clip_id: uuid::Uuid::new_v4(),
        asset_id: asset.id,
        path: PathBuf::from(&asset.path),
        range: (0.0, SECONDS),
        window: (0.5, 3.5),
        lead: 0.0,
    };
    let mut ticks = Vec::new();
    let measured = crate::project::Project::analyze_stabilization(&job, &mut |f| ticks.push(f), &|| false).unwrap();
    assert!(measured, "it measured");
    assert!(ticks.last() == Some(&1.0), "{ticks:?}");
    let found = crate::stabilize::analysis_for(asset.id, Some(Path::new(&asset.path)), 0.5, 3.5).expect("found again");
    assert_eq!(found.times.len(), (SECONDS * 30.0) as usize);
    // A second run has nothing to do; a stopped one keeps nothing.
    assert!(!crate::project::Project::analyze_stabilization(&job, &mut |_| {}, &|| false).unwrap());
    crate::stabilize::forget(asset.id);
    let stopped = crate::project::Project::analyze_stabilization(&job, &mut |_| {}, &|| true);
    assert!(matches!(stopped, Err(Error::Cancelled)), "{stopped:?}");
    assert!(crate::stabilize::analysis_for(asset.id, Some(Path::new(&asset.path)), 0.5, 3.5).is_none());
    crate::stabilize::forget(asset.id);
    let _ = std::fs::remove_dir_all(&dir);
}
