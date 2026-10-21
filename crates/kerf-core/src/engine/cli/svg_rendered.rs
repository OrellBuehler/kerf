//! Animated SVG titles through the real `ffmpeg` (`#[ignore]`d: they drive the binary, on 6.1 and
//! 9.0 alike).
//!
//! A unit test on the graph string proves the text; these prove ffmpeg accepts it and puts the
//! picture on the right frame. The title is a box that slides across a 64x36 frame at 26 px a
//! second — a frame off is a pixel off, which the edge of a saturated box shows at once — over a
//! two-colour clip, and every comparison is pixel by pixel in rgb24:
//!
//! * every frame of an export is the scrubbed still of its moment (within 2 levels), at 24 / 25 /
//!   29.97 / 30 / 60 fps; frames outside the title are the cut without it, exactly; the title is on
//!   at both of its ends (`between`, closed) and the output has no more frames than it had;
//! * a range export carries the animation on from where it was cut, a two-pass export draws the
//!   titles once, a gif and a 10-bit export take them, the playback stream shows them, a
//!   directory with a `%` in its name works, and a translucent title partly off the frame is what
//!   the still draws.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored svg_rendered`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use uuid::Uuid;

use super::svg_layer;
use super::*;
use crate::clip_timing::Rational;
use crate::engine::test_support::{make_clip, single, test_asset, video_stream, StatusBounded};
use crate::model::{Asset, SvgOverlay, TimeRange};
use crate::svg::raster::with_pinned_dir;

const W: u32 = 64;
const H: u32 = 36;

/// Held by the tests that run a two-pass export: its pass log is named for the process
/// (`kerf-2pass-{pid}`, one export at a time in the app), so two of them side by side delete each
/// other's log between the passes.
fn one_two_pass_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-svg-rendered-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A lossless four second clip at `fps` (the text ffmpeg is given): blue-ish left, green-ish right.
fn background(dir: &Path, fps: &str) -> Asset {
    let path = dir.join(format!("bg-{fps}.mp4"));
    if !path.exists() {
        let chain = format!(
            "color=c=0x303060:s={W}x{H}:r={fps}:d=4,drawbox=x={}:y=0:w={}:h={H}:color=0x306030:t=fill,format=yuv420p",
            W / 2,
            W / 2
        );
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(chain)
            .args(["-c:v", "libx264", "-crf", "0", "-g", "1", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());
    }
    let mut asset = test_asset(vec![video_stream(W, H, fps.parse().unwrap())]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 4.0;
    asset
}

/// A saturated box that slides from the left edge to the right one in two seconds (26 px a second).
fn sliding_box() -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {W} {H}\"><rect y=\"12\" width=\"12\" height=\"12\" fill=\"#ff2040\">\
         <animate attributeName=\"x\" from=\"0\" to=\"52\" dur=\"2s\" fill=\"freeze\"/></rect></svg>"
    )
}

fn title(svg: String, start: f64, end: f64) -> SvgOverlay {
    SvgOverlay {
        id: Uuid::new_v4(),
        name: String::new(),
        svg,
        start,
        end,
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
        opacity: 1.0,
        offset: 0.0,
        preset: None,
        template: None,
    }
}

/// A four second cut of the background with `titles` over it.
fn cut(bg: &Asset, titles: Vec<SvgOverlay>) -> Timeline {
    let mut tl = single(vec![make_clip(bg.id, 0.0, 4.0, 0.0)]);
    tl.svg_overlays = titles;
    tl
}

fn options(fps: f64) -> ExportOptions {
    ExportOptions {
        resolution: Some((W, H)),
        fps: Some(fps),
        ..ExportOptions::default()
    }
}

fn run_ok(args: &[String]) -> std::process::Output {
    let run = command(&ffmpeg_bin()).args(args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    run
}

/// Every frame of the export of `timeline` as raw rgb24, the way `render_with_progress` would draw
/// it (the titles prepared first, the real argv) with the encoder and the sink replaced.
fn export_rgb(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, dir: &Path, tag: &str) -> Vec<Vec<u8>> {
    let _pins = svg_layer::prepare_export(timeline, assets, opts, &mut |_| {}, &|| false)
        .unwrap()
        .expect("not cancelled");
    let out = dir.join(format!("{tag}.rgb"));
    let mut args = build_export_args(timeline, assets, "unused.mkv", opts).unwrap();
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
            "rgb24",
            "-y",
        ]
        .map(String::from),
    );
    args.push(out.to_string_lossy().into_owned());
    run_ok(&args);
    let raw = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    raw.chunks((W * H * 3) as usize).map(<[u8]>::to_vec).collect()
}

