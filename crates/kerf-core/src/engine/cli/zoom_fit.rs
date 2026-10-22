//! A clip's zoom taken from the source in the same resample as its fit ([`FitOrder`]).
//!
//! Fitting the picture to the frame first and enlarging the fitted picture afterwards keeps a
//! fraction of the source's detail (a 4096x2160 clip zoomed 3.37x in a 1080x1920 frame was
//! scaled down to 1080x570 and up again), and under `Cover` the crop to the frame ran before the
//! transform, so a position revealed black where the picture goes on. The chains here are
//! the string-level checks; the `#[ignore]`d tests at the end render them with the real `ffmpeg`
//! (`cargo test -p kerf-core --no-default-features -- --ignored zoom_fit`).

use super::*;
use crate::engine::test_support::{make_clip, test_asset, video_stream};
use crate::model::{Fit, FitOrder, Hdr, Keyframe};

fn format(fit: Fit) -> ExportFormat {
    ExportFormat {
        width: 1080,
        height: 1920,
        fit,
        ..ExportFormat::default()
    }
}

fn clip(scale: f64) -> Clip {
    let asset = test_asset(vec![video_stream(4096, 2160, 30.0)]);
    let mut clip = make_clip(asset.id, 0.0, 4.0, 0.0);
    clip.transform.scale = scale;
    clip
}

fn chain_with(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx) -> String {
    video_clip_chain(clip, fmt, fx, false, "c0")
}

fn chain(clip: &Clip, fit: Fit) -> String {
    chain_with(clip, &format(fit), &ClipFx::default())
}

fn key(time: f64, scale: f64, pos_x: f64) -> Keyframe {
    Keyframe {
        time,
        scale,
        pos_x,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    }
}

fn scales(chain: &str) -> usize {
    chain.split(',').filter(|f| f.starts_with("scale=")).count()
}

#[test]
fn the_fit_scale_of_a_zoom_is_the_frame_times_the_zoom() {
    assert_eq!(
        fit_scale(1080, 1920, 3.3713, "decrease", false, ""),
        "scale=1080*3.3713:1920*3.3713:force_original_aspect_ratio=decrease"
    );
    assert_eq!(
        fit_scale(1080, 1920, 2.0, "increase", false, ":flags=lanczos"),
        "scale=1080*2:1920*2:force_original_aspect_ratio=increase:flags=lanczos"
    );
    // No zoom is the frame's own box.
    assert_eq!(
        fit_scale(1080, 1920, 1.0, "increase", false, ""),
        "scale=1080:1920:force_original_aspect_ratio=increase"
    );
    // Ahead of a tone-map the box is made even, and the result too.
    assert_eq!(
        fit_scale(1080, 1920, 1.5, "decrease", true, ""),
        "scale=w='max(2,2*trunc((1080*1.5)/2))':h='max(2,2*trunc((1920*1.5)/2))':force_original_aspect_ratio=decrease:force_divisible_by=2"
    );
    assert_eq!(
        fit_scale(1080, 1920, 1.0, "increase", true, ""),
        "scale=1080:1920:force_original_aspect_ratio=increase:force_divisible_by=2"
    );
    // A zoom out to a fraction of a pixel keeps a pixel; the sizes would be 0, which `scale` reads as "unset".
    assert_eq!(
        fit_scale(1080, 1920, 0.0004, "increase", false, ""),
        "scale=w='max(1,1080*0.0004)':h='max(1,1920*0.0004)':force_original_aspect_ratio=increase"
    );
}

#[test]
fn a_zoom_in_under_contain_is_one_scale_from_the_source() {
    let zoomed = clip(3.3713);
    assert_eq!(zoomed.fit_order(Fit::Contain), FitOrder::Folded);
    let c = chain(&zoomed, Fit::Contain);
    assert!(
        c.contains("scale=1080*3.3713:1920*3.3713:force_original_aspect_ratio=decrease,setsar=1"),
        "{c}"
    );
    assert_eq!(scales(&c), 1, "the picture is resampled once: {c}");
    assert!(!c.contains("iw*3.3713"), "{c}");
    assert!(!c.contains("pad="), "a transformed clip is centred by the overlay: {c}");
    // The delivery's scaler rides on the one scale.
    let lanczos = ExportFormat {
        scaler: Some("lanczos".into()),
        ..format(Fit::Contain)
    };
    let c = chain_with(&zoomed, &lanczos, &ClipFx::default());
    assert!(
        c.contains("scale=1080*3.3713:1920*3.3713:force_original_aspect_ratio=decrease:flags=lanczos,setsar=1"),
        "{c}"
    );
    // A blurred fill is Contain's chain for a clip that is not the plain fit.
    assert_eq!(zoomed.fit_order(Fit::Blur), FitOrder::Folded);
    let c = chain(&zoomed, Fit::Blur);
    assert!(
        c.contains("scale=1080*3.3713:1920*3.3713:force_original_aspect_ratio=decrease,setsar=1"),
        "{c}"
    );
}

