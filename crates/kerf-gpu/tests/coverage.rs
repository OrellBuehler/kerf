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
//! Five real-world shapes follow them (T4 to T8): a 16:9 talking head with lower thirds and a music
//! bed, a phone vlog (9:16 clips in a 16:9 canvas at 60 fps, BT.709 and BT.601 mixed, dissolves; and
//! the same over a blurred fill), a photo slideshow with Ken Burns moves and crossfades, a multicam
//! cut with captions and a 4K source graded into a 1080p canvas. All sources are generated.
//!
//! `the_ledger_...` is the backlog of refusals, measured: for every output frame of every archetype, in both
//! plan modes, whether the compositor draws it and, when it does not, which `Unsupported` variants refuse it,
//! and so which one lifted alone would draw the most. It plans and renders nothing and writes
//! `target/parity/ledger.txt` (and `ledger-<ffmpeg version>.txt`, so the two builds keep their own):
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test coverage -- --ignored --nocapture ledger
//! ```

#![allow(clippy::print_stderr)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use kerf_core::model::{
    Color, CurvePoint, Curves, Fit, Keyframe, Mask, MaskShape, Transition, TransitionKind, VideoEffect, Wheel, Wheels,
};
use kerf_core::{
    composite_color_policy, Asset, Clip, Delivery, ExportOptions, GpuCaps, OriginalMedia, PlanRequest, Planner, Project,
    StreamKind, TextOverlay, Timeline, Track, Transform, Unsupported,
};
use kerf_gpu::Compositor;

#[path = "common/child.rs"]
mod child;

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

fn timeline(tracks: Vec<Track>, overlays: Vec<TextOverlay>, format: Delivery) -> Timeline {
    Timeline {
        tracks,
        overlays,
        svg_overlays: Vec::new(),
        markers: vec![],
        format: Some(format),
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

fn key(time: f64, scale: f64, pos_x: f64, pos_y: f64) -> Keyframe {
    Keyframe {
        time,
        scale,
        pos_x,
        pos_y,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    }
}

/// A lower third: the name of whoever is on screen, bottom left, on a translucent bar.
fn lower_third(text: &str, from: f64, to: f64) -> TextOverlay {
    let mut o = TextOverlay::new(text, from, to);
    o.pos_x = 0.2;
    o.pos_y = 0.86;
    o.size = 0.045;
    o.bg = Some("black@0.6".into());
    o
}

fn crossfade(secs: f64) -> Option<Transition> {
    Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: secs,
    })
}

/// The generated media the archetypes cut, probed like an import's.
struct Media {
    /// 1280x720, 25 fps, 30 s.
    base: Asset,
    /// 960x540, 25 fps, 30 s.
    side: Asset,
    /// Multicam: 1920x1080, 1920x1080 and 1280x720 at 30 fps, 20 s each.
    cams: [Asset; 3],
    /// A phone clip: 1080x1920 at 60 fps, 8 s, tagged BT.709 and BT.601.
    phone709: Asset,
    phone601: Asset,
    /// Photos: a 3:2 JPEG (`yuvj420p`), a portrait JPEG and a 16:9 PNG (`rgb24`).
    photos: [Asset; 3],
    /// 3840x2160 at 30 fps, 12 s.
    uhd: Asset,
    /// A music bed: audio only, 60 s.
    music: Asset,
}

impl Media {
    fn assets(&self) -> Vec<Asset> {
        let mut all = vec![self.base.clone(), self.side.clone()];
        all.extend(self.cams.iter().cloned());
        all.extend([self.phone709.clone(), self.phone601.clone()]);
        all.extend(self.photos.iter().cloned());
        all.extend([self.uhd.clone(), self.music.clone()]);
        all
    }
}

