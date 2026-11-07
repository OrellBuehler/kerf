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
//! T9 to T24 exist for the ledger below: between them they use every feature the editor offers that a compositor with
//! [`kerf_gpu::Compositor::CAPS`] still refuses, in the cuts people make with it: blend modes (a music video), stabilized shots and speed
//! ramps (an action cam), keyed crop / mask / colour / opacity / zoom and motion presets (an explainer), masks with feathers and
//! effects (a face blur, a spotlight, a green screen, social looks), a grade on full-range, steep and 10-bit pictures, HDR / BT.2020 /
//! untagged / unknown matrices, alpha pictures (ProRes 4444, PNG, GIF), 4:2:2 / 4:4:4 / RGB sources, text with a border, tiny text and
//! a missing font, SVG titles, behaviours, a 360 reframe, folded and uncropped fits, zoom behind a grade, and the delivery formats
//! (ProRes, GIF, a scaler, 10-bit, a pick rate). They are planned with the export options they name; T1 to T8 and the test above them
//! are unchanged.
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
    composite_color_policy, render_preset, Asset, Behavior, BlendMode, Clip, ClipLut, CompositeColorPolicy, Container, Delivery,
    DipWhy, ExportOptions, FadeEdge, GpuCaps, MotionMode, MotionPreset, OriginalMedia, PlanRequest, Planner, Project, Projection,
    Property, PropertyKey, PropertyTrack, Rational, Reframe, ReframeKeyframe, SpeedKey, Stabilize, StreamKind, SvgOverlay,
    SvgPreset, SvgPresetKind, SvgPresetParams, TextOverlay, Timeline, TitleClip, Track, Transform, Unsupported, YuvMatrix,
};
use kerf_gpu::grade::Kernels;
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

// ---------------------------------------------------------------------------------------------
// T9 and on: what the editor offers that the compositor still refuses.

/// More generated media, for T9 and on: the formats and tags real cameras, phones, drones, screen recorders and motion-graphics
/// packs write, one short source each (what a plan reads is the picture's shape, rate, format and tags, not its length). Cached
/// like [`media`]'s, keyed by the FFmpeg that made them and the arguments.
struct Extra {
    /// 1280x720 at 30000/1001, tagged BT.709: what a phone writes.
    ntsc: Asset,
    /// 1920x1080 at 30 fps, 10-bit HLG in BT.2020: an iPhone's HDR video.
    hlg: Asset,
    /// 1280x720 at 24 fps, 10-bit PQ (HDR10) in BT.2020.
    pq: Asset,
    /// 1280x720, 8-bit 4:2:0 with the BT.2020 matrix and an SDR transfer: a drone's wide-gamut SDR.
    wide: Asset,
    /// 1920x1080 at 30 fps, 10-bit 4:2:0 H.264 tagged BT.709: a mirrorless camera's.
    deep: Asset,
    /// 1920x1080 at 25 fps, ProRes 422 HQ (`yuv422p10le`): a cinema camera's.
    prores: Asset,
    /// 1280x720 at 30 fps, ProRes 4444 with an alpha channel: a motion-graphics lower third.
    lower_third: Asset,
    /// A 512x512 PNG with alpha: a logo.
    logo: Asset,
    /// A 320x240 animated GIF: a sticker.
    sticker: Asset,
    /// A 480x480 PNG with a 64-colour palette (`pal8`): an indexed-colour badge or screenshot.
    badge: Asset,
    /// A 640x360 OpenEXR still in 32-bit float planar RGB (`gbrpf32le`), which is not on the allow-list of known-opaque formats: a
    /// render from a 3D or VFX package.
    plate: Asset,
    /// A 1920x1080 VP9 `.webm` flagged full range, which ffprobe names `yuv420p` (not `yuvj420p`): a browser's screen recording.
    browser: Asset,
    /// 1920x1080 at 30 fps, `yuv444p`: an OBS screen recording.
    screen: Asset,
    /// 1280x720 at 30 fps, RGB (`libx264rgb`): a lossless-colour screen capture.
    capture: Asset,
    /// 1280x720 at 30 fps, MJPEG `yuvj422p` in a `.mov`: a webcam.
    webcam: Asset,
    /// 1920x1080 at 30 fps, H.264 `yuvj420p` flagged full range: a recorder set to "full".
    obs: Asset,
    /// 1920x960 at 30 fps whose projection is set to equirectangular, as the Inspector's override (or spherical metadata) does.
    equirect: Asset,
    /// 1280x720 tagged with the SMPTE 240M matrix, which the compositor has no coefficients for: digitised archive footage.
    archive: Asset,
    /// 1920x1080 at 30 fps: a presenter-sized test pattern on a `0x00b140` green screen.
    green: Asset,
    /// A `.cube` 3D LUT.
    lut: PathBuf,
}

impl Extra {
    fn assets(&self) -> Vec<Asset> {
        [
            &self.ntsc,
            &self.hlg,
            &self.pq,
            &self.wide,
            &self.deep,
            &self.prores,
            &self.lower_third,
            &self.logo,
            &self.sticker,
            &self.badge,
            &self.plate,
            &self.browser,
            &self.screen,
            &self.capture,
            &self.webcam,
            &self.obs,
            &self.equirect,
            &self.archive,
            &self.green,
        ]
        .into_iter()
        .cloned()
        .collect()
    }
}

/// A 2x2x2 `.cube` that warms the picture: the red index runs fastest.
const WARM_CUBE: &str = "TITLE \"kerf coverage warm\"\nLUT_3D_SIZE 2\n0.0 0.0 0.0\n1.0 0.0 0.0\n0.0 1.0 0.0\n1.0 1.0 0.0\n\
                         0.0 0.0 0.9\n1.0 0.0 0.9\n0.0 1.0 0.9\n1.0 1.0 0.9\n";

