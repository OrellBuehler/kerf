//! Titles on tracks (generated clips) in the graphs: the argv the builders write for them, and — the
//! `#[ignore]`d half, which drives the real `ffmpeg` — what that argv draws.
//!
//! A title clip's input is the same pictures a floating title reads (`svg_layer::clip_sequences`,
//! the whole frame, one per output frame the clip is on, tail included), stamped onto the canvas' grid
//! where a decoded clip is trimmed and moved (`settb=expr=den/num,setpts=N+k`), and then the clip's own
//! chain: fit, transform, colour, fades and dips, a dissolve's alpha. The still reads one picture of the
//! document at the clip's own clock, as a still image is read.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored title_clips`

use std::path::{Path, PathBuf};
use std::process::Stdio;

use uuid::Uuid;

use super::svg_layer;
use super::*;
use crate::clip_timing::Rational;
use crate::engine::test_support::{av_asset, make_clip, single, test_asset, video_stream, StatusBounded};
use crate::model::{Asset, Track, Transition, TransitionKind};
use crate::svg::raster::with_pinned_dir;

const PIN: &str = "/kerf-test/svg";

fn title(svg: &str, start: f64, length: f64) -> Clip {
    Clip::title(
        TitleClip {
            name: "Card".into(),
            svg: svg.into(),
            template: None,
        },
        start,
        length,
    )
}

/// A box that slides across a 192x108 document in two seconds.
const SLIDING: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 192 108\"><rect y=\"40\" width=\"40\" height=\"28\" fill=\"#ff3355\"><animate attributeName=\"x\" from=\"0\" to=\"152\" dur=\"2s\" fill=\"freeze\"/></rect></svg>";
/// A document with nothing that moves.
const HELD: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\"><circle cx=\"50\" cy=\"50\" r=\"40\" fill=\"#00ccaa\"/></svg>";

fn opts(fps: f64) -> ExportOptions {
    ExportOptions {
        resolution: Some((192, 108)),
        fps: Some(fps),
        ..ExportOptions::default()
    }
}

/// A shot from 0 to 2 s on V1, then a sliding title touching it under a dip to white, and a held
/// title on a track of its own over the shot from 0.5 to 1.5 s.
fn cut() -> (Timeline, Vec<Asset>) {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let mut end_card = title(SLIDING, 2.0, 2.0);
    end_card.transition_in = Some(Transition {
        kind: TransitionKind::DipToWhite,
        duration: 1.0,
    });
    let mut tl = single(vec![make_clip(asset.id, 0.0, 2.0, 0.0), end_card]);
    let mut v2 = Track::new(StreamKind::Video, "V2");
    v2.clips.push(title(HELD, 0.5, 1.0));
    tl.tracks.insert(1, v2);
    (tl, vec![asset])
}

fn joined(args: &[String]) -> String {
    args.join(" ")
}

#[test]
fn an_export_reads_each_titles_pictures_and_stamps_them_onto_the_grid() {
    let (tl, assets) = cut();
    let args = with_pinned_dir(PIN, || build_export_args(&tl, &assets, "/out.mp4", &opts(30.0))).unwrap();
    let text = joined(&args);
    assert!(text.contains("-f image2 -framerate 30 -i /kerf-test/svg/"), "{text}");
    assert!(text.contains("/%06d.png"), "the sliding title is a sequence: {text}");
    assert!(
        text.contains("-f image2 -pattern_type none -loop 1 -framerate 30 -i /kerf-test/svg/"),
        "the held one is one picture: {text}"
    );
    let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
    // Input 1 is the end card (V1's second clip), input 2 the held title on V2.
    assert!(graph.contains("[1:v]settb=expr=1/30,setpts=N+60,scale="), "{graph}");
    assert!(
        graph.contains("[2:v]settb=expr=1/30,setpts=N+15,trim=end_frame=30,scale="),
        "{graph}"
    );
    let card = graph.split(';').find(|c| c.starts_with("[1:v]")).unwrap();
    assert!(
        card.contains("fade=t=in") && card.contains(":c=white"),
        "the end card dips in from white: {card}"
    );
    assert!(
        card.contains("format=yuva420p"),
        "and keeps its transparency to the overlay: {card}"
    );
    assert!(
        !graph.contains("trim=start="),
        "no decoded window is cut from a title: {graph}"
    );
}