fn media() -> &'static Media {
    static M: OnceLock<Media> = OnceLock::new();
    M.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("coverage-media");
        std::fs::create_dir_all(&dir).unwrap();
        let video = |name: &str, lavfi: &str, rate: u32, secs: u32, extra: &[&str]| {
            let gop = rate.to_string();
            let args = [
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-crf",
                "35",
                "-g",
                &gop,
                "-pix_fmt",
                "yuv420p",
            ];
            make(
                &dir,
                name,
                &["-f", "lavfi", "-i", &format!("{lavfi}:rate={rate}:duration={secs}")],
                &args,
                extra,
            )
        };
        let tags = |matrix: &'static str, tag: &'static str| {
            ["-vf", matrix, "-colorspace", tag, "-color_primaries", tag, "-color_trc", tag]
        };
        let t709 = tags("scale=out_color_matrix=bt709:out_range=tv,format=yuv420p", "bt709");
        let t601 = tags("scale=out_color_matrix=bt601:out_range=tv,format=yuv420p", "smpte170m");
        let photo = |name: &str, size: &str, quality: &[&str]| {
            make(
                &dir,
                name,
                &["-f", "lavfi", "-i", &format!("testsrc2=size={size}:rate=1:duration=1")],
                &["-frames:v", "1"],
                quality,
            )
        };
        let probe = |p: PathBuf| Project::probe_asset(&p).unwrap_or_else(|e| panic!("probe {}: {e}", p.display()));
        Media {
            base: probe(video("base.mp4", "testsrc2=size=1280x720", 25, 30, &[])),
            side: probe(video("side.mp4", "testsrc2=size=960x540", 25, 30, &[])),
            cams: [
                probe(video("cam-a.mp4", "testsrc2=size=1920x1080", 30, 20, &[])),
                probe(video("cam-b.mp4", "testsrc=size=1920x1080", 30, 20, &[])),
                probe(video("cam-c.mp4", "rgbtestsrc=size=1280x720", 30, 20, &[])),
            ],
            phone709: probe(video("phone-709.mp4", "testsrc2=size=1080x1920", 60, 8, &t709)),
            phone601: probe(video("phone-601.mp4", "testsrc2=size=1080x1920", 60, 8, &t601)),
            photos: [
                probe(photo("photo-3x2.jpg", "1920x1280", &["-q:v", "3", "-pix_fmt", "yuvj420p"])),
                probe(photo(
                    "photo-portrait.jpg",
                    "1280x1920",
                    &["-q:v", "3", "-pix_fmt", "yuvj420p"],
                )),
                probe(photo("photo-16x9.png", "1920x1080", &["-pix_fmt", "rgb24"])),
            ],
            uhd: probe(video("uhd.mp4", "testsrc2=size=3840x2160", 30, 12, &[])),
            music: probe(make(
                &dir,
                "music.m4a",
                &["-f", "lavfi", "-i", "sine=frequency=220:duration=60"],
                &["-c:a", "aac", "-b:a", "96k"],
                &[],
            )),
        }
    })
}

fn archetypes(m: &Media) -> Vec<(&'static str, Timeline)> {
    let (base, side) = (&m.base, &m.side);
    let mut out = vec![];
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
            c.keyframes = vec![key(0.0, 1.0, 0.0, 0.0), key(15.0, 1.25, -0.05, 0.0)];
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
    out.extend(real_world(m));
    out
}