fn extra() -> &'static Extra {
    static X: OnceLock<Extra> = OnceLock::new();
    X.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("coverage-media");
        std::fs::create_dir_all(&dir).unwrap();
        let lavfi = |pattern: &str, rate: &str, secs: u32| format!("{pattern}:rate={rate}:duration={secs}");
        // Tags are set on the frames: FFmpeg 7 and later take the encoder's from them, and an output option does not win.
        let tagged = |matrix: &str, primaries: &str, trc: &str| {
            format!("setparams=colorspace={matrix}:color_primaries={primaries}:color_trc={trc}:range=tv")
        };
        let x264 = |name: &str, pattern: &str, rate: &str, secs: u32, pix: &str, vf: &str| {
            make(
                &dir,
                name,
                &["-f", "lavfi", "-i", &lavfi(pattern, rate, secs)],
                &[
                    "-vf",
                    vf,
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    "35",
                    "-g",
                    "30",
                    "-pix_fmt",
                    pix,
                ],
                &[],
            )
        };
        let x265 = |name: &str, pattern: &str, rate: &str, secs: u32, vf: &str| {
            make(
                &dir,
                name,
                &["-f", "lavfi", "-i", &lavfi(pattern, rate, secs)],
                &[
                    "-vf",
                    vf,
                    "-c:v",
                    "libx265",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    "35",
                    "-x265-params",
                    "log-level=error",
                    "-pix_fmt",
                    "yuv420p10le",
                    "-tag:v",
                    "hvc1",
                ],
                &[],
            )
        };
        let prores = |name: &str, pattern: &str, rate: &str, secs: u32, profile: &str, pix: &str| {
            make(
                &dir,
                name,
                &["-f", "lavfi", "-i", &lavfi(pattern, rate, secs)],
                &[
                    "-vf",
                    &format!("format={pix}"),
                    "-c:v",
                    "prores_ks",
                    "-profile:v",
                    profile,
                    "-pix_fmt",
                    pix,
                ],
                &[],
            )
        };
        let probe = |p: PathBuf| Project::probe_asset(&p).unwrap_or_else(|e| panic!("probe {}: {e}", p.display()));
        let t709 = tagged("bt709", "bt709", "bt709");
        let t2020 = |trc: &str| format!("format=yuv420p10le,{}", tagged("bt2020nc", "bt2020", trc));
        let mut equirect = probe(x264(
            "equirect.mp4",
            "testsrc2=size=1920x960",
            "30",
            8,
            "yuv420p",
            "format=yuv420p",
        ));
        for s in equirect.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
            s.projection = Some(Projection::Equirect);
        }
        let lut = dir.join("warm.cube");
        if !lut.exists() {
            std::fs::write(&lut, WARM_CUBE).unwrap();
        }
        Extra {
            ntsc: probe(x264("ntsc.mp4", "testsrc2=size=1280x720", "30000/1001", 8, "yuv420p", &t709)),
            hlg: probe(x265("hlg.mp4", "testsrc2=size=1920x1080", "30", 6, &t2020("arib-std-b67"))),
            pq: probe(x265("pq.mp4", "testsrc2=size=1280x720", "24", 5, &t2020("smpte2084"))),
            wide: probe(x264(
                "wide.mp4",
                "testsrc2=size=1280x720",
                "30",
                6,
                "yuv420p",
                &tagged("bt2020nc", "bt2020", "bt709"),
            )),
            deep: probe(x264(
                "deep.mp4",
                "testsrc2=size=1920x1080",
                "30",
                6,
                "yuv420p10le",
                &format!("format=yuv420p10le,{t709}"),
            )),
            prores: probe(prores("prores.mov", "testsrc2=size=1920x1080", "25", 5, "3", "yuv422p10le")),
            lower_third: probe(prores(
                "lower-third.mov",
                "testsrc2=size=1280x720",
                "30",
                4,
                "4",
                "yuva444p10le",
            )),
            logo: probe(make(
                &dir,
                "logo.png",
                &["-f", "lavfi", "-i", "testsrc2=size=512x512:rate=1:duration=1"],
                &["-frames:v", "1"],
                &["-vf", "format=rgba"],
            )),
            sticker: probe(make(
                &dir,
                "sticker.gif",
                &["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=10:duration=3"],
                &[],
                &[],
            )),
            badge: probe(make(
                &dir,
                "badge.png",
                &["-f", "lavfi", "-i", "testsrc2=size=480x480:rate=1:duration=1"],
                &["-frames:v", "1"],
                &["-vf", "split[a][b];[a]palettegen=max_colors=64[p];[b][p]paletteuse"],
            )),
            plate: probe(make(
                &dir,
                "plate.exr",
                &["-f", "lavfi", "-i", "testsrc2=size=640x360:rate=1:duration=1"],
                &["-frames:v", "1", "-c:v", "exr"],
                &["-vf", "format=gbrpf32le"],
            )),
            browser: probe(make(
                &dir,
                "browser.webm",
                &["-f", "lavfi", "-i", &lavfi("testsrc2=size=1920x1080", "30", 6)],
                &[
                    "-vf",
                    "format=yuv420p,setparams=range=pc",
                    "-c:v",
                    "libvpx-vp9",
                    "-deadline",
                    "realtime",
                    "-cpu-used",
                    "8",
                    "-crf",
                    "40",
                    "-b:v",
                    "0",
                    "-pix_fmt",
                    "yuv420p",
                ],
                &[],
            )),
            screen: probe(x264(
                "screen.mp4",
                "testsrc2=size=1920x1080",
                "30",
                8,
                "yuv444p",
                "format=yuv444p",
            )),
            capture: probe(make(
                &dir,
                "capture.mp4",
                &["-f", "lavfi", "-i", &lavfi("testsrc2=size=1280x720", "30", 8)],
                &[
                    "-c:v",
                    "libx264rgb",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    "35",
                    "-g",
                    "30",
                    "-pix_fmt",
                    "rgb24",
                ],
                &[],
            )),
            webcam: probe(make(
                &dir,
                "webcam.mov",
                &["-f", "lavfi", "-i", &lavfi("testsrc2=size=1280x720", "30", 8)],
                &["-c:v", "mjpeg", "-q:v", "5", "-pix_fmt", "yuvj422p"],
                &[],
            )),
            obs: probe(make(
                &dir,
                "obs.mp4",
                &["-f", "lavfi", "-i", &lavfi("testsrc2=size=1920x1080", "30", 8)],
                &[
                    "-vf",
                    "scale=out_range=pc,format=yuvj420p",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    "35",
                    "-g",
                    "30",
                    "-color_range",
                    "pc",
                ],
                &[],
            )),
            equirect,
            archive: probe(x264(
                "archive.mp4",
                "testsrc2=size=1280x720",
                "30",
                6,
                "yuv420p",
                "setparams=colorspace=smpte240m:range=tv",
            )),
            green: probe(make(
                &dir,
                "green.mp4",
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    &lavfi("color=c=0x00b140:size=1920x1080", "30", 8),
                    "-f",
                    "lavfi",
                    "-i",
                    &lavfi("testsrc2=size=720x900", "30", 8),
                ],
                &[
                    "-filter_complex",
                    "[0:v][1:v]overlay=600:90,format=yuv420p",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    "35",
                    "-g",
                    "30",
                    "-pix_fmt",
                    "yuv420p",
                ],
                &[],
            )),
            lut,
        }
    })
}

