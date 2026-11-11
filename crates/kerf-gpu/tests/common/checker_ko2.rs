//! Pictures for the `checker_ko2_*` families (a chroma key on a layer below full opacity, checked from outside): scenes built to put a key's
//! alpha plane through every level (a green screen with a noisy soft edge, a chroma gradient across the key's boundary, strips of flat chroma
//! whose distance from a key colour is known to the digit), in the formats and tags the plan has to judge. Everything is lossless, so what
//! FFmpeg and the decode read is what was synthesized.
//!
//! Included by `#[path = "common/checker_ko2.rs"] mod ko2;` in the suites that use it.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use kerf_core::{Asset, Project};

use crate::common;

/// The flat chroma of strip `j` of the `strips` picture: `(U, V)`, each strip 40 luma columns wide.
pub const STRIPS: [(u8, u8); 16] = [
    (128, 128),
    (129, 128),
    (130, 127),
    (126, 131),
    (120, 140),
    (100, 49),
    (101, 49),
    (102, 52),
    (110, 150),
    (90, 110),
    (140, 120),
    (60, 200),
    (200, 60),
    (16, 16),
    (240, 240),
    (127, 129),
];

/// The width in luma columns of a strip.
pub const STRIP_W: usize = 40;

pub struct Ko2 {
    /// A green screen: a disc of skin tone on a key green (`0x00b140`: U 101, V 49) with a soft edge 40 columns wide, noise in all planes,
    /// untagged, 8-bit 4:2:0, 640x360, 30 fps, 4 s.
    pub screen: Asset,
    pub screen709: Asset,
    pub screen2020: Asset,
    pub screen170m: Asset,
    pub screen470bg: Asset,
    /// The same scene as a full-range `yuvj420p` JPEG video, and as a limited / full-range-flagged `yuv420p` (FFV1).
    pub screen_full: Asset,
    pub screen_pc: Asset,
    pub screen_tv: Asset,
    pub screen10: Asset,
    pub screen422: Asset,
    pub screen444: Asset,
    pub screen_gray: Asset,
    pub screen_rgb: Asset,
    pub screen_odd: Asset,
    pub screen_png: Asset,
    pub screen_jpg: Asset,
    /// U runs 0 to 255 across the frame, V is 128 with noise, luma a ramp with noise: the key's distance crosses every value somewhere.
    pub grad: Asset,
    /// The same, down the frame instead.
    pub grad_v: Asset,
    /// Sixteen vertical strips of flat chroma (`STRIPS`) over a luma ramp, 640x360.
    pub strips: Asset,
    /// Grey noise: every luma, a chroma that wanders around 128.
    pub wander: Asset,
    /// Backdrops.
    pub bars: Asset,
    pub bars709: Asset,
    pub testsrc: Asset,
    pub testsrc709: Asset,
    /// A 25 fps and a 24 fps scene, for a cut that mixes rates.
    pub screen25: Asset,
    pub screen24: Asset,
    /// The scene at 320x180 (a source smaller than the canvas, enlarged) and 1280x720 (larger, shrunk).
    pub screen_small: Asset,
    pub screen_hd: Asset,
    /// Eight seconds of the scene and of the bars, for a dissolve that runs past a handful of frames.
    pub screen_long: Asset,
    pub bars_long: Asset,
}

const R: &str = "hypot(X/SW-320,(Y/SH-180)*1.5)";

/// The expression of the scene's subject mask: 1 inside the disc, 0 outside, a soft edge 40 columns wide.
fn subject() -> String {
    format!("clip((150-{R})/40,0,1)")
}

/// The scene as `nullsrc`'s `geq`, `duration` seconds at `rate`, at `size` (the disc scales with the frame width 640).
fn scene(size: &str, rate: u32, duration: u32) -> String {
    let s = subject();
    let (w, h): (f64, f64) = {
        let (a, b) = size.split_once('x').unwrap();
        (a.parse().unwrap(), b.parse().unwrap())
    };
    let k = w / 640.0;
    let r = format!("hypot((X/SW-{})/{k},((Y/SH-{})/{k})*1.5)", w / 2.0, h / 2.0);
    let s = if (k - 1.0).abs() < 1e-9 {
        s
    } else {
        format!("clip((150-{r})/40,0,1)")
    };
    format!(
        "nullsrc=size={size}:rate={rate}:duration={duration},format=yuv420p,geq=lum='clip(50+90*{s}+30*sin(X/37)*cos(Y/23)+(random(1)-0.5)*14,16,235)':cb='clip(101+9*{s}+(random(2)-0.5)*6,16,240)':cr='clip(49+101*{s}+(random(3)-0.5)*6,16,240)'"
    )
}