#[test]
fn the_pictures_are_the_frames_the_clip_is_on_from_its_own_clock() {
    let (mut tl, assets) = cut();
    let fmt = ExportFormat {
        width: 192,
        height: 108,
        fps: 30.0,
        ..ExportFormat::default()
    };
    // A title cut at the front starts part-way into its document.
    let card = &mut tl.tracks[0].clips[1];
    card.source_in = 0.5;
    card.source_out = 2.5;
    let fx = transition_fx(&tl, &assets);
    let seqs = with_pinned_dir(PIN, || svg_layer::clip_sequences(&tl, &fmt, &fx));
    assert!(seqs[0].is_none(), "footage has no pictures");
    let held = seqs[1].as_ref().unwrap();
    assert_eq!((held.seq.first_frame, held.count), (15, 30), "0.5 s up to, not at, 1.5 s");
    let card = seqs[2].as_ref().unwrap();
    assert_eq!((card.seq.first_frame, card.count), (60, 60));
    assert_eq!(card.seq.doc_t0, 0.5);
    assert_eq!(card.seq.svg, SLIDING);
}

#[test]
fn a_dissolve_from_a_title_borrows_as_much_of_its_document_as_it_needs() {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let mut next = make_clip(asset.id, 0.0, 2.0, 2.0);
    next.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 1.0,
    });
    let tl = single(vec![title(SLIDING, 0.0, 2.0), next]);
    let fx = transition_fx(&tl, &[asset]);
    assert_eq!(fx[0].tail, 1.0, "a title never runs out of handle");
    assert!(fx[0].alpha);
}

#[test]
fn a_still_reads_one_picture_of_the_title_at_its_own_clock() {
    let (tl, assets) = cut();
    let at = |t: f64| {
        with_pinned_dir(PIN, || {
            build_timeline_frame_args(&tl, &assets, &opts(30.0), t, 192, 4).map(|a| joined(&a))
        })
        .unwrap()
    };
    let inside = at(3.0);
    assert!(
        inside.contains("-f image2 -pattern_type none -i /kerf-test/svg/still-"),
        "{inside}"
    );
    assert_eq!(inside.matches("/kerf-test/svg/still-").count(), 1);
    assert_eq!(
        at(1.0).matches("/kerf-test/svg/still-").count(),
        1,
        "the held title over the shot"
    );
    assert!(!at(0.2).contains("/kerf-test/svg/"));
    // The picture is named for the document's moment: another time, another picture.
    let a = with_pinned_dir(PIN, || svg_layer::still_clip_picture(SLIDING, 192, 108, 0.5));
    let b = with_pinned_dir(PIN, || svg_layer::still_clip_picture(SLIDING, 192, 108, 0.6));
    assert_ne!(a.input, b.input);
}

#[test]
fn a_graph_that_draws_no_picture_names_no_title() {
    let (tl, assets) = cut();
    let mp3 = ExportOptions {
        container: Container::Mp3,
        ..opts(30.0)
    };
    let text = with_pinned_dir(PIN, || build_export_args(&tl, &assets, "/out.mp3", &mp3)).map(|a| joined(&a));
    let text = text.unwrap();
    assert!(!text.contains("image2"), "{text}");
    let levels = with_pinned_dir(PIN, || levels::build_levels_args(&tl, &assets, &ExportOptions::default()))
        .unwrap()
        .expect("the shot has sound");
    assert!(!joined(&levels.args).contains("image2"), "the levels pass reads no picture");
}

#[test]
fn the_plan_names_a_title_clip_and_a_compositor_refuses_its_frame() {
    let (tl, assets) = cut();
    let plan = RenderPlan::at(&tl, &assets, &opts(30.0), 3.0, CompositeColorPolicy::default()).unwrap();
    let card = tl.tracks[0].clips[1].id;
    assert_eq!(plan.svg, vec![card]);
    assert!(plan.layers.is_empty(), "a title is no decoded layer");
    let why = plan.reasons(&crate::plan_caps::GpuCaps::A0, (192, 108));
    assert!(why.contains(&crate::plan_caps::Unsupported::Svg), "{why:?}");
    let shot = RenderPlan::at(&tl, &assets, &opts(30.0), 0.2, CompositeColorPolicy::default()).unwrap();
    assert!(shot.svg.is_empty());
}

