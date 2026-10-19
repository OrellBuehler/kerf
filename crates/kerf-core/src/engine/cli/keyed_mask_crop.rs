//! A keyed mask and a keyed crop (B5b-2), pinned against rendered pictures (`#[ignore]`d: they
//! drive the real `ffmpeg`).
//!
//! A string-level test cannot say that `geq` reads the keyed numbers at the time of the frame it
//! is given, that a window placed by the pan lands where [`Clip::crop_window_at`] says, or that
//! the keyed `geq` and a static one agree. These render the export graph and hold every output
//! frame to the model:
//!
//! * **mask** — a white clip over black, its mask keyed (centre, size, feather, to a hard edge and
//!   back, an ellipse inverted, beside a keyed opacity, at speed 2 and reversed, late on the
//!   timeline, cut by a range export). Every fifth output frame is held to the scrubbed still of
//!   the same moment, which draws [`Clip::mask_at`] through the *static* mask `geq`: two ways to
//!   the same alpha.
//! * **crop** — a white box on black, the crop's zoom and pan keyed (alone, together, eased, on
//!   top of a static crop, at speed 2, cut by a range export). The box's centre on every output
//!   frame is where the window of that moment puts it, and on every fifth it is where the still
//!   of that moment draws it (a static `crop` and a scale: another way to the same window).
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored keyed_mask_crop`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::*;
use crate::clip_timing::ffmpeg_frame_time;
use crate::engine::test_support::{make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded};
use crate::model::{Asset, Clip, Easing, Mask, MaskShape, Property, PropertyKey, TimeRange};

const W: u32 = 160;
const H: u32 = 90;
/// The box the crop cases look at, in source pixels.
const BOX: (u32, u32, u32) = (96, 30, 16);

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-keyed-mask-crop-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn ffmpeg(args: &[&str]) {
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success(), "ffmpeg {args:?}");
}

/// A lossless 8 s clip at 30 fps of `picture` (a lavfi source).
fn source(dir: &Path, name: &str, picture: &str) -> Asset {
    let path = dir.join(format!("{name}.mp4"));
    if !path.exists() {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            picture,
            "-c:v",
            "libx264",
            "-crf",
            "0",
            "-g",
            "1",
            "-pix_fmt",
            "yuv420p",
            &path.to_string_lossy(),
        ]);
    }
    let mut asset = test_asset(vec![video_stream(W, H, 30.0)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 8.0;
    asset
}

/// One frame of `testsrc2` held for the whole clip: every pixel different from its neighbours,
/// and the same picture whichever source frame a pick lands on.
fn detailed(dir: &Path) -> Asset {
    std::fs::create_dir_all(dir.join("detail")).expect("temp dir");
    source(
        dir,
        "detail",
        &format!("testsrc2=s={W}x{H}:r=30:d=1,select=eq(n\\,0),loop=loop=299:size=1,setpts=N/(30*TB)"),
    )
}

fn white(dir: &Path) -> Asset {
    source(dir, "white", &format!("color=c=white:s={W}x{H}:r=30:d=8"))
}

fn boxed(dir: &Path) -> Asset {
    let (x, y, s) = BOX;
    source(
        dir,
        "box",
        &format!("color=c=black:s={W}x{H}:r=30:d=8,drawbox=x={x}:y={y}:w={s}:h={s}:color=white:t=fill"),
    )
}

fn options() -> ExportOptions {
    ExportOptions {
        resolution: Some((W, H)),
        fps: Some(30.0),
        ..ExportOptions::default()
    }
}

/// The export graph of `timeline`, one rgb24 frame per output frame.
fn export_frames(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, dir: &Path, tag: &str) -> Vec<Vec<u8>> {
    let out = dir.join(format!("{tag}.rgb"));
    let mut args = build_export_args(timeline, assets, "unused.mkv", opts).unwrap();
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
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::read(&out)
        .unwrap()
        .chunks((W * H * 3) as usize)
        .map(<[u8]>::to_vec)
        .collect()
}

/// The scrubbed still of `timeline` at `t`.
fn still_frame(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, t: f64) -> Vec<u8> {
    let opts = ExportOptions {
        range: None,
        ..opts.clone()
    };
    let args = build_still_args(timeline, assets, &opts, t, W, None, &StillOutput::RgbPipe).unwrap();
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout.len(), (W * H * 3) as usize, "a still is one frame");
    run.stdout
}

fn luma(frame: &[u8]) -> Vec<i32> {
    frame
        .chunks(3)
        .map(|p| (i32::from(p[0]) + i32::from(p[1]) + i32::from(p[2])) / 3)
        .collect()
}

/// The centre of the bright part of a frame, in output pixels (`None` when nothing is bright).
fn bright_centre(frame: &[u8]) -> Option<(f64, f64)> {
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
    for (i, v) in luma(frame).into_iter().enumerate() {
        if v > 128 {
            let w = f64::from(v);
            sx += w * (i as u32 % W) as f64;
            sy += w * (i as u32 / W) as f64;
            n += w;
        }
    }
    (n > 0.0).then(|| (sx / n, sy / n))
}

fn key(time: f64, value: f64, easing: Easing) -> PropertyKey {
    PropertyKey { time, value, easing }
}

/// The output frames of `timeline` that show `clip`, with their timeline time.
fn frames_of<'a>(frames: &'a [Vec<u8>], clip: &Clip, offset: f64) -> Vec<(usize, f64, &'a Vec<u8>)> {
    frames
        .iter()
        .enumerate()
        .map(|(k, f)| (k, ffmpeg_frame_time(k as u64, 30, 1) + offset, f))
        .filter(|(_, t, _)| *t >= clip.timeline_start && *t < clip.timeline_end())
        .collect()
}

struct MaskCase {
    name: &'static str,
    decorate: fn(&mut Clip),
    range: Option<(f64, f64)>,
    start: f64,
}

fn mask_case(name: &'static str, decorate: fn(&mut Clip)) -> MaskCase {
    MaskCase {
        name,
        decorate,
        range: None,
        start: 0.0,
    }
}

fn moving_mask(c: &mut Clip) {
    c.mask = Some(Mask::default());
    c.set_property_keys(
        Property::MaskX,
        vec![key(0.0, 0.25, Easing::EaseInOut), key(2.0, 0.75, Easing::Linear)],
    );
    c.set_property_keys(
        Property::MaskWidth,
        vec![
            key(0.0, 0.2, Easing::Linear),
            key(1.0, 0.6, Easing::Hold),
            key(2.0, 0.3, Easing::Linear),
        ],
    );
}

fn check_mask(dir: &Path, case: &MaskCase) {
    let src = white(dir);
    let mut probe = Clip::new(src.id, 0.0, 1.0, 0.0);
    (case.decorate)(&mut probe);
    let shown = 2.5;
    let mut clip = make_clip(src.id, 1.0, 1.0 + shown * probe.speed_mag(), case.start);
    (case.decorate)(&mut clip);
    let timeline = timeline_of(vec![video_track(vec![clip.clone()])]);
    let assets = vec![src];
    let mut opts = options();
    opts.range = case.range.map(|(start, end)| TimeRange { start, end });
    let tag = format!("mask-{}", case.name.replace([' ', ','], "-"));
    let frames = export_frames(&timeline, &assets, &opts, dir, &tag);
    let shown = frames_of(&frames, &clip, case.range.map_or(0.0, |r| r.0));
    let (mut compared, mut xs) = (0, Vec::new());
    for (k, t, frame) in &shown {
        if let Some((x, _)) = bright_centre(frame) {
            xs.push(x);
        }
        if k % 5 != 0 {
            continue;
        }
        let want = luma(&still_frame(&timeline, &assets, &opts, *t));
        let got = luma(frame);
        let worst = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).max().unwrap();
        let mean = got.iter().zip(&want).map(|(a, b)| f64::from((a - b).abs())).sum::<f64>() / got.len() as f64;
        assert!(
            worst <= 4 && mean <= 0.4,
            "{}: frame {k} (t = {t:.4}) is {worst} levels (mean {mean:.3}) off the still of that moment",
            case.name
        );
        compared += 1;
    }
    let spread = xs.iter().copied().fold(f64::MIN, f64::max) - xs.iter().copied().fold(f64::MAX, f64::min);
    assert!(
        spread > 10.0,
        "{}: the mask only moved {spread:.1} px: the keys did nothing",
        case.name
    );
    assert!(compared >= 6, "{}: only {compared} frames compared", case.name);
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_keyed_mask_is_the_still_of_the_same_moment_on_every_output_frame() {
    let dir = scratch("mask");
    let cases = [
        mask_case("centre and width", moving_mask),
        mask_case("feather to a hard edge and back", |c| {
            moving_mask(c);
            c.set_property_keys(
                Property::MaskFeather,
                vec![
                    key(0.0, 0.5, Easing::Linear),
                    key(1.0, 0.0, Easing::Hold),
                    key(1.6, 0.0, Easing::Linear),
                    key(2.4, 0.3, Easing::Linear),
                ],
            );
        }),
        mask_case("an inverted ellipse, its height and y", |c| {
            moving_mask(c);
            if let Some(m) = c.mask.as_mut() {
                m.shape = MaskShape::Ellipse;
                m.inverted = true;
            }
            c.set_property_keys(
                Property::MaskY,
                vec![key(0.0, 0.3, Easing::Linear), key(2.0, 0.7, Easing::EaseOut)],
            );
            c.set_property_keys(
                Property::MaskHeight,
                vec![key(0.5, 0.8, Easing::Linear), key(2.0, 0.2, Easing::Linear)],
            );
        }),
        mask_case("beside a keyed opacity", |c| {
            moving_mask(c);
            c.set_property_keys(
                Property::Opacity,
                vec![key(0.0, 1.0, Easing::Linear), key(2.5, 0.6, Easing::Linear)],
            );
        }),
        mask_case("double speed", |c| {
            moving_mask(c);
            c.speed = 2.0;
        }),
        mask_case("reversed", |c| {
            moving_mask(c);
            c.speed = -1.0;
        }),
        MaskCase {
            start: 1.25,
            ..mask_case("late on the timeline", moving_mask)
        },
        MaskCase {
            range: Some((0.4, 2.2)),
            ..mask_case("cut by a range export", moving_mask)
        },
    ];
    for case in &cases {
        check_mask(&dir, case);
    }
}

/// Where the box's centre lands for a crop window `(l, t, w, h)` of the picture.
fn box_in_window((l, t, w, h): (f64, f64, f64, f64)) -> (f64, f64) {
    let (bx, by, s) = BOX;
    let cx = (f64::from(bx) + f64::from(s) / 2.0) / f64::from(W);
    let cy = (f64::from(by) + f64::from(s) / 2.0) / f64::from(H);
    (((cx - l) / w) * f64::from(W) - 0.5, ((cy - t) / h) * f64::from(H) - 0.5)
}

struct CropCase {
    name: &'static str,
    decorate: fn(&mut Clip),
    range: Option<(f64, f64)>,
    start: f64,
}

fn crop_case(name: &'static str, decorate: fn(&mut Clip)) -> CropCase {
    CropCase {
        name,
        decorate,
        range: None,
        start: 0.0,
    }
}

fn zoom_into_the_box(c: &mut Clip) {
    c.set_property_keys(
        Property::CropZoom,
        vec![key(0.0, 1.0, Easing::EaseInOut), key(2.0, 2.0, Easing::Linear)],
    );
    c.set_property_keys(Property::CropX, vec![key(0.0, 0.4, Easing::Linear)]);
    c.set_property_keys(Property::CropY, vec![key(0.0, -0.3, Easing::Linear)]);
}

/// The clip of a crop case on `src`.
fn crop_clip(src: &Asset, case: &CropCase) -> Clip {
    let mut probe = Clip::new(src.id, 0.0, 1.0, 0.0);
    (case.decorate)(&mut probe);
    let mut clip = make_clip(src.id, 1.0, 1.0 + 2.5 * probe.speed_mag(), case.start);
    (case.decorate)(&mut clip);
    clip
}

/// The window the export shows at `local`: the keyed one inside what the static crop leaves.
fn window_at(clip: &Clip, local: f64) -> (f64, f64, f64, f64) {
    let t = clip.transform_at(local);
    let (cw, ch) = (1.0 - t.crop_left - t.crop_right, 1.0 - t.crop_top - t.crop_bottom);
    let (l, top, w, h) = clip.crop_window_at(local);
    (t.crop_left + cw * l, t.crop_top + ch * top, cw * w, ch * h)
}

/// The peak signal-to-noise ratio of two rgb24 frames, in dB, and their largest difference.
fn psnr(a: &[u8], b: &[u8]) -> (f64, i32) {
    let (mut sum, mut worst) = (0.0, 0);
    for (x, y) in a.iter().zip(b) {
        let d = i32::from(*x) - i32::from(*y);
        sum += f64::from(d * d);
        worst = worst.max(d.abs());
    }
    let mse = sum / a.len() as f64;
    (if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0 * 255.0 / mse).log10() }, worst)
}

fn check_crop(dir: &Path, case: &CropCase) {
    let tag = format!("crop-{}", case.name.replace([' ', ','], "-"));
    let mut opts = options();
    opts.range = case.range.map(|(start, end)| TimeRange { start, end });
    let offset = case.range.map_or(0.0, |r| r.0);
    // Where: a white box on black, its centre on every output frame where the window of that
    // moment puts it.
    let src = boxed(dir);
    let clip = crop_clip(&src, case);
    let timeline = timeline_of(vec![video_track(vec![clip.clone()])]);
    let assets = vec![src];
    let frames = export_frames(&timeline, &assets, &opts, dir, &tag);
    let mut xs = Vec::new();
    for (k, t, frame) in frames_of(&frames, &clip, offset) {
        let want = box_in_window(window_at(&clip, t - clip.timeline_start));
        let got = bright_centre(frame).unwrap_or_else(|| panic!("{}: frame {k} lost the box", case.name));
        xs.push(got.0);
        let off = (got.0 - want.0).abs().max((got.1 - want.1).abs());
        assert!(
            off <= 1.0,
            "{}: frame {k} (t = {t:.4}) draws the box at {got:?}, the window of that moment puts it at {want:?}",
            case.name
        );
    }
    let spread = xs.iter().copied().fold(f64::MIN, f64::max) - xs.iter().copied().fold(f64::MAX, f64::min);
    assert!(spread > 5.0, "{}: the box only moved {spread:.1} px: the keys did nothing", case.name);
    // What: a detailed picture (one frame of `testsrc2`, held), every fifth output frame against
    // the still of the same moment, which reads the window through the same `perspective`.
    let src = detailed(dir);
    let clip = crop_clip(&src, case);
    let timeline = timeline_of(vec![video_track(vec![clip.clone()])]);
    let assets = vec![src];
    let frames = export_frames(&timeline, &assets, &opts, &dir.join("detail"), &tag);
    let mut compared = 0;
    for (k, t, frame) in frames_of(&frames, &clip, offset) {
        if k % 5 != 0 {
            continue;
        }
        let still = still_frame(&timeline, &assets, &opts, t);
        let (db, worst) = psnr(frame, &still);
        assert!(
            db >= CROP_PSNR && worst <= CROP_WORST,
            "{}: frame {k} (t = {t:.4}) is {db:.1} dB (worst {worst} levels) off the still of that moment",
            case.name
        );
        compared += 1;
    }
    assert!(compared >= 6, "{}: only {compared} frames compared", case.name);
}

/// How close the export's keyed crop and the still of the same moment have to be on a detailed
/// picture: the same `perspective`, the same window to the last bits of a double.
const CROP_PSNR: f64 = 50.0;
const CROP_WORST: i32 = 8;

#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_keyed_crop_puts_the_picture_where_its_window_says_on_every_output_frame() {
    let dir = scratch("crop");
    let cases = [
        crop_case("zoom into the box", zoom_into_the_box),
        crop_case("pan alone at a held zoom", |c| {
            c.set_property_keys(Property::CropZoom, vec![key(0.0, 1.6, Easing::Linear)]);
            c.set_property_keys(
                Property::CropX,
                vec![key(0.0, -0.3, Easing::EaseOut), key(2.4, 1.0, Easing::Linear)],
            );
        }),
        crop_case("on top of a static crop", |c| {
            zoom_into_the_box(c);
            c.transform.crop_left = 0.2;
            c.transform.crop_top = 0.2;
            c.transform.crop_right = 0.2;
            c.transform.crop_bottom = 0.2;
        }),
        crop_case("double speed", |c| {
            zoom_into_the_box(c);
            c.speed = 2.0;
        }),
        CropCase {
            range: Some((0.3, 2.1)),
            ..crop_case("cut by a range export", zoom_into_the_box)
        },
        // Half a frame off the grid: the pinned `fps` rounds the start to a slot.
        CropCase {
            start: 1.0 + 0.5 / 30.0,
            ..crop_case("late, off the frame grid", zoom_into_the_box)
        },
        crop_case("reversed", |c| {
            zoom_into_the_box(c);
            c.speed = -1.0;
        }),
    ];
    for case in &cases {
        check_crop(&dir, case);
    }
}
