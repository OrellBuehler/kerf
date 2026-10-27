//! A 45-megapixel still with a slow keyed zoom and pan, through the real `ffmpeg` at 1080p50 and
//! 2160p50 (`#[ignore]`d: they drive the binary and take a minute or two each).
//!
//! The still is the size a full-frame camera writes (8192x5464 4:4:4 full-range JPEG, ~30 MB with
//! its grain): a light field with a dark box over 40-60% x 42-58% of it. Every output frame of a
//! 3.5 s clip is measured, sub-pixel, against what [`Clip::transform_at`] and
//! [`Clip::crop_window_at`] say for that frame's time: the box's edges are the darkness-weighted
//! centre of each row (column) band plus or minus half its summed darkness, which a resampled box
//! keeps to a hundredth of a pixel and a box that moved by whole pixels does not. Each case is held
//! to **0.25 px** (the worst edge of the worst frame) and its frame-to-frame step to the step the
//! curves make (no plateau, no double step), and to a bound on wall time.
//!
//! Before the still path (`still_motion`), a keyed scale was a per-frame `scale` to whole pixels
//! placed by an `overlay` at whole pixels (1-2 px steps, the centre wobbling as `(W-w)/2` changed
//! parity), and a keyed crop a `perspective` at the source's 45 MP per frame, the JPEG decoded
//! again for every frame (`-loop 1`).
//!
//! `KERF_STILL_MOTION_VERBOSE=1` prints every frame's error.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored still_motion_rendered --test-threads 1`
//! (`KERF_FFMPEG` picks the build).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::*;
use crate::clip_timing::ffmpeg_frame_time;
use crate::engine::test_support::{image_stream, make_clip, test_asset, timeline_of, video_track, StatusBounded};
use crate::model::{Asset, Clip, Easing, Keyframe, PropertyKey};

/// The still: a camera's full frame.
const SW: u32 = 8192;
const SH: u32 = 5464;
/// The box, as whole pixels of the still: `[x0, y0, x1, y1]` (about 40-60% x 42-58%).
const BOX_PX: [u32; 4] = [3277, 2295, 4915, 3169];
/// The clip: 3.5 s at 50 fps.
const SECS: f64 = 3.5;
const FPS: u32 = 50;

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join("kerf-still-motion-rendered");
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Held while a shared input is made: the tests run in parallel, and on Windows a second `rename`
/// onto the file another test has just made (and may be reading) is refused.
static MAKING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The 45 MP still (made once, kept across runs: it takes several seconds).
fn still(dir: &Path) -> Asset {
    let path = dir.join("still-8192x5464-grain.jpg");
    let _making = MAKING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !path.exists() {
        let [x0, y0, x1, y1] = BOX_PX;
        let tmp = dir.join("still-tmp.jpg");
        // Drawn in 4:4:4 (in 4:2:0 `drawbox` moves an odd edge to the even pixel after it), the grain
        // a zero-mean `geq` (`noise` moves the box's left edge by a pixel).
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!(
                "color=c=0xC8C8C8:s={SW}x{SH}:d=1,format=yuv444p,\
                 drawbox=x={x0}:y={y0}:w={}:h={}:color=0x282828:t=fill,\
                 geq=lum='lum(X,Y)+48*(random(0)-0.5)':cb='cb(X,Y)+24*(random(1)-0.5)':cr='cr(X,Y)+24*(random(2)-0.5)',\
                 format=yuvj444p",
                x1 - x0,
                y1 - y0
            ))
            .args(["-frames:v", "1", "-q:v", "2"])
            .arg(&tmp)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success(), "made the still");
        std::fs::rename(&tmp, &path).unwrap();
    }
    let mut stream = image_stream(SW, SH);
    stream.codec = "mjpeg".into();
    stream.pix_fmt = Some("yuvj444p".into());
    stream.color_space = Some("bt470bg".into());
    stream.color_range = Some("pc".into());
    let mut asset = test_asset(vec![stream]);
    asset.path = path.to_string_lossy().into_owned();
    asset.name = "still-8192x5464-grain.jpg".into();
    asset.duration = 0.04;
    asset
}

