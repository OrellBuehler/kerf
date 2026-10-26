//! Blend modes through the real `ffmpeg` (`#[ignore]`d): each mode's pixels against its formula over the colours
//! the composite holds, at full and half opacity; the composite outside the clip byte for byte; the same graph run
//! with `normal` against the plain `overlay`, byte for byte (an opaque picture, a feathered alpha); a keyed opacity
//! from nothing and a moving zoom; the scrubbed still against the export's frame.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored blend_rendered` (`KERF_FFMPEG` picks the build).

#![allow(clippy::print_stderr)] // what each case measured, for whoever runs the ignored ones

use std::path::Path;
use std::process::Stdio;

use super::rendered::{export_frames_sized, scratch};
use super::*;
use crate::engine::test_support::{make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded};
use crate::model::{BlendMode, PropertyKey, PropertyTrack, Transition, TransitionKind};
use crate::render_plan::YuvMatrix;

const W: u32 = 128;
const H: u32 = 72;

/// A lossless two-second solid clip of `color`, `W`x`H` at 25 fps (untagged, as most footage reads).
fn solid(dir: &Path, color: &str) -> Asset {
    solid_as(dir, color, (W, H), false)
}

/// [`solid`] `size` big, and encoded and tagged BT.709 (limited range) when `bt709`.
fn solid_as(dir: &Path, color: &str, (w, h): (u32, u32), bt709: bool) -> Asset {
    let path = dir.join(format!("{color}-{w}x{h}{}.mp4", if bt709 { "-709" } else { "" }));
    let mut make = command(&ffmpeg_bin());
    make.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("color=c={color}:s={w}x{h}:r=25:d=2"));
    if bt709 {
        make.args([
            "-vf",
            "format=gbrp,scale=out_color_matrix=bt709:out_range=tv,format=yuv420p",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
        ]);
    }
    let made = make
        .args(["-c:v", "libx264", "-crf", "0", "-g", "1", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());
    let mut stream = video_stream(w, h, 25.0);
    stream.pix_fmt = Some("yuv420p".into());
    if bt709 {
        stream.color_space = Some("bt709".into());
    }
    let mut asset = test_asset(vec![stream]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 2.0;
    asset
}

fn opts() -> ExportOptions {
    ExportOptions {
        resolution: Some((W, H)),
        fps: Some(25.0),
        ..ExportOptions::default()
    }
}

/// The export graph of `timeline` run to raw `yuv420p` (the 4:2:0 the composite is in, unconverted): one frame per
/// output frame.
fn yuv_frames(timeline: &Timeline, assets: &[Asset], dir: &Path, tag: &str) -> Vec<Vec<u8>> {
    let out = dir.join(format!("{tag}.yuv"));
    let mut args = build_export_args(timeline, assets, "unused.mkv", &opts()).unwrap();
    let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
    args.truncate(graph + 1);
    args.extend(
        [
            "-map",
            "[outv]",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
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
    let size = (W * H * 3 / 2) as usize;
    assert_eq!(raw.len() % size, 0);
    raw.chunks(size).map(<[u8]>::to_vec).collect()
}

/// A raw graph (`[o]` its picture) over lavfi sources, one `yuv420p` frame.
fn graph_frame(graph: &str) -> Vec<u8> {
    let run = command(&ffmpeg_bin())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-filter_complex",
            graph,
            "-map",
            "[o]",
            "-frames:v",
            "1",
        ])
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(run.status.success(), "{graph}\n{}", String::from_utf8_lossy(&run.stderr));
    run.stdout
}

/// The RGB of the pixel at `(x, y)` of a `yuv420p` frame, read as the composite is on both builds for untagged
/// footage: BT.601, limited range.
fn rgb(frame: &[u8], x: u32, y: u32) -> [f64; 3] {
    rgb_by(frame, x, y, YuvMatrix::Bt601)
}

/// [`rgb`] through `matrix`, limited range.
fn rgb_by(frame: &[u8], x: u32, y: u32, matrix: YuvMatrix) -> [f64; 3] {
    let (w, h) = (W as usize, H as usize);
    let luma = f64::from(frame[y as usize * w + x as usize]);
    let c = (y as usize / 2) * (w / 2) + x as usize / 2;
    let u = f64::from(frame[w * h + c]) - 128.0;
    let v = f64::from(frame[w * h + w * h / 4 + c]) - 128.0;
    let yy = (luma - 16.0) * 255.0 / 219.0;
    let (cb, cr) = (u * 255.0 / 224.0, v * 255.0 / 224.0);
    let (kr, kb) = matrix.weights();
    let (r, b) = (yy + 2.0 * (1.0 - kr) * cr, yy + 2.0 * (1.0 - kb) * cb);
    let g = (yy - kr * r - kb * b) / (1.0 - kr - kb);
    [r, g, b].map(|c| c.clamp(0.0, 255.0))
}

/// The worst channel of `got` off `mode` of `top` over `bottom` at `opacity`, all read through the same matrix.
fn off_formula(mode: BlendMode, got: [f64; 3], top: [f64; 3], bottom: [f64; 3], opacity: f64) -> f64 {
    (0..3)
        .map(|i| {
            let mixed = mode.apply(top[i] / 255.0, bottom[i] / 255.0) * 255.0;
            (got[i] - (opacity * mixed + (1.0 - opacity) * bottom[i])).abs()
        })
        .fold(0.0, f64::max)
}

/// The luma and chroma samples of `frame` that differ from `plain`'s, as `(x, y)` of the pixel or the chroma
/// block's top left.
fn moved(frame: &[u8], plain: &[u8]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let at = (y * W + x) as usize;
            let c = ((W * H) + (y / 2) * (W / 2) + x / 2) as usize;
            let q = (W * H / 4) as usize;
            if frame[at] != plain[at] || frame[c] != plain[c] || frame[c + q] != plain[c + q] {
                out.push((x, y));
            }
        }
    }
    out
}