#[test]
fn a_zoom_in_ahead_of_a_tone_map_is_even_in_the_same_scale() {
    let hlg = ClipFx {
        hdr: Some(Hdr::Hlg),
        ..ClipFx::default()
    };
    let c = chain_with(&clip(1.5), &format(Fit::Contain), &hlg);
    assert!(
        c.contains("scale=w='max(2,2*trunc((1080*1.5)/2))':h='max(2,2*trunc((1920*1.5)/2))':force_original_aspect_ratio=decrease:force_divisible_by=2,setsar=1"),
        "{c}"
    );
    assert_eq!(scales(&c), 1, "{c}");
    let tone = c.find("zscale").or_else(|| c.find("colorspace")).unwrap();
    assert!(c.find("force_divisible_by").unwrap() < tone, "{c}");
}

#[test]
fn what_is_not_a_zoom_in_keeps_the_chain_it_had() {
    // The identity transform: the fit and the pad.
    let c = chain(&clip(1.0), Fit::Contain);
    assert!(
        c.contains("scale=1080:1920:force_original_aspect_ratio=decrease,pad=1080:1920:(ow-iw)/2:(oh-ih)/2"),
        "{c}"
    );
    // A zoom out: the fit, then the shrink.
    let shrunk = clip(0.5);
    assert_eq!(shrunk.fit_order(Fit::Contain), FitOrder::Fitted);
    let c = chain(&shrunk, Fit::Contain);
    assert!(
        c.contains("scale=1080:1920:force_original_aspect_ratio=decrease,scale=iw*0.5:ih*0.5,setsar=1"),
        "{c}"
    );
    // A move that does not zoom: the fit alone, centred by the overlay.
    let mut moved = clip(1.0);
    moved.transform.pos_x = 0.2;
    assert_eq!(moved.fit_order(Fit::Contain), FitOrder::Fitted);
    let c = chain(&moved, Fit::Contain);
    assert!(
        c.contains("scale=1080:1920:force_original_aspect_ratio=decrease,setsar=1"),
        "{c}"
    );
    // A zoom that moves is the chain's last stage, at the fit size.
    let mut moving = clip(1.0);
    moving.keyframes = vec![key(0.0, 0.5, 0.0), key(2.0, 3.0, 0.0)];
    assert_eq!(moving.fit_order(Fit::Contain), FitOrder::Fitted);
    let c = chain(&moving, Fit::Contain);
    assert!(
        c.contains("scale=1080:1920:force_original_aspect_ratio=decrease,setsar=1"),
        "{c}"
    );
    assert!(c.contains("eval=frame"), "{c}");
}

#[test]
fn a_zoom_that_is_keyed_but_held_is_the_same_scale() {
    let mut held = clip(1.0);
    held.keyframes = vec![key(0.0, 2.0, 0.0), key(2.0, 2.0, 0.1)];
    assert_eq!(held.held_scale(), Some(2.0));
    assert_eq!(held.fit_order(Fit::Contain), FitOrder::Folded);
    let c = chain(&held, Fit::Contain);
    assert!(
        c.contains("scale=1080*2:1920*2:force_original_aspect_ratio=decrease,setsar=1") && !c.contains("eval=frame"),
        "{c}"
    );
    // Held at or under 1 it keeps the per-frame scale it always had.
    let mut under = clip(1.0);
    under.keyframes = vec![key(0.0, 0.8, 0.0), key(2.0, 0.8, 0.1)];
    assert_eq!(under.fit_order(Fit::Contain), FitOrder::Fitted);
    assert!(chain(&under, Fit::Contain).contains("eval=frame"));
}

// ---- rendered (`#[ignore]`d: they drive the real ffmpeg) --------------------------------------------

use std::path::Path;
use std::process::Stdio;

use super::rendered::scratch;
use crate::engine::test_support::{single, StatusBounded};
use crate::model::Delivery;

const FW: u32 = 1080;
const FH: u32 = 1920;

/// A 4096x2160 picture of fine detail: a grid of sines about 8 pixels across, in 4:2:0 and lossless.
/// Fitted to a 1080-wide frame first it is below the fitted picture's resolution (2 px across, past
/// what a scaler keeps), so the detail only survives a zoom that is taken from this file.
fn fine(dir: &Path) -> Asset {
    let path = dir.join("fine.mp4");
    if !path.exists() {
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(
                "nullsrc=s=4096x2160:r=5:d=0.6,format=yuv420p,\
                 geq=lum='128+100*sin(X*0.8)*sin(Y*0.8)':cb=128:cr=128",
            )
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "0", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());
    }
    let mut asset = test_asset(vec![video_stream(4096, 2160, 5.0)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 0.6;
    asset
}

/// The luma-ish mean of (R, G, B) of a pixel of an `FW`x`FH` rgb24 frame.
fn luma(frame: &[u8], x: u32, y: u32) -> f64 {
    let at = ((y * FW + x) * 3) as usize;
    frame[at..at + 3].iter().map(|v| f64::from(*v)).sum::<f64>() / 3.0
}

/// The standard deviation of the luma of a window of the frame: what is left of a detail of amplitude
/// 100 that has been resampled.
fn spread(frame: &[u8], rows: std::ops::Range<u32>, cols: std::ops::Range<u32>) -> f64 {
    let values: Vec<f64> = rows
        .flat_map(|y| cols.clone().map(move |x| (x, y)))
        .map(|(x, y)| luma(frame, x, y))
        .collect();
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / values.len() as f64).sqrt()
}