fn key(time: f64, scale: f64, pos_x: f64, pos_y: f64) -> Keyframe {
    Keyframe {
        time,
        scale,
        pos_x,
        pos_y,
        rotation: 0.0,
        opacity: 1.0,
        easing: Easing::EaseInOut,
    }
}

fn eased(time: f64, value: f64) -> PropertyKey {
    PropertyKey {
        time,
        value,
        easing: Easing::EaseInOut,
    }
}

/// What moves.
#[derive(Clone, Copy, Debug)]
enum Motion {
    /// The whole transform keyed: a slow zoom in with a pan (the bundle's keys).
    ScalePan,
    /// A held zoom with a pan.
    HeldPan,
    /// The animated crop: a zoom in with a pan across.
    Crop,
    /// The animated crop zooming out, panning down.
    CropOut,
}

fn clip(asset: &Asset, motion: Motion) -> Clip {
    let mut c = make_clip(asset.id, 0.0, SECS, 0.0);
    match motion {
        Motion::ScalePan => c.keyframes = vec![key(0.0, 1.10, 0.02, 0.0), key(SECS, 1.17, -0.02, 0.0)],
        Motion::HeldPan => c.keyframes = vec![key(0.0, 1.12, -0.025, 0.0), key(SECS, 1.12, 0.025, 0.0)],
        Motion::Crop => {
            c.set_property_keys(Property::CropZoom, vec![eased(0.0, 1.06), eased(SECS, 1.14)]);
            c.set_property_keys(Property::CropX, vec![eased(0.0, -0.4), eased(SECS, 0.4)]);
        }
        Motion::CropOut => {
            c.set_property_keys(Property::CropZoom, vec![eased(0.0, 1.18), eased(SECS, 1.05)]);
            c.set_property_keys(Property::CropY, vec![eased(0.0, 0.4), eased(SECS, -0.4)]);
        }
    }
    c
}

fn options(w: u32, h: u32) -> ExportOptions {
    ExportOptions {
        resolution: Some((w, h)),
        fps: Some(f64::from(FPS)),
        fit: Fit::Cover,
        ..ExportOptions::default()
    }
}

/// The box's edges on the frame `[left, top, right, bottom]` (pixel edges) at `local`: the
/// picture covers the frame (fitted, then zoomed by the transform's scale round the centre it is
/// moved to) and shows the window of the still the animated crop keeps.
fn expected(c: &Clip, local: f64, fw: f64, fh: f64) -> [f64; 4] {
    let t = c.transform_at(local);
    let (wl, wt, ws, _) = c.crop_window_at(local);
    let aspect = f64::from(SW) / f64::from(SH);
    let (pw, ph) = (fw.max(fh * aspect) * t.scale, fh.max(fw / aspect) * t.scale);
    let x = |u: f64| fw / 2.0 + t.pos_x * fw + ((u - wl) / ws - 0.5) * pw;
    let y = |v: f64| fh / 2.0 + t.pos_y * fh + ((v - wt) / ws - 0.5) * ph;
    let [x0, y0, x1, y1] = BOX_PX.map(f64::from);
    [
        x(x0 / f64::from(SW)),
        y(y0 / f64::from(SH)),
        x(x1 / f64::from(SW)),
        y(y1 / f64::from(SH)),
    ]
}

