//! The golden argv oracle: 4000 seeded timelines whose ffmpeg argv must not move.
//!
//! `build_export_args_phase`, `build_still_args` and `build_preview_args_with` are
//! built for every case and their argv reduced to a digest, checked against three
//! committed files (`golden/{export,still,preview}.txt`, one line per block of
//! [`BLOCK`] cases). A refactor of `video_clip_chain`, `audio_clip_chain`,
//! `transition_fx` or `build_filter_complex` has to leave all three untouched; an
//! intended change to one builder re-blesses only its own file, so the other two
//! keep proving nothing else moved.
//!
//! **Machine-independent by construction.** The builders read the machine in three
//! places, each pinned: the preview's decode acceleration (the
//! `build_preview_args_with` seam), `zscale_available()` (`with_zscale`; a case
//! holding HDR footage is built *both* ways) and `drawtext`'s resolved font path
//! (no overlay names a font, so none is looked up).
//!
//! **Bless** after an intended argv change, from the repo root:
//!
//! ```text
//! KERF_GOLDEN_BLESS=1 cargo test -p kerf-core --no-default-features golden
//! ```
//!
//! then review `git diff` of the three files. To find the case behind a failing
//! block, run the test on the base and on the change with `KERF_GOLDEN_CASES=<file>`
//! (one digest line per case) and diff the files; `KERF_GOLDEN_DUMP=<case>` then
//! prints that case's argv. The files are identical under any `KERF_HWACCEL` (blessed unset, `none`
//! and `auto`): that is the independence proof.
//!
//! **Coverage.** [`FAMILIES`] lists the branches the oracle exists to protect, each
//! as text that must appear in a case's argv; the test fails if any family shows up
//! in fewer than [`MIN_PER_FAMILY`] cases, so it cannot quietly stop covering one.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{TimeZone, Utc};
use uuid::Uuid;

use super::*;
use crate::model::{Keyframe, TextKeyframe, Track, Transition, TransitionKind};

const CASES: usize = 4000;
const BLOCK: usize = 100;
const MIN_PER_FAMILY: usize = 20;
const KINDS: [&str; 3] = ["export", "still", "preview"];
const BLESSED: [&str; 3] = [
    include_str!("golden/export.txt"),
    include_str!("golden/still.txt"),
    include_str!("golden/preview.txt"),
];