// ---- through the real ffmpeg ---------------------------------------------------------------------

const W: u32 = 64;
const H: u32 = 36;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-title-clips-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A lossless four second clip at `fps`: blue-ish left, green-ish right.
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

/// A saturated box that slides from the left edge to the right one in two seconds (26 px a second),
/// over nothing, or over a dark card that fills the frame.
fn sliding_box(card: bool) -> String {
    let under = if card {
        format!("<rect width=\"{W}\" height=\"{H}\" fill=\"#203040\"/>")
    } else {
        String::new()
    };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {W} {H}\">{under}<rect y=\"12\" width=\"12\" height=\"12\" fill=\"#ff2040\">\
         <animate attributeName=\"x\" from=\"0\" to=\"52\" dur=\"2s\" fill=\"freeze\"/></rect></svg>"
    )
}

fn render_opts(fps: f64) -> ExportOptions {
    ExportOptions {
        resolution: Some((W, H)),
        fps: Some(fps),
        ..ExportOptions::default()
    }
}

/// Every frame of the export of `timeline` as raw rgb24, drawn as `render_with_progress` would (the
/// titles prepared first, the real argv) with the encoder and the sink replaced.
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
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    let raw = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    raw.chunks((W * H * 3) as usize).map(<[u8]>::to_vec).collect()
}

fn still_rgb(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, t: f64) -> Vec<u8> {
    let out = run_still(timeline, assets, opts, t, W, None, &StillOutput::RgbPipe, &|| false).unwrap();
    assert_eq!(out.len(), (W * H * 3) as usize);
    out
}