/// The first frame of the export graph of `tl` at the delivery size, its graph passed through `edit` first.
fn first_frame(tl: &Timeline, assets: &[Asset], dir: &Path, tag: &str, edit: impl Fn(&str) -> String) -> Vec<u8> {
    let opts = ExportOptions {
        resolution: Some((FW, FH)),
        fps: Some(5.0),
        ..ExportOptions::default()
    };
    let mut args = build_export_args(tl, assets, "unused.mkv", &opts).unwrap();
    let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
    args[graph] = edit(&args[graph]);
    args.truncate(graph + 1);
    let out = dir.join(format!("{tag}.rgb"));
    args.extend(
        [
            "-map",
            "[outv]",
            "-frames:v",
            "1",
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
    let raw = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    assert_eq!(raw.len(), (FW * FH * 3) as usize);
    raw
}

/// The rows (top, bottom) of the frame that hold the picture, found by the bars of black above and below.
fn picture_rows(frame: &[u8]) -> (u32, u32) {
    let lit = |y: u32| (0..FW).step_by(7).any(|x| luma(frame, x, y) > 12.0);
    ((0..FH).find(|&y| lit(y)).unwrap(), (0..FH).rev().find(|&y| lit(y)).unwrap())
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a
        .iter()
        .zip(b)
        .map(|(x, y)| {
            let d = f64::from(*x) - f64::from(*y);
            d * d
        })
        .sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

/// A 4096x2160 clip zoomed 3x in a 1080x1920 frame under Contain keeps the source's detail: its picture
/// is the source scaled once, where fitting it to the frame first (to 1080x570) and enlarging that
/// 3x kept next to none of it. The picture is also where it was, to a few pixels.
#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn a_zoom_in_under_contain_keeps_the_detail_the_fitted_picture_lost() {
    let dir = scratch("zoom-fit-contain");
    let asset = fine(&dir);
    let assets = vec![asset.clone()];
    let mut c = make_clip(asset.id, 0.0, 0.4, 0.0);
    c.transform.scale = 3.0;
    let mut tl = single(vec![c]);
    tl.format = Some(Delivery::new(FW, FH, Fit::Contain));

    let new = first_frame(&tl, &assets, &dir, "new", |g| {
        assert!(g.contains("scale=1080*3:1920*3:force_original_aspect_ratio=decrease"), "{g}");
        g.to_string()
    });
    // The order the graph had: fit to the frame, then enlarge the fitted picture.
    let old = first_frame(&tl, &assets, &dir, "old", |g| {
        g.replace(
            "scale=1080*3:1920*3:force_original_aspect_ratio=decrease",
            "scale=1080:1920:force_original_aspect_ratio=decrease,scale=iw*3:ih*3",
        )
    });
    // The picture the source gives at the zoom, taken once and laid out by hand: 3240x1708, its middle
    // 1080 columns, 104 rows down.
    let reference = first_frame(&tl, &assets, &dir, "reference", |_| {
        "color=c=black:s=1080x1920:r=5:d=1,format=yuv420p[vbase];\
         [0:v]trim=start=0:end=0.4,setpts=PTS-STARTPTS,scale=3240:1709,crop=1080:1709:1080:0,\
         setsar=1,fps=5,format=yuv420p[v0];[vbase][v0]overlay=x=0:y=104[outv]"
            .to_string()
    });

    let (top, bottom) = picture_rows(&new);
    let (old_top, old_bottom) = picture_rows(&old);
    eprintln!("picture rows: new {top}..{bottom}, old {old_top}..{old_bottom}");
    assert!(
        top.abs_diff(old_top) <= 2 && bottom.abs_diff(old_bottom) <= 2,
        "the picture is where it was"
    );
    assert!(top.abs_diff(104) <= 2 && bottom.abs_diff(1812) <= 2, "{top}..{bottom}");

    let window = (400..1500, 100..980);
    let (kept, lost, truth) = (
        spread(&new, window.0.clone(), window.1.clone()),
        spread(&old, window.0.clone(), window.1.clone()),
        spread(&reference, window.0, window.1),
    );
    eprintln!("detail (std-dev of luma): one resample {kept:.1}, the source once {truth:.1}, fitted then enlarged {lost:.1}");
    assert!(truth > 30.0, "the pattern survives a single resample: {truth}");
    assert!(kept > 0.9 * truth, "the zoom is the source's detail: {kept} of {truth}");
    assert!(lost < 0.3 * truth, "fitting first lost it: {lost} of {truth}");
    let (to_new, to_old) = (psnr(&new, &reference), psnr(&old, &reference));
    eprintln!("PSNR against the source scaled once: new {to_new:.1} dB, old {to_old:.1} dB");
    assert!(to_new > 35.0, "{to_new}");
    assert!(to_new > to_old + 10.0, "well above the old path: {to_new} vs {to_old}");
    let _ = std::fs::remove_dir_all(&dir);
}
