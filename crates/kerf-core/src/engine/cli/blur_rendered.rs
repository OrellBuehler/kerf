//! The blurred fill ([`Fit::Blur`]) through the real `ffmpeg` (`#[ignore]`d): a landscape clip in
//! a vertical frame keeps its whole picture in the middle, and the bars above and below it are a
//! blurred copy of the picture instead of black; the scrubbed still draws the same frame.
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
    let _ = std::fs::remove_dir_all(&dir);
}
