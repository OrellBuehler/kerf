//! An export in parts through the real `ffmpeg` (`#[ignore]`d: they drive the binary).
//!
//! A unit test on the graph string proves the text; these prove that a cut rendered in parts is the
//! cut rendered whole. The export is lossless (x264 `-crf 0`, FLAC), so "the same" means every byte
//! of every decoded frame and every sample, over a timeline with a crossfade, a dip, fades, a keyed
//! zoom, a speed change, a still logo that fades out, an animated SVG title, a title on a track and captions, and sound
//! through ducking, the master limiter and a loudness target — and the parts are cut by the planner
//! and, harder, at frames chosen to land inside each of those.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored segments_rendered`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::segments::*;
use super::*;
use crate::engine::test_support::{audio_stream, image_stream, make_clip, test_asset, video_stream, StatusBounded};
use crate::model::{
    Asset, Easing, Property, PropertyKey, PropertyTrack, StreamKind, SvgOverlay, TextKeyframe, TextOverlay, Track, Transition,
    TransitionKind,
};
use crate::svg::raster::with_pinned_dir;

const W: u32 = 128;
const H: u32 = 72;
const FPS: f64 = 30.0;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-segments-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
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

fn ffmpeg_ok(args: &[&str]) {
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success(), "ffmpeg {args:?}");
}

