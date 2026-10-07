//! What the export graph draws, pinned against rendered pixels (`#[ignore]`d: they
//! drive the real `ffmpeg`, on 6.1 and 9.0 alike).
//!
//! `ClipTiming` decides the numbers the graph prints, and a renderer that draws a
//! frame itself has to land on the same pictures. A string-level test cannot say
//! whether it does: `enable='between(t,s,e)'` parsed back out of the graph is the
//! number the builder just formatted. These render a lossless 32x18 solid clip over
//! another and read what is on the first pixel of every output frame:
//!
//! * which frames a clip is drawn on at its start and its end, at 24 / 25 / 29.97 /
//!   30 / 60 fps (the frame *pick* of A1a-3 has to reproduce this);
//! * how far into a fade, dissolve or dip each frame is (`FadeStep::progress_at_frame`).
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored rendered`

use std::path::{Path, PathBuf};

use super::*;
use crate::clip_timing::{clips_with_fx, ffmpeg_frame_time, ClipTiming, FadeStep, FadeTint};
use crate::engine::test_support::{make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded};
use crate::model::{Asset, Transition, TransitionKind};

const W: u32 = 32;
const H: u32 = 18;

/// The export frame rates under test: `(the text the graph carries, numerator,
/// denominator)`. 29.97 is `2997/100`, as FFmpeg parses `fps=29.97`.
const RATES: [(&str, u32, u32); 5] = [
    ("24", 24, 1),
    ("25", 25, 1),
    ("29.97", 2997, 100),
    ("30", 30, 1),
    ("60", 60, 1),
];

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-rendered-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A lossless `secs`-long solid-colour clip at `fps` (the text ffmpeg is given).
fn solid(dir: &Path, color: &str, fps: &str, secs: f64) -> Asset {
    let path = dir.join(format!("{color}-{fps}.mp4"));
    if !path.exists() {
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("color=c={color}:s={W}x{H}:r={fps}:d={secs}"))
            .args(["-c:v", "libx264", "-crf", "0", "-g", "1", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());
    }
    let mut asset = test_asset(vec![video_stream(W, H, fps.parse().unwrap())]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = secs;
    asset
}

/// The export graph of `timeline` rendered at `fps`, one `W`x`H` rgb24 frame per
/// output frame (no frame-rate conversion on the way out).
fn export_frames(timeline: &Timeline, assets: &[Asset], fps: f64, dir: &Path, tag: &str) -> Vec<Vec<u8>> {
    let opts = ExportOptions {
        resolution: Some((W, H)),
        fps: Some(fps),
        ..ExportOptions::default()
    };
    let out = dir.join(format!("{tag}.rgb"));
    let mut args = build_export_args(timeline, assets, "unused.mkv", &opts).unwrap();
    // Keep the inputs and the graph; replace the encoder and the sink with raw frames.
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
    std::fs::read(&out)
        .unwrap()
        .chunks((W * H * 3) as usize)
        .map(<[u8]>::to_vec)
        .collect()
}

/// The first pixel of a frame.
fn rgb(frame: &[u8]) -> [f64; 3] {
    [f64::from(frame[0]), f64::from(frame[1]), f64::from(frame[2])]
}

fn is_red(p: [f64; 3]) -> bool {
    p[0] > 128.0 && p[1] < 100.0 && p[2] < 100.0
}

/// What `FadeStep`s of one tint leave of the picture at output frame `k`.
fn strength(timing: &ClipTiming, tint: FadeTint, k: u64, (num, den): (u32, u32)) -> f64 {
    timing
        .fades()
        .iter()
        .filter(|s: &&FadeStep| s.tint == tint)
        .map(|s| s.progress_at_frame(k as i64, num, den))
        .product()
}

/// A clip is drawn on exactly the output frames whose FFmpeg time is inside its
/// window — closed at the start, open at the end, where the source has no frame.
///
/// 36 frame-aligned red clips of 3 to 7 frames over a green base, at each rate, cut
/// from the head of the footage and from one second in. For every frame from one
/// before a clip to one after it: red exactly when
/// `start <= ffmpeg_frame_time(k) < end`. It is not `k / fps`: at 24 fps a third of
/// the clips (and at 29.97 half) start on a frame whose FFmpeg time is an ulp under
/// their start, and lose that frame. A slower source behaves differently again (it
/// is drawn on the frame at `end` through the `fps` filter, covered below); picking
/// which frames those are is A1a-3's work.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_clip_is_drawn_on_the_frames_its_window_and_its_source_leave_it() {
    let dir = scratch("boundary");
    let mut lost_first_frames = Vec::new();
    for (name, num, den) in RATES {
        let fps: f64 = name.parse().unwrap();
        let mut lost = 0;
        for src_in in [0.0, 1.0] {
            let red = solid(&dir, "red", name, 4.0);
            let green = solid(&dir, "green", name, 16.0);
            // (first output frame, frames) of each clip, all on the frame grid.
            let spans: Vec<(u64, u64)> = (0..36u64).map(|i| (4 + 9 * i, 3 + i % 5)).collect();
            let clips = spans
                .iter()
                .map(|&(sf, n)| make_clip(red.id, src_in, src_in + n as f64 / fps, sf as f64 / fps))
                .collect();
            let total = (4 + 9 * 36 + 12) as f64 / fps;
            let timeline = timeline_of(vec![
                video_track(vec![make_clip(green.id, 0.0, total, 0.0)]),
                video_track(clips),
            ]);
            let assets = [red, green];
            let frames = export_frames(&timeline, &assets, fps, &dir, &format!("boundary-{name}-{src_in}"));
            for (ti, ci, clip, fx) in clips_with_fx(&timeline, &assets).filter(|r| r.0 == 1) {
                let timing = ClipTiming::new(clip, &fx);
                let (start, end) = timing.window();
                let (sf, n) = spans[ci];
                assert_eq!(ti, 1);
                for k in sf - 1..=sf + n {
                    let drawn = is_red(rgb(&frames[k as usize]));
                    let t = ffmpeg_frame_time(k, num, den);
                    let expected = t >= start && t < end - 1e-9;
                    assert_eq!(
                        drawn, expected,
                        "{name} fps, source from {src_in}: clip {ci} (frames {sf}..{}) at output frame {k}, t = {t:?}, window {start:?}..{end:?}",
                        sf + n
                    );
                    // The enable window is necessary for a frame to be drawn.
                    assert!(
                        !drawn || timing.enabled(t),
                        "{name} fps: frame {k} drawn outside the enable window"
                    );
                    lost += usize::from(k == sf && !drawn);
                }
            }
        }
        lost_first_frames.push((name, lost));
    }
    // The premise: FFmpeg's time is not the exact one. Where `k * (den / num)` lands
    // under `k / fps` the first frame is lost, and where it does not, nothing is.
    let lost = |name: &str| lost_first_frames.iter().find(|r| r.0 == name).unwrap().1;
    assert!(lost("24") > 0 && lost("29.97") > 0, "{lost_first_frames:?}");
    assert_eq!(lost("25"), 0, "{lost_first_frames:?}");

    // A source slower than the export is held by the `fps` filter, so a clip can be
    // drawn on the frame at its own end — which an equal-rate one never is.
    let (fps, (num, den)) = (30.0, (30, 1));
    let red = solid(&dir, "red", "24", 4.0);
    let green = solid(&dir, "green", "30", 16.0);
    let spans: Vec<(u64, u64)> = (0..30u64).map(|i| (4 + 20 * i, 6 + i % 7)).collect();
    let clips = spans
        .iter()
        .map(|&(sf, n)| make_clip(red.id, 1.0, 1.0 + n as f64 / fps, sf as f64 / fps))
        .collect();
    let timeline = timeline_of(vec![
        video_track(vec![make_clip(green.id, 0.0, (4 + 20 * 30 + 12) as f64 / fps, 0.0)]),
        video_track(clips),
    ]);
    let assets = [red, green];
    let frames = export_frames(&timeline, &assets, fps, &dir, "boundary-slower");
    let mut at_end = 0;
    for (_, ci, clip, fx) in clips_with_fx(&timeline, &assets).filter(|r| r.0 == 1) {
        let timing = ClipTiming::new(clip, &fx);
        let (sf, n) = spans[ci];
        for k in sf - 1..=sf + n + 1 {
            let drawn = is_red(rgb(&frames[k as usize]));
            assert!(
                !drawn || (k <= sf + n && timing.enabled(ffmpeg_frame_time(k, num, den))),
                "24 -> 30: clip {ci} drawn at output frame {k}, outside {sf}..={}",
                sf + n
            );
            at_end += usize::from(k == sf + n && drawn);
        }
    }
    assert!(
        at_end > 0,
        "a 24 fps source in a 30 fps export is never drawn on the frame at its end"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fade counts frames: it starts on frame `round(st * fps)` and moves in
/// `round(d * fps)` equal steps, whatever `st` and `d` are between frames — and
/// the black fades, the dissolve's alpha ramp and a dip all count the same way.
///
/// At each rate: a red clip with its own off-grid fade-in and fade-out over black; a
/// red-to-blue dissolve (the blue share of the mix is the ramp); and a dip to black
/// (the outgoing red leaving, the incoming blue arriving). Every drawn frame must read
/// within 8/255 of `FadeStep::progress_at_frame`; `(t - st) / d` is off by up to a
/// frame's worth of fade, more than that at the ends.
#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn a_fade_follows_ffmpegs_frame_counting_at_every_rate() {
    const TOLERANCE: f64 = 8.0 / 255.0;
    let dir = scratch("fade");
    let mut worst = 0.0f64;
    for (name, num, den) in RATES {
        let fps: f64 = name.parse().unwrap();
        let rate = (num, den);
        let red = solid(&dir, "red", name, 12.0);
        let blue = solid(&dir, "blue", name, 12.0);
        // A start on the frame grid, `secs` in.
        let at = |secs: f64| (secs * fps).round() as u64;

        // ---- a clip's own fades over black -------------------------------------
        let (sf, n) = (at(1.0), at(4.0));
        let mut clip = make_clip(red.id, 0.0, n as f64 / fps, sf as f64 / fps);
        clip.fade_in = 0.7;
        clip.fade_out = 1.3;
        let timeline = timeline_of(vec![video_track(vec![clip])]);
        let assets = [red.clone()];
        let frames = export_frames(&timeline, &assets, fps, &dir, &format!("own-{name}"));
        for (_, _, clip, fx) in clips_with_fx(&timeline, &assets) {
            let timing = ClipTiming::new(clip, &fx);
            for k in sf..sf + n {
                let (measured, expected) = (
                    rgb(&frames[k as usize])[0] / 255.0,
                    strength(&timing, FadeTint::Black, k, rate),
                );
                worst = worst.max((measured - expected).abs());
                assert!(
                    (measured - expected).abs() <= TOLERANCE,
                    "{name} fps, own fades: frame {k} reads {measured:.3}, the frame count says {expected:.3} ({:?})",
                    timing.fades()
                );
            }
        }

        // ---- a dissolve and a dip: red, then blue from `cut` --------------------
        for (label, kind, secs) in [
            ("dissolve", TransitionKind::Crossfade, 0.7),
            ("dip", TransitionKind::DipToBlack, 1.3),
        ] {
            let cut = at(3.0);
            let a = make_clip(red.id, 0.0, cut as f64 / fps, 0.0);
            let mut b = make_clip(blue.id, 0.0, at(3.0) as f64 / fps, cut as f64 / fps);
            b.transition_in = Some(Transition { kind, duration: secs });
            let timeline = timeline_of(vec![video_track(vec![a, b])]);
            let assets = [red.clone(), blue.clone()];
            let frames = export_frames(&timeline, &assets, fps, &dir, &format!("{label}-{name}"));
            let rows: Vec<_> = clips_with_fx(&timeline, &assets).collect();
            let (timing_a, timing_b) = (ClipTiming::new(rows[0].2, &rows[0].3), ClipTiming::new(rows[1].2, &rows[1].3));
            for k in cut.saturating_sub(at(1.5))..cut + at(1.5) {
                let p = rgb(&frames[k as usize]);
                let (measured, expected) = if label == "dissolve" {
                    // The blue share of the mix: A is red underneath, B's alpha is the ramp.
                    (p[2] / (p[0] + p[2]).max(1.0), strength(&timing_b, FadeTint::Alpha, k, rate))
                } else if k >= cut {
                    // The incoming blue rises from black ...
                    (p[2] / 255.0, strength(&timing_b, FadeTint::Black, k, rate))
                } else {
                    // ... after the outgoing red has gone.
                    (p[0] / 255.0, strength(&timing_a, FadeTint::Black, k, rate))
                };
                worst = worst.max((measured - expected).abs());
                assert!(
                    (measured - expected).abs() <= TOLERANCE,
                    "{name} fps, {label}: frame {k} reads {measured:.3}, the frame count says {expected:.3} ({:?} / {:?})",
                    timing_a.fades(),
                    timing_b.fades()
                );
            }
        }
    }
    eprintln!("worst fade error against the frame count: {:.1}/255", worst * 255.0);
    let _ = std::fs::remove_dir_all(&dir);
}
