//! The blurred fill ([`Fit::Blur`]) through the real `ffmpeg` (`#[ignore]`d): a landscape clip in
//! a vertical frame keeps its whole picture in the middle, and the bars above and below it are a
//! blurred copy of the picture instead of black; the scrubbed still draws the same frame; a
//! picture with an alpha channel takes no fill (it would show through as a halo); an HDR picture
//! composes in 10 bits ahead of its tone-map, so its picture is Contain's.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored blur_rendered`

use std::path::Path;
use std::process::Stdio;

use super::rendered::{export_frames_sized, scratch};
use super::*;
use crate::engine::test_support::{make_clip, single, test_asset, video_stream, StatusBounded};
use crate::model::{Delivery, Fit};

const W: u32 = 72;
const H: u32 = 128;

/// A 128x72 picture, white on its left half and black on its right.
fn halves(dir: &Path) -> Asset {
    let path = dir.join("halves.mp4");
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg("color=c=black:s=128x72:r=25:d=2,drawbox=x=0:y=0:w=64:h=72:c=white:t=fill")
        .args(["-c:v", "libx264", "-crf", "0", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());
    let mut asset = test_asset(vec![video_stream(128, 72, 25.0)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 2.0;
    asset
}

/// The luma-ish mean (of R, G and B) of the pixels of `frame` in rows `rows` and columns `cols`.
fn mean(frame: &[u8], rows: std::ops::Range<u32>, cols: std::ops::Range<u32>) -> f64 {
    let (mut sum, mut n) = (0.0, 0.0);
    for y in rows {
        for x in cols.clone() {
            let at = ((y * W + x) * 3) as usize;
            sum += frame[at..at + 3].iter().map(|v| f64::from(*v)).sum::<f64>() / 3.0;
            n += 1.0;
        }
    }
    sum / n
}

#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn a_blurred_fill_fills_the_bars_with_the_picture_blurred() {
    let dir = scratch("blur-fill");
    let asset = halves(&dir);
    let assets = vec![asset.clone()];
    let cut = |fit| {
        let mut tl = single(vec![make_clip(asset.id, 0.0, 1.0, 0.0)]);
        tl.format = Some(Delivery::new(W, H, fit));
        tl
    };
    let frame = |fit| export_frames_sized(&cut(fit), &assets, 25.0, (W, H), &dir, &format!("{fit:?}")).swap_remove(10);
    let (blur, contain) = (frame(Fit::Blur), frame(Fit::Contain));
    // The picture fits 72x40 in the middle (rows 44..84), the same under both fits.
    let middle = |f: &[u8]| (mean(f, 50..78, 4..30), mean(f, 50..78, 42..68));
    for f in [&blur, &contain] {
        let (left, right) = middle(f);
        assert!(left > 230.0 && right < 25.0, "the picture itself: {left} {right}");
    }
    // The bars: black under Contain; under Blur a soft copy of the picture's middle (the cover
    // crop of its centre), so the edge between the halves is a ramp across the bar.
    let bar = |f: &[u8], cols| mean(f, 4..36, cols);
    eprintln!(
        "top bar, blur: left {:.1} centre {:.1} right {:.1}; contain {:.1}",
        bar(&blur, 0..12),
        bar(&blur, 30..42),
        bar(&blur, 60..72),
        bar(&contain, 0..72)
    );
    assert!(bar(&contain, 0..72) < 20.0, "Contain letterboxes in black");
    let (left, centre, right) = (bar(&blur, 0..12), bar(&blur, 30..42), bar(&blur, 60..72));
    assert!(left > 150.0 && right < 100.0, "the fill is the picture: {left} {right}");
    assert!(
        centre < left - 20.0 && centre > right + 20.0,
        "and blurred across its edge: {left} {centre} {right}"
    );
    // The scrubbed still at the same moment draws the same frame.
    let opts = ExportOptions {
        resolution: Some((W, H)),
        fps: Some(25.0),
        ..ExportOptions::default()
    };
    let args = build_still_args(&cut(Fit::Blur), &assets, &opts, 0.4, W, None, &StillOutput::RgbPipe).unwrap();
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    let worst = run.stdout.iter().zip(&blur).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(255);
    assert_eq!(run.stdout.len(), blur.len());
    assert!(worst <= 3, "the still is the export's frame: {worst} levels apart at worst");

    // A picture that is half transparent takes no fill: a blurred copy would show through its
    // clear half as a halo. Under Blur it renders as under Contain, pixel for pixel.
    let sticker = sticker(&dir);
    let stickers = vec![sticker.clone()];
    let over = |fit| {
        let mut tl = single(vec![make_clip(sticker.id, 0.0, 1.0, 0.0)]);
        tl.format = Some(Delivery::new(W, H, fit));
        export_frames_sized(&tl, &stickers, 25.0, (W, H), &dir, &format!("sticker-{fit:?}")).swap_remove(10)
    };
    let (blur, contain) = (over(Fit::Blur), over(Fit::Contain));
    assert!(blur == contain, "an alpha source under Blur is drawn as under Contain");
    assert!(mean(&blur, 4..36, 0..72) < 20.0, "with no fill in its bars");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A 128x72 picture with an alpha channel (FFV1 `yuva420p`): opaque white on its left half, clear
/// on its right.
fn sticker(dir: &Path) -> Asset {
    let path = dir.join("sticker.mkv");
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(
            "color=c=white:s=128x72:r=25:d=2,format=yuva420p,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(lt(X,64),255,0)'",
        )
        .args(["-c:v", "ffv1"])
        .arg(&path)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());
    let mut stream = video_stream(128, 72, 25.0);
    stream.pix_fmt = Some("yuva420p".into());
    let mut asset = test_asset(vec![stream]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 2.0;
    asset
}

/// A 128x72 10-bit HLG BT.2020 picture (as `keyed_zoom.rs` makes one), a shallow horizontal ramp
/// that 8 bits band: or `None` when this ffmpeg cannot make one (no `zscale` or no `libx265`).
fn hlg_ramp(dir: &Path) -> Option<Asset> {
    let path = dir.join("ramp-hlg.mp4");
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(
            "nullsrc=s=128x72:r=25:d=2,format=gbrp16le,geq=r='20000+X*24':g='20000+X*24':b='20000+X*24',\
             setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=pc",
        )
        .args([
            "-vf",
            "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt2020:t=arib-std-b67:m=bt2020nc:r=tv,format=yuv420p10le",
            "-c:v",
            "libx265",
            "-x265-params",
            "log-level=error:lossless=1",
            "-color_primaries",
            "bt2020",
            "-color_trc",
            "arib-std-b67",
            "-colorspace",
            "bt2020nc",
        ])
        .arg(&path)
        .status_bounded()
        .ok()?;
    if !made.success() {
        return None;
    }
    let mut stream = video_stream(128, 72, 25.0);
    stream.pix_fmt = Some("yuv420p10le".into());
    stream.color_transfer = Some("arib-std-b67".into());
    stream.color_primaries = Some("bt2020".into());
    let mut asset = test_asset(vec![stream]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 2.0;
    Some(asset)
}

/// Frame 10 of the export graph of `tl`, its graph passed through `edit` first.
fn graph_frame(tl: &Timeline, assets: &[Asset], dir: &Path, tag: &str, edit: impl Fn(&str) -> String) -> Vec<u8> {
    let opts = ExportOptions {
        resolution: Some((W, H)),
        fps: Some(25.0),
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
            "11",
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
    raw.chunks((W * H * 3) as usize).nth(10).expect("eleven frames").to_vec()
}

#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn an_hdr_picture_is_filled_in_ten_bits_ahead_of_its_tone_map() {
    let dir = scratch("blur-fill-hdr");
    let Some(ramp) = hlg_ramp(&dir) else {
        eprintln!("skipped: this ffmpeg cannot make 10-bit HLG footage");
        return;
    };
    let assets = vec![ramp.clone()];
    let cut = |fit| {
        let mut tl = single(vec![make_clip(ramp.id, 0.0, 1.0, 0.0)]);
        tl.format = Some(Delivery::new(W, H, fit));
        tl
    };
    let contain = graph_frame(&cut(Fit::Contain), &assets, &dir, "contain", str::to_string);
    let deep = graph_frame(&cut(Fit::Blur), &assets, &dir, "deep", |g| {
        assert!(g.contains(":format=yuv420p10,"), "{g}");
        g.to_string()
    });
    let shallow = graph_frame(&cut(Fit::Blur), &assets, &dir, "shallow", |g| {
        g.replace(":format=yuv420p10", "")
    });
    // The picture: 72x40 in the middle (rows 44..84), clear of the bars' rows.
    let apart = |f: &[u8]| {
        let (mut worst, mut sum) = (0u8, 0u64);
        for y in 48..80 {
            for x in 0..W {
                for c in 0..3 {
                    let at = ((y * W + x) * 3 + c) as usize;
                    let d = f[at].abs_diff(contain[at]);
                    worst = worst.max(d);
                    sum += u64::from(d);
                }
            }
        }
        (worst, sum)
    };
    let (deep_apart, shallow_apart) = (apart(&deep), apart(&shallow));
    eprintln!("the picture against Contain's, worst / summed levels: 10-bit {deep_apart:?}, 8-bit {shallow_apart:?}");
    // What is left is the tone-map's error diffusion, which carries the bars' error into the
    // picture's rows (2 levels at worst, 798 summed on 6.1.1 and 720 on 9.0.2; the 8-bit fill 5, and ~7150).
    assert!(deep_apart.0 <= 2, "the picture is Contain's: {deep_apart:?}");
    assert!(
        deep_apart.1 * 4 < shallow_apart.1,
        "and far closer to it than an 8-bit fill: {deep_apart:?} {shallow_apart:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