/// Five shapes of project people make, T4 to T8 (the review's archetypes above are T1 to T3).
fn real_world(m: &Media) -> Vec<(&'static str, Timeline)> {
    let mut out = vec![];
    let music = |secs: f64| Track {
        clips: vec![Clip::new(m.music.id, 0.0, secs, 0.0)],
        ..Track::new(StreamKind::Audio, "A1")
    };

    // T4: one talking head in 16:9 with jump cuts, a title card, lower thirds and music under it.
    let mut clips = vec![];
    let (mut at, mut i) = (0.0, 0);
    while at < 48.0 {
        let dur = [7.0, 4.0, 9.0, 5.0, 6.0][i % 5];
        let mut c = clip(&m.base, f64::from((i * 5 % 9) as u32), dur, at);
        if i == 0 {
            c.fade_in = 0.5;
        }
        clips.push(c);
        at += dur;
        i += 1;
    }
    clips.last_mut().expect("clips").fade_out = 0.8;
    let mut titles = vec![{
        let mut t = TextOverlay::new("How I edit in one afternoon", 0.5, 4.0);
        t.pos_y = 0.5;
        t.size = 0.09;
        t
    }];
    titles.extend((0..4).map(|i| {
        lower_third(
            "Jane Doe, video editor",
            6.0 + f64::from(i) * 11.0,
            10.0 + f64::from(i) * 11.0,
        )
    }));
    let delivery = Delivery::new(1920, 1080, Fit::Cover);
    out.push((
        "T4  talking head 16:9, jump cuts, title + lower thirds, music",
        timeline(vec![track(clips, "V1"), music(at)], titles, delivery),
    ));

    // T5: a phone vlog: vertical clips in a landscape canvas at 60 fps, some BT.709 and some BT.601,
    // dissolved into each other; once beside a black bar fit, once over a blurred fill.
    let vlog = |fit: Fit| {
        let mut clips = vec![];
        for i in 0..8 {
            let src = if i % 3 == 1 { &m.phone601 } else { &m.phone709 };
            let mut c = clip(src, f64::from(i % 3), 4.0, f64::from(i) * 4.0);
            if i > 0 {
                c.transition_in = crossfade(0.5);
            }
            clips.push(c);
        }
        clips[0].fade_in = 0.5;
        clips[7].fade_out = 1.0;
        let delivery = Delivery::new(1920, 1080, fit).with_fps(Some(60.0)).expect("a rate");
        timeline(
            vec![track(clips, "V1")],
            vec![TextOverlay::new("Day 3: the coast", 1.0, 4.0)],
            delivery,
        )
    };
    out.push((
        "T5  phone vlog 9:16 in 16:9 at 60 fps, 709 + 601, dissolves",
        vlog(Fit::Contain),
    ));
    out.push(("T5b same over a blurred fill", vlog(Fit::Blur)));

    // T6: a photo slideshow: JPEG and PNG stills of three shapes, each on a slow Ken Burns move, crossfaded.
    let mut clips = vec![];
    for i in 0..10 {
        let photo = &m.photos[i % 3];
        let mut c = Clip::new(photo.id, 0.0, 4.5, f64::from(i as u32) * 4.5);
        let dir = if i % 2 == 0 { 1.0 } else { -1.0 };
        c.keyframes = if i % 4 < 2 {
            vec![key(0.0, 1.0, -0.03 * dir, 0.0), key(4.5, 1.18, 0.03 * dir, -0.02)]
        } else {
            vec![key(0.0, 1.2, 0.04 * dir, 0.03), key(4.5, 1.0, 0.0, 0.0)]
        };
        if i > 0 {
            c.transition_in = crossfade(0.5);
        }
        clips.push(c);
    }
    clips[0].fade_in = 0.7;
    clips[9].fade_out = 0.7;
    let delivery = Delivery::new(1920, 1080, Fit::Cover).with_fps(Some(30.0)).expect("a rate");
    out.push((
        "T6  photo slideshow 16:9, Ken Burns, crossfades, music",
        timeline(
            vec![track(clips, "V1"), music(45.0)],
            vec![TextOverlay::new("Summer 2026", 0.8, 4.0)],
            delivery,
        ),
    ));

    // T7: a multicam interview: hard cuts among three angles, captions on top.
    let mut clips = vec![];
    let (mut at, mut i) = (0.0, 0);
    while at < 60.0 {
        let dur = [3.0, 5.5, 2.5, 7.0, 4.0, 3.5][i % 6];
        let cam = &m.cams[[0, 1, 0, 2, 0, 1, 2][i % 7]];
        clips.push(clip(cam, f64::from((i * 3 % 11) as u32), dur, at));
        at += dur;
        i += 1;
    }
    let delivery = Delivery::new(1920, 1080, Fit::Cover).with_fps(Some(30.0)).expect("a rate");
    out.push((
        "T7  multicam 16:9, hard cuts among 3 angles, captions",
        timeline(vec![track(clips, "V1")], captions(1.0, at), delivery),
    ));

    // T8: 4K footage in a 1080p canvas, graded: contrast, saturation and warmth, a tone curve, and
    // lifted shadows on two of the shots.
    let mut clips = vec![];
    for i in 0..6 {
        let mut c = clip(&m.uhd, f64::from(i % 4), 5.0, f64::from(i) * 5.0);
        c.color = Color {
            contrast: 1.08,
            saturation: 1.15,
            temperature: 0.1,
            ..Color::default()
        };
        c.grade.curves = Some(Curves {
            master: [(0.0, 0.0), (0.25, 0.2), (0.75, 0.8), (1.0, 1.0)]
                .map(|(x, y)| CurvePoint { x, y })
                .to_vec(),
            ..Curves::default()
        });
        if i % 3 == 1 {
            c.grade.wheels = Some(Wheels {
                shadows: Wheel {
                    x: 0.1,
                    y: -0.05,
                    lightness: 0.1,
                },
                ..Wheels::default()
            });
        }
        clips.push(c);
    }
    out.push((
        "T8  4K in a 1080p canvas, graded (eq + curves + wheels), hard cuts",
        timeline(
            vec![track(clips, "V1")],
            vec![lower_third("Mont Blanc, 4,808 m", 2.0, 7.0)],
            Delivery::new(1920, 1080, Fit::Cover),
        ),
    ));
    out
}

/// How long an encode of one generated source may take: the longest (4K, 12 s) takes seconds, a loaded box minutes.
const MAKE_LIMIT: std::time::Duration = std::time::Duration::from_secs(600);