/// A cut of `bottom` under `top` scaled to half and moved to the right half: the clip covers `x` 64..128,
/// `y` 18..54.
fn stacked(bottom: &Asset, top: &Asset, mode: BlendMode, opacity: f64) -> Timeline {
    let mut pip = make_clip(top.id, 0.0, 1.0, 0.0);
    pip.transform.scale = 0.5;
    pip.transform.pos_x = 0.25;
    pip.transform.opacity = opacity;
    pip.blend_mode = mode;
    timeline_of(vec![
        video_track(vec![make_clip(bottom.id, 0.0, 1.0, 0.0)]),
        video_track(vec![pip]),
    ])
}

fn inside(x: u32, y: u32) -> bool {
    (64..128).contains(&x) && (18..54).contains(&y)
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn every_mode_mixes_the_clip_with_the_composite_by_its_formula_and_leaves_the_rest() {
    let dir = scratch("blend-modes");
    let (bottom, top) = (solid(&dir, "0x3080C0"), solid(&dir, "0xC06020"));
    let assets = vec![bottom.clone(), top.clone()];
    let plain = yuv_frames(&stacked(&bottom, &top, BlendMode::Normal, 1.0), &assets, &dir, "normal").swap_remove(5);
    // The two colours as the composite holds them.
    let (b, a) = (rgb(&plain, 96, 36), rgb(&plain, 16, 36));
    let mut worst_seen = 0.0_f64;
    for mode in BlendMode::ALL.into_iter().filter(|m| !m.is_normal()) {
        for opacity in [1.0, 0.5] {
            let tag = format!("{}-{opacity}", mode.as_str());
            let frame = yuv_frames(&stacked(&bottom, &top, mode, opacity), &assets, &dir, &tag).swap_remove(5);
            let got = rgb(&frame, 96, 36);
            let want: Vec<f64> = (0..3)
                .map(|i| {
                    let mixed = mode.apply(b[i] / 255.0, a[i] / 255.0) * 255.0;
                    opacity * mixed + (1.0 - opacity) * a[i]
                })
                .collect();
            let worst = (0..3).map(|i| (got[i] - want[i]).abs()).fold(0.0, f64::max);
            eprintln!("{tag}: got {got:.1?} want {want:.1?} ({worst:.1} off)");
            worst_seen = worst_seen.max(worst);
            assert!(worst <= 4.0, "{tag}: {got:?} against {want:?}");
            // Outside the clip the composite is untouched, byte for byte.
            for y in (0..H).step_by(2) {
                for x in (0..W).step_by(2) {
                    if !inside(x, y) && !inside(x + 1, y + 1) {
                        let at = (y * W + x) as usize;
                        assert_eq!(frame[at], plain[at], "{tag}: luma at {x},{y}");
                        let c = ((W * H) + (y / 2) * (W / 2) + x / 2) as usize;
                        assert_eq!(frame[c], plain[c], "{tag}: chroma at {x},{y}");
                    }
                }
            }
        }
    }
    eprintln!("worst {worst_seen:.1} levels off the formula");
    // Multiply darkens and screen lightens, whatever the rounding.
    let m = rgb(
        &yuv_frames(&stacked(&bottom, &top, BlendMode::Multiply, 1.0), &assets, &dir, "m").swap_remove(5),
        96,
        36,
    );
    let s = rgb(
        &yuv_frames(&stacked(&bottom, &top, BlendMode::Screen, 1.0), &assets, &dir, "s").swap_remove(5),
        96,
        36,
    );
    for i in 0..3 {
        assert!(
            m[i] <= a[i].min(b[i]) + 2.0 && s[i] + 2.0 >= a[i].max(b[i]),
            "{m:?} {s:?} over {a:?} {b:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The graph itself with `normal` is the plain `overlay`: byte for byte for a flat colour, opaque or not, and to a
/// level of rounding for a smooth picture whose alpha ramps across it — the RGB round trip and the two placements
/// lose nothing more. (A 4:2:0 picture with sharp saturated edges, where a pixel's luma and its block's chroma make
/// no RGB colour, is clipped into the gamut on the way: up to ~25 levels at those edges of a test pattern. Any RGB
/// blend has to.) A rotated picture's luma is the plain overlay's byte for byte; along its slanted edge, where a 2x2
/// chroma block is only part covered, the blended frame takes the block's chroma from one pixel of it and the
/// `overlay` weighs it by the block's alpha (tens of levels, in those blocks only).
#[test]
#[ignore = "needs the ffmpeg binary"]
fn normal_through_the_blend_graph_is_the_plain_overlay() {
    let base = format!("testsrc2=s={W}x{H}:r=25:d=1,format=yuv420p[base]");
    let rotated = format!(
        "color=c=0xC04020:s=48x30:r=25:d=1,format=yuva420p,{}[v0]",
        static_rotate(20.0, true)
    );
    for (name, layer, within, chroma) in [
        ("opaque", "color=c=0xC04020:s=64x36:r=25:d=1,format=yuva420p[v0]", 0, true),
        (
            "translucent",
            "color=c=0x30A060@0.6:s=64x36:r=25:d=1,format=yuva420p[v0]",
            0,
            true,
        ),
        (
            "a smooth picture, alpha ramped",
            "color=c=black:s=64x36:r=25:d=1,format=yuva420p,geq=lum='60+X*2':cb='100+Y':cr='150-X/2':a='X*4'[v0]",
            1,
            true,
        ),
        (
            "a rotated picture, its corners filled as a blended clip's are",
            rotated.as_str(),
            0,
            false,
        ),
    ] {
        let placement = "overlay=x=32:y=18:eof_action=pass:enable='between(t,0,1)'";
        let plain = graph_frame(&format!("{base};{layer};[base][v0]{placement}[o]"));
        let blend = Blend {
            mode: "normal",
            placement,
            window: Some("between(t,0,1)"),
            matrix: None,
        };
        let blended = graph_frame(&format!(
            "{base};{layer};{}",
            blend_chains("base", "v0", &blend, "0", "o").join(";")
        ));
        assert_eq!(plain.len(), blended.len());
        let off = |range: std::ops::Range<usize>| {
            plain[range.clone()]
                .iter()
                .zip(&blended[range])
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0)
        };
        let luma = (W * H) as usize;
        let (worst, chroma_off) = (off(0..luma), off(luma..plain.len()));
        eprintln!("{name}: normal through the blend graph is {worst} off the plain overlay in luma, {chroma_off} in chroma");
        assert!(worst <= within, "{name}: {worst} levels off the plain overlay in luma");
        assert!(
            !chroma || chroma_off <= within,
            "{name}: {chroma_off} levels off the plain overlay in chroma"
        );
    }
}

/// A keyed opacity from 0 reads the composite exactly at its first frame and the whole mix at its last, and a
/// zoom that moves (a frame of another size every frame) still blends, over a growing area.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_keyed_opacity_and_a_moving_zoom_blend_frame_by_frame() {
    let dir = scratch("blend-keyed");
    let (bottom, top) = (solid(&dir, "0x3080C0"), solid(&dir, "0xC06020"));
    let assets = vec![bottom.clone(), top.clone()];
    let base = yuv_frames(&stacked(&bottom, &top, BlendMode::Normal, 0.0), &assets, &dir, "base");
    let full = yuv_frames(&stacked(&bottom, &top, BlendMode::Screen, 1.0), &assets, &dir, "full");

    let mut fading = stacked(&bottom, &top, BlendMode::Screen, 1.0);
    fading.tracks[1].clips[0].channels = vec![PropertyTrack {
        prop: Property::Opacity,
        keys: vec![PropertyKey::new(0.0, 0.0), PropertyKey::new(0.8, 1.0)],
    }];
    let frames = yuv_frames(&fading, &assets, &dir, "fading");
    assert!(frames[0] == base[0], "nothing of the clip at opacity 0");
    let late = rgb(&frames[22], 96, 36);
    let whole = rgb(&full[22], 96, 36);
    let worst = (0..3).map(|i| (late[i] - whole[i]).abs()).fold(0.0, f64::max);
    assert!(worst <= 2.0, "the whole mix past the last key: {late:?} {whole:?}");

    let mut zooming = stacked(&bottom, &top, BlendMode::Screen, 1.0);
    zooming.tracks[1].clips[0].channels = vec![PropertyTrack {
        prop: Property::Scale,
        keys: vec![PropertyKey::new(0.0, 0.25), PropertyKey::new(0.9, 0.5)],
    }];
    let frames = yuv_frames(&zooming, &assets, &dir, "zooming");
    let covered = |f: &[u8], b: &[u8]| f[..(W * H) as usize].iter().zip(b).filter(|(x, y)| x != y).count();
    let (early, late) = (covered(&frames[1], &base[1]), covered(&frames[23], &base[23]));
    eprintln!("zoom: {early} px blended at frame 1, {late} at frame 23");
    assert!(
        early > 0 && late > early * 3,
        "the blended area grows with the zoom: {early} {late}"
    );
    let centre = rgb(&frames[23], 96, 36);
    let worst = (0..3).map(|i| (centre[i] - whole[i]).abs()).fold(0.0, f64::max);
    assert!(worst <= 2.0, "and is the screen of the two: {centre:?} {whole:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A mask cuts the blended layer as it cuts a plain one: outside the shape the composite is untouched, inside it is
/// the mode's mix.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_mask_cuts_the_blended_layer() {
    use crate::model::{Mask, MaskShape};
    let dir = scratch("blend-mask");
    let (bottom, top) = (solid(&dir, "0x3080C0"), solid(&dir, "0xC06020"));
    let assets = vec![bottom.clone(), top.clone()];
    let cut = |mode| {
        let mut over = make_clip(top.id, 0.0, 1.0, 0.0);
        over.blend_mode = mode;
        over.mask = Some(Mask {
            shape: MaskShape::Ellipse,
            feather: 0.0,
            ..Mask::default()
        });
        timeline_of(vec![
            video_track(vec![make_clip(bottom.id, 0.0, 1.0, 0.0)]),
            video_track(vec![over]),
        ])
    };
    let plain = yuv_frames(&cut(BlendMode::Normal), &assets, &dir, "plain").swap_remove(5);
    let multiplied = yuv_frames(&cut(BlendMode::Multiply), &assets, &dir, "multiply").swap_remove(5);
    let (a, b) = (rgb(&plain, 2, 2), rgb(&plain, 64, 36));
    // The corners are outside the ellipse: the composite, byte for byte.
    for (x, y) in [(0, 0), (W - 2, 0), (0, H - 2), (W - 2, H - 2)] {
        let at = (y * W + x) as usize;
        assert_eq!(multiplied[at], plain[at], "luma at {x},{y}");
    }
    let got = rgb(&multiplied, 64, 36);
    let worst = (0..3)
        .map(|i| (got[i] - BlendMode::Multiply.apply(b[i] / 255.0, a[i] / 255.0) * 255.0).abs())
        .fold(0.0, f64::max);
    assert!(
        worst <= 3.0,
        "inside the mask the multiply of the two: {got:?} over {a:?} by {b:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The scrubbed still at a moment is the export's frame there.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn the_still_draws_the_exports_blended_frame() {
    let dir = scratch("blend-still");
    let (bottom, top) = (solid(&dir, "0x3080C0"), solid(&dir, "0xC06020"));
    let assets = vec![bottom.clone(), top.clone()];
    for mode in [BlendMode::Multiply, BlendMode::Screen, BlendMode::Difference] {
        let worst = still_off_export(&stacked(&bottom, &top, mode, 0.7), &assets, &dir, mode.as_str(), 10);
        eprintln!("{}: the still is {worst} levels off the export at worst", mode.as_str());
        assert!(worst <= 3, "{}: {worst}", mode.as_str());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// How far the still of `cut` at output frame `frame`'s time is off the export's frame, at worst, in RGB.
fn still_off_export(cut: &Timeline, assets: &[Asset], dir: &Path, tag: &str, frame: usize) -> u8 {
    let export = export_frames_sized(cut, assets, 25.0, (W, H), dir, tag).swap_remove(frame);
    let t = frame as f64 / 25.0;
    let args = build_still_args(cut, assets, &opts(), t, W, None, &StillOutput::RgbPipe).unwrap();
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
    assert_eq!(run.stdout.len(), export.len());
    run.stdout
        .iter()
        .zip(&export)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(255)
}

/// Two clips on one track over `bottom`, both `mode`, meeting at 0.5 s through `transition` (none for a cut), the
/// first fading in and the second out by `fades`: full-frame layers, so every frame's centre is the mix.
fn faded(bottom: &Asset, top: &Asset, mode: BlendMode, fades: (f64, f64), transition: Option<Transition>) -> Timeline {
    let mut first = make_clip(top.id, 0.0, 0.5, 0.0);
    first.fade_in = fades.0;
    first.blend_mode = mode;
    let mut second = make_clip(top.id, 0.5, 1.0, 0.5);
    second.fade_out = fades.1;
    second.blend_mode = mode;
    second.transition_in = transition;
    timeline_of(vec![
        video_track(vec![make_clip(bottom.id, 0.0, 1.0, 0.0)]),
        video_track(vec![first, second]),
    ])
}

/// A fade or a dip on a blended clip weighs the mix: every frame's centre lies between the composite alone and
/// the whole mix, from the composite itself at the fade's first frame. (Faded as colour, a multiplied clip went
/// through black and a screened one through white.)
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_fade_or_a_dip_weighs_the_mix_and_never_goes_through_black_or_white() {
    let dir = scratch("blend-fades");
    let (bottom, top) = (solid(&dir, "0x3080C0"), solid(&dir, "0xE0E0E0"));
    let assets = vec![bottom.clone(), top.clone()];
    let mut lower = faded(&bottom, &top, BlendMode::Normal, (0.0, 0.0), None);
    lower.tracks.truncate(1);
    let alone = yuv_frames(&lower, &assets, &dir, "alone");
    let dip = |kind| Some(Transition { kind, duration: 0.4 });
    for (mode, fades, transition) in [
        (BlendMode::Multiply, (0.4, 0.4), None),
        (BlendMode::Multiply, (0.0, 0.0), dip(TransitionKind::DipToBlack)),
        (BlendMode::Screen, (0.0, 0.0), dip(TransitionKind::DipToWhite)),
        (BlendMode::Screen, (0.4, 0.0), dip(TransitionKind::DipToWhite)),
    ] {
        let tag = format!("{}-{}-{}", mode.as_str(), fades.0, transition.is_some());
        let full = yuv_frames(
            &faded(&bottom, &top, mode, (0.0, 0.0), None),
            &assets,
            &dir,
            &format!("{tag}-full"),
        );
        let cut = faded(&bottom, &top, mode, fades, transition);
        let frames = yuv_frames(&cut, &assets, &dir, &tag);
        assert_eq!(frames.len(), full.len(), "{tag}");
        let (b, f) = (rgb(&alone[0], 64, 36), rgb(&full[6], 64, 36));
        let mut lows = Vec::new();
        for (k, frame) in frames.iter().enumerate() {
            let c = rgb(frame, 64, 36);
            lows.push(c[0].round());
            for i in 0..3 {
                assert!(
                    c[i] + 2.0 >= b[i].min(f[i]) && c[i] <= b[i].max(f[i]) + 2.0,
                    "{tag}: frame {k} is {c:?}, outside the composite {b:?} and the mix {f:?}"
                );
            }
        }
        eprintln!("{tag}: red at the centre {lows:?}");
        if fades.0 > 0.0 {
            assert!(frames[0] == alone[0], "{tag}: the fade starts on the composite alone");
            let worst = still_off_export(&cut, &assets, &dir, &tag, 5);
            eprintln!("{tag}: the still mid-fade is {worst} levels off the export");
            assert!(worst <= 3, "{tag}: {worst}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// BT.709 footage is blended in BT.709: each mode's pixels against its formula read through that matrix, and the
/// composite outside the clip untouched byte for byte (no build converts the whole composite into the matrix it
/// is told), in the export and the still alike.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn bt709_footage_blends_in_its_own_matrix() {
    let dir = scratch("blend-709");
    let bottom = solid_as(&dir, "0x3080C0", (W, H), true);
    let top = solid_as(&dir, "0xC06020", (W, H), true);
    let assets = vec![bottom.clone(), top.clone()];
    let plain = yuv_frames(&stacked(&bottom, &top, BlendMode::Normal, 1.0), &assets, &dir, "normal").swap_remove(5);
    let (b, a) = (
        rgb_by(&plain, 96, 36, YuvMatrix::Bt709),
        rgb_by(&plain, 16, 36, YuvMatrix::Bt709),
    );
    for mode in [
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Difference,
        BlendMode::Overlay,
    ] {
        let cut = stacked(&bottom, &top, mode, 1.0);
        let frame = yuv_frames(&cut, &assets, &dir, mode.as_str()).swap_remove(5);
        let got = rgb_by(&frame, 96, 36, YuvMatrix::Bt709);
        let worst = off_formula(mode, got, b, a, 1.0);
        eprintln!("{} in BT.709: {got:.1?}, {worst:.1} off the formula", mode.as_str());
        assert!(worst <= 3.0, "{}: {got:?} over {a:?} by {b:?}", mode.as_str());
        assert!(
            moved(&frame, &plain)
                .iter()
                .all(|&(x, y)| inside(x, y) || inside(x + 1, y + 1)),
            "{}: the composite outside the clip moved",
            mode.as_str()
        );
        let worst = still_off_export(&cut, &assets, &dir, mode.as_str(), 10);
        assert!(worst <= 3, "{}: the still is {worst} off the export", mode.as_str());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Only the frames the clip can show on go through the blend: every other frame is the composite's, byte for
/// byte, and there are as many frames as without the clip.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn the_composite_outside_the_clips_window_is_untouched() {
    let dir = scratch("blend-window");
    let (bottom, top) = (solid(&dir, "0x3080C0"), solid(&dir, "0xC06020"));
    let assets = vec![bottom.clone(), top.clone()];
    let mut cut = stacked(&bottom, &top, BlendMode::Multiply, 1.0);
    let pip = &mut cut.tracks[1].clips[0];
    (pip.source_out, pip.timeline_start) = (0.3, 0.4);
    let alone = yuv_frames(&stacked(&bottom, &top, BlendMode::Normal, 0.0), &assets, &dir, "alone");
    let frames = yuv_frames(&cut, &assets, &dir, "window");
    assert_eq!(frames.len(), alone.len());
    let shown: Vec<usize> = (0..frames.len()).filter(|&k| frames[k] != alone[k]).collect();
    eprintln!("blended on frames {shown:?}");
    assert_eq!(
        shown,
        (10..18).collect::<Vec<_>>(),
        "the clip's 0.4 s to 0.7 s, and nothing else"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A blended clip letterboxed into the frame leaves the lower track alone in its bars: only its picture mixes. (Its
/// colour correction runs after the pad and keeps the bars' alpha.)
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_letterboxed_clip_leaves_the_lower_track_alone_in_its_bars() {
    let dir = scratch("blend-letterbox");
    let bottom = solid(&dir, "0x3080C0");
    let square = solid_as(&dir, "0xC06020", (H, H), false);
    let assets = vec![bottom.clone(), square.clone()];
    let cut = |mode| {
        let mut over = make_clip(square.id, 0.0, 1.0, 0.0);
        over.blend_mode = mode;
        over.color.brightness = 0.05;
        timeline_of(vec![
            video_track(vec![make_clip(bottom.id, 0.0, 1.0, 0.0)]),
            video_track(vec![over]),
        ])
    };
    let alone = yuv_frames(&stacked(&bottom, &square, BlendMode::Normal, 0.0), &assets, &dir, "alone").swap_remove(5);
    let plain = yuv_frames(&cut(BlendMode::Normal), &assets, &dir, "plain").swap_remove(5);
    let picture = |x: u32| (28..100).contains(&x);
    for mode in [BlendMode::Multiply, BlendMode::Screen] {
        let frame = yuv_frames(&cut(mode), &assets, &dir, mode.as_str()).swap_remove(5);
        let off: Vec<_> = moved(&frame, &alone).into_iter().filter(|&(x, _)| !picture(x)).collect();
        assert!(
            off.is_empty(),
            "{}: the bars moved the lower track at {:?}",
            mode.as_str(),
            &off[..off.len().min(8)]
        );
        let worst = off_formula(mode, rgb(&frame, 64, 36), rgb(&plain, 64, 36), rgb(&alone, 64, 36), 1.0);
        assert!(worst <= 4.0, "{}: the picture is the mix, {worst} off", mode.as_str());
        let worst = still_off_export(&cut(mode), &assets, &dir, mode.as_str(), 10);
        assert!(worst <= 3, "{}: the still is {worst} off the export", mode.as_str());
    }
    let _ = std::fs::remove_dir_all(&dir);
}