fn probed(path: &Path) -> Asset {
    Project::probe_asset(path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()))
}

pub fn ko2() -> &'static Ko2 {
    static M: std::sync::OnceLock<Ko2> = std::sync::OnceLock::new();
    M.get_or_init(|| {
        let dir: PathBuf = common::media_dir("ko2-media");
        let untagged = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
        let bt709 = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709";
        let bt2020 = "setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt709";
        let smpte170m = "setparams=colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m";
        let bt470bg = "setparams=colorspace=bt470bg:color_primaries=bt470bg:color_trc=bt470bg";
        // `vf`: appended to the lavfi source; `pix`: the format of the file; `codec`: its encoder arguments.
        let make = |name: &str, lavfi: &str, pix: &str, codec: &[&str], extra: &[&str]| -> Asset {
            let p = dir.join(name);
            let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", lavfi, "-c:v"];
            args.extend_from_slice(codec);
            args.extend(["-pix_fmt", pix]);
            args.extend_from_slice(extra);
            args.push(p.to_str().unwrap());
            common::ffmpeg(&args);
            probed(&p)
        };
        let x264 = ["libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"];
        let ffv1 = ["ffv1"];
        let base = scene("640x360", 30, 4);
        let tagged = |tag: &str| format!("{base},{tag}");
        let grad_src = |axis: &str, rate: u32| {
            format!(
                "nullsrc=size=640x360:rate={rate}:duration=4,format=yuv420p,geq=lum='clip(30+190*{axis}/640*1+(random(1)-0.5)*20,16,235)':cb='clip({axis}/640*255+(random(2)-0.5)*4,0,255)':cr='clip(128+(random(3)-0.5)*8,0,255)',{untagged}"
            )
        };
        let strips = {
            let chain = |idx: usize| {
                let mut e = String::new();
                for (j, (u, v)) in STRIPS.iter().enumerate() {
                    let c = if idx == 0 { u } else { v };
                    if j + 1 == STRIPS.len() {
                        e.push_str(&c.to_string());
                    } else {
                        e.push_str(&format!("if(lt(X/SW,{}),{c},", (j + 1) * STRIP_W));
                    }
                }
                e.push_str(&")".repeat(STRIPS.len() - 1));
                e
            };
            format!(
                "nullsrc=size=640x360:rate=30:duration=4,format=yuv420p,geq=lum='clip(40+X/SW*190/640+(random(1)-0.5)*10,16,235)':cb='{}':cr='{}',{untagged}",
                chain(0),
                chain(1)
            )
        };
        let still_png = dir.join("screen.png");
        let still_jpg = dir.join("screen.jpg");
        common::ffmpeg(&["-f", "lavfi", "-i", &base, "-frames:v", "1", "-pix_fmt", "rgb24", still_png.to_str().unwrap()]);
        common::ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &base,
            "-frames:v",
            "1",
            "-q:v",
            "1",
            "-pix_fmt",
            "yuvj420p",
            still_jpg.to_str().unwrap(),
        ]);
        Ko2 {
            screen: make("screen.mp4", &format!("{base},{untagged}"), "yuv420p", &x264, &[]),
            screen709: make("screen709.mp4", &tagged(bt709), "yuv420p", &x264, &[]),
            screen2020: make("screen2020.mp4", &tagged(bt2020), "yuv420p", &x264, &[]),
            screen170m: make("screen170m.mp4", &tagged(smpte170m), "yuv420p", &x264, &[]),
            screen470bg: make("screen470bg.mp4", &tagged(bt470bg), "yuv420p", &x264, &[]),
            screen_full: make(
                "screen-full.mkv",
                &format!("{base},{untagged}"),
                "yuvj420p",
                &["mjpeg", "-qscale:v", "1"],
                &[],
            ),
            screen_pc: make("screen-pc.mkv", &format!("{base},{untagged}"), "yuv420p", &ffv1, &["-color_range", "pc"]),
            screen_tv: make("screen-tv.mkv", &format!("{base},{untagged}"), "yuv420p", &ffv1, &["-color_range", "tv"]),
            screen10: make(
                "screen10.mp4",
                &format!("{base},{untagged}"),
                "yuv420p10le",
                &["libx264", "-preset", "ultrafast", "-qp", "0"],
                &[],
            ),
            screen422: make("screen422.mkv", &format!("{base},{untagged}"), "yuv422p", &ffv1, &[]),
            screen444: make("screen444.mkv", &format!("{base},{untagged}"), "yuv444p", &ffv1, &[]),
            screen_gray: make("screen-gray.mkv", &format!("{base},{untagged}"), "gray", &ffv1, &[]),
            screen_rgb: make("screen-rgb.mkv", &format!("{base},{untagged}"), "bgr0", &ffv1, &[]),
            screen_odd: make(
                "screen-odd.mkv",
                &format!("{},scale=641:361,{untagged}", scene("642x362", 30, 4)),
                "yuv420p",
                &ffv1,
                &[],
            ),
            screen_png: probed(&still_png),
            screen_jpg: probed(&still_jpg),
            grad: make("grad.mp4", &grad_src("X/SW", 30), "yuv420p", &x264, &[]),
            grad_v: make(
                "grad-v.mp4",
                &format!(
                    "nullsrc=size=640x360:rate=30:duration=4,format=yuv420p,geq=lum='clip(30+190*(Y/SH)/360+(random(1)-0.5)*20,16,235)':cb='clip(128+(random(2)-0.5)*8,0,255)':cr='clip((Y/SH)/360*255+(random(3)-0.5)*4,0,255)',{untagged}"
                ),
                "yuv420p",
                &x264,
                &[],
            ),
            strips: make("strips.mp4", &strips, "yuv420p", &x264, &[]),
            wander: make(
                "wander.mp4",
                &format!(
                    "nullsrc=size=640x360:rate=30:duration=4,format=yuv420p,geq=lum='random(1)*219+16':cb='128+(random(2)-0.5)*40':cr='128+(random(3)-0.5)*40',{untagged}"
                ),
                "yuv420p",
                &x264,
                &[],
            ),
            bars: make(
                "bars.mp4",
                &format!("smptehdbars=size=640x360:rate=30:duration=4,{untagged}"),
                "yuv420p",
                &x264,
                &[],
            ),
            bars709: make(
                "bars709.mp4",
                &format!("smptehdbars=size=640x360:rate=30:duration=4,{bt709}"),
                "yuv420p",
                &x264,
                &[],
            ),
            testsrc: make(
                "testsrc.mp4",
                &format!("testsrc2=size=640x360:rate=30:duration=4,{untagged}"),
                "yuv420p",
                &x264,
                &[],
            ),
            testsrc709: make(
                "testsrc709.mp4",
                &format!("testsrc2=size=640x360:rate=30:duration=4,{bt709}"),
                "yuv420p",
                &x264,
                &[],
            ),
            screen25: make("screen25.mp4", &format!("{},{untagged}", scene("640x360", 25, 4)), "yuv420p", &x264, &[]),
            screen24: make("screen24.mp4", &format!("{},{untagged}", scene("640x360", 24, 4)), "yuv420p", &x264, &[]),
            screen_small: make(
                "screen-small.mp4",
                &format!("{},{untagged}", scene("320x180", 30, 4)),
                "yuv420p",
                &x264,
                &[],
            ),
            screen_hd: make(
                "screen-hd.mp4",
                &format!("{},{untagged}", scene("1280x720", 30, 2)),
                "yuv420p",
                &x264,
                &[],
            ),
            screen_long: make(
                "screen-long.mp4",
                &format!("{},{untagged}", scene("640x360", 30, 8)),
                "yuv420p",
                &x264,
                &[],
            ),
            bars_long: make(
                "bars-long.mp4",
                &format!("smptehdbars=size=640x360:rate=30:duration=8,{untagged}"),
                "yuv420p",
                &x264,
                &[],
            ),
        }
    })
}

/// The U and V FFmpeg's `chromakey` takes a colour's chroma to be (`RGB_TO_U` / `RGB_TO_V`, vf_chromakey.c), written out from the filter's
/// source for a colour given as RGB.
pub fn chromakey_uv(rgb: [i64; 3]) -> (i64, i64) {
    let fix = |x: f64| (x * 1024.0).round() as i64;
    let u = ((-fix(0.16874) * rgb[0] - fix(0.33126) * rgb[1] + fix(0.5) * rgb[2] + (1 << 9) - 1) >> 10) + 128;
    let v = ((fix(0.5) * rgb[0] - fix(0.41869) * rgb[1] - fix(0.08131) * rgb[2] + (1 << 9) - 1) >> 10) + 128;
    (u, v)
}

/// A float's neighbour `by` steps up or down (the doubles `chromakey` compares with are floats widened).
pub fn f32_step(x: f32, by: i32) -> f32 {
    f32::from_bits((x.to_bits() as i64 + i64::from(by)) as u32)
}