/// The box's edges measured on a luma plane, around where `near` says it is: across the middle 60%
/// of it, the mean profile through each pair of edges, its levels read on the profile itself (the
/// field at both ends, the box in the middle), the edges the profile's darkness-weighted centre plus
/// or minus half its summed darkness.
fn measure(y: &[u8], w: usize, h: usize, near: [f64; 4]) -> [f64; 4] {
    let at = |x: usize, r: usize| f64::from(y[r * w + x]);
    let margin = w / 60;
    let edges = |profile: &[f64], from: usize| {
        let ends = profile[..margin / 2].iter().chain(&profile[profile.len() - margin / 2..]);
        let light = ends.sum::<f64>() / (2 * (margin / 2)) as f64;
        let inside = &profile[2 * margin..profile.len() - 2 * margin];
        let dark = inside.iter().sum::<f64>() / inside.len() as f64;
        let d: Vec<f64> = profile.iter().map(|p| (light - p) / (light - dark)).collect();
        let sum: f64 = d.iter().sum();
        let centre = d.iter().enumerate().map(|(i, v)| v * ((from + i) as f64 + 0.5)).sum::<f64>() / sum;
        (centre - sum / 2.0, centre + sum / 2.0)
    };
    let span = |a: f64, b: f64, n: usize| (((a as usize).saturating_sub(margin)), ((b as usize) + margin).min(n));
    let band = |a: f64, b: f64| ((a + 0.2 * (b - a)) as usize, (b - 0.2 * (b - a)) as usize);
    let (x0, x1) = span(near[0], near[2], w);
    let (r0, r1) = band(near[1], near[3]);
    let across: Vec<f64> = (x0..x1)
        .map(|x| (r0..r1).map(|r| at(x, r)).sum::<f64>() / (r1 - r0) as f64)
        .collect();
    let (left, right) = edges(&across, x0);
    let (y0, y1) = span(near[1], near[3], h);
    let (c0, c1) = band(near[0], near[2]);
    let down: Vec<f64> = (y0..y1)
        .map(|r| (c0..c1).map(|x| at(x, r)).sum::<f64>() / (c1 - c0) as f64)
        .collect();
    let (top, bottom) = edges(&down, y0);
    [left, top, right, bottom]
}

/// What a run of a case came to.
#[derive(Debug, Default)]
struct Report {
    frames: usize,
    secs: f64,
    peak_mb: Option<f64>,
    /// The worst edge error of any frame, output pixels.
    error: f64,
    /// The worst difference between a frame-to-frame step and the step the curves make.
    step: f64,
}

/// The peak resident memory of a running process, where the platform says (Linux).
fn peak_mb(pid: u32) -> Option<f64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024.0)
}