/// `family needle`: the family is hit when the needle is in the argv of a case.
const FAMILIES: &str = r"
fade-in fade=t=in:st=
fade-out fade=t=out:st=
dissolve :alpha=1
dip-white :c=white
reverse ,reverse,
speed setpts=(PTS-STARTPTS)/
scale-static scale=iw*
scale-keyed eval=frame
crop crop=w=iw*
rotate-static :fillcolor=none:ow=rotw(
rotate-keyed rotate=a='(
opacity-static colorchannelmixer=aa=
opacity-keyed a='(if(lt((T-
mask-rect max(abs(
mask-ellipse hypot((X-
mask-feather clip((1-
mask-inverted-hard (1-lte(
mask-inverted-feathered (1-clip(
mask+keyed-opacity )*(if(lt((T-
effect-blur gblur=sigma=
effect-sharpen unsharp=
effect-grayscale hue=s=0
effect-invert negate
effect-vignette vignette
chroma-key chromakey=
colour eq=brightness=
colour-temperature :gamma_r=
hdr-zscale zscale=tin=
hdr-colorspace colorspace=all=bt709:iall=bt2020
hdr-pq tin=smpte2084
hdr-hlg tin=arib-std-b67
reframe v360@c
reframe-animated sendcmd=c='
reframe-equirect-out output=e:
reframe-fisheye ih_fov=
still-reframe v360=input=
shared-input [vsp
still-image -loop
overlay drawtext=
overlay-keyed alpha='
overlay-percent \\%
overlay-quote '\''
overlay-bold borderw=2
overlay-box box=1
cover-fit force_original_aspect_ratio=increase
scaler-flag :flags=lanczos
pix-fmt format=yuv422p10le
gif paletteuse=dither=
hwaccel -hwaccel
still-region crop=2*trunc(
ducking sidechaincompress
audio-pan pan=stereo|c0=
audio-compressor acompressor=
audio-gate agate=
audio-equalizer equalizer=
audio-highpass highpass=
audio-lowpass lowpass=
audio-tempo atempo=
audio-reverse areverse
loudnorm loudnorm=
mono-delivery channel_layouts=mono
";

// ---- a seeded generator --------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        let mut r = Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        r.next();
        r
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() >> 11) as usize % n
    }

    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())]
    }

    /// A real in `[lo, hi)`: 0.37-style two decimals or a full-precision double,
    /// never a dyadic value (0.5, 0.25) whose shortest form hides what `{}` does to
    /// a real one.
    fn real(&mut self, lo: f64, hi: f64) -> f64 {
        let v = lo + (hi - lo) * self.unit();
        let two = (v * 100.0).round() / 100.0;
        match (self.chance(0.6), (two * 8.0).fract() == 0.0) {
            (true, false) => two,
            (true, true) => two + 0.01,
            _ => v,
        }
    }

    fn id(&mut self) -> Uuid {
        Uuid::from_u128((u128::from(self.next()) << 64) | u128::from(self.next()))
    }
}

#[rustfmt::skip]
fn stream(kind: StreamKind, codec: &str, size: (u32, u32), fps: f64) -> StreamInfo {
    let video = kind == StreamKind::Video;
    StreamInfo {
        index: 0, kind, codec: codec.into(), width: video.then_some(size.0), height: video.then_some(size.1), fps: video.then_some(fps),
        sample_rate: None, channels: None, image: false, projection: None, rotation: 0, color_transfer: None, color_primaries: None,
        pix_fmt: video.then(|| "yuv420p".into()), color_space: None,
    }
}

#[rustfmt::skip]
fn audio(rate: u32, channels: u16) -> StreamInfo {
    StreamInfo { sample_rate: Some(rate), channels: Some(channels), ..stream(StreamKind::Audio, "aac", (0, 0), 0.0) }
}

/// The fixed library every case draws from: shapes and rates, a still, both HDR
/// transfers, two 360 projections, a phone clip turned by its metadata, audio only.
#[rustfmt::skip]
fn pool() -> Vec<Asset> {
    let video = |size, fps| stream(StreamKind::Video, "h264", size, fps);
    let hdr = |size, fps, trc: &str| StreamInfo {
        color_transfer: Some(trc.into()), color_primaries: Some("bt2020".into()), color_space: Some("bt2020nc".into()), ..video(size, fps)
    };
    let sphere = |p| StreamInfo { projection: Some(p), ..video((5760, 2880), 30.0) };
    let still = StreamInfo { image: true, codec: "png".into(), fps: None, ..video((1280, 720), 0.0) };
    let phone = StreamInfo { rotation: 90, ..video((1080, 1920), 30.0) };
    let specs = vec![
        ("interview", 120.0, vec![video((1920, 1080), 30_000.0 / 1001.0), audio(48_000, 2)]),
        ("broll", 90.0, vec![video((3840, 2160), 24.0)]),
        ("phone", 40.0, vec![phone, audio(44_100, 1)]),
        ("still", 5.0, vec![still]),
        ("hdr-pq", 60.0, vec![hdr((3840, 2160), 30.0, "smpte2084"), audio(48_000, 2)]),
        ("hdr-hlg", 30.0, vec![hdr((1920, 1080), 59.94, "arib-std-b67")]),
        ("sphere", 50.0, vec![sphere(Projection::Equirect), audio(48_000, 1)]),
        ("lenses", 20.0, vec![sphere(Projection::DualFisheye)]),
        ("wide", 75.0, vec![video((1280, 720), 25.0), audio(44_100, 2)]),
        ("music", 200.0, vec![audio(44_100, 2)]),
        ("voice", 60.0, vec![audio(16_000, 1)]),
    ];
    let imported_at = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
    let asset = |(i, (name, duration, streams)): (usize, (&str, f64, Vec<StreamInfo>))| Asset {
        id: Uuid::from_u128(i as u128 + 1), path: format!("/golden/{name}.mp4"), name: name.into(), duration, streams, imported_at,
        source_paths: Vec::new(), voiceover: None,
    };
    specs.into_iter().enumerate().map(asset).collect()
}

fn has_video(a: &Asset) -> bool {
    a.streams.iter().any(|s| s.kind == StreamKind::Video)
}

/// `Some(f(r))` with probability `p`, else `None`.
fn maybe<T>(r: &mut Rng, p: f64, f: impl FnOnce(&mut Rng) -> T) -> Option<T> {
    r.chance(p).then(|| f(r))
}

// The generator is a table of dice rolls: kept as written (130 columns) rather than
// one field per line.
#[rustfmt::skip]
fn clip(r: &mut Rng, a: &Asset, start: f64, earlier: &[Clip]) -> Clip {
    let len = if a.is_image() { r.real(1.0, 6.0) } else { r.real(0.5, 8.0) };
    let (src_in, src_out) = match earlier.last() {
        // The same footage again (a picture-in-picture of one source): a shared input.
        Some(e) if r.chance(0.1) && !a.is_image() => (e.source_in, e.source_out),
        _ if a.is_image() => (0.0, len),
        _ => {
            let src_in = if r.chance(0.3) { 0.0 } else { r.real(0.0, (a.duration - len).max(0.0)) };
            let out = match r.below(10) { 0 => a.duration + r.real(0.1, 2.0), 1 | 2 => a.duration, _ => (src_in + len).min(a.duration) };
            (src_in, out)
        }
    };
    let mut c = Clip::new(a.id, src_in, src_out, start);
    c.id = r.id();
    if !a.is_image() || r.chance(0.2) {
        c.speed = r.pick(&[1.0, 1.0, 1.0, 1.0, 0.5, 2.0, 0.7, 1.3, 4.0, 0.25, -1.0, -2.0, -0.7, 0.0]);
    }
    c.volume = maybe(r, 0.3, |r| r.real(0.0, 2.0) as f32).unwrap_or(1.0);
    c.fade_in = maybe(r, 0.35, |r| r.real(0.05, 2.0)).unwrap_or(0.0);
    c.fade_out = maybe(r, 0.35, |r| r.real(0.05, 2.0)).unwrap_or(0.0);
    c.transition_in = maybe(r, 0.4, |r| Transition { kind: r.pick(&TransitionKind::ALL), duration: r.real(0.1, 2.5) });
    if r.chance(0.5) {
        let t = &mut c.transform;
        t.scale = maybe(r, 0.5, |r| r.real(0.2, 2.4)).unwrap_or(1.0);
        (t.pos_x, t.pos_y) = maybe(r, 0.35, |r| (r.real(-0.6, 0.6), r.real(-0.6, 0.6))).unwrap_or_default();
        t.rotation = maybe(r, 0.25, |r| r.real(-120.0, 120.0)).unwrap_or(0.0);
        t.opacity = maybe(r, 0.3, |r| r.real(0.05, 0.99)).unwrap_or(1.0);
        if r.chance(0.25) {
            (t.crop_left, t.crop_right, t.crop_top, t.crop_bottom) = (r.real(0.0, 0.3), r.real(0.0, 0.3), r.real(0.0, 0.3), r.real(0.0, 0.3));
        }
    }
    if r.chance(0.3) {
        let k = &mut c.color;
        (k.brightness, k.contrast) = (r.real(-0.5, 0.5), r.real(0.5, 2.0));
        k.saturation = maybe(r, 0.5, |r| r.real(0.0, 2.0)).unwrap_or(1.0);
        k.gamma = maybe(r, 0.4, |r| r.real(0.5, 2.0)).unwrap_or(1.0);
        k.temperature = maybe(r, 0.4, |r| r.real(-1.0, 1.0)).unwrap_or(0.0);
    }
    for _ in 0..maybe(r, 0.28, |r| 1 + r.below(3)).unwrap_or(0) {
        let color = r.pick(&["green", "0x00ff00", "#00FF00@0.9", "bad colour!", "blue"]).to_string();
        let (x, y) = (r.real(0.01, 20.0), r.real(0.01, 0.6));
        c.effects.push(match r.below(6) {
            0 => VideoEffect::Blur { sigma: x },
            1 => VideoEffect::Sharpen { amount: x / 7.0 },
            2 => VideoEffect::Grayscale,
            3 => VideoEffect::Invert,
            4 => VideoEffect::Vignette,
            _ => VideoEffect::ChromaKey { color, similarity: y, blend: y / 2.0 },
        });
    }
    for _ in 0..maybe(r, 0.2, |r| 1 + r.below(2)).unwrap_or(0) {
        let (x, y) = (r.real(1.0, 80.0), r.real(-40.0, 12.0));
        c.audio.push(match r.below(5) {
            0 => AudioEffect::Highpass { hz: x * 5.0 },
            1 => AudioEffect::Lowpass { hz: x * 150.0 },
            2 => AudioEffect::Equalizer { hz: x * 90.0, width: x * 20.0, gain_db: y / 3.0 },
            3 => AudioEffect::Compressor { threshold_db: y, ratio: x / 8.0 + 1.0, attack_ms: x, release_ms: x * 8.0, makeup_db: x / 9.0 },
            _ => AudioEffect::Gate { threshold_db: y },
        });
    }
    for _ in 0..maybe(r, 0.25, |r| 1 + r.below(4)).unwrap_or(0) {
        let mut k = Keyframe::from_transform(r.real(0.0, c.duration() * 1.1 + 0.2), &c.transform);
        k.scale = maybe(r, 0.5, |r| r.real(0.3, 2.2)).unwrap_or(k.scale);
        (k.pos_x, k.pos_y) = maybe(r, 0.4, |r| (r.real(-0.5, 0.5), r.real(-0.5, 0.5))).unwrap_or((k.pos_x, k.pos_y));
        k.rotation = maybe(r, 0.3, |r| r.real(-90.0, 90.0)).unwrap_or(k.rotation);
        k.opacity = maybe(r, 0.4, |r| r.real(0.0, 1.0)).unwrap_or(k.opacity);
        c.keyframes.push(k);
    }
    // A reframe's `sendcmd` list grows with duration x fps, so keep slow clips out.
    if let (Some(p), true, true) = (a.projection(), c.speed_mag() >= 0.2, r.chance(0.85)) {
        let mut rf = Reframe::new(p);
        if r.chance(0.5) {
            (rf.yaw, rf.pitch, rf.roll, rf.fov) = (r.real(-200.0, 200.0), r.real(-100.0, 100.0), r.real(-20.0, 20.0), r.real(20.0, 150.0));
        }
        if r.chance(0.12) {
            rf.output = Projection::Equirect;
        }
        for _ in 0..maybe(r, 0.4, |r| 2 + r.below(3)).unwrap_or(0) {
            let (time, yaw, pitch) = (r.real(0.0, c.duration() + 0.5), r.real(-200.0, 200.0), r.real(-100.0, 100.0));
            rf.keyframes.push(ReframeKeyframe { time, yaw, pitch, roll: yaw / 7.0, fov: r.real(10.0, 170.0) });
        }
        c.reframe = Some(rf);
    }
    c.mask = maybe(r, 0.13, |r| Mask {
        shape: r.pick(&[MaskShape::Rect, MaskShape::Ellipse]), x: r.real(-0.2, 1.2), y: r.real(-0.2, 1.2),
        width: r.real(0.0, 1.5), height: r.real(0.0, 1.5), feather: maybe(r, 0.5, |r| r.real(0.0, 1.2)).unwrap_or(0.0), inverted: r.chance(0.3),
    });
    c.enabled = !r.chance(0.05);
    c
}

#[rustfmt::skip]
fn overlay(r: &mut Rng) -> TextOverlay {
    let text = r.pick(&["Hello", "50% off", "It's \"quoted\"", "back\\slash", "two\nlines", "a: b, c; [d] =e", "日本語 ünï", "", "%{pts}"]);
    let start = r.real(0.0, 20.0);
    let mut o = TextOverlay::new(text, start, start + r.real(0.2, 6.0));
    o.id = r.id();
    (o.pos_x, o.pos_y, o.size) = (r.real(0.0, 1.0), r.real(0.0, 1.0), r.real(0.02, 0.2));
    o.color = r.pick(&["white", "yellow@0.9", "#ffcc00", "0xFF00FF80", "not a colour!", "a,b"]).into();
    o.bg = r.pick(&[None, None, Some("black@0.5"), Some("#00000080"), Some("bad:colour")]).map(String::from);
    (o.bold, o.generated) = (r.chance(0.2), r.chance(0.2));
    for _ in 0..maybe(r, 0.3, |r| 1 + r.below(3)).unwrap_or(0) {
        let (time, pos_x, pos_y) = (r.real(0.0, 5.0), r.real(0.0, 1.0), r.real(0.0, 1.0));
        o.keyframes.push(TextKeyframe { time, pos_x, pos_y, opacity: r.real(0.0, 1.0) });
    }
    o
}

#[rustfmt::skip]
fn timeline(r: &mut Rng, assets: &[Asset]) -> Timeline {
    let video: Vec<&Asset> = assets.iter().filter(|a| has_video(a)).collect();
    let mut kinds = vec![StreamKind::Video; 1 + r.below(4)];
    kinds.extend(vec![StreamKind::Audio; r.below(4)]);
    for i in (1..kinds.len()).rev() {
        kinds.swap(i, r.below(i + 1));
    }
    let mut tl = Timeline::new();
    tl.tracks.clear();
    for (n, kind) in kinds.into_iter().enumerate() {
        let mut t = Track::new(kind, format!("T{n}"));
        t.id = r.id();
        (t.muted, t.solo) = (r.chance(0.04), r.chance(0.04));
        t.duck = kind == StreamKind::Audio && r.chance(0.3);
        t.volume = maybe(r, 0.3, |r| r.real(0.1, 1.6) as f32).unwrap_or(1.0);
        t.pan = maybe(r, 0.3, |r| r.real(-1.0, 1.0) as f32).unwrap_or(0.0);
        let mut cursor = maybe(r, 0.5, |r| r.real(0.0, 3.0)).unwrap_or(0.0);
        for _ in 0..r.below(if kind == StreamKind::Video { 7 } else { 5 }) {
            let a = if kind == StreamKind::Video { video[r.below(video.len())] } else { &assets[r.below(assets.len())] };
            // Adjacent exactly, adjacent within the 1 ms transitions tolerate, overlapping, or a gap.
            let gap = match r.below(10) { 0..=3 => 0.0, 4 => 0.0005, 5 => -r.real(0.05, 0.8), _ => r.real(0.1, 4.0) };
            let c = clip(r, a, (cursor + gap).max(0.0), &t.clips);
            cursor = c.timeline_end();
            t.clips.push(c);
        }
        tl.tracks.push(t);
    }
    for _ in 0..maybe(r, 0.35, |r| 1 + r.below(3)).unwrap_or(0) {
        tl.overlays.push(overlay(r));
    }
    tl.format = maybe(r, 0.3, |r| {
        let (w, h) = r.pick(&[(1080, 1920), (1080, 1080), (1920, 1080), (1080, 1350), (721, 405), (3840, 2160)]);
        Delivery::new(w, h, if r.chance(0.5) { Fit::Cover } else { Fit::Contain })
    });
    tl
}

#[rustfmt::skip]
fn options(r: &mut Rng, dur: f64) -> ExportOptions {
    let plain = [Container::Mp4, Container::Mp4, Container::Mp4, Container::Mkv, Container::Webm, Container::Mov];
    let other = [Container::Gif, Container::Mp3, Container::M4a, Container::Wav, Container::Flac];
    let mut o = ExportOptions { container: if r.chance(0.2) { r.pick(&other) } else { r.pick(&plain) }, ..ExportOptions::default() };
    o.video_codec = maybe(r, 0.5, |r| r.pick(&["libx264", "libx265", "libvpx-vp9", "prores_ks", "gif"]).into());
    o.crf = maybe(r, 0.3, |r| r.below(40) as u32);
    o.resolution = maybe(r, 0.25, |r| (r.pick(&[640, 1280, 1920, 721, 1080]), r.pick(&[360, 720, 1080, 405, 1920])));
    o.fps = maybe(r, 0.3, |r| r.pick(&[24.0, 25.0, 29.97, 30_000.0 / 1001.0, 50.0, 60.0]));
    o.pix_fmt = maybe(r, 0.15, |r| r.pick(&["yuv420p", "yuv422p10le", "yuvj420p", "nope"]).into());
    o.scaler = maybe(r, 0.25, |r| r.pick(&["bicubic", "bilinear", "lanczos", "not-a-scaler"]).into());
    o.hwaccel = maybe(r, 0.15, |_| "auto".into());
    (o.include_audio, o.loudnorm) = (!r.chance(0.1), r.chance(0.1));
    o.audio_channels = maybe(r, 0.12, |r| r.pick(&[1, 2]));
    o.audio_sample_rate = maybe(r, 0.1, |r| r.pick(&[22_050, 44_100]));
    o.gif_dither = maybe(r, 0.3, |r| r.pick(&["sierra2", "bayer", "bogus"]).into());
    o.fit = if r.chance(0.25) { Fit::Cover } else { Fit::Contain };
    o.range = maybe(r, 0.1, |r| { let start = r.real(0.0, dur.max(1.0)); TimeRange { start, end: start + r.real(0.5, 10.0) } });
    o
}

struct Case {
    timeline: Timeline,
    opts: ExportOptions,
    /// The two stills (time, sink) and the rest of what `build_still_args` takes.
    stills: [(f64, StillOutput); 2],
    region: Option<Region>,
    still_width: u32,
    /// The preview: playhead, fps, width, decode acceleration.
    preview: (f64, f64, u32, Option<String>),
}

#[rustfmt::skip]
fn case(i: usize, assets: &[Asset]) -> Case {
    let mut r = Rng::new(i as u64 + 1);
    let timeline = timeline(&mut r, assets);
    let dur = timeline.duration();
    let edges: Vec<f64> = (timeline.tracks.iter().flat_map(|t| &t.clips))
        .flat_map(|c| [c.timeline_start, c.timeline_end(), c.timeline_end() - 1e-3, c.timeline_start + 1e-4])
        .collect();
    let time = |r: &mut Rng| match r.below(6) {
        0 => 0.0,
        1 => dur,
        _ if !edges.is_empty() && r.chance(0.5) => edges[r.below(edges.len())],
        _ => r.real(0.0, dur.max(0.5)),
    };
    let sink = |r: &mut Rng| match r.below(3) {
        0 => StillOutput::JpegPipe { quality: r.pick(&[2, 4, 31]) },
        1 => StillOutput::File { path: "/golden/cover.png".into(), format: ImageFormat::Png, quality: 2 },
        _ => StillOutput::RgbPipe,
    };
    let opts = options(&mut r, dur);
    Case {
        stills: [(time(&mut r), sink(&mut r)), (time(&mut r), sink(&mut r))],
        region: maybe(&mut r, 0.2, |r| Region { left: r.real(-0.1, 0.8), top: r.real(-0.1, 0.8), width: r.real(0.0, 1.2), height: r.real(0.0, 1.2) }),
        still_width: r.pick(&[320, 640, 960, 1280, 1920, u32::MAX]),
        preview: (time(&mut r), r.pick(&[24.0, 25.0, 29.97, 30.0, 60.0]), r.pick(&[640, 960, 1280, 1920]), maybe(&mut r, 0.3, |_| "auto".into())),
        timeline,
        opts,
    }
}

// ---- digests and coverage ------------------------------------------------------

fn repr(args: &Result<Vec<String>>) -> String {
    match args {
        Ok(a) => a.join("\0"),
        Err(e) => format!("ERR\0{e}"),
    }
}

/// What one case builds: the argv text of each builder (a case holding HDR footage
/// built with `zscale` present *and* absent), and the transition branches it took.
fn build(c: &Case, assets: &[Asset]) -> ([String; 3], Vec<String>) {
    let hdr = |id| assets.iter().any(|a| a.id == id && a.hdr().is_some());
    let both = c.timeline.tracks.iter().flat_map(|t| &t.clips).any(|k| hdr(k.asset_id));
    let mut text = [String::new(), String::new(), String::new()];
    for &zscale in if both { &[true, false][..] } else { &[true][..] } {
        with_zscale(zscale, || {
            let export = build_export_args_phase(&c.timeline, assets, "/golden/out.mp4", &c.opts, PassPhase::Single, "", "");
            text[0] += &repr(&export);
            for (t, out) in &c.stills {
                text[1] += &repr(&build_still_args(
                    &c.timeline,
                    assets,
                    &c.opts,
                    *t,
                    c.still_width,
                    c.region,
                    out,
                ));
                text[1].push('\u{1}');
            }
            let (start, fps, width, hw) = c.preview.clone();
            text[2] += &repr(&build_preview_args_with(&c.timeline, assets, start, fps, width, 6, hw));
        });
    }
    (text, transitions(c, assets))
}

/// Which way each transition went, from what `transition_fx` did with it: a dip
/// next to a clip or across a gap, a dissolve or motion transition with a source
/// handle to borrow, without one (a hard cut), or across a gap.
fn transitions(c: &Case, assets: &[Asset]) -> Vec<String> {
    let tl = c.timeline.for_render();
    let fx = transition_fx(&tl, assets);
    let (mut out, mut base) = (Vec::new(), 0);
    for track in &tl.tracks {
        let mut order: Vec<usize> = (0..track.clips.len()).collect();
        order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
        for (w, &j) in order.iter().enumerate() {
            let clip = &track.clips[j];
            let Some(tr) = clip
                .transition_in
                .filter(|t| t.duration > 0.0 && track.kind == StreamKind::Video)
            else {
                continue;
            };
            let adjacent = w > 0 && (track.clips[order[w - 1]].timeline_end() - clip.timeline_start).abs() < 1e-3;
            let moved = fx[base + j].xfade_in > 0.0 || fx[base + j].move_in.is_some();
            let state = match (adjacent, tr.kind.dip_color().is_some(), moved) {
                (false, ..) => "gap",
                (true, true, _) => "adjacent",
                (true, false, true) => "handle",
                (true, false, false) => "no-handle",
            };
            out.push(format!("transition/{}/{state}", tr.kind.as_str()));
        }
        base += track.clips.len();
    }
    out
}

fn expected(table: &[(&str, &str)]) -> Vec<String> {
    let mut f: Vec<String> = table.iter().map(|(n, _)| n.to_string()).collect();
    f.extend(["muted-track", "solo-track", "disabled-clip", "no-video"].map(String::from));
    for k in TransitionKind::ALL {
        let states: &[&str] = if k.dip_color().is_some() {
            &["adjacent", "gap"]
        } else {
            &["handle", "no-handle", "gap"]
        };
        f.extend(states.iter().map(|s| format!("transition/{}/{s}", k.as_str())));
    }
    f
}

/// The families a case hits: its transition branches, the table entries whose text
/// is in the export or still argv (the preview repeats the export's graph), and what
/// only the structure shows.
fn families(c: &Case, text: &[String; 3], table: &[(&str, &str)], transitions: Vec<String>) -> Vec<String> {
    let tracks = &c.timeline.tracks;
    let video = c
        .timeline
        .for_render()
        .tracks
        .iter()
        .any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    let mut f = transitions;
    f.extend(
        table
            .iter()
            .filter(|(_, n)| text[0].contains(n) || text[1].contains(n))
            .map(|(name, _)| name.to_string()),
    );
    for (hit, name) in [
        (tracks.iter().any(|t| t.muted), "muted-track"),
        (tracks.iter().any(|t| t.solo), "solo-track"),
        (tracks.iter().flat_map(|t| &t.clips).any(|k| !k.enabled), "disabled-clip"),
        (!video, "no-video"),
    ] {
        if hit {
            f.push(name.into());
        }
    }
    f
}

#[allow(clippy::print_stderr)]
fn dump(n: usize, assets: &[Asset]) {
    let c = case(n, assets);
    let (text, _) = build(&c, assets);
    eprintln!("case {n}: {:?}", c.opts);
    for (kind, t) in KINDS.iter().zip(&text) {
        eprintln!("-- {kind}\n{}", t.replace('\0', " ").replace('\u{1}', "\n"));
    }
}

/// A case's three digests and the families it hits.
type Done = ([u64; 3], Vec<String>);

/// One case's three digests and the families it hits. Cases are independent, so the
/// test spreads them over the machine's cores (`with_zscale` is per thread).
fn run(i: usize, assets: &[Asset], table: &[(&str, &str)]) -> Done {
    let c = case(i, assets);
    let (text, transitions) = build(&c, assets);
    let mut hit = families(&c, &text, table, transitions);
    hit.sort();
    hit.dedup();
    (text.each_ref().map(|t| fnv1a(t)), hit)
}

#[test]
fn the_argv_builders_still_produce_the_golden_digests() {
    let assets = pool();
    if let Some(n) = std::env::var("KERF_GOLDEN_DUMP").ok().and_then(|v| v.parse().ok()) {
        dump(n, &assets);
    }
    let table: Vec<(&str, &str)> = FAMILIES.lines().filter_map(|l| l.split_once(' ')).collect();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
    let mut results: Vec<(usize, Done)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|w| {
                let (assets, table) = (&assets, &table);
                s.spawn(move || {
                    (w..CASES)
                        .step_by(threads)
                        .map(|i| (i, run(i, assets, table)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    results.sort_by_key(|(i, _)| *i);

    let mut seen: BTreeMap<String, usize> = expected(&table).into_iter().map(|f| (f, 0)).collect();
    let mut digests: [Vec<u64>; 3] = Default::default();
    let mut per_case = String::new();
    for (i, (h, hit)) in &results {
        per_case += &format!("{i} {:016x} {:016x} {:016x}\n", h[0], h[1], h[2]);
        h.iter().zip(&mut digests).for_each(|(h, d)| d.push(*h));
        for f in hit {
            *seen
                .get_mut(f)
                .unwrap_or_else(|| panic!("a family the oracle does not list: {f}")) += 1;
        }
    }
    if let Some(path) = std::env::var_os("KERF_GOLDEN_CASES") {
        std::fs::write(path, per_case).unwrap();
    }
    let thin: Vec<_> = seen.iter().filter(|(_, n)| **n < MIN_PER_FAMILY).collect();
    assert!(
        thin.is_empty(),
        "families under {MIN_PER_FAMILY} cases (the generator stopped covering them): {thin:?}"
    );

    let mut changed = Vec::new();
    for ((kind, cases), blessed) in KINDS.iter().zip(&digests).zip(BLESSED) {
        let mut want = format!(
            "# {kind} argv digests (engine/cli/golden.rs): block, then FNV-1a of its {BLOCK} cases.\n\
             # Regenerate after an intended change: KERF_GOLDEN_BLESS=1 cargo test -p kerf-core --no-default-features golden\n"
        );
        for (i, block) in cases.chunks(BLOCK).enumerate() {
            want += &format!("{i} {:016x}\n", fnv1a(&format!("{block:x?}")));
        }
        if std::env::var_os("KERF_GOLDEN_BLESS").is_some() {
            let file = format!("src/engine/cli/golden/{kind}.txt");
            std::fs::write(Path::new(env!("CARGO_MANIFEST_DIR")).join(file), want).unwrap();
        } else if want != blessed {
            let bad: Vec<_> = want
                .lines()
                .zip(blessed.lines())
                .filter(|(a, b)| a != b)
                .map(|(a, _)| a.split(' ').next().unwrap())
                .collect();
            changed.push(format!("{kind}: blocks {bad:?}"));
        }
    }
    assert!(
        changed.is_empty(),
        "the argv moved ({}; case = block x {BLOCK} .. +{BLOCK}; an empty list is a stale file). If that is intended, re-bless the \
         files of the builders you changed (see the module docs); otherwise find the case with KERF_GOLDEN_CASES=<file>.",
        changed.join("; ")
    );
}
