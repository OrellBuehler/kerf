//! How much of a cut the compositor draws, frame by frame, as it grew: layers and text (GPU-2), then
//! fades, dips, dissolves, slides and pushes (GPU-4), then a moving zoom drawn last, masks and the
//! blur / sharpen / grayscale / invert effects (GPU-6), in still plans and in export frames:
//! constructed cuts (not measured projects), planned
//! with the real planner and judged by what a compositor says it draws
//! ([`kerf_gpu::Compositor::CAPS`]). No adapter and no decode — one short source file is made so the
//! assets are probed like an import's. `#[ignore]`d (needs `ffmpeg`); run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test coverage -- --ignored --nocapture
//! ```
//!
//! The archetypes are the review's (`docs/review-2026-10-10/gpu-compositor.md` §4): a captioned
//! 9:16 talking head, the same with hard cuts, the same without captions, a 16:9 montage with
//! dissolves and two titles, and a tutorial with a webcam picture-in-picture, lower thirds and a
//! keyed push-in. Titles and captions carry no font (`font: None`, what every one the app makes
//! has), which before the default font was `TextWithoutFont` for every frame that held one.

#![allow(clippy::print_stderr)]

use std::path::Path;
use std::process::{Command, Stdio};

use kerf_core::model::{Color, Fit, Keyframe, Mask, MaskShape, Transition, TransitionKind, VideoEffect};
use kerf_core::{
    composite_color_policy, Asset, Clip, Delivery, ExportOptions, GpuCaps, OriginalMedia, PlanRequest, Planner, Project,
    StreamKind, TextOverlay, Timeline, Track, Transform,
};
use kerf_gpu::Compositor;

fn clip(a: &Asset, src_in: f64, dur: f64, start: f64) -> Clip {
    let src_in = src_in.min((a.duration - dur - 0.1).max(0.0));
    Clip::new(a.id, src_in, src_in + dur, start)
}

/// Chunks of ~2.2 s with a 0.2 s breath between them, like `generate_captions` on a talking head.
fn captions(from: f64, to: f64) -> Vec<TextOverlay> {
    let mut out = vec![];
    let (mut t, mut i) = (from, 0);
    while t + 1.0 < to {
        let end = (t + 1.6 + f64::from(i % 3) * 0.5).min(to);
        let mut o = TextOverlay::new(format!("caption {i}"), t, end);
        o.bg = Some("black@0.5".into());
        out.push(o);
        t = end + 0.2;
        i += 1;
    }
    out
}

fn track(clips: Vec<Clip>, name: &str) -> Track {
    Track {
        clips,
        ..Track::new(StreamKind::Video, name)
    }
}