/// An archetype: its name, the cut and the export options it is planned with (what the project's delivery does not state).
type Archetype = (&'static str, Timeline, ExportOptions);

/// `clip` after `edit` has changed it: a clip built in one expression.
fn with(mut clip: Clip, edit: impl FnOnce(&mut Clip)) -> Clip {
    edit(&mut clip);
    clip
}

/// `clips` one after another from the first one's start: a ramp, a speed or a reverse changes a clip's length, so the starts are
/// laid out from [`Clip::duration`].
fn lay(mut clips: Vec<Clip>) -> Vec<Clip> {
    let mut at = clips.first().map_or(0.0, |c| c.timeline_start);
    for c in &mut clips {
        c.timeline_start = at;
        at += c.duration();
    }
    clips
}

/// The keys `(clip-local seconds, value)` of one property.
fn channel(prop: Property, keys: &[(f64, f64)]) -> PropertyTrack {
    PropertyTrack {
        prop,
        keys: keys.iter().map(|&(time, value)| PropertyKey::new(time, value)).collect(),
    }
}

fn delivery(width: u32, height: u32, fit: Fit, fps: f64) -> Delivery {
    Delivery::new(width, height, fit).with_fps(Some(fps)).expect("a rate")
}

fn music_bed(m: &Media, secs: f64) -> Track {
    Track {
        clips: vec![Clip::new(m.music.id, 0.0, secs, 0.0)],
        ..Track::new(StreamKind::Audio, "A1")
    }
}

fn transition(kind: TransitionKind, secs: f64) -> Option<Transition> {
    Some(Transition { kind, duration: secs })
}

fn mask(shape: MaskShape, x: f64, y: f64, width: f64, height: f64, feather: f64) -> Mask {
    Mask {
        shape,
        x,
        y,
        width,
        height,
        feather,
        inverted: false,
    }
}

/// A motion preset (an entrance, an exit, an emphasis) written into the clip's keys as the Inspector and the MCP tool do.
fn motion(c: &mut Clip, preset: MotionPreset) {
    c.apply_motion(preset, MotionMode::Add, None, None, None)
        .expect("a preset the clip has room for");
}

/// A family installed here that has no bold face, so that a bold title in it is `drawtext` thickened with a border; `None` on a
/// machine whose every font has one (then no archetype holds such a title).
fn regular_only_family() -> Option<String> {
    kerf_core::list_system_fonts()
        .into_iter()
        .find(|family| kerf_core::fonts::resolve_font_file(family, true).is_some_and(|(_, bold)| !bold))
}

/// Everything T9 and on: the features the editor offers and the compositor still refuses, in the cuts people make with them.
fn extended(m: &Media, x: &Extra) -> Vec<Archetype> {
    let mut out = vec![
        blends(m),
        action_cam(m, x),
        explainer(m),
        privacy(m),
        green_screen(m, x),
        social_looks(m, x),
        wedding(m, x),
        hdr_travel(m, x),
        branded_overlays(m, x),
        styled_text(m),
        svg_titles(m),
        behaviours(m, x),
        sphere_and_punch_ins(m, x),
        screen_tutorial(m, x),
    ];
    out.extend(delivery_variants(m));
    out.push(archive_mix(m, x));
    out
}

/// T9: a music video on one picture: light leaks, film grain, a shadow and beat flashes laid over it in every blend mode.
fn blends(m: &Media) -> Archetype {
    let v1 = lay((0..8u32)
        .map(|i| {
            with(clip(&m.base, f64::from(i % 5) * 2.0, 4.0, 0.0), |c| {
                c.fade_in = if i == 0 { 0.5 } else { 0.0 };
                c.fade_out = if i == 7 { 1.0 } else { 0.0 };
            })
        })
        .collect());
    let over = |a: &Asset, src_in: f64, start: f64, dur: f64, mode: BlendMode, opacity: f64| {
        with(clip(a, src_in, dur, start), |c| {
            c.blend_mode = mode;
            c.transform.opacity = opacity;
        })
    };
    let leaks = vec![
        with(over(&m.side, 0.0, 4.0, 10.0, BlendMode::Screen, 0.6), |c| {
            c.fade_in = 1.0;
            c.fade_out = 1.0;
        }),
        over(&m.side, 12.0, 18.0, 8.0, BlendMode::HardLight, 0.8),
    ];
    let grain = vec![
        over(&m.cams[1], 0.0, 0.0, 16.0, BlendMode::Overlay, 0.35),
        over(&m.cams[1], 2.0, 16.0, 16.0, BlendMode::Overlay, 0.35),
    ];
    let shadow = vec![over(&m.cams[0], 0.0, 12.0, 14.0, BlendMode::Multiply, 1.0)];
    let flashes = vec![
        over(&m.cams[0], 5.0, 9.5, 0.5, BlendMode::Add, 1.0),
        over(&m.cams[2], 5.0, 17.5, 0.5, BlendMode::Difference, 1.0),
        over(&m.cams[0], 9.0, 25.5, 0.5, BlendMode::Exclusion, 0.8),
        over(&m.cams[1], 9.0, 28.0, 0.5, BlendMode::Darken, 1.0),
        over(&m.cams[2], 9.0, 30.0, 0.5, BlendMode::Lighten, 1.0),
    ];
    (
        "T9  music video 16:9, leaks / grain / flashes in 10 blend modes",
        timeline(
            vec![
                track(v1, "V1"),
                track(leaks, "V2"),
                track(grain, "V3"),
                track(shadow, "V4"),
                track(flashes, "V5"),
            ],
            vec![],
            delivery(1920, 1080, Fit::Cover, 30.0),
        ),
        ExportOptions::default(),
    )
}

/// T10: an action-cam edit: stabilized shots, speed ramps into slow motion and out, a time-lapse, a reverse, a flash through
/// white, a push and a handheld shake on an impact.
fn action_cam(m: &Media, x: &Extra) -> Archetype {
    let ramp = |keys: &[(f64, f64)]| keys.iter().map(|&(t, s)| SpeedKey::new(t, s)).collect::<Vec<_>>();
    let steady = |strength: f64, zoom: Option<f64>| Some(Stabilize { strength, zoom });
    let clips = lay(vec![
        with(clip(&m.cams[0], 2.0, 5.0, 0.0), |c| {
            c.stabilize = steady(0.5, None);
            c.fade_in = 0.4;
        }),
        with(clip(&x.ntsc, 0.0, 6.0, 0.0), |c| {
            c.stabilize = steady(0.5, None);
            c.speed_ramp = ramp(&[(0.0, 1.0), (1.5, 1.0), (2.5, 0.25), (4.0, 0.25), (5.0, 1.0)]);
            c.transition_in = crossfade(0.5);
        }),
        with(clip(&m.cams[1], 0.0, 12.0, 0.0), |c| c.speed = 4.0),
        with(clip(&m.cams[0], 8.0, 6.0, 0.0), |c| {
            c.stabilize = steady(0.8, Some(8.0));
            c.speed_ramp = ramp(&[(0.0, 1.0), (1.0, 3.0), (2.0, 3.0), (3.0, 1.0)]);
            c.transition_in = transition(TransitionKind::DipToWhite, 0.5);
        }),
        with(clip(&x.ntsc, 0.0, 3.0, 0.0), |c| {
            c.speed = -1.0;
            c.transition_in = transition(TransitionKind::PushLeft, 0.4);
        }),
        with(clip(&m.cams[2], 4.0, 5.0, 0.0), |c| {
            c.behaviors = vec![Behavior::Shake {
                amount: 0.01,
                speed: 6.0,
                seed: 7,
                phase: 0.0,
            }];
            c.fade_out = 1.0;
        }),
    ]);
    let secs = clips.last().map_or(0.0, Clip::timeline_end);
    (
        "T10 action cam 16:9, stabilized, speed ramps, time-lapse, flash",
        timeline(
            vec![track(clips, "V1"), music_bed(m, secs)],
            vec![],
            delivery(1920, 1080, Fit::Cover, 30.0),
        ),
        ExportOptions::default(),
    )
}

/// T11: an explainer whose shots are animated by keys: a zoom into a detail (the animated crop), a spotlight that travels (a keyed
/// mask), a colour reveal (keyed colour), a spin-in (a motion preset) and a push-in, with a callout that pops in over them.
fn explainer(m: &Media) -> Archetype {
    let v1 = lay(vec![
        with(clip(&m.base, 0.0, 8.0, 0.0), |c| {
            c.channels = vec![
                channel(Property::CropZoom, &[(0.0, 1.0), (6.0, 1.8)]),
                channel(Property::CropX, &[(0.0, 0.0), (6.0, 0.6)]),
                channel(Property::CropY, &[(0.0, 0.0), (6.0, -0.4)]),
            ];
        }),
        with(clip(&m.cams[0], 2.0, 6.0, 0.0), |c| {
            c.mask = Some(mask(MaskShape::Ellipse, 0.3, 0.5, 0.35, 0.5, 0.25));
            c.channels = vec![channel(Property::MaskX, &[(0.0, 0.3), (5.0, 0.7)])];
            c.fade_in = 0.4;
        }),
        with(clip(&m.side, 0.0, 6.0, 0.0), |c| {
            c.channels = vec![
                channel(Property::Saturation, &[(0.0, 0.0), (3.0, 1.0)]),
                channel(Property::Brightness, &[(0.0, -0.3), (3.0, 0.0)]),
            ];
        }),
        with(clip(&m.cams[1], 0.0, 6.0, 0.0), |c| motion(c, MotionPreset::SpinIn)),
        with(clip(&m.base, 10.0, 8.0, 0.0), |c| {
            c.keyframes = vec![key(0.0, 1.0, 0.0, 0.0), key(8.0, 1.25, -0.05, 0.0)];
        }),
    ]);
    let callout = with(clip(&m.uhd, 0.0, 10.0, 6.0), |c| {
        c.transform = Transform {
            scale: 0.3,
            pos_x: 0.3,
            pos_y: -0.3,
            ..Transform::default()
        };
        c.mask = Some(mask(MaskShape::Rect, 0.5, 0.5, 0.9, 0.9, 0.15));
        motion(c, MotionPreset::PopIn);
    });
    (
        "T11 explainer 16:9, keyed crop / mask / colour, spin-in, push-in",
        timeline(
            vec![track(v1, "V1"), track(vec![callout], "V2")],
            vec![lower_third("Step by step", 3.0, 8.0)],
            delivery(1920, 1080, Fit::Cover, 30.0),
        ),
        ExportOptions::default(),
    )
}

/// T12: an interview with a face blurred (a blurred copy of the shot on the track above, masked to the face), a spotlight (the
/// same shot in grey with an inverted mask), a tilted picture-in-picture in an ellipse and a translucent ghost of the other angle.
fn privacy(m: &Media) -> Archetype {
    let v1 = lay(vec![
        clip(&m.cams[0], 0.0, 14.0, 0.0),
        clip(&m.cams[1], 0.0, 12.0, 0.0),
        clip(&m.cams[0], 3.0, 14.0, 0.0),
    ]);
    let v2 = vec![
        with(clip(&m.cams[0], 0.0, 12.0, 1.0), |c| {
            c.effects = vec![VideoEffect::Blur { sigma: 14.0 }];
            c.mask = Some(mask(MaskShape::Ellipse, 0.5, 0.35, 0.2, 0.32, 0.35));
        }),
        with(clip(&m.cams[1], 0.0, 10.0, 15.0), |c| {
            c.effects = vec![VideoEffect::Grayscale];
            c.mask = Some(Mask {
                inverted: true,
                ..mask(MaskShape::Ellipse, 0.5, 0.5, 0.5, 0.7, 0.3)
            });
        }),
        with(clip(&m.cams[2], 0.0, 10.0, 26.0), |c| {
            c.transform = Transform {
                scale: 0.35,
                pos_x: 0.28,
                pos_y: -0.25,
                rotation: -4.0,
                ..Transform::default()
            };
            c.mask = Some(mask(MaskShape::Ellipse, 0.5, 0.5, 0.9, 0.9, 0.1));
        }),
    ];
    let v3 = vec![with(clip(&m.side, 0.0, 12.0, 4.0), |c| {
        c.transform = Transform {
            scale: 0.3,
            pos_x: -0.3,
            pos_y: -0.3,
            opacity: 0.85,
            ..Transform::default()
        };
        c.mask = Some(mask(MaskShape::Rect, 0.5, 0.5, 0.9, 0.9, 0.1));
    })];
    (
        "T12 interview 16:9, face blur, spotlight, tilted PiP, masked ghost",
        timeline(
            vec![track(v1, "V1"), track(v2, "V2"), track(v3, "V3")],
            captions(1.0, 38.0),
            delivery(1920, 1080, Fit::Cover, 30.0),
        ),
        ExportOptions::default(),
    )
}

/// T13: a presenter on a green screen over a backdrop: keyed plain, then with the edge softened, a vignette, a grade, a sharpen, a
/// round mask, a little transparency and a tilt.
fn green_screen(m: &Media, x: &Extra) -> Archetype {
    let backdrop = lay((0..4u32).map(|i| clip(&m.side, f64::from(i) * 6.0, 8.0, 0.0)).collect());
    let key = |more: Vec<VideoEffect>| {
        let mut effects = vec![VideoEffect::ChromaKey {
            color: "0x00b140".into(),
            similarity: 0.25,
            blend: 0.1,
        }];
        effects.extend(more);
        effects
    };
    let presenter = |src_in: f64, edit: &dyn Fn(&mut Clip)| {
        with(clip(&x.green, src_in, 4.0, 0.0), |c| {
            c.transform.scale = 0.9;
            edit(c);
        })
    };
    let fg = lay(vec![
        presenter(0.0, &|c| c.effects = key(vec![])),
        presenter(1.0, &|c| c.effects = key(vec![VideoEffect::Blur { sigma: 1.5 }])),
        presenter(2.0, &|c| c.effects = key(vec![VideoEffect::Vignette])),
        presenter(3.0, &|c| {
            c.effects = key(vec![]);
            c.color = Color {
                contrast: 1.15,
                saturation: 1.2,
                ..Color::default()
            };
        }),
        presenter(0.0, &|c| c.effects = key(vec![VideoEffect::Sharpen { amount: 0.8 }])),
        presenter(1.0, &|c| {
            c.effects = key(vec![]);
            c.mask = Some(mask(MaskShape::Ellipse, 0.5, 0.5, 0.7, 0.9, 0.2));
        }),
        presenter(2.0, &|c| {
            c.effects = key(vec![]);
            c.transform.opacity = 0.9;
        }),
        presenter(3.0, &|c| {
            c.effects = key(vec![]);
            c.transform.rotation = -3.0;
        }),
    ]);
    (
        "T13 green screen 16:9, key + blur / vignette / grade / mask / tilt",
        timeline(
            vec![track(backdrop, "V1"), track(fg, "V2")],
            vec![lower_third("Dr. Ada Lovelace", 2.0, 8.0)],
            delivery(1920, 1080, Fit::Contain, 30.0),
        ),
        ExportOptions::default(),
    )
}

/// T14: a vertical social edit of landscape shots kept whole (`Contain`, bars above and below): a vignette, a blur, a sharpen, a
/// grey flashback, an inverted glitch frame, and a 4K drone shot and a ProRes one given the same looks.
fn social_looks(m: &Media, x: &Extra) -> Archetype {
    let looks =
        |a: &Asset, src_in: f64, dur: f64, effects: Vec<VideoEffect>| with(clip(a, src_in, dur, 0.0), |c| c.effects = effects);
    let clips = lay(vec![
        looks(&m.cams[0], 0.0, 4.0, vec![VideoEffect::Vignette]),
        looks(&m.cams[2], 0.0, 3.0, vec![VideoEffect::Blur { sigma: 3.0 }]),
        looks(&m.cams[0], 6.0, 4.0, vec![VideoEffect::Sharpen { amount: 1.2 }]),
        looks(&m.base, 0.0, 4.0, vec![VideoEffect::Grayscale, VideoEffect::Vignette]),
        looks(&m.cams[1], 0.0, 0.4, vec![VideoEffect::Invert]),
        with(looks(&m.uhd, 0.0, 5.0, vec![VideoEffect::Sharpen { amount: 2.0 }]), |c| {
            c.color = Color {
                brightness: -0.1,
                contrast: 2.4,
                saturation: 2.2,
                gamma: 0.6,
                ..Color::default()
            }
        }),
        looks(&x.prores, 0.0, 4.0, vec![VideoEffect::Vignette]),
        looks(
            &x.deep,
            0.0,
            4.0,
            vec![VideoEffect::Sharpen { amount: 0.4 }, VideoEffect::Blur { sigma: 2.0 }],
        ),
        looks(&m.cams[2], 6.0, 3.0, vec![VideoEffect::Blur { sigma: 20.0 }]),
    ]);
    (
        "T14 vertical social edit of 16:9 shots (Contain), looks and effects",
        timeline(
            vec![track(clips, "V1")],
            captions(0.5, 30.0),
            delivery(1080, 1920, Fit::Contain, 30.0),
        ),
        ExportOptions::default(),
    )
}

/// T15: a wedding highlight from every camera in the room: ProRes 4:2:2, 10-bit H.264, photographs, a phone held upright and a 4K
/// drone, warmed, curved and looked-up, joined by dips to white, dissolves and a slide, with a punch-in on the phone shot.
fn wedding(m: &Media, x: &Extra) -> Archetype {
    let warm = Color {
        contrast: 1.1,
        saturation: 1.1,
        temperature: 0.15,
        ..Color::default()
    };
    let curve = Curves {
        master: [(0.0, 0.0), (0.4, 0.1), (0.6, 0.9), (1.0, 1.0)]
            .map(|(x, y)| CurvePoint { x, y })
            .to_vec(),
        ..Curves::default()
    };
    let lut = ClipLut {
        path: x.lut.display().to_string(),
        intensity: 0.8,
    };
    let dip = transition(TransitionKind::DipToWhite, 0.8);
    let clips = lay(vec![
        with(clip(&x.prores, 0.0, 4.5, 0.0), |c| {
            c.color = warm;
            c.fade_in = 1.0;
        }),
        with(clip(&x.deep, 0.0, 5.0, 0.0), |c| {
            c.grade.lut = Some(lut.clone());
            c.transition_in = dip;
        }),
        with(clip(&m.photos[0], 0.0, 4.0, 0.0), |c| {
            c.color = warm;
            c.transition_in = crossfade(0.8);
        }),
        with(clip(&m.photos[1], 0.0, 4.0, 0.0), |c| {
            c.grade.curves = Some(curve.clone());
            c.transition_in = dip;
        }),
        with(clip(&m.phone709, 0.0, 5.0, 0.0), |c| {
            c.transform.scale = 1.3;
            c.color = warm;
            c.transition_in = crossfade(0.8);
        }),
        with(clip(&m.uhd, 0.0, 5.0, 0.0), |c| {
            c.color = Color {
                contrast: 1.8,
                saturation: 1.5,
                gamma: 0.7,
                ..Color::default()
            };
            c.transition_in = transition(TransitionKind::SlideLeft, 0.6);
        }),
        with(clip(&x.prores, 0.0, 4.5, 0.0), |c| {
            c.grade.curves = Some(curve.clone());
            c.transition_in = dip;
        }),
        with(clip(&x.deep, 1.0, 4.0, 0.0), |c| {
            c.color = warm;
            c.fade_out = 1.5;
            c.transition_in = crossfade(0.8);
        }),
    ]);
    let secs = clips.last().map_or(0.0, Clip::timeline_end);
    (
        "T15 wedding highlight 16:9, 5 camera formats, warm grade, dips to white",
        timeline(
            vec![track(clips, "V1"), music_bed(m, secs)],
            vec![],
            delivery(1920, 1080, Fit::Contain, 25.0),
        ),
        ExportOptions::default(),
    )
}

/// T16: a travel vlog from an iPhone's HLG HDR, a PQ clip, a drone's BT.2020, a mirrorless camera's 10-bit and a BT.709 phone clip,
/// dissolved into each other, the 10-bit shots graded.
fn hdr_travel(m: &Media, x: &Extra) -> Archetype {
    let clips = lay(vec![
        with(clip(&x.hlg, 0.0, 5.5, 0.0), |c| c.fade_in = 0.5),
        with(clip(&m.phone709, 0.0, 5.0, 0.0), |c| c.transition_in = crossfade(0.6)),
        with(clip(&x.pq, 0.0, 4.5, 0.0), |c| c.transition_in = crossfade(0.6)),
        with(clip(&x.wide, 0.0, 5.5, 0.0), |c| c.transition_in = crossfade(0.6)),
        with(clip(&x.deep, 0.0, 5.5, 0.0), |c| {
            c.color = Color {
                contrast: 1.1,
                saturation: 1.15,
                ..Color::default()
            };
            c.transition_in = crossfade(0.6);
        }),
        with(clip(&x.hlg, 0.0, 5.5, 0.0), |c| c.transition_in = crossfade(0.6)),
        with(clip(&x.ntsc, 0.0, 6.0, 0.0), |c| {
            c.transition_in = crossfade(0.6);
            c.fade_out = 1.0;
        }),
    ]);
    let secs = clips.last().map_or(0.0, Clip::timeline_end);
    (
        "T16 travel vlog 16:9, HLG / PQ / BT.2020 / 10-bit / BT.709, dissolves",
        timeline(
            vec![track(clips, "V1"), music_bed(m, secs)],
            vec![TextOverlay::new("Lofoten, 68.3 N", 1.0, 4.0)],
            delivery(1920, 1080, Fit::Contain, 30.0),
        ),
        ExportOptions::default(),
    )
}

/// T17: a branded cut: ProRes 4444 lower thirds with alpha, a PNG logo in a corner for the whole cut, a GIF sticker, an
/// indexed-colour badge and a PNG that fades in over the footage.
fn branded_overlays(m: &Media, x: &Extra) -> Archetype {
    let v1 = lay((0..7u32).map(|i| clip(&m.base, f64::from(i % 4) * 3.0, 5.0, 0.0)).collect());
    let v2: Vec<Clip> = (0..3u32)
        .map(|i| clip(&x.lower_third, 0.0, 3.8, 3.0 + f64::from(i) * 11.0))
        .collect();
    let logo = with(Clip::new(x.logo.id, 0.0, 35.0, 0.0), |c| {
        c.transform = Transform {
            scale: 0.12,
            pos_x: 0.42,
            pos_y: -0.4,
            opacity: 0.8,
            ..Transform::default()
        };
    });
    let stickers: Vec<Clip> = (0..2u32)
        .map(|i| {
            with(clip(&x.sticker, 0.0, 2.8, 8.0 + f64::from(i) * 14.0), |c| {
                c.transform = Transform {
                    scale: 0.2,
                    pos_x: -0.35,
                    pos_y: 0.25,
                    rotation: 12.0,
                    ..Transform::default()
                };
            })
        })
        .collect();
    let end_card = with(Clip::new(x.logo.id, 0.0, 4.0, 31.0), |c| {
        c.transform.scale = 0.5;
        c.fade_in = 1.0;
    });
    let render = with(Clip::new(x.plate.id, 0.0, 6.0, 22.0), |c| {
        c.transform = Transform {
            scale: 0.4,
            pos_x: -0.25,
            pos_y: 0.2,
            ..Transform::default()
        };
        c.fade_in = 0.5;
    });
    let badge = with(Clip::new(x.badge.id, 0.0, 10.0, 14.0), |c| {
        c.transform = Transform {
            scale: 0.18,
            pos_x: 0.38,
            pos_y: 0.3,
            ..Transform::default()
        };
    });
    (
        "T17 branded cut 16:9, ProRes 4444 lower thirds, PNG logo, GIF sticker",
        timeline(
            vec![
                track(v1, "V1"),
                track(v2, "V2"),
                track(vec![logo], "V3"),
                track(stickers, "V4"),
                track(vec![badge, end_card], "V5"),
                track(vec![render], "V6"),
            ],
            vec![],
            delivery(1920, 1080, Fit::Cover, 30.0),
        ),
        ExportOptions::default(),
    )
}

/// T18: captions as the styled formats make them: bold in a typeface with no bold face (a border thickens it) and an animated title
/// in a family that is not installed (the shipped font is drawn in its place).
fn styled_text(m: &Media) -> Archetype {
    let clips = lay((0..6u32).map(|i| clip(&m.base, f64::from(i) * 3.0, 6.0, 0.0)).collect());
    let family = regular_only_family();
    let mut overlays: Vec<TextOverlay> = captions(1.0, 34.0)
        .into_iter()
        .map(|mut o| {
            o.bold = true;
            o.font = family.clone();
            o.size = 0.07;
            o
        })
        .collect();
    let mut title = TextOverlay::new("Summer sale", 2.0, 6.0);
    title.font = Some("Comic Neue".into());
    title.bold = true;
    title.size = 0.12;
    title.pos_y = 0.4;
    title.keyframes = [(0.0, 0.45, 0.0), (0.5, 0.4, 1.0)]
        .map(|(time, pos_y, opacity)| kerf_core::TextKeyframe {
            time,
            pos_x: 0.5,
            pos_y,
            opacity,
        })
        .to_vec();
    overlays.push(title);
    (
        "T18 styled captions 9:16, bold without a bold face, missing font",
        timeline(vec![track(clips, "V1")], overlays, delivery(1080, 1920, Fit::Cover, 30.0)),
        ExportOptions::default(),
    )
}

/// T19: an explainer with animated titles: the four SVG presets floating over the footage, a title clip on a track of its own and
/// one made from a motion template, with a channel handle and small print too small for a mask to stand for.
fn svg_titles(m: &Media) -> Archetype {
    let frame = (1920, 1080);
    let preset = |kind: SvgPresetKind, text: &str, subtitle: Option<&str>, start: f64, end: f64| {
        let params = SvgPresetParams {
            subtitle: subtitle.map(str::to_string),
            ..SvgPresetParams::new(text)
        };
        SvgOverlay {
            id: uuid::Uuid::new_v4(),
            name: String::new(),
            svg: render_preset(kind, &params, end - start, frame),
            start,
            end,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            opacity: 1.0,
            offset: 0.0,
            preset: Some(SvgPreset { kind, params }),
            template: None,
        }
    };
    let clips = lay((0..7u32).map(|i| clip(&m.base, f64::from(i % 5) * 2.0, 6.0, 0.0)).collect());
    let intro_params = SvgPresetParams::new("How it works");
    let intro = Clip::title(
        TitleClip {
            name: "Intro".into(),
            svg: render_preset(SvgPresetKind::FadeUp, &intro_params, 3.0, frame),
            template: None,
        },
        0.0,
        3.0,
    );
    let templated = Clip::title(
        TitleClip {
            name: "Lower third".into(),
            svg: render_preset(SvgPresetKind::LowerThird, &SvgPresetParams::new("Chapter 2"), 4.0, frame),
            template: Some(kerf_core::TemplateInstance {
                template_id: "lower-third-bar".into(),
                name: "Lower third".into(),
                controls: Vec::new(),
                values: Default::default(),
            }),
        },
        36.0,
        4.0,
    );
    // A channel handle in the corner and the small print: under 12 px at the 540 px a preview plans at.
    let mut handle = TextOverlay::new("@kerf.studio", 0.0, 10.0);
    handle.pos_x = 0.9;
    handle.pos_y = 0.95;
    handle.size = 0.03;
    handle.color = "white@0.7".into();
    let mut small_print = TextOverlay::new("*Results vary. Terms apply.", 26.0, 34.0);
    small_print.pos_y = 0.92;
    small_print.size = 0.025;
    let mut timeline = timeline(
        vec![track(clips, "V1"), track(vec![intro, templated], "V2")],
        vec![handle, small_print],
        delivery(1920, 1080, Fit::Cover, 30.0),
    );
    timeline.svg_overlays = vec![
        preset(SvgPresetKind::FadeUp, "Step one", Some("Import your footage"), 5.0, 9.0),
        preset(SvgPresetKind::LowerThird, "Ada Lovelace", Some("Engineer"), 12.0, 16.0),
        preset(SvgPresetKind::Typewriter, "Cut what does not help", None, 20.0, 25.0),
        preset(SvgPresetKind::WordPop, "Export", None, 30.0, 33.0),
    ];
    (
        "T19 explainer 16:9, SVG titles, title clips, a template, small text",
        timeline,
        ExportOptions::default(),
    )
}

/// T20: a social montage with life added by behaviours: a breathing photograph, a drifting one, a swaying shot, a handheld shake,
/// a sticker that spins and a wiggling logo.
fn behaviours(m: &Media, x: &Extra) -> Archetype {
    let v1 = lay(vec![
        with(Clip::new(m.photos[0].id, 0.0, 4.5, 0.0), |c| {
            c.behaviors = vec![Behavior::Breathe {
                amount: 0.04,
                speed: 0.25,
                phase: 0.0,
            }];
        }),
        with(Clip::new(m.photos[1].id, 0.0, 4.5, 0.0), |c| {
            c.behaviors = vec![Behavior::Drift {
                speed: 0.01,
                angle: 90.0,
                phase: 0.0,
            }];
        }),
        with(clip(&m.base, 0.0, 5.0, 0.0), |c| {
            c.behaviors = vec![Behavior::Sway {
                amount: 1.5,
                speed: 0.3,
                phase: 0.0,
            }];
        }),
        with(Clip::new(m.photos[2].id, 0.0, 4.5, 0.0), |c| {
            c.behaviors = vec![
                Behavior::Breathe {
                    amount: 0.03,
                    speed: 0.2,
                    phase: 0.5,
                },
                Behavior::Drift {
                    speed: 0.008,
                    angle: 0.0,
                    phase: 0.0,
                },
            ];
        }),
        with(clip(&m.cams[2], 0.0, 5.0, 0.0), |c| {
            c.behaviors = vec![Behavior::Shake {
                amount: 0.006,
                speed: 7.0,
                seed: 4,
                phase: 0.0,
            }];
        }),
        clip(&m.cams[0], 0.0, 4.0, 0.0),
    ]);
    let v2 = vec![
        with(clip(&x.sticker, 0.0, 2.8, 3.0), |c| {
            c.transform = Transform {
                scale: 0.25,
                pos_x: 0.3,
                pos_y: -0.25,
                ..Transform::default()
            };
            c.behaviors = vec![Behavior::Spin { speed: 0.4, phase: 0.0 }];
        }),
        with(Clip::new(x.logo.id, 0.0, 6.0, 12.0), |c| {
            c.transform = Transform {
                scale: 0.15,
                pos_x: -0.38,
                pos_y: -0.35,
                ..Transform::default()
            };
            motion(c, MotionPreset::Wiggle);
        }),
        with(clip(&x.sticker, 0.0, 2.8, 20.0), |c| {
            c.transform = Transform {
                scale: 0.25,
                pos_x: -0.3,
                pos_y: 0.2,
                ..Transform::default()
            };
            motion(c, MotionPreset::Bounce);
        }),
    ];
    (
        "T20 social montage 16:9, breathe / drift / sway / shake / spin, presets",
        timeline(
            vec![track(v1, "V1"), track(v2, "V2")],
            vec![],
            delivery(1920, 1080, Fit::Cover, 30.0),
        ),
        ExportOptions::default(),
    )
}

/// T21: a 360 clip reframed with a camera that pans, then punch-ins on ordinary footage: a static zoom and a pan under `Cover`, a
/// zoom out, and moving zooms over a grade, a dip to white and a turn.
fn sphere_and_punch_ins(m: &Media, x: &Extra) -> Archetype {
    let pan = |from: f64, to: f64| Reframe {
        keyframes: vec![
            ReframeKeyframe {
                time: 0.0,
                yaw: from,
                pitch: 0.0,
                roll: 0.0,
                fov: 100.0,
            },
            ReframeKeyframe {
                time: 5.0,
                yaw: to,
                pitch: 5.0,
                roll: 0.0,
                fov: 90.0,
            },
        ],
        ..Reframe::new(Projection::Equirect)
    };
    let push = |c: &mut Clip| c.keyframes = vec![key(0.0, 1.0, 0.0, 0.0), key(4.0, 1.2, 0.0, 0.0)];
    let clips = lay(vec![
        with(clip(&x.equirect, 0.0, 5.0, 0.0), |c| c.reframe = Some(pan(-60.0, 60.0))),
        with(clip(&x.equirect, 2.0, 5.0, 0.0), |c| {
            c.reframe = Some(Reframe::new(Projection::Equirect));
            c.transition_in = crossfade(0.5);
        }),
        with(clip(&m.cams[0], 0.0, 4.0, 0.0), |c| {
            c.transform.scale = 1.3;
            c.transform.pos_x = -0.1;
        }),
        with(clip(&m.cams[1], 0.0, 4.0, 0.0), |c| c.transform.scale = 0.8),
        with(clip(&m.cams[0], 6.0, 4.0, 0.0), |c| {
            push(c);
            c.grade.wheels = Some(Wheels {
                shadows: Wheel {
                    x: 0.1,
                    y: -0.05,
                    lightness: 0.1,
                },
                ..Wheels::default()
            });
        }),
        with(clip(&m.cams[1], 6.0, 4.0, 0.0), |c| {
            push(c);
            c.transition_in = transition(TransitionKind::DipToWhite, 0.6);
        }),
        with(clip(&m.cams[2], 6.0, 4.0, 0.0), |c| {
            push(c);
            c.transform.rotation = 3.0;
        }),
        with(clip(&m.cams[0], 12.0, 4.0, 0.0), |c| {
            push(c);
            c.mask = Some(mask(MaskShape::Rect, 0.5, 0.5, 0.8, 0.8, 0.1));
            c.effects = vec![VideoEffect::Vignette];
        }),
    ]);
    (
        "T21 360 reframe + punch-ins 16:9 (Cover), zoom / pan / zoom out / keyed",
        timeline(vec![track(clips, "V1")], vec![], delivery(1920, 1080, Fit::Cover, 30.0)),
        ExportOptions::default(),
    )
}

/// T22: a screen-recording tutorial: OBS recordings in 4:4:4, an RGB capture, full-range ones (an OBS one and a browser's VP9),
/// crops into regions of the screen, a webcam in a round bubble, a 4K thumbnail in a corner and a graded camera angle over a
/// full-range recording.
fn screen_tutorial(m: &Media, x: &Extra) -> Archetype {
    let crop = |left: f64, right: f64, top: f64, bottom: f64| Transform {
        crop_left: left,
        crop_right: right,
        crop_top: top,
        crop_bottom: bottom,
        ..Transform::default()
    };
    let v1 = lay(vec![
        clip(&x.screen, 0.0, 7.5, 0.0),
        with(clip(&x.screen, 0.0, 7.5, 0.0), |c| c.transform = crop(0.25, 0.25, 0.1, 0.4)),
        with(clip(&x.capture, 0.0, 7.5, 0.0), |c| c.transform = crop(0.2, 0.2, 0.0, 0.3)),
        with(clip(&x.obs, 0.0, 7.5, 0.0), |c| {
            c.color = Color {
                contrast: 1.1,
                ..Color::default()
            }
        }),
        with(clip(&x.browser, 0.0, 5.5, 0.0), |c| {
            c.transition_in = crossfade(0.5);
            c.fade_out = 0.5;
        }),
        with(clip(&x.screen, 0.0, 7.5, 0.0), |c| {
            c.keyframes = vec![key(0.0, 1.0, 0.0, 0.0), key(7.5, 1.4, -0.1, -0.05)];
        }),
    ]);
    let bubble = |start: f64| {
        with(clip(&x.webcam, 0.0, 7.5, start), |c| {
            c.transform = Transform {
                scale: 0.22,
                pos_x: 0.36,
                pos_y: 0.3,
                ..Transform::default()
            };
            c.mask = Some(mask(MaskShape::Ellipse, 0.5, 0.5, 0.56, 1.0, 0.1));
        })
    };
    let v2 = vec![bubble(2.0), bubble(15.0), bubble(26.0)];
    let thumbnail = with(clip(&m.uhd, 0.0, 8.0, 3.0), |c| {
        c.transform = Transform {
            scale: 0.07,
            pos_x: -0.42,
            pos_y: -0.4,
            ..Transform::default()
        };
    });
    // A camera angle in a corner, graded, over the RGB capture and then over the full-range recording.
    let angle = |a: &Asset, start: f64| {
        with(clip(a, 0.0, 7.5, start), |c| {
            c.transform = Transform {
                scale: 0.3,
                pos_x: -0.3,
                pos_y: 0.28,
                ..Transform::default()
            };
            c.color = Color {
                contrast: 1.1,
                saturation: 1.1,
                ..Color::default()
            };
        })
    };
    let angles = vec![angle(&x.ntsc, 15.0), angle(&m.cams[0], 22.5)];
    (
        "T22 screen tutorial 16:9 (Cover), 4:4:4 / RGB / full range, webcam bubble",
        timeline(
            vec![
                track(v1, "V1"),
                track(v2, "V2"),
                track(vec![thumbnail], "V3"),
                track(angles, "V4"),
            ],
            vec![lower_third("Open the settings", 4.0, 9.0), {
                let mut hint = TextOverlay::new("Cmd + K", 12.0, 16.0);
                hint.pos_y = 0.1;
                hint.size = 0.03;
                hint
            }],
            delivery(1920, 1080, Fit::Cover, 30.0),
        ),
        ExportOptions::default(),
    )
}

/// T24: a family documentary from every era: digitised archive footage tagged SMPTE 240M, an untagged clip, a BT.2020 one and a
/// phone's BT.709, some of it translucent over the next, dissolved into each other.
fn archive_mix(m: &Media, x: &Extra) -> Archetype {
    let clips = lay(vec![
        with(clip(&x.archive, 0.0, 5.5, 0.0), |c| {
            c.transform.opacity = 0.9;
            c.fade_in = 0.6;
        }),
        with(clip(&m.base, 0.0, 5.0, 0.0), |c| c.transition_in = crossfade(0.8)),
        with(clip(&x.archive, 0.0, 5.0, 0.0), |c| {
            c.transform.opacity = 0.85;
            c.transition_in = transition(TransitionKind::DipToBlack, 0.8);
        }),
        with(clip(&x.wide, 0.0, 5.0, 0.0), |c| c.transition_in = crossfade(0.8)),
        with(clip(&x.ntsc, 0.0, 5.0, 0.0), |c| c.transition_in = crossfade(0.8)),
        with(clip(&m.phone601, 0.0, 5.0, 0.0), |c| {
            c.transition_in = crossfade(0.8);
            c.fade_out = 1.2;
        }),
    ]);
    (
        "T24 family documentary 16:9, SMPTE 240M / untagged / BT.2020 / BT.709",
        timeline(
            vec![track(clips, "V1")],
            vec![TextOverlay::new("1987 - 2026", 1.0, 5.0)],
            delivery(1920, 1080, Fit::Contain, 30.0),
        ),
        ExportOptions::default(),
    )
}

/// T23: one short teaser delivered five ways, which is what the export dialog's format choices change in a plan: a ProRes file, a
/// GIF, a different scaler, a 10-bit file and a rate whose two parses in the graph are not the same fraction.
fn delivery_variants(m: &Media) -> Vec<Archetype> {
    let teaser = || {
        let clips = lay((0..8u32).map(|i| clip(&m.base, f64::from(i % 6) * 2.0, 2.0, 0.0)).collect());
        let mut clips = clips;
        clips[0].fade_in = 0.4;
        clips[7].fade_out = 0.6;
        timeline(
            vec![track(clips, "V1")],
            vec![TextOverlay::new("Out now", 4.0, 8.0)],
            delivery(1920, 1080, Fit::Cover, 30.0),
        )
    };
    let with_options = |name: &'static str, options: ExportOptions| (name, teaser(), options);
    vec![
        with_options(
            "T23a teaser delivered as ProRes 422 HQ (10-bit 4:2:2)",
            ExportOptions {
                video_codec: Some("prores_ks".into()),
                ..ExportOptions::default()
            },
        ),
        with_options(
            "T23b teaser delivered as a 640 px GIF",
            ExportOptions {
                container: Container::Gif,
                resolution: Some((640, 360)),
                ..ExportOptions::default()
            },
        ),
        with_options(
            "T23c teaser delivered with the lanczos scaler",
            ExportOptions {
                scaler: Some("lanczos".into()),
                ..ExportOptions::default()
            },
        ),
        with_options(
            "T23d teaser delivered as 10-bit 4:2:0",
            ExportOptions {
                pix_fmt: Some("yuv420p10le".into()),
                ..ExportOptions::default()
            },
        ),
        with_options(
            "T23e teaser delivered at 29.970029 fps",
            ExportOptions {
                fps: Some(29.970_029),
                ..ExportOptions::default()
            },
        ),
    ]
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
    caps.placement = false;
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
    // alike), which the placement pass draws: the whole cut, the mask and the blur included.
    for mode in ["still", "motion"] {
        let (was, before, after) = at("T3", mode);
        assert!(before < 20.0, "T3 {mode}: {before}");
        assert!(was < 100.0 && (after - 100.0).abs() < 1e-9, "T3 {mode}: {was} -> {after}");
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
        | Unsupported::BlurFillWith(_, s)
        | Unsupported::MixedMatricesWith(_, s)
        | Unsupported::Matte(_, s)
        | Unsupported::ZoomBehind(_, s)
        | Unsupported::Rotation(_, s)
        | Unsupported::FoldedFit(_, s)
        | Unsupported::FullRangeResized(_, s) => Some(s.to_string()),
        Unsupported::DipToWhite(_, why) => Some(format!("{why:?}").split('(').next().unwrap_or_default().to_string()),
        Unsupported::Fade(_, edge, dissolve) => Some(format!("{edge:?} {}", if *dissolve { "dissolve" } else { "step" })),
        Unsupported::BlendMode(_, _, why) => Some(why.to_string()),
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

/// (g) The photo slideshow (T6) by the set of reasons that refuses a frame: what the frames that are still refused wait for.
fn slideshow(cuts: &[Cut], at_delivery: &[Cut]) -> String {
    let mut out = String::from("\n(g) T6, the photo slideshow: frames by the reasons that refuse them together\n");
    for cut in cuts.iter().filter(|c| c.name.starts_with("T6")).chain(at_delivery) {
        let _ = writeln!(out, "  {} [{}]", cut.name, cut.mode);
        for (why, n) in &cut.frames {
            let causes: Vec<String> = why
                .iter()
                .map(|r| format!("{}/{}", r.variant, column(&r.detail, 60)))
                .collect();
            let _ = writeln!(
                out,
                "    {n:>5}  {}",
                if causes.is_empty() {
                    "drawn".to_string()
                } else {
                    causes.join(" + ")
                }
            );
        }
    }
    out
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
    let extra = extra();
    let mut assets = media.assets();
    assets.extend(extra.assets());
    let policy = composite_color_policy();
    // The caps the app asks with: the grade is drawn only where this FFmpeg's conversions are the model's (one probe).
    let kernels = kerf_gpu::grade::kernels();
    let scaler = kerf_gpu::scaler::kernels();
    let convert = kerf_gpu::convert::kernels();
    // ... and text whose glyph bitmaps share samples (a synthetic-bold border, a font under 12 px) only where this FFmpeg's
    // `drawtext` lays text out in a way the compositor can take apart (one probe).
    let layout = kerf_gpu::text::model();
    // ...and the blend modes where this FFmpeg's blend chain is the model's (a probe; `KERF_LEDGER_BLEND_OFF=1` asks as if
    // it had matched none, which is the ledger of a build that draws no blend: the "before" of a blend change).
    let off = std::env::var_os("KERF_LEDGER_BLEND_OFF").is_some();
    let blend = if off {
        [kerf_core::BlendKinds::NONE; 2]
    } else {
        kerf_gpu::blend::kernels()
    };
    let extras = if off {
        kerf_core::BlendExtras::NONE
    } else {
        kerf_gpu::blend::extras()
    };
    // ... and a moving zoom or position only where this FFmpeg's `pad`, `perspective` and `crop` place a picture as the model does
    // (the placement probe), a 4:4:4 or 4:2:2 picture under it only where its graph converts it as the decode does (the
    // conversion probe).
    let placement = kerf_gpu::placement_probe::placement();
    let conversion = kerf_gpu::conversion::conversion();
    // ... and a moving still only for the chains of the kinds of picture this FFmpeg draws as the model does (the still probe: every chain, as
    // a project holding all of them would have asked).
    let stills = kerf_gpu::still_probe::all();
    // ... and a mask or a keyed opacity only where the edge of this FFmpeg's `geq` read is known (the caps of a host that has no
    // compositor leave them out until it is measured, as the app's `prepare_timeline` does ahead of the render bar).
    let _ = kerf_core::geq_edge();
    // ... and a layer below full opacity only where this FFmpeg takes it through RGB and back as the compositor does (the
    // translucency probe).
    let translucency = kerf_gpu::roundtrip_probe::translucency();
    // ... and a blur only where this FFmpeg's `gblur` is the fused-multiply-add recursion the shader reproduces (the blur probe).
    let blur = kerf_gpu::blur_probe::blur();
    let caps = Compositor::caps_pinned(kernels, scaler, convert, layout, blend, extras);
    let mut cuts = vec![];
    let mut slideshow_at_delivery = vec![];
    // T1 to T8 are planned with the default export options; T9 and on say theirs (a ProRes file, a GIF, a scaler).
    let mut every: Vec<Archetype> = archetypes(media)
        .into_iter()
        .map(|(name, tl)| (name, tl, ExportOptions::default()))
        .collect();
    every.extend(extended(media, extra));
    for (name, tl, options) in every {
        for (mode, request) in [("still", PlanRequest::still(policy)), ("motion", PlanRequest::motion(policy))] {
            let planner = Planner::new(&tl, &assets, &options, request.with_media(&OriginalMedia)).expect("planner");
            let first = planner.at_frame(0).expect("a plan");
            let fps = first.canvas.fps;
            let frames = (tl.duration() * f64::from(fps.num) / f64::from(fps.den)).floor() as u64;
            let size = first.size(540);
            let mut grouped: BTreeMap<Vec<Reason>, u64> = BTreeMap::new();
            for k in 0..frames {
                *grouped.entry(why_not(&planner, k, size, &caps)).or_default() += 1;
            }
            // An export frame is drawn at the delivery's frame whatever the ledger's width: the slideshow's are judged there too.
            if mode == "motion" && name.starts_with("T6") {
                let delivery = (first.canvas.width, first.canvas.height);
                let mut at_delivery: BTreeMap<Vec<Reason>, u64> = BTreeMap::new();
                for k in 0..frames {
                    *at_delivery.entry(why_not(&planner, k, delivery, &caps)).or_default() += 1;
                }
                slideshow_at_delivery.push(Cut {
                    name: name.to_string(),
                    mode: "export frames at the delivery's frame",
                    frames: at_delivery.into_iter().collect(),
                });
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
        format!("grade kernels: {kernels:?}, scaler: {scaler:?}, matrix conversion: {convert:?}; text layout: {layout:?}; placement: {placement:?}; planar conversion: {conversion:?}; moving stills: {stills:?}; translucent round trip: {translucency:?}; gblur: {blur:?} (caps: Compositor::caps_for(grade::kernels(), scaler::kernels(), convert::kernels()) once text::model(), placement_probe::placement() and conversion::conversion() have probed, as the app asks)"),
        format!("blend kernels [BT.601, BT.709]: {blend:?}; extras (BT.2020, RGB pictures): {extras:?}"),
        "plans: every output frame of every archetype over OriginalMedia at 540 px wide (the app's preview plans ProxyMedia at 960); nothing is rendered".to_string(),
        "archetypes: the review's T1 to T3, the real-world T4 to T8 and T9 to T24, which between them use every feature the editor offers that still refuses (tests/coverage.rs)".to_string(),
    ];
    let mut text = render_ledger(&header, &cuts);
    text.push_str(&slideshow(&cuts, &slideshow_at_delivery));
    text.push_str(&unreached(&cuts, policy, kernels, layout));
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

/// One value of every [`Unsupported`] variant: the kinds the ledger can be asked about. [`ordinal`] is an exhaustive `match`, so a
/// variant added to the enum does not compile here until it is given a number, and a test holds the numbers to this list.
fn every_variant() -> Vec<Unsupported> {
    let layer = layer_ref;
    let rate = || Rational::new(30, 1).expect("a rate");
    vec![
        Unsupported::NoPicture(layer()),
        Unsupported::Motion,
        Unsupported::Delivery(String::new()),
        Unsupported::Gif,
        Unsupported::BlurFill,
        Unsupported::PickRate {
            canvas: rate(),
            clips: rate(),
        },
        Unsupported::Hdr(layer()),
        Unsupported::AlphaPicture(layer(), String::new()),
        Unsupported::NotKnownOpaque(layer(), String::new()),
        Unsupported::TranslucentMatrix(layer(), String::new()),
        Unsupported::GradedFullRange(layer(), String::new()),
        Unsupported::GradeSteep(layer(), 3),
        Unsupported::Grade(layer(), ""),
        Unsupported::GradeWith(layer(), ""),
        Unsupported::Effects(layer(), ""),
        Unsupported::Mask(layer()),
        Unsupported::MaskWith(layer(), ""),
        Unsupported::Matte(layer(), ""),
        Unsupported::Reframe(layer()),
        Unsupported::Fade(layer(), FadeEdge::In, false),
        Unsupported::FadeOnTranslucent(layer()),
        Unsupported::Travel(layer()),
        Unsupported::DipToWhite(layer(), DipWhy::NoRoundTrip),
        Unsupported::FullRangeNegotiated(layer()),
        Unsupported::FullRangeCut,
        Unsupported::FullRangeResized(layer(), ""),
        Unsupported::SpeedRamp(layer()),
        Unsupported::Stabilized(layer()),
        Unsupported::BlendMode(layer(), BlendMode::Screen, ""),
        Unsupported::Tail(layer()),
        Unsupported::KeyedOpacity(layer()),
        Unsupported::KeyedColor(layer()),
        Unsupported::KeyedMask(layer()),
        Unsupported::KeyedCrop(layer()),
        Unsupported::StillMotion(layer(), String::new()),
        Unsupported::KeyedZoom(layer()),
        Unsupported::KeyedPlacement(layer()),
        Unsupported::Rotation(layer(), ""),
        Unsupported::PlacedFormat(layer(), String::new()),
        Unsupported::Behavior(layer()),
        Unsupported::ZoomBehind(layer(), ""),
        Unsupported::FoldedFit(layer(), ""),
        Unsupported::Text,
        Unsupported::TextWithoutFont,
        Unsupported::TextSequence(String::new()),
        Unsupported::Svg,
        Unsupported::Scaler(String::new()),
        Unsupported::MatrixUnknown(0),
        Unsupported::MixedMatrices,
        Unsupported::UnmeasuredPolicy(YuvMatrix::Bt709),
        Unsupported::RgbInStack(YuvMatrix::Bt709),
        Unsupported::GradedInFullRangeStack(0),
        Unsupported::Shrink(0, (4, 4), (1, 1)),
        Unsupported::FormatResize(0, (4, 4), (2, 2), String::new()),
        Unsupported::Geometry(0, String::new()),
        Unsupported::BlurFillWith(layer(), ""),
        Unsupported::MixedMatricesWith(layer(), ""),
        Unsupported::ScalerApproximate(layer()),
        Unsupported::TranslucentApproximate(layer()),
        Unsupported::FullRangeStill(layer()),
    ]
}

/// A number for each variant, with no gaps: `match` has no wildcard, so adding a variant breaks this build until it has one.
fn ordinal(u: &Unsupported) -> usize {
    match u {
        Unsupported::NoPicture(_) => 0,
        Unsupported::Motion => 1,
        Unsupported::Delivery(_) => 2,
        Unsupported::Gif => 3,
        Unsupported::BlurFill => 4,
        Unsupported::PickRate { .. } => 5,
        Unsupported::Hdr(_) => 6,
        Unsupported::AlphaPicture(..) => 7,
        Unsupported::NotKnownOpaque(..) => 8,
        Unsupported::TranslucentMatrix(..) => 9,
        Unsupported::GradedFullRange(..) => 10,
        Unsupported::GradeSteep(..) => 11,
        Unsupported::Grade(..) => 12,
        Unsupported::GradeWith(..) => 13,
        Unsupported::Effects(..) => 14,
        Unsupported::Mask(_) => 15,
        Unsupported::MaskWith(..) => 16,
        Unsupported::Matte(..) => 17,
        Unsupported::Reframe(_) => 18,
        Unsupported::Fade(..) => 19,
        Unsupported::FadeOnTranslucent(_) => 20,
        Unsupported::Travel(_) => 21,
        Unsupported::DipToWhite(..) => 22,
        Unsupported::FullRangeNegotiated(_) => 23,
        Unsupported::FullRangeCut => 24,
        Unsupported::FullRangeResized(..) => 25,
        Unsupported::SpeedRamp(_) => 26,
        Unsupported::Stabilized(_) => 27,
        Unsupported::BlendMode(..) => 28,
        Unsupported::Tail(_) => 29,
        Unsupported::KeyedOpacity(_) => 30,
        Unsupported::KeyedColor(_) => 31,
        Unsupported::KeyedMask(_) => 32,
        Unsupported::KeyedCrop(_) => 33,
        Unsupported::StillMotion(..) => 34,
        Unsupported::KeyedZoom(_) => 35,
        Unsupported::KeyedPlacement(_) => 36,
        Unsupported::PlacedFormat(..) => 37,
        Unsupported::Behavior(_) => 38,
        Unsupported::ZoomBehind(..) => 39,
        Unsupported::FoldedFit(..) => 40,
        Unsupported::Text => 41,
        Unsupported::TextWithoutFont => 42,
        Unsupported::TextSequence(_) => 43,
        Unsupported::Svg => 44,
        Unsupported::Scaler(_) => 45,
        Unsupported::MatrixUnknown(_) => 46,
        Unsupported::MixedMatrices => 47,
        Unsupported::UnmeasuredPolicy(_) => 48,
        Unsupported::RgbInStack(_) => 49,
        Unsupported::GradedInFullRangeStack(_) => 50,
        Unsupported::Shrink(..) => 51,
        Unsupported::FormatResize(..) => 52,
        Unsupported::Geometry(..) => 53,
        Unsupported::Rotation(..) => 54,
        Unsupported::BlurFillWith(..) => 55,
        Unsupported::MixedMatricesWith(..) => 56,
        Unsupported::ScalerApproximate(..) => 57,
        Unsupported::TranslucentApproximate(..) => 58,
        Unsupported::FullRangeStill(..) => 59,
    }
}

/// Why no archetype can hit `kind` on this machine, when that is known: a refusal a compositor with `Compositor::CAPS` no longer
/// makes, one nothing a user can build reaches, or one this FFmpeg's probes take out of play.
fn why_unreached(
    kind: &str,
    policy: CompositeColorPolicy,
    kernels: Kernels,
    layout: Option<kerf_gpu::text::Model>,
) -> &'static str {
    let negotiating = policy != CompositeColorPolicy::FixedBt601;
    match kind {
        "motion" | "fade" | "travel" | "tail" | "keyed_opacity" | "keyed_color" | "keyed_zoom" | "mask" | "text" | "blur_fill" => {
            "drawn: Compositor::CAPS has the ability, so a plan never reports it"
        }
        "translucent_approximate" if kerf_gpu::roundtrip_probe::measured() == kerf_gpu::roundtrip_probe::Translucency::Match => {
            "drawn: this FFmpeg takes a translucent layer through RGB as the model does (roundtrip_probe), so a plan never reports it"
        }
        "keyed_placement" if kerf_gpu::placement_probe::measured() == kerf_gpu::placement_probe::Placement::Match => {
            "drawn: this FFmpeg places a picture as the model does (placement_probe), so a plan never reports it"
        }
        "scaler" if kerf_gpu::scaler::measured().named() == kerf_core::ScalerKinds::ALL => {
            "drawn: this FFmpeg's bilinear, lanczos, neighbor and spline tables are the model's (scaler probe), so a name from the engine's list is never refused; no archetype names another"
        }
        "no_picture" => {
            "unreachable: the project keeps an audio-only asset off a video track, and a probe states a video stream's size"
        }
        "text_without_font" => {
            "unreachable: the shipped default font is written to disk (it fails only with no writable cache or temp directory)"
        }
        "unmeasured_policy" => {
            "unreachable here: the composite colour policy was measured (Unknown only when the probe cannot tell)"
        }
        "grade" if kernels == Kernels::Match => "drawn: this FFmpeg's grade conversions are the model's",
        "grade_with" if kernels != Kernels::Match => {
            "not judged: this FFmpeg's grade conversions differ, so every grade is refused as `grade` first"
        }
        "mixed_matrices"
        | "mixed_matrices_with"
        | "matrix_unknown"
        | "rgb_in_stack"
        | "full_range_cut"
        | "graded_in_full_range_stack"
            if !negotiating =>
        {
            "FixedBt601: this FFmpeg composites every stack as BT.601, so there is nothing to negotiate"
        }
        "text_sequence" if layout.is_some() => {
            "no archetype builds it (a tab, over 160 characters, a text too wide to measure): the rest of text is drawn once this FFmpeg's `drawtext` is one the compositor takes apart (text::model)"
        }
        _ => "no archetype builds it",
    }
}

/// (f) The `Unsupported` variants no archetype hit, so that what the ledger cannot measure is stated rather than silent.
fn unreached(cuts: &[Cut], policy: CompositeColorPolicy, kernels: Kernels, layout: Option<kerf_gpu::text::Model>) -> String {
    let hit: BTreeSet<&str> = cuts
        .iter()
        .flat_map(|cut| cut.frames.iter().flat_map(|(why, _)| why.iter().map(|r| r.variant)))
        .collect();
    let mut kinds: Vec<&'static str> = every_variant().iter().map(Unsupported::kind).collect();
    kinds.sort_unstable();
    let missing: Vec<&str> = kinds.iter().copied().filter(|k| !hit.contains(k)).collect();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "\n(f) the Unsupported variants no archetype hits: {} of {}, {} hit",
        missing.len(),
        kinds.len(),
        kinds.len() - missing.len()
    );
    for kind in missing {
        let _ = writeln!(out, "  {kind:<28} {}", why_unreached(kind, policy, kernels, layout));
    }
    out
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

#[test]
fn the_ledger_lists_every_unsupported_variant_once() {
    let all = every_variant();
    let mut numbers: Vec<usize> = all.iter().map(ordinal).collect();
    numbers.sort_unstable();
    assert_eq!(
        numbers,
        (0..numbers.len()).collect::<Vec<_>>(),
        "every_variant() holds one value of each variant, numbered by ordinal()"
    );
    let kinds: BTreeSet<&str> = all.iter().map(Unsupported::kind).collect();
    assert_eq!(kinds.len(), all.len(), "a kind names one variant");
}

#[test]
fn a_variant_no_archetype_hits_is_listed_with_what_keeps_it_out_of_reach() {
    let cuts = [cut("T9  x", "still", &[(&[], 10), (&[("blend_mode", "")], 5)])];
    let text = unreached(&cuts, CompositeColorPolicy::FixedBt601, Kernels::Match, None);
    assert!(text.contains("(f)") && text.contains("1 hit"), "{text}");
    assert!(!text.contains("blend_mode "), "a variant that is hit is not listed:\n{text}");
    let line = |kind: &str| {
        text.lines()
            .find(|l| l.trim_start().starts_with(kind))
            .unwrap_or_else(|| panic!("no {kind} in\n{text}"))
    };
    assert!(line("fade").contains("drawn"), "{text}");
    assert!(line("mixed_matrices").contains("FixedBt601"), "{text}");
    assert!(line("grade ").contains("drawn"), "{text}");
    assert!(line("behavior").contains("no archetype builds it"), "{text}");
    let negotiated = unreached(&cuts, CompositeColorPolicy::BottomLayerTag, Kernels::Differs("a LUT"), None);
    let line = |kind: &str| {
        negotiated
            .lines()
            .find(|l| l.trim_start().starts_with(kind))
            .unwrap_or_else(|| panic!("no {kind} in\n{negotiated}"))
    };
    assert!(line("mixed_matrices").contains("no archetype builds it"), "{negotiated}");
    assert!(line("grade_with").contains("not judged"), "{negotiated}");
}