/// A `secs`-long clip with a moving picture (`hue` tells two of them apart) and a tone, lossless.
fn source(dir: &Path, name: &str, fps: u32, secs: u32, hue: u32, tone: u32) -> Asset {
    let path = dir.join(format!("{name}.mp4"));
    ffmpeg_ok(&[
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc2=s={W}x{H}:r={fps}:d={secs},hue=h={hue},format=yuv420p"),
        "-f",
        "lavfi",
        "-i",
        &format!("sine=f={tone}:r=48000:d={secs}"),
        "-c:v",
        "libx264",
        "-crf",
        "0",
        "-g",
        "12",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-shortest",
        path.to_str().unwrap(),
    ]);
    let mut asset = test_asset(vec![video_stream(W, H, f64::from(fps)), audio_stream(48_000, 2)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = f64::from(secs);
    asset
}

fn logo(dir: &Path) -> Asset {
    let path = dir.join("logo.png");
    ffmpeg_ok(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=0xff3040@0.8:s=32x18,format=rgba",
        "-frames:v",
        "1",
        path.to_str().unwrap(),
    ]);
    let mut asset = test_asset(vec![image_stream(32, 18)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = crate::model::DEFAULT_IMAGE_DURATION;
    asset
}

fn music(dir: &Path) -> Asset {
    let path = dir.join("bed.wav");
    ffmpeg_ok(&[
        "-f",
        "lavfi",
        "-i",
        "sine=f=110:r=48000:d=20",
        "-ac",
        "2",
        path.to_str().unwrap(),
    ]);
    let mut asset = test_asset(vec![audio_stream(48_000, 2)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 20.0;
    asset
}

fn sliding_box() -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {W} {H}\"><rect y=\"24\" width=\"24\" height=\"24\" fill=\"#20c0ff\">\
         <animate attributeName=\"x\" from=\"0\" to=\"104\" dur=\"2.5s\" fill=\"freeze\"/></rect></svg>"
    )
}

/// A title on a track of its own (pictures drawn over the whole frame, one per frame it is on).
fn title_card(start: f64, length: f64) -> Clip {
    Clip::title(
        crate::model::TitleClip {
            name: "Card".into(),
            svg: format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {W} {H}\"><rect x=\"8\" y=\"4\" width=\"30\" height=\"20\" fill=\"#ffcc00\">\
                 <animate attributeName=\"y\" from=\"4\" to=\"46\" dur=\"2s\" fill=\"freeze\"/></rect></svg>"
            ),
            template: None,
        },
        start,
        length,
    )
}

fn svg_title(start: f64, end: f64) -> SvgOverlay {
    SvgOverlay {
        id: uuid::Uuid::new_v4(),
        name: String::new(),
        svg: sliding_box(),
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

/// Twelve one-second clips of two sources with everything a cut can cross, a logo over all of it,
/// a music bed under it, titles and captions.
fn timeline(a: &Asset, b: &Asset, logo: &Asset, bed: &Asset) -> Timeline {
    let mut clips: Vec<Clip> = (0..12)
        .map(|i| {
            let asset = if i % 2 == 0 { a } else { b };
            make_clip(asset.id, 0.5 + 0.13 * f64::from(i), 1.5 + 0.13 * f64::from(i), f64::from(i))
        })
        .collect();
    clips[4].transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.6,
    });
    clips[8].transition_in = Some(Transition {
        kind: TransitionKind::DipToBlack,
        duration: 0.8,
    });
    clips[6].fade_in = 0.4;
    clips[6].fade_out = 0.4;
    clips[2].channels = vec![PropertyTrack {
        prop: Property::Scale,
        keys: vec![
            PropertyKey::new(0.0, 1.0),
            PropertyKey {
                easing: Easing::EaseInOut,
                ..PropertyKey::new(1.0, 1.6)
            },
        ],
    }];
    clips[9].speed = 2.0;
    clips[10].volume = 0.5;
    let mut watermark = make_clip(logo.id, 0.0, 12.0, 0.0);
    watermark.transform.scale = 0.5;
    watermark.transform.pos_x = 0.35;
    watermark.transform.pos_y = -0.35;
    watermark.fade_out = 1.0;
    let mut tl = Timeline {
        tracks: vec![
            crate::model::Track {
                clips,
                ..Track::new(StreamKind::Video, "V1")
            },
            crate::model::Track {
                clips: vec![watermark],
                ..Track::new(StreamKind::Video, "V2")
            },
            crate::model::Track {
                clips: vec![title_card(4.4, 2.5)],
                ..Track::new(StreamKind::Video, "V3")
            },
            crate::model::Track {
                clips: vec![make_clip(bed.id, 0.0, 12.0, 0.0)],
                duck: true,
                volume: 0.6,
                ..Track::new(StreamKind::Audio, "A1")
            },
        ],
        overlays: Vec::new(),
        svg_overlays: vec![svg_title(3.2, 5.7)],
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    };
    tl.master.limiter = true;
    tl.master.ceiling_db = -3.0;
    let mut caption = TextOverlay::new("one", 2.3, 2.8);
    caption.generated = true;
    let mut moving = TextOverlay::new("two", 5.2, 7.9);
    moving.keyframes = vec![
        TextKeyframe {
            time: 0.0,
            pos_x: 0.2,
            pos_y: 0.5,
            opacity: 1.0,
        },
        TextKeyframe {
            time: 2.0,
            pos_x: 0.8,
            pos_y: 0.7,
            opacity: 0.5,
        },
    ];
    tl.overlays = vec![caption, moving];
    tl
}

fn lossless(container: Container) -> ExportOptions {
    ExportOptions {
        container,
        video_codec: Some("libx264".into()),
        audio_codec: Some("flac".into()),
        rate_control: RateControl::Lossless,
        resolution: Some((W, H)),
        fps: Some(FPS),
        loudness: Some(LoudnessPreset::Youtube),
        ..ExportOptions::default()
    }
}

/// The tiny limits that make three of these clips a part.
fn small() -> SegmentLimits {
    SegmentLimits {
        decode_mpx: 0.03,
        inputs: 16,
        preroll_s: 30.0,
    }
}

struct Fixture {
    dir: PathBuf,
    assets: Vec<Asset>,
    timeline: Timeline,
}

fn fixture(tag: &str) -> Fixture {
    let dir = scratch(tag);
    let a = source(&dir, "a", 30, 8, 0, 330);
    let b = source(&dir, "b", 30, 8, 120, 550);
    let l = logo(&dir);
    let bed = music(&dir);
    let timeline = timeline(&a, &b, &l, &bed);
    Fixture {
        dir,
        assets: vec![a, b, l, bed],
        timeline,
    }
}

/// The whole export in one run, as the builder gives it.
fn render_whole(fx: &Fixture, opts: &ExportOptions, out: &Path) {
    let args = build_export_args(&fx.timeline, &fx.assets, out.to_str().unwrap(), opts).unwrap();
    let mut args = args;
    externalize_filter_complex(&mut args, "whole").unwrap();
    run_ok(&args);
}

fn render_parts(plan: &PartsPlan, fx: &Fixture, opts: &ExportOptions, out: &Path) -> RenderStatus {
    render_parts_with(plan, fx, opts, out, &mut |_| {}, &|| false).unwrap()
}

fn render_parts_with(
    plan: &PartsPlan,
    fx: &Fixture,
    opts: &ExportOptions,
    out: &Path,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    render_in_parts(plan, &fx.assets, out, opts, progress, cancel, &std::cell::Cell::new(false))
}

fn plan_of(fx: &Fixture, opts: &ExportOptions, limits: SegmentLimits) -> PartsPlan {
    let mut plan = plan_export(&fx.timeline, &fx.assets, opts, limits).expect("a plan in parts");
    plan.scratch = Some(fx.dir.join("parts"));
    plan
}

/// Parts that begin at `cuts` (and at 0).
fn parts_at(cuts: &[u64], frames: u64) -> Vec<Part> {
    let mut starts = vec![0];
    starts.extend_from_slice(cuts);
    starts.push(frames);
    starts
        .windows(2)
        .map(|w| Part {
            first: w[0],
            frames: w[1] - w[0],
        })
        .collect()
}

fn yuv_frames(path: &Path, size: (u32, u32), pix_fmt: &str) -> Vec<Vec<u8>> {
    let bytes = match pix_fmt {
        "yuv420p10le" => size.0 * size.1 * 3,
        _ => size.0 * size.1 * 3 / 2,
    } as usize;
    let out = run_ok(
        &[
            "-i",
            path.to_str().unwrap(),
            "-map",
            "0:v:0",
            "-f",
            "rawvideo",
            "-pix_fmt",
            pix_fmt,
            "-",
        ]
        .map(String::from),
    );
    out.stdout.chunks(bytes).map(<[u8]>::to_vec).collect()
}

fn samples(path: &Path) -> Vec<u8> {
    run_ok(
        &[
            "-i",
            path.to_str().unwrap(),
            "-map",
            "0:a:0",
            "-f",
            "f32le",
            "-ac",
            "2",
            "-ar",
            "48000",
            "-",
        ]
        .map(String::from),
    )
    .stdout
}

/// The first frame where two decodes differ, and by how much.
fn first_difference(a: &[Vec<u8>], b: &[Vec<u8>]) -> Option<String> {
    if a.len() != b.len() {
        return Some(format!("{} frames against {}", a.len(), b.len()));
    }
    a.iter().zip(b).position(|(x, y)| x != y).map(|k| {
        let worst = a[k].iter().zip(&b[k]).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        let wrong = a[k].iter().zip(&b[k]).filter(|(x, y)| x != y).count();
        format!("frame {k} differs ({wrong} bytes, by up to {worst})")
    })
}

/// Every sample the same — but for the last [`SOUND_TAIL`] (half a second), which FFmpeg 9.0.2 does not write
/// the same twice (two runs of the *same* whole export differ from 0.1 to 0.25 s before the end, by up to
/// 0.08 of full scale; 4.4 does not): there the two have to agree to within what that run-to-run spread is.
const SOUND_TAIL: usize = 48_000 * 2 * 4 / 2;

fn assert_same_sound(whole: &[u8], parts: &[u8], tag: &str) {
    let head = whole.len() - SOUND_TAIL;
    let at = whole[..head].iter().zip(&parts[..head]).position(|(x, y)| x != y);
    assert_eq!(at, None, "{tag}: sound differs from byte {at:?} of {}", whole.len());
    let floats = |b: &[u8]| -> Vec<f32> {
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    };
    let worst = floats(&whole[head..])
        .iter()
        .zip(floats(&parts[head..]))
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 0.15, "{tag}: the last half second of sound differs by {worst}");
}

fn assert_same_export(fx: &Fixture, plan: &PartsPlan, opts: &ExportOptions, tag: &str) {
    let whole = fx.dir.join(format!("{tag}-whole.mkv"));
    let parts = fx.dir.join(format!("{tag}-parts.mkv"));
    let _pins = svg_layer::prepare_export(&fx.timeline, &fx.assets, opts, &mut |_| {}, &|| false)
        .unwrap()
        .expect("not cancelled");
    render_whole(fx, opts, &whole);
    assert_eq!(render_parts(plan, fx, opts, &parts), RenderStatus::Completed);
    let pix = opts.pix_fmt.as_deref().unwrap_or("yuv420p");
    let (w, p) = (yuv_frames(&whole, (W, H), pix), yuv_frames(&parts, (W, H), pix));
    assert_eq!(w.len() as u64, plan.frames, "{tag}: the frames of the plan");
    assert_eq!(
        first_difference(&w, &p),
        None,
        "{tag}: picture, in {} parts",
        plan.parts.len()
    );
    let (sw, sp) = (samples(&whole), samples(&parts));
    assert!(sw.len() > 48_000 * 8 * 5, "{tag}: the whole export has its sound");
    assert_eq!(sw.len(), sp.len(), "{tag}: sound length");
    assert_same_sound(&sw, &sp, tag);
    assert!(
        !fx.dir.join("parts").exists() || std::fs::read_dir(fx.dir.join("parts")).unwrap().next().is_none(),
        "{tag}: the parts' files are gone"
    );
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn the_planners_parts_are_the_whole_export_frame_for_frame_and_sample_for_sample() {
    let fx = fixture("planned");
    with_pinned_dir(fx.dir.join("cache").to_str().unwrap(), || {
        let opts = lossless(Container::Mkv);
        let plan = plan_of(&fx, &opts, small());
        assert!(plan.parts.len() >= 3, "{:?}", plan.parts);
        assert_same_export(&fx, &plan, &opts, "planned");
    });
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn cuts_through_a_transition_a_fade_a_zoom_a_title_a_caption_and_the_logo_leave_no_trace() {
    let fx = fixture("adversarial");
    with_pinned_dir(fx.dir.join("cache").to_str().unwrap(), || {
        let opts = lossless(Container::Mkv);
        let mut plan = plan_of(&fx, &opts, small());
        // 37: inside clip 1; 75: inside the zoom of clip 2 and the caption; 130: inside the
        // crossfade into clip 4; 150: inside the title; 185: inside clip 6's fade-in; 205: inside
        // its fade-out; 240: inside the dip; 345: inside the logo's fade-out.
        plan.parts = parts_at(&[37, 75, 130, 150, 185, 205, 240, 345], plan.frames);
        assert_eq!(plan.frames, 360);
        assert_same_export(&fx, &plan, &opts, "adversarial");
        // Every frame a cut is made at, too: cut at each of a run of frames across the crossfade.
        for cut in [119u64, 120, 121, 126, 137, 138, 139] {
            plan.parts = parts_at(&[cut], plan.frames);
            assert_same_export(&fx, &plan, &opts, &format!("at-{cut}"));
        }
    });
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_ten_bit_export_in_parts_is_the_whole_export() {
    let mut fx = fixture("tenbit");
    with_pinned_dir(fx.dir.join("cache").to_str().unwrap(), || {
        let opts = ExportOptions {
            pix_fmt: Some("yuv420p10le".into()),
            ..lossless(Container::Mkv)
        };
        let mut plan = plan_of(&fx, &opts, small());
        plan.parts = parts_at(&[100, 289, 292, 296], plan.frames);
        assert_same_export(&fx, &plan, &opts, "tenbit");
        plan.parts = parts_at(&[37, 75, 130, 150, 185, 205, 240, 345], plan.frames);
        assert_same_export(&fx, &plan, &opts, "tenbit-adversarial");

        // Without the logo clip 9 (speed 2) leaves a gap of black, with a caption drawn over it:
        // a part with nothing on screen but the canvas, in the format the overlays compose in.
        fx.timeline.tracks.retain(|t| t.name != "V2");
        let mut gap_caption = TextOverlay::new("gap", 9.6, 9.9);
        gap_caption.generated = true;
        fx.timeline.overlays.push(gap_caption);
        let mut plan = plan_of(&fx, &opts, small());
        plan.parts = parts_at(&[100, 289, 292, 296], plan.frames);
        assert!(
            part_contents(&plan, &plan.parts[2]).include.iter().all(|&on| !on),
            "a part with no clip"
        );
        assert_same_export(&fx, &plan, &opts, "tenbit-gap");
    });
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn two_passes_faststart_and_the_container_options_are_the_last_passs() {
    let fx = fixture("two-pass");
    with_pinned_dir(fx.dir.join("cache").to_str().unwrap(), || {
        let opts = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            audio_codec: Some("aac".into()),
            audio_bitrate: Some("128k".into()),
            rate_control: RateControl::TwoPass,
            video_bitrate: Some("400k".into()),
            faststart: true,
            metadata_title: Some("in parts".into()),
            resolution: Some((W, H)),
            fps: Some(FPS),
            ..ExportOptions::default()
        };
        let plan = plan_of(&fx, &opts, small());
        let out = fx.dir.join("two-pass.mp4");
        let _pins = svg_layer::prepare_export(&fx.timeline, &fx.assets, &opts, &mut |_| {}, &|| false)
            .unwrap()
            .expect("not cancelled");
        assert_eq!(render_parts(&plan, &fx, &opts, &out), RenderStatus::Completed);
        let frames = yuv_frames(&out, (W, H), "yuv420p");
        assert_eq!(frames.len() as u64, plan.frames);
        // `+faststart`: the index is in front of the media.
        let head = std::fs::read(&out).unwrap();
        let (moov, mdat) = (
            head.windows(4).position(|w| w == b"moov").expect("moov"),
            head.windows(4).position(|w| w == b"mdat").expect("mdat"),
        );
        assert!(moov < mdat, "moov at {moov}, mdat at {mdat}");
        assert!(head.windows(8).any(|w| w == b"in parts"), "the title tag");
        assert!(std::fs::read_dir(fx.dir.join("parts")).map_or(true, |mut d| d.next().is_none()));
    });
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn progress_names_the_part_and_a_cancel_removes_every_file() {
    let fx = fixture("cancel");
    with_pinned_dir(fx.dir.join("cache").to_str().unwrap(), || {
        let opts = lossless(Container::Mkv);
        let plan = plan_of(&fx, &opts, small());
        let out = fx.dir.join("cancelled.mkv");
        let _pins = svg_layer::prepare_export(&fx.timeline, &fx.assets, &opts, &mut |_| {}, &|| false)
            .unwrap()
            .expect("not cancelled");
        // The whole run, listening.
        let mut seen: Vec<(u32, u32, bool, f64)> = Vec::new();
        let status = render_parts_with(
            &plan,
            &fx,
            &opts,
            &out,
            &mut |p| {
                if let Some(part) = p.part {
                    seen.push((part.index, part.total, part.encoding, p.fraction));
                }
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(status, RenderStatus::Completed);
        let total = plan.parts.len() as u32;
        assert!(seen.iter().all(|s| s.1 == total));
        for index in 1..=total {
            assert!(seen.iter().any(|s| s.0 == index && !s.2), "part {index} reported");
        }
        assert!(seen.iter().any(|s| s.2), "the last pass reported");
        assert!(
            seen.windows(2).all(|w| w[1].3 + 1e-9 >= w[0].3),
            "the bar never goes back: {seen:?}"
        );
        assert!(seen.last().unwrap().3 > 0.95);
        let _ = std::fs::remove_file(&out);

        // A stop in the middle of the second part.
        let stop = std::sync::atomic::AtomicBool::new(false);
        let status = render_parts_with(
            &plan,
            &fx,
            &opts,
            &out,
            &mut |p| {
                if p.part.is_some_and(|part| part.index == 2) {
                    stop.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            },
            &|| stop.load(std::sync::atomic::Ordering::SeqCst),
        )
        .unwrap();
        assert_eq!(status, RenderStatus::Cancelled);
        assert!(!out.exists(), "no half-written output");
        assert!(
            std::fs::read_dir(fx.dir.join("parts")).map_or(true, |mut d| d.next().is_none()),
            "the parts' files are gone"
        );

        // A stop before anything has started.
        let status = render_parts_with(&plan, &fx, &opts, &out, &mut |_| {}, &|| true).unwrap();
        assert_eq!(status, RenderStatus::Cancelled);
        assert!(!out.exists());
    });
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_failing_part_is_an_error_and_leaves_nothing() {
    let mut fx = fixture("fail");
    with_pinned_dir(fx.dir.join("cache").to_str().unwrap(), || {
        let opts = lossless(Container::Mkv);
        let plan = plan_of(&fx, &opts, small());
        let _pins = svg_layer::prepare_export(&fx.timeline, &fx.assets, &opts, &mut |_| {}, &|| false)
            .unwrap()
            .expect("not cancelled");
        // The second source goes missing: the parts that use it fail, the earlier ones rendered.
        std::fs::remove_file(&fx.assets[1].path).unwrap();
        fx.assets[1].path = fx.dir.join("gone.mp4").to_string_lossy().into_owned();
        let out = fx.dir.join("failed.mkv");
        let err = render_parts_with(&plan, &fx, &opts, &out, &mut |_| {}, &|| false).unwrap_err();
        assert!(
            err.to_string().contains("gone.mp4") || err.to_string().contains("ffmpeg"),
            "{err}"
        );
        assert!(!out.exists());
        assert!(
            std::fs::read_dir(fx.dir.join("parts")).map_or(true, |mut d| d.next().is_none()),
            "the parts' files are gone"
        );
    });
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn the_canvas_of_a_plan_has_the_frames_the_color_source_makes() {
    let dir = scratch("canvas");
    for (text, rate) in [
        ("24", 24.0),
        ("25", 25.0),
        ("29.97", 29.97),
        ("30", 30.0),
        ("50", 50.0),
        ("59.94", 59.94),
    ] {
        for total in [0.01, 1.0 / 3.0, 2.0, 2.000_001, 7.123_456_789, 10.01, 59.999_999_9, 60.0] {
            let out = dir.join("count.gray");
            ffmpeg_ok(&[
                "-f",
                "lavfi",
                "-i",
                &format!("color=c=black:s=16x16:r={rate}:d={total}"),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gray",
                out.to_str().unwrap(),
            ]);
            let frames = std::fs::metadata(&out).unwrap().len() / 256;
            let planned = canvas_frames(total, Rational::from_fps(rate).unwrap());
            assert_eq!(planned, frames, "{total} s at {text} fps");
        }
    }
}

#[test]
#[ignore = "drives the real ffmpeg; run it alone (it reads the children's peak memory)"]
#[allow(clippy::print_stderr)]
fn a_reel_of_many_big_inputs_completes_in_parts_with_bounded_memory() {
    let dir = scratch("many");
    // Two seconds of 4K HEVC 10-bit, and 24 clips of it at different points: 24 inputs of 8 Mpx.
    let big = dir.join("big.mp4");
    ffmpeg_ok(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=3840x2160:r=25:d=2,noise=alls=3:allf=t,format=yuv420p10le",
        "-c:v",
        "libx265",
        "-preset",
        "ultrafast",
        "-crf",
        "30",
        "-x265-params",
        "log-level=error:keyint=25",
        "-pix_fmt",
        "yuv420p10le",
        big.to_str().unwrap(),
    ]);
    let mut asset = test_asset(vec![video_stream(3840, 2160, 25.0)]);
    asset.path = big.to_string_lossy().into_owned();
    asset.duration = 2.0;
    let clips: Vec<Clip> = (0..24)
        .map(|i| make_clip(asset.id, 0.05 * f64::from(i), 0.05 * f64::from(i) + 0.5, 0.5 * f64::from(i)))
        .collect();
    let timeline = Timeline {
        tracks: vec![Track {
            clips,
            ..Track::new(StreamKind::Video, "V1")
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    };
    let opts = ExportOptions {
        container: Container::Mp4,
        video_codec: Some("libx264".into()),
        crf: Some(23),
        preset: Some("ultrafast".into()),
        resolution: Some((960, 540)),
        fps: Some(25.0),
        ..ExportOptions::default()
    };
    let fx = Fixture {
        dir: dir.clone(),
        assets: vec![asset],
        timeline,
    };
    let mut plan = plan_export(&fx.timeline, &fx.assets, &opts, SEGMENT_LIMITS).expect("24 UHD inputs are cut");
    plan.scratch = Some(dir.join("parts"));
    assert!(plan.parts.len() >= 6, "{:?}", plan.parts);
    let out = dir.join("many.mp4");
    // What the source's own encode peaked at (the largest child so far), to tell it from the parts'.
    let before = peak_child_mb();
    let started = std::time::Instant::now();
    assert_eq!(render_parts(&plan, &fx, &opts, &out), RenderStatus::Completed);
    let took = started.elapsed();
    assert_eq!(yuv_frames(&out, (960, 540), "yuv420p").len() as u64, plan.frames);
    let peak_mb = peak_child_mb();
    eprintln!(
        "{} parts, {took:?}; the largest child peaked at {peak_mb} MB (at {before} MB before the parts)",
        plan.parts.len()
    );
    // Four UHD inputs at four decoder threads each are about 2 GB with the graph; 24 whole are over 20.
    assert!(peak_mb < 2_500, "a part held {peak_mb} MB");
}

/// The largest resident set of any child this process has waited for, in MB (Linux; 0 elsewhere).
fn peak_child_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `getrusage` fills a plain struct.
        unsafe {
            let mut usage: libc::rusage = std::mem::zeroed();
            libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage);
            usage.ru_maxrss as u64 / 1024
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}