/// FNV-1a over the parts (each ended by a zero byte): the name of a cache directory, which has to be the same in every run
/// and need not be secret.
fn fingerprint(parts: &[&str]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.bytes().chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

/// `name` made by FFmpeg from `input` and `encode` (then `extra`) in a directory of `dir` named for those arguments and for the
/// FFmpeg that runs them (its path and `-version`), unless an earlier run with the same left it: another encode, or another
/// build, is another file. Written under another name and renamed, so a run that was killed leaves nothing a later one trusts.
fn make(dir: &Path, name: &str, input: &[&str], encode: &[&str], extra: &[&str]) -> PathBuf {
    let ffmpeg = kerf_core::ffmpeg_path();
    let mut parts: Vec<&str> = vec![name, ffmpeg.as_str(), ffmpeg_version().0.as_str()];
    parts.extend(input.iter().chain(encode).chain(extra));
    let dir = dir.join(format!("{:016x}", fingerprint(&parts)));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    if path.metadata().is_ok_and(|m| m.len() > 0) {
        return path;
    }
    let tmp = dir.join(format!("part-{}-{name}", std::process::id()));
    let mut cmd = Command::new(&ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(input)
        .args(encode)
        .args(extra)
        .arg(&tmp);
    let out = child::output_within(cmd, MAKE_LIMIT).unwrap_or_else(|e| panic!("{name}: ffmpeg {e}"));
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    std::fs::rename(&tmp, &path).unwrap();
    path
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
    let media = media();
    let assets = media.assets();
    let policy = composite_color_policy();
    eprintln!("composite colour policy: {policy:?}");
    eprintln!(
        "{:<76} {:>14} {:>14} {:>14}   reasons left",
        "cut (render 540 px wide), plan", "GPU-2", "GPU-4", "GPU-6"
    );
    let mut table = vec![];
    for (name, tl) in archetypes(media) {
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
    // A push-in is placed at sub-pixel positions by FFmpeg's `perspective` (the export and the still
    // alike), which no compositor pass draws: refused, the rest drawn.
    for mode in ["still", "motion"] {
        let (_, _, after) = at("T3", mode);
        assert!(after > 10.0 && after < 20.0, "T3 {mode}: {after}");
    }
}

// ---------------------------------------------------------------------------------------------
// The ledger.

/// Why a frame is refused: the `Unsupported` variant ([`Unsupported::kind`]) and, for the variants that bundle
/// unrelated causes under one name, the cause (`effects/a blur …`, `dip_to_white/Resized`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Reason {
    variant: &'static str,
    detail: String,
}

/// A frame the planner could not plan at all (a clip whose asset is gone): never drawn, whatever the caps.
const UNPLANNABLE: &str = "unplannable";

/// `s` on one line, at most `max` characters: for a table's column. Never for a key (two causes that differ late in a long
/// message are two causes).
fn column(s: &str, max: usize) -> String {
    let s: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if s.chars().count() <= max {
        s
    } else {
        format!("{}...", s.chars().take(max.saturating_sub(3)).collect::<String>())
    }
}

fn reason(u: &Unsupported) -> Reason {
    let variant = u.kind();
    let cause: Option<String> = match u {
        Unsupported::Delivery(s) | Unsupported::Scaler(s) | Unsupported::Geometry(_, s) => Some(s.clone()),
        Unsupported::AlphaPicture(_, s)
        | Unsupported::NotKnownOpaque(_, s)
        | Unsupported::TranslucentMatrix(_, s)
        | Unsupported::GradedFullRange(_, s) => Some(s.clone()),
        Unsupported::FormatResize(_, _, _, pix_fmt) => Some(pix_fmt.clone()),
        Unsupported::Effects(_, s)
        | Unsupported::Grade(_, s)
        | Unsupported::GradeWith(_, s)
        | Unsupported::MaskWith(_, s)
        | Unsupported::Matte(_, s)
        | Unsupported::ZoomBehind(_, s)
        | Unsupported::FoldedFit(_, s)
        | Unsupported::FullRangeResized(_, s) => Some(s.to_string()),
        Unsupported::DipToWhite(_, why) => Some(format!("{why:?}").split('(').next().unwrap_or_default().to_string()),
        Unsupported::Fade(_, edge, dissolve) => Some(format!("{edge:?} {}", if *dissolve { "dissolve" } else { "step" })),
        Unsupported::BlendMode(_, mode) => Some(format!("{mode:?}")),
        Unsupported::UnmeasuredPolicy(m) | Unsupported::RgbInStack(m) => Some(format!("{m:?}")),
        _ => None,
    };
    Reason {
        variant,
        detail: cause.map_or_else(|| variant.to_string(), |c| format!("{variant}/{c}")),
    }
}

/// The frames of one archetype in one plan mode, grouped by the set of reasons that refuse them (empty: drawn).
struct Cut {
    name: String,
    mode: &'static str,
    frames: Vec<(Vec<Reason>, u64)>,
}

impl Cut {
    /// `T2` of `T2  montage ...`: how the tables name it.
    fn id(&self) -> &str {
        self.name.split_whitespace().next().unwrap_or("?")
    }

    fn sampled(&self) -> u64 {
        self.frames.iter().map(|(_, n)| n).sum()
    }

    fn drawn(&self) -> u64 {
        self.frames.iter().filter(|(why, _)| why.is_empty()).map(|(_, n)| n).sum()
    }
}

/// The reasons the compositor with `caps` refuses output frame `k` of `planner`, sorted and without repeats.
fn why_not(planner: &Planner, k: u64, size: (u32, u32), caps: &GpuCaps) -> Vec<Reason> {
    let mut why: Vec<Reason> = match planner.at_frame(k) {
        Ok(plan) => plan.reasons(caps, size).iter().map(reason).collect(),
        Err(_) => vec![Reason {
            variant: UNPLANNABLE,
            detail: UNPLANNABLE.to_string(),
        }],
    };
    why.sort();
    why.dedup();
    why
}

/// What lifting one reason alone would draw.
#[derive(Default)]
struct Tally {
    /// Frames this reason is among the reasons of.
    refused: u64,
    /// Frames it is the only reason of: drawn once it is lifted.
    only: u64,
    only_still: u64,
    only_motion: u64,
    hits: BTreeSet<String>,
}

/// Every reason's tally, keyed by `key` of the reason: the variant, or the variant with its cause.
fn tally(cuts: &[Cut], key: impl Fn(&Reason) -> String) -> BTreeMap<String, Tally> {
    let mut all: BTreeMap<String, Tally> = BTreeMap::new();
    for cut in cuts {
        for (why, n) in &cut.frames {
            let keys: BTreeSet<String> = why.iter().map(&key).collect();
            for k in &keys {
                let t = all.entry(k.clone()).or_default();
                t.refused += n;
                t.hits.insert(format!("{}:{}", cut.id(), &cut.mode[..1]));
                if keys.len() == 1 {
                    t.only += n;
                    if cut.mode == "still" {
                        t.only_still += n;
                    } else {
                        t.only_motion += n;
                    }
                }
            }
        }
    }
    all
}

/// `T1:s T2:sm`: the archetypes a reason hits, with the plan modes (s still, m motion).
fn hit_list(hits: &BTreeSet<String>) -> String {
    let mut ids: BTreeMap<&str, (bool, bool)> = BTreeMap::new();
    for h in hits {
        if let Some((id, mode)) = h.split_once(':') {
            let e = ids.entry(id).or_default();
            e.0 |= mode == "s";
            e.1 |= mode == "m";
        }
    }
    ids.into_iter()
        .map(|(id, (s, m))| format!("{id}:{}{}", if s { "s" } else { "" }, if m { "m" } else { "" }))
        .collect::<Vec<_>>()
        .join(" ")
}

fn pct(n: u64, of: u64) -> f64 {
    if of == 0 {
        0.0
    } else {
        100.0 * n as f64 / of as f64
    }
}

/// A ranking of reasons, the most frames drawn by lifting one alone first.
fn ranking(title: &str, rows: &BTreeMap<String, Tally>, total: u64, out: &mut String) {
    let mut sorted: Vec<(&String, &Tally)> = rows.iter().collect();
    sorted.sort_by(|a, b| b.1.only.cmp(&a.1.only).then(b.1.refused.cmp(&a.1.refused)).then(a.0.cmp(b.0)));
    let _ = writeln!(out, "{title}");
    let _ = writeln!(
        out,
        "  {:>2}  {:<72} {:>8} {:>6}  {:>8} {:>6} {:>8} {:>8}  archetypes hit",
        "#", "reason", "refused", "%", "gain", "%", "still", "motion"
    );
    for (i, (name, t)) in sorted.iter().enumerate() {
        let _ = writeln!(
            out,
            "  {:>2}  {:<72} {:>8} {:>5.1}%  {:>8} {:>5.1}% {:>8} {:>8}  {}",
            i + 1,
            column(name, 72),
            t.refused,
            pct(t.refused, total),
            t.only,
            pct(t.only, total),
            t.only_still,
            t.only_motion,
            hit_list(&t.hits)
        );
    }
    let _ = writeln!(out);
}

/// The ledger as text: see the module docs for what the columns hold.
fn render_ledger(header: &[String], cuts: &[Cut]) -> String {
    let mut out = String::new();
    for line in header {
        let _ = writeln!(out, "{line}");
    }
    let total: u64 = cuts.iter().map(Cut::sampled).sum();
    let drawn: u64 = cuts.iter().map(Cut::drawn).sum();
    let _ = writeln!(out);
    let _ = writeln!(out, "(a) frames sampled / drawn / refused, per archetype and plan mode");
    let _ = writeln!(
        out,
        "  {:<68} {:<6} {:>8} {:>8} {:>8} {:>7}",
        "archetype", "mode", "sampled", "drawn", "refused", "drawn %"
    );
    for cut in cuts {
        let (sampled, drawn) = (cut.sampled(), cut.drawn());
        let _ = writeln!(
            out,
            "  {:<68} {:<6} {:>8} {:>8} {:>8} {:>6.1}%",
            column(&cut.name, 66),
            cut.mode,
            sampled,
            drawn,
            sampled - drawn,
            pct(drawn, sampled)
        );
    }
    for mode in ["still", "motion"] {
        let of: Vec<&Cut> = cuts.iter().filter(|c| c.mode == mode).collect();
        let (sampled, drawn) = (
            of.iter().map(|c| c.sampled()).sum::<u64>(),
            of.iter().map(|c| c.drawn()).sum::<u64>(),
        );
        let _ = writeln!(
            out,
            "  {:<68} {:<6} {:>8} {:>8} {:>8} {:>6.1}%",
            "all archetypes",
            mode,
            sampled,
            drawn,
            sampled - drawn,
            pct(drawn, sampled)
        );
    }
    let _ = writeln!(
        out,
        "  {:<68} {:<6} {:>8} {:>8} {:>8} {:>6.1}%",
        "all archetypes",
        "both",
        total,
        drawn,
        total - drawn,
        pct(drawn, total)
    );
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "(b) per Unsupported variant, by the frames drawn if that variant alone were lifted\n    refused = frames it is among the reasons of; gain = frames it is the only reason of (still / motion split);\n    archetypes hit = id:s (still plan) / id:m (motion) / id:sm; % of all {total} sampled frames"
    );
    ranking("", &tally(cuts, |r| r.variant.to_string()), total, &mut out);
    let _ = writeln!(
        out,
        "(c) the same by variant and cause (a variant that bundles unrelated causes is split)"
    );
    ranking("", &tally(cuts, |r| r.detail.clone()), total, &mut out);

    // (d) the order a backlog would be worked in: lift the variant that draws the most frames, again.
    let mut sets: BTreeMap<BTreeSet<&'static str>, u64> = BTreeMap::new();
    for cut in cuts {
        for (why, n) in &cut.frames {
            *sets.entry(why.iter().map(|r| r.variant).collect()).or_default() += n;
        }
    }
    let mut lifted: BTreeSet<&'static str> = BTreeSet::new();
    let mut now = drawn;
    let _ = writeln!(out, "(d) lifting one variant after another, the most frames drawn first");
    let _ = writeln!(out, "  {:>2}  {:<40} {:>8} {:>9}", "#", "lift", "gain", "drawn %");
    let _ = writeln!(out, "  {:>2}  {:<40} {:>8} {:>8.1}%", 0, "(today)", "", pct(now, total));
    loop {
        let mut gains: BTreeMap<&'static str, u64> = BTreeMap::new();
        for (set, n) in &sets {
            let left: Vec<&&str> = set.iter().filter(|v| !lifted.contains(**v)).collect();
            if let [only] = left.as_slice() {
                *gains.entry(**only).or_default() += n;
            }
        }
        // A frame the planner could not plan is not made drawable by lifting anything.
        let Some((best, gain)) = gains
            .into_iter()
            .filter(|(variant, _)| *variant != UNPLANNABLE)
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
        else {
            break;
        };
        lifted.insert(best);
        now += gain;
        let _ = writeln!(
            out,
            "  {:>2}  {:<40} {:>8} {:>8.1}%",
            lifted.len(),
            best,
            gain,
            pct(now, total)
        );
    }
    let _ = writeln!(out);

    // (e) what is left: the variants that still refuse a frame together, the ones (d) lifted left out.
    let mut left: BTreeMap<String, u64> = BTreeMap::new();
    for (set, n) in &sets {
        let rest: Vec<&str> = set.iter().copied().filter(|v| !lifted.contains(v)).collect();
        if !rest.is_empty() {
            *left.entry(rest.join(" + ")).or_default() += n;
        }
    }
    let mut left: Vec<(String, u64)> = left.into_iter().collect();
    left.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let _ = writeln!(
        out,
        "(e) frames still refused after (d): {} ({:.1}%), by the variants that still refuse them together",
        total - now,
        pct(total - now, total)
    );
    for (set, n) in left.iter().take(12) {
        let _ = writeln!(out, "  {n:>8} {:>5.1}%  {set}", pct(*n, total));
    }
    out
}

/// The first line `ffmpeg -version` prints (`ffmpeg version 6.1.1-3ubuntu5 ...`) and the number in it (`6.1.1`), which
/// names the ledger's file so each build keeps its own. Only a file name and a cache key: nothing is decided by it.
fn ffmpeg_version() -> &'static (String, String) {
    static V: OnceLock<(String, String)> = OnceLock::new();
    V.get_or_init(|| {
        let mut cmd = Command::new(kerf_core::ffmpeg_path());
        cmd.arg("-version");
        let out = child::output_within(cmd, std::time::Duration::from_secs(60)).expect("ffmpeg -version");
        let line = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("ffmpeg")
            .to_string();
        let number: String = line
            .split_whitespace()
            .nth(2)
            .unwrap_or("")
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let number = if number.is_empty() { "unknown".to_string() } else { number };
        (line, number)
    })
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_ledger_says_which_refusals_hold_the_compositor_back() {
    let media = media();
    let assets = media.assets();
    let policy = composite_color_policy();
    // The caps the app asks with: the grade is drawn only where this FFmpeg's conversions are the model's (one probe).
    let kernels = kerf_gpu::grade::kernels();
    let caps = Compositor::caps_for(kernels);
    let mut cuts = vec![];
    for (name, tl) in archetypes(media) {
        for (mode, request) in [("still", PlanRequest::still(policy)), ("motion", PlanRequest::motion(policy))] {
            let planner =
                Planner::new(&tl, &assets, &ExportOptions::default(), request.with_media(&OriginalMedia)).expect("planner");
            let first = planner.at_frame(0).expect("a plan");
            let fps = first.canvas.fps;
            let frames = (tl.duration() * f64::from(fps.num) / f64::from(fps.den)).floor() as u64;
            let size = first.size(540);
            let mut grouped: BTreeMap<Vec<Reason>, u64> = BTreeMap::new();
            for k in 0..frames {
                *grouped.entry(why_not(&planner, k, size, &caps)).or_default() += 1;
            }
            cuts.push(Cut {
                name: name.to_string(),
                mode,
                frames: grouped.into_iter().collect(),
            });
        }
    }
    let (version_line, version) = ffmpeg_version();
    let header = vec![
        "kerf-gpu coverage ledger: the frames the compositor draws and what refuses the rest".to_string(),
        format!("ffmpeg: {version_line}"),
        format!("composite colour policy: {policy:?}"),
        format!("grade kernels: {kernels:?} (caps: Compositor::caps_for(grade::kernels()), as the app asks)"),
        "plans: every output frame of every archetype over OriginalMedia at 540 px wide (the app's preview plans ProxyMedia at 960); nothing is rendered".to_string(),
        "archetypes: the review's T1 to T3 and the real-world T4 to T8 of tests/coverage.rs".to_string(),
    ];
    let text = render_ledger(&header, &cuts);
    eprintln!("{text}");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap().join("parity");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ledger.txt"), &text).unwrap();
    std::fs::write(dir.join(format!("ledger-{version}.txt")), &text).unwrap();

    // The ledger accounts for every frame: drawn plus refused is what was sampled, and a refused frame has a reason.
    for cut in &cuts {
        assert!(cut.sampled() > 0, "{} [{}]: no frame sampled", cut.name, cut.mode);
        assert!(
            cut.frames
                .iter()
                .all(|(why, n)| *n > 0 && why.windows(2).all(|w| w[0] < w[1])),
            "{}: {:?}",
            cut.name,
            cut.frames
        );
    }
}

fn layer_ref() -> kerf_core::LayerRef {
    kerf_core::LayerRef {
        index: 0,
        name: "a.mp4".into(),
    }
}

fn cut(name: &str, mode: &'static str, frames: &[(&[(&'static str, &str)], u64)]) -> Cut {
    Cut {
        name: name.to_string(),
        mode,
        frames: frames
            .iter()
            .map(|(why, n)| {
                let why = why
                    .iter()
                    .map(|(variant, cause)| Reason {
                        variant,
                        detail: if cause.is_empty() {
                            (*variant).to_string()
                        } else {
                            format!("{variant}/{cause}")
                        },
                    })
                    .collect();
                (why, *n)
            })
            .collect(),
    }
}

