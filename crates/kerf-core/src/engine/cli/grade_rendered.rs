//! A clip's grade (curves, wheels, LUT) rendered by the real `ffmpeg` (`#[ignore]`d), held to the
//! model: the same flat picture is exported with and without the grade, and the graded centre pixel
//! must be the model applied to the ungraded one — to within the YUV round trip around the RGB
//! filters — and must have moved, so a filter that silently did nothing fails. The scrubbed still is
//! held to the export's frame the same way.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored grade_rendered`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::*;
use crate::color_lut::tests::cube_text;
use crate::engine::test_support::{make_clip, single, test_asset, video_stream, StatusBounded};
use crate::model::{ClipLut, CurvePoint, Curves, Grade, Wheel, Wheels};

const CW: u32 = 64;
const CH: u32 = 36;
/// What the round trip into RGB and back to 4:2:0 may move a flat colour by, in levels.
const TOLERANCE: i32 = 4;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-grade-rendered-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A lossless one-second flat clip of `color`.
fn flat(dir: &Path, color: &str) -> Asset {
    let path = dir.join(format!("flat-{color}.mp4"));
    if !path.exists() {
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("color=c={color}:s={CW}x{CH}:r=30:d=1"))
            .args(["-c:v", "libx264", "-crf", "0", "-g", "1", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());
    }
    let mut asset = test_asset(vec![video_stream(CW, CH, 30.0)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 1.0;
    asset
}

fn options() -> ExportOptions {
    ExportOptions {
        resolution: Some((CW, CH)),
        fps: Some(30.0),
        ..ExportOptions::default()
    }
}

fn centre(frame: &[u8]) -> [i32; 3] {
    let at = ((CH / 2 * CW + CW / 2) * 3) as usize;
    [i32::from(frame[at]), i32::from(frame[at + 1]), i32::from(frame[at + 2])]
}

/// The centre pixel of the export's frame at 0.5 s, as rgb24.
fn exported(asset: &Asset, grade: &Grade, dir: &Path, tag: &str) -> [i32; 3] {
    let mut clip = make_clip(asset.id, 0.0, 1.0, 0.0);
    clip.grade = grade.clone();
    let timeline = single(vec![clip]);
    let out = dir.join(format!("{tag}.rgb"));
    let mut args = build_export_args(&timeline, std::slice::from_ref(asset), "unused.mkv", &options()).unwrap();
    let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
    args.truncate(graph + 1);
    args.extend(["-map", "[outv]", "-f", "rawvideo", "-pix_fmt", "rgb24", "-y"].map(String::from));
    args.push(out.to_string_lossy().into_owned());
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    let bytes = std::fs::read(&out).unwrap();
    let frame = (CW * CH * 3) as usize;
    assert!(bytes.len() >= frame * 16, "{tag}: {} bytes", bytes.len());
    let at = centre(&bytes[frame * 15..frame * 16]);
    let still = {
        let args = build_still_args(
            &timeline,
            std::slice::from_ref(asset),
            &options(),
            0.5,
            CW,
            None,
            &StillOutput::RgbPipe,
        )
        .unwrap();
        let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
        assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
        centre(&run.stdout)
    };
    assert!(
        (0..3).all(|c| (at[c] - still[c]).abs() <= TOLERANCE),
        "{tag}: the still {still:?} is not the export's frame {at:?}"
    );
    at
}

fn assert_near(tag: &str, got: [i32; 3], want: [f64; 3]) {
    for c in 0..3 {
        assert!(
            (f64::from(got[c]) - want[c]).abs() <= f64::from(TOLERANCE),
            "{tag}: rendered {got:?}, the model says {want:?}"
        );
    }
}

fn moved(a: [i32; 3], b: [i32; 3]) -> i32 {
    (0..3).map(|c| (a[c] - b[c]).abs()).max().unwrap()
}

fn pts(p: &[(f64, f64)]) -> Vec<CurvePoint> {
    p.iter().map(|&(x, y)| CurvePoint { x, y }).collect()
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn curves_render_as_the_model_draws_them() {
    let dir = scratch("curves");
    for color in ["0x3C78B4", "0xD2A03C"] {
        let asset = flat(&dir, color);
        let plain = exported(&asset, &Grade::default(), &dir, &format!("{color}-plain"));
        let cases = [
            (
                "invert",
                Curves {
                    master: pts(&[(0.0, 1.0), (1.0, 0.0)]),
                    ..Curves::default()
                },
            ),
            (
                "red-s",
                Curves {
                    red: pts(&[(0.0, 0.0), (0.25, 0.1), (0.5, 0.6), (0.75, 0.95), (1.0, 1.0)]),
                    ..Curves::default()
                },
            ),
            (
                "channels-then-master",
                Curves {
                    master: pts(&[(0.0, 0.1), (1.0, 0.8)]),
                    green: pts(&[(0.0, 0.9), (0.6, 0.2), (1.0, 0.4)]),
                    blue: pts(&[(0.3, 0.7), (0.9, 0.1)]),
                    ..Curves::default()
                },
            ),
        ];
        for (name, curves) in cases {
            let grade = Grade {
                curves: Some(curves.clone()),
                ..Grade::default()
            };
            let got = exported(&asset, &grade, &dir, &format!("{color}-{name}"));
            let want = [0, 1, 2].map(|c| 255.0 * curves.apply(c, f64::from(plain[c]) / 255.0));
            assert_near(&format!("{color} {name}"), got, want);
            assert!(moved(got, plain) >= 20, "{color} {name}: {plain:?} -> {got:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_lut_maps_a_known_colour_and_its_intensity_mixes() {
    let dir = scratch("lut");
    let asset = flat(&dir, "0x3C78B4");
    let plain = exported(&asset, &Grade::default(), &dir, "plain");
    // Channels rotated, and a non-affine table (a square per channel) for the interpolation.
    let rotate = dir.join("rotate.cube");
    std::fs::write(&rotate, cube_text(2, |[r, g, b]| [g, b, r])).unwrap();
    let square = dir.join("square.cube");
    std::fs::write(&square, cube_text(33, |[r, g, b]| [r * r, g * g, b * b])).unwrap();
    let lut = |path: &Path, intensity: f64| Grade {
        lut: Some(ClipLut {
            path: path.to_string_lossy().into_owned(),
            intensity,
        }),
        ..Grade::default()
    };
    let p = plain.map(f64::from);
    let got = exported(&asset, &lut(&rotate, 1.0), &dir, "rotate");
    assert_near("rotate", got, [p[1], p[2], p[0]]);
    assert!(moved(got, plain) >= 40);
    let got = exported(&asset, &lut(&rotate, 0.5), &dir, "rotate-half");
    assert_near(
        "rotate at 50%",
        got,
        [(p[0] + p[1]) / 2.0, (p[1] + p[2]) / 2.0, (p[2] + p[0]) / 2.0],
    );
    let table = crate::color_lut::Lut3d::read(&square).unwrap();
    let want = table.apply(plain.map(|v| v as f32 / 255.0)).map(|v| f64::from(v) * 255.0);
    let got = exported(&asset, &lut(&square, 1.0), &dir, "square");
    assert_near("square", got, want);
    assert!(moved(got, plain) >= 20);
    // A file that is gone is left out: the picture is the ungraded one.
    let gone = dir.join("gone.cube");
    let got = exported(&asset, &lut(&gone, 1.0), &dir, "gone");
    assert!(moved(got, plain) <= 1, "{plain:?} -> {got:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// FFmpeg's `colorbalance` (4.4 and later) on one channel value `v` of a pixel whose `l` is its largest
/// plus its smallest channel (the filter's own `l`: the sum, not the mean).
fn colorbalance(v: f64, l: f64, s: f64, m: f64, h: f64) -> f64 {
    let (a, b, scale) = (4.0, 0.333, 0.7);
    let s = s * ((b - l) * a + 0.5).clamp(0.0, 1.0) * scale;
    let m = m * ((l - b) * a + 0.5).clamp(0.0, 1.0) * ((1.0 - l - b) * a + 0.5).clamp(0.0, 1.0) * scale;
    let h = h * ((l + b - 1.0) * a + 0.5).clamp(0.0, 1.0) * scale;
    (v + s + m + h).clamp(0.0, 1.0)
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn wheels_render_as_colorbalance_numbers() {
    let dir = scratch("wheels");
    let wheel = |x, y, lightness| Wheel { x, y, lightness };
    let cases = [
        (
            "0x282828",
            Wheels {
                shadows: wheel(0.0, 0.0, 0.3),
                ..Wheels::default()
            },
        ),
        (
            "0x404040",
            Wheels {
                midtones: wheel(-0.4, 0.0, 0.0),
                ..Wheels::default()
            },
        ),
        (
            "0xDCDCDC",
            Wheels {
                highlights: wheel(0.0, -0.5, -0.1),
                ..Wheels::default()
            },
        ),
        (
            "0x50648C",
            Wheels {
                shadows: wheel(0.2, 0.1, 0.0),
                midtones: wheel(0.0, 0.3, 0.05),
                highlights: wheel(-0.3, -0.2, 0.0),
            },
        ),
    ];
    for (color, wheels) in cases {
        let asset = flat(&dir, color);
        let plain = exported(&asset, &Grade::default(), &dir, &format!("{color}-plain"));
        let grade = Grade {
            wheels: Some(wheels),
            ..Grade::default()
        };
        let got = exported(&asset, &grade, &dir, &format!("{color}-wheels"));
        let rgb = plain.map(|v| f64::from(v) / 255.0);
        let l = rgb.iter().copied().fold(0.0, f64::max) + rgb.iter().copied().fold(1.0, f64::min);
        let (s, m, h) = (wheels.shadows.rgb(), wheels.midtones.rgb(), wheels.highlights.rgb());
        let want = [0, 1, 2].map(|c| 255.0 * colorbalance(rgb[c], l, s[c], m[c], h[c]));
        assert_near(color, got, want);
        assert!(moved(got, plain) >= 10, "{color}: {plain:?} -> {got:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn all_three_render_in_order_on_one_clip() {
    let dir = scratch("all");
    let asset = flat(&dir, "0x3C78B4");
    let plain = exported(&asset, &Grade::default(), &dir, "plain");
    let rotate = dir.join("rotate.cube");
    std::fs::write(&rotate, cube_text(2, |[r, g, b]| [g, b, r])).unwrap();
    let curves = Curves {
        master: pts(&[(0.0, 1.0), (1.0, 0.0)]),
        ..Curves::default()
    };
    let grade = Grade {
        lut: Some(ClipLut {
            path: rotate.to_string_lossy().into_owned(),
            intensity: 1.0,
        }),
        curves: Some(curves),
        wheels: Some(Wheels {
            highlights: Wheel {
                x: 0.0,
                y: 0.0,
                lightness: -0.1,
            },
            ..Wheels::default()
        }),
    };
    let got = exported(&asset, &grade, &dir, "all");
    // LUT, then curves, then wheels.
    let p = plain.map(|v| f64::from(v) / 255.0);
    let looked = [p[1], p[2], p[0]];
    let inverted = looked.map(|v| 1.0 - v);
    let l = inverted.iter().copied().fold(0.0, f64::max) + inverted.iter().copied().fold(1.0, f64::min);
    let want = inverted.map(|v| 255.0 * colorbalance(v, l, 0.0, 0.0, -0.1));
    assert_near("all", got, want);
    let _ = std::fs::remove_dir_all(&dir);
}