/// Render the export graph of `motion` at `w`x`h` and measure every frame as it comes out.
#[allow(clippy::print_stderr)]
fn run(dir: &Path, asset: &Asset, motion: Motion, w: u32, h: u32) -> Report {
    let c = clip(asset, motion);
    let timeline = timeline_of(vec![video_track(vec![c.clone()])]);
    let mut args = build_export_args(&timeline, std::slice::from_ref(asset), "unused.mkv", &options(w, h)).unwrap();
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
            "gray",
            "pipe:1",
        ]
        .map(String::from),
    );
    let tag = format!("{motion:?}-{h}");
    std::fs::write(dir.join(format!("{tag}.args")), args.join(" ")).unwrap();
    let _script = externalize_filter_complex(&mut args, &tag).unwrap();
    let started = Instant::now();
    let mut child = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error"])
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let peak = std::sync::Arc::new(std::sync::Mutex::new(None::<f64>));
    let watch = {
        let peak = peak.clone();
        std::thread::spawn(move || {
            while let Some(mb) = peak_mb(pid) {
                *peak.lock().unwrap() = Some(mb);
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };
    let mut stdout = child.stdout.take().unwrap();
    let size = (w * h) as usize;
    let mut frame = vec![0u8; size];
    let mut report = Report::default();
    let mut last: Option<([f64; 4], [f64; 4])> = None;
    let verbose = std::env::var_os("KERF_STILL_MOTION_VERBOSE").is_some();
    while stdout.read_exact(&mut frame).is_ok() {
        let local = ffmpeg_frame_time(report.frames as u64, FPS, 1);
        let want = expected(&c, local, f64::from(w), f64::from(h));
        let got = measure(&frame, w as usize, h as usize, want);
        let error = (0..4).map(|i| (got[i] - want[i]).abs()).fold(0.0, f64::max);
        if let Some((pg, pw)) = last {
            let step = (0..4)
                .map(|i| ((got[i] - pg[i]) - (want[i] - pw[i])).abs())
                .fold(0.0, f64::max);
            report.step = report.step.max(step);
        }
        if verbose {
            eprintln!(
                "{tag} frame {:3} t={local:.3} want {want:.2?} got {got:.2?} error {error:.3}",
                report.frames
            );
        }
        report.error = report.error.max(error);
        last = Some((got, want));
        report.frames += 1;
    }
    let mut err = String::new();
    let _ = child.stderr.take().unwrap().read_to_string(&mut err);
    let status = child.wait().unwrap();
    report.secs = started.elapsed().as_secs_f64();
    let _ = watch.join();
    report.peak_mb = *peak.lock().unwrap();
    assert!(status.success(), "{tag}: {err}");
    report
}

#[allow(clippy::print_stderr)]
fn check(motion: Motion, w: u32, h: u32, budget: Duration) {
    let dir = scratch();
    let asset = still(&dir);
    let r = run(&dir, &asset, motion, w, h);
    eprintln!(
        "{motion:?} {w}x{h}@{FPS} on {}: {} frames in {:.1} s ({:.0} ms a frame), peak {:?} MB, \
         worst edge error {:.3} px, worst step error {:.3} px",
        ffmpeg_version(),
        r.frames,
        r.secs,
        1000.0 * r.secs / r.frames.max(1) as f64,
        r.peak_mb.map(f64::round),
        r.error,
        r.step
    );
    assert_eq!(r.frames, (SECS * f64::from(FPS)).round() as usize, "every frame of the clip");
    assert!(r.error < 0.25, "{motion:?} {w}x{h}: edges off by {:.3} px", r.error);
    assert!(r.step < 0.25, "{motion:?} {w}x{h}: a step off by {:.3} px", r.step);
    assert!(r.secs < budget.as_secs_f64(), "{motion:?} {w}x{h}: {:.1} s", r.secs);
}

fn ffmpeg_version() -> String {
    command(&ffmpeg_bin())
        .arg("-version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().next().map(str::to_string))
        .unwrap_or_default()
}

#[test]
#[ignore = "needs the ffmpeg binary; a minute or two"]
fn a_keyed_zoom_and_pan_on_a_45mp_still_is_smooth_at_1080p50() {
    check(Motion::ScalePan, 1920, 1080, Duration::from_secs(60));
    check(Motion::HeldPan, 1920, 1080, Duration::from_secs(60));
}

#[test]
#[ignore = "needs the ffmpeg binary; a minute or two"]
fn a_keyed_crop_on_a_45mp_still_is_smooth_at_1080p50() {
    check(Motion::Crop, 1920, 1080, Duration::from_secs(60));
    check(Motion::CropOut, 1920, 1080, Duration::from_secs(60));
}

#[test]
#[ignore = "needs the ffmpeg binary; a minute or two"]
fn a_keyed_zoom_and_pan_on_a_45mp_still_is_smooth_and_quick_at_2160p50() {
    check(Motion::ScalePan, 3840, 2160, Duration::from_secs(90));
}

#[test]
#[ignore = "needs the ffmpeg binary; a minute or two"]
fn a_keyed_crop_on_a_45mp_still_is_smooth_and_quick_at_2160p50() {
    check(Motion::Crop, 3840, 2160, Duration::from_secs(90));
}

/// `get_frame` and `skim_asset` on the still: any `-ss` on a JPEG (the `image2` demuxer), 0
/// included, made ffmpeg exit 0 having decoded nothing, which read as "could not extract frame at
/// 0.000s:" with no reason.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_45mp_jpeg_still_gives_a_frame_and_a_contact_sheet() {
    let asset = still(&scratch());
    let frame = crate::project::Project::decode_preview_frame(&asset, 0.0, 1280, 5, true).expect("a frame");
    assert!(frame.starts_with(&[0xFF, 0xD8]), "a JPEG");
    let (sheet, times) = crate::project::Project::decode_contact_sheet(&asset, None, None, 4, 3, 320, 5).expect("a sheet");
    assert!(sheet.starts_with(&[0xFF, 0xD8]) && times.len() == 12, "a JPEG of 12 cells");
}

/// A Canon-like 4K DCI 10-bit 4:2:2 HEVC clip at 100 fps with sound (`secs` long, made once).
fn canon_like(dir: &Path, secs: u32) -> Asset {
    let path = dir.join(format!("canon-like-{secs}.mp4"));
    let _making = MAKING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !path.exists() {
        let tmp = dir.join("canon-like-tmp.mp4");
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=s=4096x2160:r=100:d={secs}"))
            .args(["-f", "lavfi", "-i"])
            .arg(format!("sine=f=440:d={secs}:r=48000"))
            .args([
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-profile:v",
                "main422-10",
                "-pix_fmt",
                "yuv422p10le",
            ])
            .args([
                "-x265-params",
                "log-level=error:keyint=100",
                "-b:v",
                "100M",
                "-c:a",
                "aac",
                "-b:a",
                "256k",
                "-ac",
                "2",
            ])
            .arg(&tmp)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success(), "made the footage");
        std::fs::rename(&tmp, &path).unwrap();
    }
    let mut video = crate::engine::test_support::video_stream(4096, 2160, 100.0);
    video.codec = "hevc".into();
    video.pix_fmt = Some("yuv422p10le".into());
    video.color_space = Some("bt709".into());
    let mut asset = test_asset(vec![video, crate::engine::test_support::audio_stream(48_000, 2)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.name = "canon-like.mp4".into();
    asset.duration = f64::from(secs);
    asset
}

/// The user's cut in small: footage at half speed, a still with a keyed crop, footage, a still with a
/// keyed zoom and pan, 1 s crossfades between them, a fade in and out, graded, delivered at
/// 3840x2160@50 (Cover) with x264 CRF 18. It exports, with every frame, in bounded time.
#[test]
#[ignore = "needs the ffmpeg binary with libx265; a few minutes"]
#[allow(clippy::print_stderr)]
fn a_cut_of_4k_footage_and_moving_45mp_stills_exports_at_2160p50() {
    use crate::model::{Color, Transition, TransitionKind};
    let dir = scratch();
    let photo = still(&dir);
    let footage = canon_like(&dir, 4);
    let graded = |c: &mut Clip| {
        c.color = Color {
            brightness: 0.02,
            contrast: 1.22,
            saturation: 1.5,
            gamma: 1.2,
            temperature: 0.03,
        };
    };
    let fade = |c: &mut Clip| {
        c.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
    };
    let mut clips = Vec::new();
    let mut first = make_clip(footage.id, 0.0, 1.5, 0.0);
    first.speed = 0.5;
    first.fade_in = 1.5;
    graded(&mut first);
    clips.push(first);
    let mut crop = clip(&photo, Motion::Crop);
    crop.timeline_start = 3.0;
    fade(&mut crop);
    clips.push(crop);
    let mut second = make_clip(footage.id, 2.0, 3.5, 6.5);
    second.speed = 0.5;
    graded(&mut second);
    fade(&mut second);
    clips.push(second);
    let mut zoom = clip(&photo, Motion::ScalePan);
    zoom.timeline_start = 9.5;
    zoom.fade_out = 2.0;
    graded(&mut zoom);
    fade(&mut zoom);
    clips.push(zoom);
    let timeline = timeline_of(vec![video_track(clips)]);
    let out = dir.join("cut-2160p50.mp4");
    let opts = ExportOptions {
        crf: Some(18),
        audio_bitrate: Some("256k".into()),
        faststart: true,
        ..options(3840, 2160)
    };
    let assets = [photo, footage];
    assert!(
        build_export_args(&timeline, &assets, "x.mp4", &opts)
            .unwrap()
            .join(" ")
            .contains(":interpolation=linear:eval=frame,crop=3840:2160:0:0"),
        "the stills are drawn by `still_motion`"
    );
    let started = Instant::now();
    let status = render_with_progress(&timeline, &assets, &out, &opts, &mut |_| {}, &|| false).expect("exported");
    let took = started.elapsed();
    assert_eq!(status, RenderStatus::Completed);
    let frames = command(&ffprobe_bin())
        .args([
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_read_frames,width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(&out)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&frames.stdout).trim().to_string();
    eprintln!(
        "13 s cut at 3840x2160@50 on {}: {:.1} s, {said}",
        ffmpeg_version(),
        took.as_secs_f64()
    );
    assert_eq!(said, "3840,2160,650", "every frame of the cut");
    assert!(took < Duration::from_secs(600), "{took:?}");
}