/// The scrubbed still of `timeline` at `t` as raw rgb24, through `run_still` (so its titles are
/// drawn first, as an agent's `preview_timeline` has them).
fn still_rgb(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, t: f64) -> Vec<u8> {
    let out = run_still(timeline, assets, opts, t, W, None, &StillOutput::RgbPipe, &|| false).unwrap();
    assert_eq!(out.len(), (W * H * 3) as usize);
    out
}

fn distance(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

/// The column the box starts at in `frame` (row 18), `None` when there is no box on it.
fn box_left(frame: &[u8]) -> Option<u32> {
    (0..W).find(|&x| {
        let p = &frame[((18 * W + x) * 3) as usize..][..3];
        p[0] > 150 && p[1] < 110 && p[2] < 130
    })
}

const RATES: [(&str, f64); 5] = [("24", 24.0), ("25", 25.0), ("29.97", 29.97), ("30", 30.0), ("60", 60.0)];

#[test]
#[ignore = "drives the real ffmpeg"]
fn every_export_frame_is_the_scrubbed_still_of_its_moment() {
    let dir = scratch("frames");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        for (text, fps) in RATES {
            let bg = background(&dir, text);
            let assets = [bg.clone()];
            let with = cut(&bg, vec![title(sliding_box(), 0.5, 2.5)]);
            let mut without = with.clone();
            without.svg_overlays.clear();
            let opts = options(fps);
            let rate = Rational::from_fps(fps).unwrap();
            let titled = export_rgb(&with, &assets, &opts, &dir, &format!("with-{text}"));
            let plain = export_rgb(&without, &assets, &opts, &dir, &format!("without-{text}"));
            assert_eq!(titled.len(), plain.len(), "{text}: the title adds no frame");
            let mut present = 0;
            for (k, (a, b)) in titled.iter().zip(&plain).enumerate() {
                let time = rate.frame_time(k as u64);
                let on = (0.5..=2.5).contains(&time);
                if !on {
                    assert_eq!(
                        a, b,
                        "{text}: frame {k} ({time:.4} s) is outside the title and must be the plain cut"
                    );
                    continue;
                }
                present += 1;
                assert_ne!(a, b, "{text}: frame {k} ({time:.4} s) has no title");
                let left = box_left(a).unwrap_or_else(|| panic!("{text}: frame {k} has no box"));
                let want = 26.0 * (rate.exact_time(k as u64) - 0.5).max(0.0);
                assert!(
                    (f64::from(left) - want).abs() <= 1.6,
                    "{text}: frame {k}: the box starts at {left}, wanted {want:.2}"
                );
                // The still stops before the title's last instant (as a text overlay's does).
                if time < 2.5 {
                    let still = still_rgb(&with, &assets, &opts, rate.exact_time(k as u64));
                    let worst = distance(a, &still);
                    assert!(worst <= 2, "{text}: frame {k} is {worst} levels off its still");
                }
            }
            let want_frames = (0..titled.len() as u64)
                .filter(|k| (0.5..=2.5).contains(&rate.frame_time(*k)))
                .count();
            assert_eq!(present, want_frames);
            assert!(present > 40, "{text}: {present}");
        }
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_title_is_on_at_both_of_its_ends_and_the_frames_beside_them_are_plain() {
    let dir = scratch("ends");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "25");
        let assets = [bg.clone()];
        let rate = Rational::from_fps(25.0).unwrap();
        // The start and end are exactly the graph's time of frames 13 and 50.
        let with = cut(&bg, vec![title(sliding_box(), rate.frame_time(13), rate.frame_time(50))]);
        let mut without = with.clone();
        without.svg_overlays.clear();
        let opts = options(25.0);
        let a = export_rgb(&with, &assets, &opts, &dir, "with");
        let b = export_rgb(&without, &assets, &opts, &dir, "without");
        assert_eq!(a[12], b[12], "the frame before the start");
        assert_ne!(a[13], b[13], "the first frame");
        assert_ne!(a[50], b[50], "the last frame is drawn: `between` is closed");
        assert_eq!(a[51], b[51], "and the one after it is not");
        assert_eq!(box_left(&a[13]), Some(0));
        let _ = Rational::from_fps(25.0);
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_range_export_carries_the_animation_on_from_where_it_was_cut() {
    let dir = scratch("range");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "24");
        let assets = [bg.clone()];
        let with = cut(&bg, vec![title(sliding_box(), 0.5, 3.5)]);
        let full = export_rgb(&with, &assets, &options(24.0), &dir, "full");
        let ranged_opts = ExportOptions {
            range: Some(TimeRange { start: 1.0, end: 3.0 }),
            ..options(24.0)
        };
        let ranged = export_rgb(&with, &assets, &ranged_opts, &dir, "range");
        assert_eq!(ranged.len(), 48);
        for k in 0..ranged.len() {
            let worst = distance(&ranged[k], &full[k + 24]);
            assert!(
                worst <= 2,
                "frame {k} of the range is {worst} levels off frame {} of the whole",
                k + 24
            );
        }
        assert!(
            box_left(&ranged[0]).is_some_and(|x| (12..=14).contains(&x)),
            "it starts a half second in, not at its first pose: {:?}",
            box_left(&ranged[0])
        );
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_translucent_title_partly_off_the_frame_is_what_the_still_draws() {
    let dir = scratch("placed");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "30");
        let assets = [bg.clone()];
        let mut t = title(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 36\"><rect width=\"64\" height=\"36\" fill=\"#ff2040\"/>\
             <text x=\"6\" y=\"24\" font-size=\"18\" fill=\"#ffffff\">Kerf</text></svg>"
                .to_string(),
            0.5,
            2.0,
        );
        (t.x, t.y, t.width, t.height, t.opacity) = (-0.25, 0.25, 0.75, 0.5, 0.5);
        let with = cut(&bg, vec![t]);
        let opts = options(30.0);
        let frames = export_rgb(&with, &assets, &opts, &dir, "placed");
        let still = still_rgb(&with, &assets, &opts, 1.0);
        let worst = distance(&frames[30], &still);
        assert!(worst <= 2, "{worst} levels off");
        // Half transparent: the red box is blended with the 0x303060 behind it, not opaque.
        let p = &frames[30][((18 * W + 8) * 3) as usize..][..3];
        assert!(p[0] > 110 && p[0] < 190, "{p:?}");
        // And outside its rectangle the cut is as it was.
        let corner = &frames[30][(((H - 1) * W + W - 1) * 3) as usize..][..3];
        assert!(
            corner
                .iter()
                .zip([0x30u8, 0x60, 0x30])
                .all(|(got, want)| got.abs_diff(want) <= 2),
            "{corner:?}"
        );
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_percent_in_the_cache_directory_is_escaped_for_the_image_demuxer() {
    let dir = scratch("percent");
    let cache = dir.join("100%25 cache");
    with_pinned_dir(cache.to_str().unwrap(), || {
        let bg = background(&dir, "24");
        let assets = [bg.clone()];
        let with = cut(&bg, vec![title(sliding_box(), 0.5, 1.5)]);
        let frames = export_rgb(&with, &assets, &options(24.0), &dir, "percent");
        assert!(box_left(&frames[24]).is_some(), "the title is there");
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// A box that does not move: the same picture at every moment.
fn held_box() -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {W} {H}\"><rect x=\"20\" y=\"12\" width=\"12\" height=\"12\" fill=\"#ff2040\"/></svg>"
    )
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_title_that_cannot_move_is_one_looped_picture_and_the_export_ends_with_the_cut() {
    let dir = scratch("held");
    let cache = dir.join("cache");
    with_pinned_dir(cache.to_str().unwrap(), || {
        for (text, fps) in [("24", 24.0), ("29.97", 29.97)] {
            let bg = background(&dir, text);
            let assets = [bg.clone()];
            let with = cut(&bg, vec![title(held_box(), 0.5, 2.5)]);
            let mut without = with.clone();
            without.svg_overlays.clear();
            let opts = options(fps);
            let rate = Rational::from_fps(fps).unwrap();
            let titled = export_rgb(&with, &assets, &opts, &dir, &format!("held-{text}"));
            let plain = export_rgb(&without, &assets, &opts, &dir, &format!("plain-{text}"));
            // The loop is infinite and the cut is not: the export ends where the cut does.
            assert_eq!(titled.len(), plain.len(), "{text}: the looped picture adds no frame");
            let mut on = 0;
            for (k, (a, b)) in titled.iter().zip(&plain).enumerate() {
                let time = rate.frame_time(k as u64);
                if (0.5..=2.5).contains(&time) {
                    on += 1;
                    assert_eq!(box_left(a), Some(20), "{text}: frame {k} ({time:.4} s)");
                    if time < 2.5 {
                        let still = still_rgb(&with, &assets, &opts, rate.exact_time(k as u64));
                        assert!(distance(a, &still) <= 2, "{text}: frame {k} is not the still");
                    }
                } else {
                    assert_eq!(a, b, "{text}: frame {k} ({time:.4} s) is outside the title");
                }
            }
            assert!(on > 40, "{text}: {on}");
        }
        // One picture per fps-independent title, not one a frame.
        let files: usize = std::fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|d| {
                std::fs::read_dir(d.path())
                    .unwrap()
                    .flatten()
                    .filter(|f| f.path().extension().is_some())
                    .count()
            })
            .sum();
        assert_eq!(files, 1, "both rates read the one picture");
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_held_title_is_in_the_stream_and_in_a_two_pass_export() {
    let _one = one_two_pass_at_a_time();
    let dir = scratch("held-stream");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "24");
        let assets = [bg.clone()];
        let with = cut(&bg, vec![title(held_box(), 0.5, 2.5)]);
        // From half a second before the title ends: it is on for the first twelve frames (and the
        // thirteenth, `between` being closed), then gone.
        let frames = play(&with, &assets, 2.0, 24);
        assert_eq!(frames.len(), 24);
        assert!(frames[..12].iter().all(|f| box_left(f) == Some(20)), "the rest of the window");
        assert!(
            frames[14..].iter().all(|f| box_left(f).is_none()),
            "and nothing after it ends"
        );
        let early = play(&with, &assets, 0.0, 18);
        assert!(early[..11].iter().all(|f| box_left(f).is_none()), "nothing before it starts");
        assert!(
            early[13..].iter().all(|f| box_left(f) == Some(20)),
            "and the looped picture from its first frame"
        );
        let out = dir.join("two-pass.mp4");
        let opts = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".to_string()),
            rate_control: RateControl::TwoPass,
            video_bitrate: Some("2M".to_string()),
            include_audio: false,
            ..options(24.0)
        };
        render_with_progress(&with, &assets, &out, &opts, &mut |_| {}, &|| false).unwrap();
        let decoded = decode(&out);
        assert_eq!(decoded.len(), 96);
        assert!(decoded[14..58].iter().all(|f| box_left(f).is_some()));
        assert!(decoded[..10].iter().all(|f| box_left(f).is_none()) && decoded[62..].iter().all(|f| box_left(f).is_none()));
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_cache_path_with_pattern_characters_is_one_file_for_stills_and_held_pictures() {
    let dir = scratch("oddpath");
    // `%d` is a frame number to the image demuxer, `[` `]` `*` `?` a glob: none is in the path of a file.
    // Windows has no file named with `*` or `?`, so there the glob is the `[1]` alone.
    let cache = dir.join(if cfg!(windows) { "a%d [1] cache" } else { "a%d [1]*? cache" });
    with_pinned_dir(cache.to_str().unwrap(), || {
        let bg = background(&dir, "24");
        let assets = [bg.clone()];
        let moving = cut(&bg, vec![title(sliding_box(), 0.5, 3.5)]);
        let opts = options(24.0);
        let still = still_rgb(&moving, &assets, &opts, 1.5);
        assert!(box_left(&still).is_some(), "the scrubbed still has its title");
        let held = cut(&bg, vec![title(held_box(), 0.5, 2.5)]);
        let frames = export_rgb(&held, &assets, &opts, &dir, "oddpath");
        assert_eq!(box_left(&frames[24]), Some(20), "and so does the looped picture");
        assert_eq!(box_left(&still_rgb(&held, &assets, &opts, 1.0)), Some(20));
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_ten_bit_export_takes_the_titles_too() {
    let dir = scratch("tenbit");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "24");
        let assets = [bg.clone()];
        let with = cut(&bg, vec![title(sliding_box(), 0.5, 2.5)]);
        let eight = export_rgb(&with, &assets, &options(24.0), &dir, "8");
        let ten_opts = ExportOptions {
            pix_fmt: Some("yuv422p10le".to_string()),
            ..options(24.0)
        };
        let ten = export_rgb(&with, &assets, &ten_opts, &dir, "10");
        assert_eq!(ten.len(), eight.len());
        for k in [0, 12, 24, 36, 48] {
            let worst = distance(&ten[k], &eight[k]);
            assert!(worst <= 6, "frame {k}: {worst} levels off the 8-bit export");
        }
        assert!(box_left(&ten[30]).is_some());
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// Frames of the real `render_with_progress` read back as rgb24.
fn decode(file: &Path) -> Vec<Vec<u8>> {
    let run = run_ok(
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            file.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ]
        .map(String::from),
    );
    run.stdout.chunks((W * H * 3) as usize).map(<[u8]>::to_vec).collect()
}

fn sequences_in(root: &Path) -> usize {
    std::fs::read_dir(root)
        .map(|rd| rd.flatten().filter(|e| e.path().join("complete").is_file()).count())
        .unwrap_or(0)
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_two_pass_export_draws_its_titles_once_and_both_passes_read_them() {
    let _one = one_two_pass_at_a_time();
    let dir = scratch("twopass");
    let cache = dir.join("cache");
    with_pinned_dir(cache.to_str().unwrap(), || {
        let bg = background(&dir, "24");
        let assets = [bg.clone()];
        let with = cut(&bg, vec![title(sliding_box(), 0.5, 2.5)]);
        let out = dir.join("two-pass.mp4");
        let opts = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".to_string()),
            rate_control: RateControl::TwoPass,
            video_bitrate: Some("2M".to_string()),
            include_audio: false,
            ..options(24.0)
        };
        let mut waited = Vec::new();
        let status = render_with_progress(&with, &assets, &out, &opts, &mut |p| waited.extend(p.waiting), &|| false).unwrap();
        assert_eq!(status, RenderStatus::Completed);
        assert_eq!(sequences_in(&cache), 1, "drawn once for both passes");
        assert!(waited.contains(&svg_layer::RENDERING), "{waited:?}");
        let frames = decode(&out);
        assert_eq!(frames.len(), 96);
        let lefts: Vec<Option<u32>> = frames.iter().map(|f| box_left(f)).collect();
        assert!(lefts[..11].iter().all(Option::is_none), "nothing before the start: {lefts:?}");
        assert!(lefts[12..60].iter().all(Option::is_some), "the box through its window");
        assert!(lefts[61..].iter().all(Option::is_none), "and nothing after: {lefts:?}");
        let xs: Vec<u32> = lefts[12..60].iter().flatten().copied().collect();
        assert!(
            xs.windows(2).all(|w| w[0] <= w[1]) && xs[0] < 4 && *xs.last().unwrap() > 20,
            "{xs:?}"
        );

        // A second export of the same cut finds its titles drawn: no waiting, no new sequence.
        let again = dir.join("again.mp4");
        let mut said = Vec::new();
        render_with_progress(&with, &assets, &again, &opts, &mut |p| said.extend(p.waiting), &|| false).unwrap();
        assert!(!said.contains(&svg_layer::RENDERING), "{said:?}");
        assert_eq!(sequences_in(&cache), 1);
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_cancelled_export_stops_drawing_titles_and_writes_nothing() {
    let dir = scratch("cancel");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "24");
        let assets = [bg.clone()];
        let with = cut(&bg, vec![title(sliding_box(), 0.0, 4.0)]);
        let out = dir.join("cancelled.mp4");
        let opts = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".to_string()),
            include_audio: false,
            ..options(24.0)
        };
        let status = render_with_progress(&with, &assets, &out, &opts, &mut |_| {}, &|| true).unwrap();
        assert_eq!(status, RenderStatus::Cancelled);
        assert!(!out.exists());
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_gif_export_puts_the_titles_ahead_of_the_palette() {
    let dir = scratch("gif");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "24");
        let assets = [bg.clone()];
        let with = cut(&bg, vec![title(sliding_box(), 0.5, 2.5)]);
        let out = dir.join("out.gif");
        let opts = ExportOptions {
            container: Container::Gif,
            video_codec: Some("gif".to_string()),
            include_audio: false,
            ..options(24.0)
        };
        let status = render_with_progress(&with, &assets, &out, &opts, &mut |_| {}, &|| false).unwrap();
        assert_eq!(status, RenderStatus::Completed);
        let frames = decode(&out);
        assert_eq!(frames.len(), 96);
        assert!(box_left(&frames[0]).is_none() && box_left(&frames[36]).is_some());
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// `frames` frames of the playback stream of `with` from `from` at 24 fps, decoded to rgb24.
fn play(with: &Timeline, assets: &[Asset], from: f64, frames: usize) -> Vec<Vec<u8>> {
    let mut jpegs = Vec::new();
    let request = PlaybackRequest {
        fps: Some(24.0),
        max_width: Some(W),
    };
    stream_preview_with(with, assets, from, request, &mut |f| {
        jpegs.push(f.jpeg);
        jpegs.len() < frames
    })
    .unwrap();
    // Decode the pictures through ffmpeg: concatenated JPEGs are an image2pipe stream.
    let mut child = command(&ffmpeg_bin())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "image2pipe",
            "-c:v",
            "mjpeg",
            "-i",
            "-",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let feeder = std::thread::spawn(move || {
        for j in jpegs {
            stdin.write_all(&j).unwrap();
        }
    });
    let out = child.wait_with_output().unwrap();
    feeder.join().unwrap();
    out.stdout.chunks((W * H * 3) as usize).map(<[u8]>::to_vec).collect()
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn the_playback_stream_shows_the_titles_and_carries_the_animation_on_from_the_playhead() {
    let dir = scratch("stream");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "24");
        let assets = [bg.clone()];
        let with = cut(&bg, vec![title(sliding_box(), 0.5, 3.5)]);
        let from_start = play(&with, &assets, 0.0, 72);
        assert_eq!(from_start.len(), 72);
        assert!(box_left(&from_start[6]).is_none(), "before the start");
        let xs: Vec<u32> = from_start[13..70].iter().flat_map(|f| box_left(f)).collect();
        assert!(xs.len() > 50, "the box is there through its window: {}", xs.len());
        assert!(xs.windows(2).all(|w| w[0] <= w[1] + 1), "and moves right: {xs:?}");
        // From the middle of the title: it is already part-way across, not back at the edge.
        let from_middle = play(&with, &assets, 1.5, 12);
        assert_eq!(
            box_left(&from_middle[0]).map(|x| x / 4),
            Some(26 / 4),
            "{:?}",
            box_left(&from_middle[0])
        );
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn streams_from_other_playheads_read_the_pictures_one_stream_drew() {
    let dir = scratch("replay");
    let cache = dir.join("cache");
    with_pinned_dir(cache.to_str().unwrap(), || {
        let bg = background(&dir, "24");
        let assets = [bg.clone()];
        let with = cut(&bg, vec![title(sliding_box(), 0.5, 3.5)]);
        // From the middle first: only the rest of the title is drawn, and no sequence is whole.
        let middle = play(&with, &assets, 1.5, 24);
        assert_eq!(sequences_in(&cache), 0, "a part of the title is not a finished sequence");
        let dirs = |root: &Path| {
            std::fs::read_dir(root)
                .unwrap()
                .flatten()
                .filter(|e| e.path().is_dir())
                .count()
        };
        assert_eq!(dirs(&cache), 1);
        // From before the title: the rest of the same directory is drawn, and it is whole.
        let early = play(&with, &assets, 0.0, 72);
        assert_eq!(
            (dirs(&cache), sequences_in(&cache)),
            (1, 1),
            "one sequence for the title, whatever the playhead"
        );
        // The same moment from either start: the same place for the box.
        for i in 0..24 {
            let (a, b) = (box_left(&middle[i]), box_left(&early[36 + i]));
            assert!(
                a.zip(b).is_some_and(|(a, b)| a.abs_diff(b) <= 1),
                "frame {i} of the stream from 1.5 s is {a:?}, the same moment of the one from 0 is {b:?}"
            );
        }
        // And from the middle again, from a frame of the title's second half: still the one directory.
        let _ = play(&with, &assets, 2.5, 6);
        assert_eq!((dirs(&cache), sequences_in(&cache)), (1, 1));
        // A playhead between frames has the exact sequence of its own.
        let off = play(&with, &assets, 1.51, 6);
        assert_eq!(dirs(&cache), 2);
        assert!(box_left(&off[0]).is_some());
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_text_heavy_title_is_drawn_inside_the_streams_budget() {
    let dir = scratch("budget");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "30");
        let _ = bg;
        // A lower third for ten seconds at the preview's default width, thirty frames a second.
        let preset = crate::svg::render_preset(
            crate::model::SvgPresetKind::LowerThird,
            &crate::model::SvgPresetParams {
                subtitle: Some("Director of photography".to_string()),
                ..crate::model::SvgPresetParams::new("Jane Doe")
            },
            10.0,
            (1920, 1080),
        );
        let mut tl = single(vec![make_clip(Uuid::new_v4(), 0.0, 12.0, 0.0)]);
        tl.svg_overlays = vec![title(preset, 0.0, 10.0)];
        let fmt = ExportFormat {
            width: 960,
            height: 540,
            fps: 30.0,
            ..ExportFormat::default()
        };
        let started = std::time::Instant::now();
        let seqs = svg_layer::svg_sequences(&tl, &fmt);
        let pins = svg_layer::prepare_preview(&seqs, &|| false).unwrap();
        let took = started.elapsed();
        eprintln_timing(took, 301);
        assert!(took < std::time::Duration::from_secs(10), "{took:?} for 301 frames at 960 px");
        drop(pins);
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// The pixel at the middle of an exported frame.
fn centre(frame: &[u8]) -> [u8; 3] {
    let at = (((H / 2) * W + W / 2) * 3) as usize;
    [frame[at], frame[at + 1], frame[at + 2]]
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_template_title_is_exported_with_the_values_it_was_set_to() {
    use crate::template::fixtures::template;
    let dir = scratch("template");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "30");
        let assets = [bg.clone()];
        let t = template();
        let drawn = |brand: Option<&str>, tag: &str| {
            let values = brand
                .map(|b| [("brand".to_string(), b.to_string())].into())
                .unwrap_or_default();
            let prepared = t.prepare(&values).unwrap();
            let tl = cut(&bg, vec![title(prepared.doc.sanitized().to_string(), 0.0, 4.0)]);
            let frames = export_rgb(&tl, &assets, &options(30.0), &dir, tag);
            centre(&frames[60])
        };
        // The fixture's backdrop fills the whole title; its colour is the control.
        let as_made = drawn(None, "made");
        let red = drawn(Some("#ff0000"), "red");
        let green = drawn(Some("lime"), "green");
        let close = |got: [u8; 3], want: [u8; 3]| got.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= 8);
        assert!(close(as_made, [0x10, 0x20, 0x30]), "as made: {as_made:?}");
        assert!(close(red, [255, 0, 0]), "red: {red:?}");
        assert!(close(green, [0, 255, 0]), "lime: {green:?}");
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_text_set_through_a_control_still_fades_in() {
    use crate::template::fixtures::template;
    let dir = scratch("template-fade");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "30");
        let assets = [bg.clone()];
        let t = template();
        // The fixture's text fades in with a SMIL `animate` that sits inside the element a control sets.
        let drawn = |values: &[(&str, &str)], tag: &str| {
            let values = values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            let prepared = t.prepare(&values).unwrap();
            assert!(prepared.doc.is_animated(), "{tag}: the animation survived the splice");
            let tl = cut(&bg, vec![title(prepared.doc.sanitized().to_string(), 0.0, 4.0)]);
            export_rgb(&tl, &assets, &options(30.0), &dir, tag)
        };
        for (values, tag) in [(&[][..], "made"), (&[("tagline", "Bye")][..], "set")] {
            let frames = drawn(values, tag);
            assert_ne!(
                frames[0], frames[60],
                "{tag}: no text at the first frame, text at two seconds"
            );
            assert_eq!(frames[60], frames[90], "{tag}: and it holds once faded in");
        }
        // Changing the words changes the picture: the text that is drawn is the text that was set.
        assert_ne!(drawn(&[], "words-a")[60], drawn(&[("tagline", "Bye")], "words-b")[60]);
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// A text typed out letter by letter (a text animation compiled into per-glyph SMIL), through the real export.
fn typed_title(animation: &crate::svg::TextAnimation) -> String {
    let art = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {W} {H}\"><text id=\"t\" x=\"3\" y=\"26\" font-family=\"Geist\" \
         font-size=\"22\" font-weight=\"bold\" fill=\"#ffffff\" data-kerf-animate='{}'>Kerf</text></svg>",
        animation.to_attribute()
    );
    let doc = crate::svg::SvgDoc::parse(&art)
        .unwrap()
        .outlined(&crate::fonts::svg_fontdb())
        .unwrap();
    assert!(
        doc.is_animated() && doc.sanitized().contains("<animate"),
        "{}",
        doc.sanitized()
    );
    doc.sanitized().to_string()
}

fn white(frame: &[u8]) -> usize {
    frame.chunks(3).filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200).count()
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_typed_text_arrives_letter_by_letter_in_the_export_and_each_frame_is_its_still() {
    let dir = scratch("typed");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "30");
        let assets = [bg.clone()];
        let mut typewriter = crate::svg::text_animation_preset("typewriter").unwrap().animation;
        typewriter.duration = 1.0;
        let with = cut(&bg, vec![title(typed_title(&typewriter), 0.0, 3.0)]);
        let mut without = with.clone();
        without.svg_overlays.clear();
        let opts = options(30.0);
        let typed = export_rgb(&with, &assets, &opts, &dir, "typed");
        let plain = export_rgb(&without, &assets, &opts, &dir, "plain");
        assert_eq!(typed.len(), plain.len());
        assert_eq!(typed[0], plain[0], "every letter starts hidden");
        let counts: Vec<usize> = typed[..=30].iter().map(|f| white(f)).collect();
        assert!(counts.windows(2).all(|w| w[0] <= w[1]), "letters only arrive: {counts:?}");
        assert_eq!(counts[0], 0);
        assert!(counts[15] > 0 && counts[15] < counts[30], "half way: {counts:?}");
        assert_eq!(typed[45], typed[85], "and the text holds once typed");
        assert_ne!(typed[45], plain[45]);
        for k in [4usize, 11, 19, 27, 36] {
            let still = still_rgb(&with, &assets, &opts, k as f64 / 30.0);
            let worst = distance(&typed[k], &still);
            assert!(worst <= 2, "frame {k} is {worst} levels off its still");
        }
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_wiggling_blurred_text_is_the_same_picture_in_every_export_of_it() {
    let dir = scratch("wiggle");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "30");
        let assets = [bg.clone()];
        let mut animation = crate::svg::text_animation_preset("wiggle").unwrap().animation;
        animation.properties.blur = 0.15;
        animation.duration = 2.0;
        let with = cut(&bg, vec![title(typed_title(&animation), 0.0, 3.0)]);
        let opts = options(30.0);
        let first = export_rgb(&with, &assets, &opts, &dir, "first");
        assert_eq!(
            first,
            export_rgb(&with, &assets, &opts, &dir, "second"),
            "seeded: the same every time"
        );
        assert_eq!(first[0], first[89], "it starts and ends at rest");
        assert_ne!(first[0], first[30], "and moves in between");
        for k in [10usize, 25, 41] {
            let still = still_rgb(&with, &assets, &opts, k as f64 / 30.0);
            assert!(distance(&first[k], &still) <= 2, "frame {k}");
        }
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_sprung_text_grows_past_its_size_and_settles_in_the_export_and_each_frame_is_its_still() {
    use crate::svg::{curve_preset, Offset, Properties, RangeSelector, Shape, TextAnimation};
    let dir = scratch("sprung");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "30");
        let assets = [bg.clone()];
        // Four letters that grow from nothing, each taking a second over a two second animation.
        let grow = TextAnimation {
            duration: 2.0,
            range: RangeSelector {
                start: 0.0,
                end: 1.0,
                offset: Offset { from: -1.0, to: 1.0 },
                shape: Shape::RampUp,
            },
            properties: Properties {
                scale: 0.0,
                ..Properties::default()
            },
            ..TextAnimation::default()
        };
        let sprung = grow.with_motion_curve(curve_preset("spring").unwrap().curve).unwrap();
        let opts = options(30.0);
        let straight_cut = cut(&bg, vec![title(typed_title(&grow), 0.0, 3.0)]);
        let sprung_cut = cut(&bg, vec![title(typed_title(&sprung), 0.0, 3.0)]);
        let straight = export_rgb(&straight_cut, &assets, &opts, &dir, "straight");
        let sprung_frames = export_rgb(&sprung_cut, &assets, &opts, &dir, "sprung");
        assert_eq!(
            export_rgb(&sprung_cut, &assets, &opts, &dir, "again"),
            sprung_frames,
            "the same every time"
        );
        let (rest_straight, rest_sprung) = (white(&straight[89]), white(&sprung_frames[89]));
        assert_eq!(straight[89], sprung_frames[89], "both end as the text is");
        assert_eq!(white(&straight[0]), 0);
        let most = |frames: &[Vec<u8>]| frames[..80].iter().map(|f| white(f)).max().unwrap();
        assert!(
            (most(&straight) as f64) <= rest_straight as f64 * 1.05,
            "straight: {} vs {rest_straight}",
            most(&straight)
        );
        assert!(
            (most(&sprung_frames) as f64) > rest_sprung as f64 * 1.1,
            "a spring grows past the size it settles at: {} vs {rest_sprung}",
            most(&sprung_frames)
        );
        for k in [8usize, 19, 31, 44, 58] {
            let still = still_rgb(&sprung_cut, &assets, &opts, k as f64 / 30.0);
            let worst = distance(&sprung_frames[k], &still);
            assert!(worst <= 2, "frame {k} is {worst} levels off its still");
        }
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[allow(clippy::print_stderr)]
fn eprintln_timing(took: std::time::Duration, frames: u32) {
    eprintln!(
        "svg_rendered: {frames} frames in {took:?} ({:.1} ms a frame)",
        took.as_secs_f64() * 1000.0 / f64::from(frames)
    );
}