fn archetypes(base: &Asset, side: &Asset) -> Vec<(&'static str, Timeline)> {
    let mut out = vec![];
    let timeline = |tracks, overlays, format| Timeline {
        tracks,
        overlays,
        svg_overlays: Vec::new(),
        markers: vec![],
        format: Some(format),
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let mut clips = vec![];
    for i in 0..15 {
        let mut c = clip(base, f64::from(i % 6), 4.0, f64::from(i) * 4.0);
        c.fade_in = 0.1;
        c.fade_out = 0.1;
        c.transform = Transform {
            crop_left: 0.2,
            crop_right: 0.2,
            ..Transform::default()
        };
        clips.push(c);
    }
    let t1 = timeline(
        vec![track(clips, "V1")],
        captions(1.0, 59.0),
        Delivery::new(1080, 1920, Fit::Cover),
    );
    let mut hard = t1.clone();
    hard.tracks[0].clips.iter_mut().for_each(|c| {
        c.fade_in = 0.0;
        c.fade_out = 0.0;
    });
    let mut bare = t1.clone();
    bare.overlays.clear();
    out.push(("T1  talking head 9:16, clip fades, captions ~85%", t1));
    out.push(("T1b same, hard cuts, captions", hard));
    out.push(("T1c same, clip fades, no captions", bare));

    let (n, dur) = (18, 2.5);
    let mut clips = vec![];
    for i in 0..n {
        let mut c = clip(base, f64::from(i % 8), dur, f64::from(i) * dur);
        c.color = Color {
            contrast: 1.1,
            saturation: 1.2,
            ..Color::default()
        };
        if i > 0 {
            let kind = match i % 10 {
                0..=5 => Some(TransitionKind::Crossfade),
                6 | 7 => Some(TransitionKind::DipToBlack),
                8 => Some(TransitionKind::SlideLeft),
                _ => None,
            };
            c.transition_in = kind.map(|kind| Transition {
                kind,
                duration: if kind == TransitionKind::Crossfade { 0.5 } else { 0.4 },
            });
        }
        clips.push(c);
    }
    clips[0].fade_in = 1.0;
    clips[n as usize - 1].fade_out = 1.0;
    let total = f64::from(n) * dur;
    let titles = [("Summer 2026", 0.5, 3.5), ("The end", total - 3.5, total - 0.5)]
        .map(|(text, from, to)| {
            let mut t = TextOverlay::new(text, from, to);
            t.bg = Some("black@0.5".into());
            t
        })
        .to_vec();
    out.push((
        "T2  montage 16:9, dissolves / dips / slides, graded, 2 titles",
        timeline(vec![track(clips, "V1")], titles, Delivery::new(1920, 1080, Fit::Cover)),
    ));

    let mut v1 = vec![];
    for i in 0..6 {
        let mut c = clip(base, f64::from(i % 5), 15.0, f64::from(i) * 15.0);
        if i < 5 {
            let key = |time, scale, pos_x| Keyframe {
                time,
                scale,
                pos_x,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
                easing: Default::default(),
            };
            c.keyframes = vec![key(0.0, 1.0, 0.0), key(15.0, 1.25, -0.05)];
        }
        if i == 3 {
            c.mask = Some(Mask {
                shape: MaskShape::Rect,
                x: 0.5,
                y: 0.5,
                width: 0.6,
                height: 0.6,
                feather: 0.1,
                inverted: false,
            });
            c.effects = vec![VideoEffect::Blur { sigma: 6.0 }];
        }
        v1.push(c);
    }
    let v2: Vec<Clip> = (0..4)
        .map(|i| {
            let mut c = clip(side, 0.0, 20.0, f64::from(i) * 22.0);
            c.transform = Transform {
                scale: 0.25,
                pos_x: 0.35,
                pos_y: 0.3,
                ..Transform::default()
            };
            c
        })
        .collect();
    let lower_thirds: Vec<TextOverlay> = (0..5)
        .map(|i| TextOverlay::new(format!("Step {i}"), 3.0 + f64::from(i) * 17.0, 7.0 + f64::from(i) * 17.0))
        .collect();
    out.push((
        "T3  tutorial 16:9, webcam PiP, 5 lower thirds, keyed push-ins, 1 mask + blur",
        timeline(
            vec![track(v1, "V1"), track(v2, "V2")],
            lower_thirds,
            Delivery::new(1920, 1080, Fit::Cover),
        ),
    ));
    out
}

fn source(dir: &Path, name: &str, size: &str) -> Asset {
    let path = dir.join(name);
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={size}:rate=25:duration=30"))
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "35",
            "-g",
            "25",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    Project::probe_asset(&path).expect("probe")
}

/// The share of frames a compositor with `caps` draws, over a cut at `render` px wide.
fn drawn(planner: &Planner, caps: &GpuCaps, frames: u64, size: (u32, u32), end: f64) -> (f64, u64, u64) {
    let span = planner.span(0.0, end, size, caps);
    let ok: u64 = span
        .runs()
        .iter()
        .filter(|r| r.supported)
        .map(|r| r.frames.end - r.frames.start)
        .sum();
    let total = frames.max(1);
    (100.0 * ok as f64 / total as f64, ok, total)
}

/// What the compositor drew before fades, dips, dissolves, slides and pushes: layers, transform,
/// crop, fit, `eq`, opacity, stills, sampled keyframes and text.
fn before_transitions() -> GpuCaps {
    let mut caps = GpuCaps::A0;
    caps.text = true;
    caps
}

/// ... and before a moving zoom, masks and effects: fades and transitions in still plans and export frames.
fn before_zoom_mask_effects() -> GpuCaps {
    let mut caps = Compositor::CAPS;
    caps.keyed_zoom = false;
    caps.mask = false;
    caps.effects = kerf_core::EffectKinds::NONE;
    caps
}

#[test]
#[ignore = "needs ffmpeg"]
fn captioned_cuts_and_transitions_are_drawn_by_the_compositor_where_they_were_refused() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("coverage-media");
    std::fs::create_dir_all(&dir).unwrap();
    let base = source(&dir, "base.mp4", "1280x720");
    let side = source(&dir, "side.mp4", "960x540");
    let assets = vec![base.clone(), side.clone()];
    let policy = composite_color_policy();
    eprintln!("composite colour policy: {policy:?}");
    eprintln!(
        "{:<76} {:>14} {:>14} {:>14}   reasons left",
        "cut (render 540 px wide), plan", "GPU-2", "GPU-4", "GPU-6"
    );
    let mut table = vec![];
    for (name, tl) in archetypes(&base, &side) {
        for (mode, request) in [("still", PlanRequest::still(policy)), ("motion", PlanRequest::motion(policy))] {
            let planner =
                Planner::new(&tl, &assets, &ExportOptions::default(), request.with_media(&OriginalMedia)).expect("planner");
            let first = planner.at_frame(0).expect("a plan");
            let fps = first.canvas.fps;
            let end = tl.duration();
            let frames = (end * f64::from(fps.num) / f64::from(fps.den)).floor() as u64;
            let size = first.size(540);
            // GPU-2: text drawn, no fades, travel or tails, no export frames. GPU-4: those. GPU-6: a moving
            // zoom, masks and effects as well.
            let was = drawn(&planner, &before_transitions(), frames, size, end);
            let before = drawn(&planner, &before_zoom_mask_effects(), frames, size, end);
            let after = drawn(&planner, &Compositor::CAPS, frames, size, end);
            // What refuses the frames, before and after: the reasons' kinds and the frames each is a reason for.
            let reasons_left = |caps: &GpuCaps| {
                let mut left: std::collections::BTreeMap<String, u64> = Default::default();
                for k in 0..frames {
                    if let Ok(plan) = planner.at_frame(k) {
                        let mut seen: Vec<String> = plan
                            .reasons(caps, size)
                            .iter()
                            .map(|r| {
                                format!("{r:?}")
                                    .split(|c: char| !c.is_alphanumeric())
                                    .next()
                                    .unwrap_or("")
                                    .to_string()
                            })
                            .collect();
                        seen.sort();
                        seen.dedup();
                        for s in seen {
                            *left.entry(s).or_default() += 1;
                        }
                    }
                }
                left.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")
            };
            let (gpu4, left) = (reasons_left(&before_zoom_mask_effects()), reasons_left(&Compositor::CAPS));
            eprintln!(
                "{:<76} {:>5.1}% {:>4}/{:<4} {:>5.1}% {:>4}/{:<4} {:>5.1}% {:>4}/{:<4}   {}",
                format!("{name} [{mode}]"),
                was.0,
                was.1,
                was.2,
                before.0,
                before.1,
                before.2,
                after.0,
                after.1,
                after.2,
                left
            );
            eprintln!("{:<76} refused by GPU-4's caps: {gpu4}", "");
            table.push((name, mode, was.0, before.0, after.0));
        }
    }
    let at = |name: &str, mode: &str| {
        table
            .iter()
            .find(|(n, m, ..)| n.starts_with(name) && *m == mode)
            .map(|(_, _, was, before, after)| (*was, *before, *after))
            .expect("an archetype")
    };
    // A talking head with clip fades: drawn throughout once fades are.
    for mode in ["still", "motion"] {
        let (was, _, after) = at("T1  ", mode);
        assert!(was < 100.0 && (after - 100.0).abs() < 1e-9, "T1 {mode}: {was} -> {after}");
        let (was, _, after) = at("T1c", mode);
        assert!(was < 100.0 && (after - 100.0).abs() < 1e-9, "T1c {mode}: {was} -> {after}");
        let (was, _, after) = at("T1b", mode);
        assert!((after - 100.0).abs() < 1e-9 && after >= was, "T1b {mode}: {was} -> {after}");
    }
    // The montage: dissolves, dips and a slide, drawn but for what is not a transition (the titles are).
    for mode in ["still", "motion"] {
        let (was, _, after) = at("T2", mode);
        assert!(
            after > was + 20.0 && (after - 100.0).abs() < 1e-9,
            "T2 {mode}: {was} -> {after}"
        );
    }
    // The tutorial: its push-ins are a moving zoom, and one clip adds a feathered mask and a blur to it.
    // Drawn throughout once a zoom comes last and masks and blurs are drawn.
    for mode in ["still", "motion"] {
        let (_, before, after) = at("T3", mode);
        assert!(
            before < 90.0 && (after - 100.0).abs() < 1e-9,
            "T3 {mode}: {before} -> {after}"
        );
    }
}