#[test]
fn a_reason_is_named_by_its_variant_and_the_cause_a_variant_bundles() {
    let r = reason(&Unsupported::Text);
    assert_eq!((r.variant, r.detail.as_str()), ("text", "text"));
    let r = reason(&Unsupported::Effects(layer_ref(), "a blur on a letterboxed picture"));
    assert_eq!(
        (r.variant, r.detail.as_str()),
        ("effects", "effects/a blur on a letterboxed picture")
    );
    let r = reason(&Unsupported::DipToWhite(layer_ref(), kerf_core::DipWhy::OddSize(3, 5)));
    assert_eq!(r.detail, "dip_to_white/OddSize");
    let r = reason(&Unsupported::FormatResize(1, (4, 4), (2, 2), "yuv422p".into()));
    assert_eq!(r.detail, "format_resize/yuv422p");
    // A key is never cut: causes that differ late in a long message are two causes. A table's column is.
    let long = "x".repeat(300);
    let late = format!("{}y", "x".repeat(300));
    assert_eq!(reason(&Unsupported::Scaler(long.clone())).detail, format!("scaler/{long}"));
    assert_ne!(
        reason(&Unsupported::Scaler(long)).detail,
        reason(&Unsupported::Scaler(late)).detail
    );
    assert_eq!(column(&"x".repeat(300), 72).chars().count(), 72);
    assert_eq!(column("a\nb", 72), "a b");
    // Two layers refused for the same cause are one reason.
    let (a, b) = (
        reason(&Unsupported::Mask(layer_ref())),
        reason(&Unsupported::Mask(kerf_core::LayerRef {
            index: 3,
            name: "b".into(),
        })),
    );
    assert_eq!(a, b);
}