fn distance(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

/// The column the box starts at (row 18), `None` when there is no box on it.
fn box_left(frame: &[u8]) -> Option<u32> {
    (0..W).find(|&x| {
        let p = &frame[((18 * W + x) * 3) as usize..][..3];
        p[0] > 150 && p[1] < 110 && p[2] < 130
    })
}

fn mean(frame: &[u8]) -> f64 {
    frame.iter().map(|&v| f64::from(v)).sum::<f64>() / frame.len() as f64
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn every_frame_of_a_title_on_a_track_is_its_still_and_the_rest_is_the_plain_cut() {
    let dir = scratch("frames");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        for (text, fps) in [("24", 24.0), ("25", 25.0), ("29.97", 29.97), ("30", 30.0)] {
            let bg = background(&dir, text);
            let assets = [bg.clone()];
            let mut with = single(vec![make_clip(bg.id, 0.0, 4.0, 0.0)]);
            let mut v2 = Track::new(StreamKind::Video, "V2");
            v2.clips.push(title(&sliding_box(false), 0.5, 2.0));
            with.tracks.insert(1, v2);
            let mut without = with.clone();
            without.tracks[1].clips.clear();
            let opts = render_opts(fps);
            let rate = Rational::from_fps(fps).unwrap();
            let titled = export_rgb(&with, &assets, &opts, &dir, &format!("with-{text}"));
            let plain = export_rgb(&without, &assets, &opts, &dir, &format!("without-{text}"));
            assert_eq!(titled.len(), plain.len(), "{text}");
            let mut present = 0;
            for (k, (a, b)) in titled.iter().zip(&plain).enumerate() {
                let time = rate.frame_time(k as u64);
                if !(0.5..2.5).contains(&time) {
                    assert_eq!(a, b, "{text}: frame {k} ({time:.4} s) is the plain cut");
                    continue;
                }
                present += 1;
                let left = box_left(a).unwrap_or_else(|| panic!("{text}: frame {k} has no box"));
                let want = 26.0 * (rate.exact_time(k as u64) - 0.5).max(0.0);
                assert!(
                    (f64::from(left) - want).abs() <= 1.6,
                    "{text}: frame {k}: {left}, wanted {want:.2}"
                );
                let still = still_rgb(&with, &assets, &opts, rate.exact_time(k as u64));
                let worst = distance(a, &still);
                assert!(worst <= 2, "{text}: frame {k} is {worst} levels off its still");
            }
            assert!(present > 40, "{text}: {present}");
        }
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_title_dips_to_white_from_the_shot_before_it_and_holds_its_last_state_when_extended() {
    let dir = scratch("dip");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "25");
        let assets = [bg.clone()];
        let mut card = title(&sliding_box(true), 2.0, 3.0);
        card.transition_in = Some(Transition {
            kind: TransitionKind::DipToWhite,
            duration: 1.0,
        });
        let tl = single(vec![make_clip(bg.id, 0.0, 2.0, 0.0), card]);
        let opts = render_opts(25.0);
        let frames = export_rgb(&tl, &assets, &opts, &dir, "dip");
        assert_eq!(frames.len(), 125, "the title extends the cut to 5 s");
        assert!(mean(&frames[30]) < 120.0, "the shot, before the dip");
        assert!(
            mean(&frames[49]) > 225.0,
            "the shot has dipped to white: {}",
            mean(&frames[49])
        );
        assert!(
            mean(&frames[50]) > 225.0,
            "and the title comes up from it: {}",
            mean(&frames[50])
        );
        assert!(mean(&frames[75]) < 120.0, "the card, after it");
        assert_eq!(
            box_left(&frames[75]).map(|x| x / 3),
            Some(26 / 3),
            "half way across a second in"
        );
        assert_eq!(box_left(&frames[124]), Some(52), "held at the end of its animation");
        let rate = Rational::from_fps(25.0).unwrap();
        for k in [40, 45, 49, 50, 55, 60, 70, 100] {
            let still = still_rgb(&tl, &assets, &opts, rate.exact_time(k));
            let worst = distance(&frames[k as usize], &still);
            assert!(worst <= 3, "frame {k} is {worst} levels off its still");
        }
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_title_cut_at_the_front_starts_part_way_and_a_held_one_ends_with_its_clip() {
    let dir = scratch("trim");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "25");
        let assets = [bg.clone()];
        let mut trimmed = title(&sliding_box(false), 0.0, 2.0);
        trimmed.source_in = 1.0;
        trimmed.source_out = 3.0;
        let held = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {W} {H}\"><rect x=\"40\" y=\"12\" width=\"12\" height=\"12\" fill=\"#ff2040\"/></svg>"
        );
        let mut v2 = Track::new(StreamKind::Video, "V2");
        v2.clips.push(trimmed);
        v2.clips.push(title(&held, 2.4, 1.0));
        let mut tl = single(vec![make_clip(bg.id, 0.0, 4.0, 0.0)]);
        tl.tracks.insert(1, v2);
        let frames = export_rgb(&tl, &assets, &render_opts(25.0), &dir, "trim");
        assert_eq!(
            box_left(&frames[0]),
            Some(26),
            "a second into its animation from the first frame"
        );
        assert_eq!(frames.len(), 100);
        assert!(box_left(&frames[55]).is_none(), "between the two");
        assert_eq!(box_left(&frames[60]), Some(40), "the held one from 2.4 s");
        assert_eq!(box_left(&frames[84]), Some(40));
        assert!(box_left(&frames[85]).is_none(), "and not at its end, 3.4 s");
    });
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn the_playback_stream_plays_a_title_on_a_track_from_the_playhead() {
    let dir = scratch("stream");
    with_pinned_dir(dir.join("cache").to_str().unwrap(), || {
        let bg = background(&dir, "24");
        let assets = [bg.clone()];
        let mut tl = single(vec![make_clip(bg.id, 0.0, 4.0, 0.0)]);
        let mut v2 = Track::new(StreamKind::Video, "V2");
        v2.clips.push(title(&sliding_box(false), 0.5, 3.0));
        tl.tracks.insert(1, v2);
        let mut jpegs = Vec::new();
        let request = PlaybackRequest {
            fps: Some(24.0),
            max_width: Some(W),
        };
        stream_preview_with(&tl, &assets, 1.5, request, &mut |f| {
            jpegs.push(f.jpeg);
            jpegs.len() < 6
        })
        .unwrap();
        let first = &jpegs[0];
        let decoded = command(&ffmpeg_bin())
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
            ])
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(first)?;
                child.wait_with_output()
            })
            .unwrap();
        assert_eq!(
            box_left(&decoded.stdout).map(|x| x / 4),
            Some(26 / 4),
            "already a second across: {:?}",
            box_left(&decoded.stdout)
        );
    });
    let _ = std::fs::remove_dir_all(dir);
}