#[test]
fn the_ledger_ranks_by_the_frames_a_variant_alone_would_draw() {
    // 100 frames: 40 drawn, 30 refused only by `fade`, 20 by `mask` alone, 10 by `mask` and `text` together.
    let a = cut(
        "T9  one",
        "still",
        &[
            (&[], 40),
            (&[("fade", "fade/In step")], 30),
            (&[("mask", "")], 20),
            (&[("mask", ""), ("text", "")], 10),
        ],
    );
    let b = cut("T9b two", "motion", &[(&[], 100), (&[("text", "")], 4)]);
    let cuts = [a, b];
    let by_variant = tally(&cuts, |r| r.variant.to_string());
    let (fade, mask, text) = (&by_variant["fade"], &by_variant["mask"], &by_variant["text"]);
    assert_eq!((fade.refused, fade.only, fade.only_still, fade.only_motion), (30, 30, 30, 0));
    assert_eq!((mask.refused, mask.only), (30, 20));
    assert_eq!((text.refused, text.only, text.only_still, text.only_motion), (14, 4, 0, 4));
    assert_eq!(hit_list(&text.hits), "T9:s T9b:m");
    assert_eq!(hit_list(&by_variant["fade"].hits), "T9:s");

    let text = render_ledger(&["header".to_string()], &cuts);
    let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("no {needle:?} in\n{text}"));
    // (b) puts the biggest gain first; (d) lifts fade, then mask, then text (which frees the 10 mask shares).
    let b_section = &text[at("(b)")..at("(c)")];
    assert!(
        b_section.find("fade").unwrap() < b_section.find("mask").unwrap(),
        "{b_section}"
    );
    assert!(
        b_section.find("mask").unwrap() < b_section.find("text").unwrap(),
        "{b_section}"
    );
    let d_section = &text[at("(d)")..at("(e)")];
    assert!(d_section.contains("(today)") && d_section.contains("68.6%"), "{d_section}");
    assert!(
        d_section.find("fade").unwrap() < d_section.find("mask").unwrap(),
        "{d_section}"
    );
    assert!(
        d_section.trim_end().ends_with("100.0%"),
        "everything is drawn once all three are lifted:\n{d_section}"
    );
    // (a): 40 + 100 drawn of 100 + 104 sampled.
    let both = text.lines().find(|l| l.contains("both")).expect("the total");
    assert!(both.contains("204") && both.contains("140") && both.contains("64"), "{both}");
}

#[test]
fn an_unplannable_frame_is_never_lifted_and_what_is_left_lists_only_what_is_left() {
    // 24 frames: 10 drawn; 5 unplannable; 3 unplannable and `text`; 4 `mask` and `text`; 2 `mask` alone.
    let a = cut(
        "T9  x",
        "still",
        &[
            (&[], 10),
            (&[("unplannable", "")], 5),
            (&[("text", ""), ("unplannable", "")], 3),
            (&[("mask", ""), ("text", "")], 4),
            (&[("mask", "")], 2),
        ],
    );
    let text = render_ledger(&["header".to_string()], &[a]);
    let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("no {needle:?} in\n{text}"));
    let d = &text[at("(d)")..at("(e)")];
    assert!(
        d.find("mask").unwrap() < d.find("text").unwrap() && !d.contains("unplannable"),
        "{d}"
    );
    assert!(d.trim_end().ends_with("66.7%"), "{d}");
    let e = &text[at("(e)")..];
    assert!(e.contains("8 ") && e.contains("unplannable"), "{e}");
    assert!(!e.contains("text") && !e.contains("mask") && !e.contains(" + "), "{e}");
}
