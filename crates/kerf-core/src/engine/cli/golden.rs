//! The golden argv oracle: 4000 seeded timelines whose ffmpeg argv must not move.
//!
//! `build_export_args_phase`, `build_still_args` and `build_preview_args_with` are
//! built for every case and their argv reduced to a digest, checked against three
//! committed files (`golden/{export,still,preview}.txt`, one line per block of
//! [`BLOCK`] cases). A refactor of `video_clip_chain`, `audio_clip_chain`,
//! `transition_fx` or `build_filter_complex` has to leave all three untouched; an
//! intended change to one builder moves only its own file, so the other two keep
//! proving nothing else moved.
//!
//! **Machine-independent by construction.** The builders read the machine in four
//! places, each pinned: the preview's decode acceleration (the
//! `build_preview_args_with` seam), `zscale_available()` (`with_zscale`; a case
//! holding HDR footage is built *both* ways), `drawtext`'s resolved font path (no
//! overlay names a font, so none is looked up), and libm: the compressor and gate
//! thresholds go through `powf`, whose last digits differ between glibc builds
//! (FMA or not), macOS, Windows and arm, so [`round_libm`] compares those numbers
//! to 10 significant digits, which still pins the dB mapping. The digest files are
//! LF whatever the checkout (`.gitattributes`, and the comparison ignores `\r`).
//!
//! **Bless** after an intended argv change, from the repo root:
//!
//! ```text
//! KERF_GOLDEN_BLESS=1 cargo test -p kerf-core --no-default-features golden -- --nocapture
//! ```
//!
//! It rewrites all three files (and says so); `git diff` is the guard, and only the
//! files of the builders you changed should move. To find the case behind a failing
//! block, run the test on the base and on the change with `KERF_GOLDEN_CASES=<file>`
//! (one digest line per case) and diff the two files; `KERF_GOLDEN_DUMP=<case>` then
//! prints that case's argv (`KERF_GOLDEN_COVERAGE=1` prints the thinnest families).
//! The files are identical under any `KERF_HWACCEL` and
//! with `GLIBC_TUNABLES=glibc.cpu.hwcaps=-FMA,-FMA4`.
//!
//! **Coverage.** [`FAMILIES`] lists the branches the oracle exists to protect, each
//! as text that must appear in a case's argv, and the test fails if any family shows
//! up in fewer than [`MIN_PER_FAMILY`] cases (the generator stopped covering it, or
//! the argv text changed), so it cannot quietly stop covering one.

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
/// The assets the generator draws from; the head-padded twins after them are only
/// ever reached by [`retarget`], so that adding them moved no other case's dice.
const LIBRARY: usize = 11;
const BLESSED: [&str; 3] = [
    include_str!("golden/export.txt"),
    include_str!("golden/still.txt"),
    include_str!("golden/preview.txt"),
];

/// `family needle`: the family is hit when the needle is in the argv text (words
/// joined by spaces) of a case: of the still for `still-...`, of the preview for
/// `preview-...`, of the export for the rest.
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
head-padded-proxy trim=start_frame=1
image-input -loop 1 -framerate
image-under-a-frame -t 0.0
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
gif-loop-forever -loop 0
gif-loop-once -loop -1
hwaccel -hwaccel
still-region crop=2*trunc(
still-file-png image2 -vcodec png
still-file-jpeg image2 -vcodec mjpeg -y
still-jpeg-pipe image2pipe
still-rgb-pipe -pix_fmt rgb24
ducking sidechaincompress
audio-pan pan=stereo|c0=
audio-compressor acompressor=
libm-canary-39 threshold=1.065368864e-2
libm-canary-47 threshold=4.246195639e-3
libm-canary-52 threshold=2.401596268e-3
audio-gate agate=
audio-equalizer equalizer=
audio-highpass highpass=
audio-lowpass lowpass=
audio-tempo atempo=
audio-reverse areverse
audio-aac -c:a aac
audio-mp3 -c:a libmp3lame
audio-opus -c:a libopus
audio-flac -c:a flac
audio-bitrate -b:a
audio-flac-level -compression_level
audio-muted -an
loudnorm loudnorm=
mono-delivery channel_layouts=mono
codec-x264 -c:v libx264
codec-x265 -c:v libx265
codec-svtav1 -c:v libsvtav1
codec-vp9 -c:v libvpx-vp9
codec-prores -c:v prores_ks
codec-gif -c:v gif
codec-nvenc -c:v h264_nvenc
codec-qsv -c:v hevc_qsv
codec-videotoolbox -c:v h264_videotoolbox
codec-amf -c:v hevc_amf
codec-unknown -c:v libfoo
rate-crf -crf
rate-vp9-constant-quality -b:v 0
rate-nvenc-vbr -rc vbr
rate-nvenc-cq -cq
rate-qsv -global_quality
rate-videotoolbox -q:v
rate-amf-cqp -qp_i
rate-bitrate -b:v 8M
rate-bitrate-k -b:v 2500k
rate-maxrate -maxrate
rate-bufsize -bufsize
rate-lossless-vp9 -lossless 1
rate-lossless-nvenc -rc constqp
two-pass-first -pass 1
two-pass-second -pass 2
two-pass-log -passlogfile
first-pass-null -f null
preset -preset
vp9-speed -cpu-used
tune -tune
profile-high -profile:v high
profile-main -profile:v main
prores-profile-3 -profile:v 3
prores-4444 -profile:v 4
prores-4444-xq -profile:v 5
prores-4444-pix -pix_fmt yuva444p10le
hevc-tag -tag:v hvc1
faststart -movflags +faststart
title -metadata title=
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
/// transfers, two 360 projections, a phone clip turned by its metadata, audio only —
/// and, after those, the head-padded proxies of four of them (see [`retarget`]).
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
    let mut pool: Vec<Asset> = specs.into_iter().enumerate().map(asset).collect();
    debug_assert_eq!(pool.len(), LIBRARY);
    // The proxy a preview of a late-starting source is cut from: the same footage at a
    // path `is_head_padded_proxy` recognizes (`.../kerf/proxies/<16 hex>.lead.mp4`).
    for (n, name) in ["interview", "broll", "phone", "wide"].into_iter().enumerate() {
        let original = pool.iter().find(|a| a.name == name).unwrap().clone();
        pool.push(Asset {
            id: Uuid::from_u128(100 + n as u128),
            path: format!("/golden/kerf/proxies/{:016x}.lead.mp4", 0x5eed_0000 + n as u64),
            name: format!("{name}-padded"),
            ..original
        });
    }
    pool
}

/// Every seventh case, about half of the clips whose footage has a head-padded proxy
/// are cut from that proxy instead. It is done once the timeline is drawn and rolls
/// no dice of its own (the clip's id decides), so every other case is exactly what it
/// was; it is what puts `ClipFx.head_pad` — and its `trim=start_frame=1` — in the oracle.
fn retarget(tl: &mut Timeline, i: usize, assets: &[Asset]) {
    if i % 7 != 3 {
        return;
    }
    for clip in tl.tracks.iter_mut().flat_map(|t| &mut t.clips) {
        let Some(original) = assets[..LIBRARY].iter().find(|a| a.id == clip.asset_id) else {
            continue;
        };
        if clip.id.as_u128() % 2 == 1 {
            if let Some(twin) = assets[LIBRARY..]
                .iter()
                .find(|a| a.name.strip_suffix("-padded") == Some(&original.name))
            {
                clip.asset_id = twin.id;
            }
        }
    }
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
    let len = if a.is_image() { if r.chance(0.15) { r.real(0.001, 0.03) } else { r.real(1.0, 6.0) } } else { r.real(0.5, 8.0) };
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
        // A tenth of the thresholds are dB values whose `powf` differs between glibc with and without FMA.
        let (x, y) = (r.real(1.0, 80.0), if r.chance(0.1) { r.pick(&[-39.45, -47.44, -52.39]) } else { r.real(-40.0, 12.0) });
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
        // A moving camera: every channel, exactly one, a single key, or a held pose.
        let (keys, mode, one) = (maybe(r, 0.4, |r| 1 + r.below(4)).unwrap_or(0), r.below(4), r.below(4));
        for _ in 0..keys {
            let v = [r.real(-200.0, 200.0), r.real(-100.0, 100.0), r.real(-30.0, 30.0), r.real(10.0, 170.0)];
            let held = [rf.yaw, rf.pitch, rf.roll, rf.fov];
            let at = |n: usize| if mode == 0 || mode == 2 || (mode == 1 && n == one) { v[n] } else { held[n] };
            let time = r.real(0.0, c.duration() + 0.5);
            rf.keyframes.push(ReframeKeyframe { time, yaw: at(0), pitch: at(1), roll: at(2), fov: at(3) });
        }
        if mode == 2 {
            rf.keyframes.truncate(1);
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
    // ProRes thrice: its profile has six values and the 4444 ones change the pixel format.
    let codecs = ["libx264", "libx265", "libsvtav1", "libvpx-vp9", "prores_ks", "prores_ks", "prores_ks", "gif", "h264_nvenc", "hevc_nvenc",
                  "av1_nvenc", "h264_qsv", "hevc_qsv", "h264_videotoolbox", "hevc_videotoolbox", "h264_amf", "hevc_amf", "libfoo"];
    o.video_codec = maybe(r, 0.6, |r| r.pick(&codecs).into());
    o.rate_control = r.pick(&[RateControl::Crf, RateControl::Crf, RateControl::Bitrate, RateControl::TwoPass, RateControl::Lossless, RateControl::Lossless]);
    o.crf = maybe(r, 0.5, |r| r.below(40) as u32);
    o.video_bitrate = maybe(r, 0.5, |r| r.pick(&["8M", "2500k"]).into());
    o.max_rate = maybe(r, 0.4, |r| r.pick(&["12M", "4000k"]).into());
    o.buf_size = maybe(r, 0.4, |r| r.pick(&["16M", "5000k"]).into());
    o.preset = maybe(r, 0.4, |r| r.pick(&["slow", "veryfast", "p4", "5"]).into());
    o.tune = maybe(r, 0.5, |r| r.pick(&["film", "zerolatency", "stillimage", "bogus"]).into());
    o.profile_v = maybe(r, 0.3, |r| r.pick(&["high", "main", "main10"]).into());
    o.prores_profile = maybe(r, 0.6, |r| r.pick(&[0, 1, 2, 3, 4, 4, 5, 5]));
    o.audio_codec = maybe(r, 0.5, |r| r.pick(&["aac", "libmp3lame", "libopus", "flac", "pcm_s16le", "bogus"]).into());
    o.audio_bitrate = maybe(r, 0.5, |r| r.pick(&["128k", "192k"]).into());
    o.flac_compression = maybe(r, 0.6, |r| r.below(13) as u8);
    o.faststart = r.chance(0.3);
    o.metadata_title = maybe(r, 0.25, |r| r.pick(&["My cut", "Say \"hi\"", ""]).into());
    o.gif_loop = !r.chance(0.4);
    o.resolution = maybe(r, 0.25, |r| (r.pick(&[640, 1280, 1920, 721, 1080]), r.pick(&[360, 720, 1080, 405, 1920])));
    o.fps = maybe(r, 0.3, |r| r.pick(&[24.0, 25.0, 29.97, 30_000.0 / 1001.0, 50.0, 60.0]));
    o.pix_fmt = maybe(r, 0.15, |r| r.pick(&["yuv420p", "yuv422p10le", "yuvj420p", "nope"]).into());
    o.scaler = maybe(r, 0.25, |r| r.pick(&["bicubic", "bilinear", "lanczos", "not-a-scaler"]).into());
    o.hwaccel = maybe(r, 0.25, |r| r.pick(&["auto", "none", "", "NONE", "cuda"]).into());
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
    /// The preview: playhead, fps, width, JPEG quality (clamped by the builder), decode acceleration.
    preview: (f64, f64, u32, u8, Option<String>),
    /// The export's pass, null sink and passlog file.
    pass: (PassPhase, &'static str, &'static str),
}

#[rustfmt::skip]
fn case(i: usize, assets: &[Asset]) -> Case {
    let mut r = Rng::new(i as u64 + 1);
    let mut timeline = timeline(&mut r, &assets[..LIBRARY]);
    retarget(&mut timeline, i, assets);
    let dur = timeline.duration();
    let edges: Vec<f64> = (timeline.tracks.iter().flat_map(|t| &t.clips))
        .flat_map(|c| [c.timeline_start, c.timeline_end(), c.timeline_end() - 1e-3, c.timeline_start + 1e-4])
        .collect();
    let overlay_edges: Vec<f64> = timeline.overlays.iter().flat_map(|o| [o.start, o.end]).collect();
    let time = |r: &mut Rng| match r.below(20) {
        0..=2 => 0.0,
        3 | 4 => dur,
        5 => -r.real(0.0, 2.0),
        6 | 7 if !overlay_edges.is_empty() => overlay_edges[r.below(overlay_edges.len())],
        8..=12 if !edges.is_empty() => edges[r.below(edges.len())],
        _ => r.real(0.0, dur.max(0.5)),
    };
    let sink = |r: &mut Rng| match r.below(4) {
        0 => StillOutput::JpegPipe { quality: r.pick(&[2, 4, 31]) },
        1 => StillOutput::File { path: "/golden/cover.png".into(), format: ImageFormat::Png, quality: 2 },
        2 => StillOutput::File { path: "/golden/cover.jpg".into(), format: ImageFormat::Jpeg, quality: r.pick(&[2, 15, 31]) },
        _ => StillOutput::RgbPipe,
    };
    let opts = options(&mut r, dur);
    let pass = match r.below(20) { 0..=11 => PassPhase::Single, 12..=15 => PassPhase::First, _ => PassPhase::Second };
    Case {
        stills: [(time(&mut r), sink(&mut r)), (time(&mut r), sink(&mut r))],
        region: maybe(&mut r, 0.2, |r| Region { left: r.real(-0.1, 0.8), top: r.real(-0.1, 0.8), width: r.real(0.0, 1.2), height: r.real(0.0, 1.2) }),
        still_width: r.pick(&[320, 640, 960, 1280, 1920, u32::MAX]),
        preview: (time(&mut r), r.pick(&[24.0, 25.0, 29.97, 30.0, 60.0]), r.pick(&[2, 320, 640, 960, 1280, 1920, 100_000]),
                  r.pick(&[0, 1, 2, 6, 31, 32, 255]), maybe(&mut r, 0.3, |r| r.pick(&["auto", "none", ""]).into())),
        pass: (pass, r.pick(&["/dev/null", "NUL"]), r.pick(&["", "/golden/pass"])),
        timeline,
        opts,
    }
}

// ---- digests and coverage ------------------------------------------------------

/// `db_to_linear` is `10f64.powf(db / 20.0)`, and a libm's `pow` is not correctly
/// rounded: the last digits differ between glibc builds (with and without FMA),
/// macOS, Windows and arm. The compressor and gate numbers (`threshold=`, `makeup=`)
/// are therefore kept to 10 significant digits, which still pins the dB mapping: a
/// changed formula moves the third digit, a different libm only the seventeenth.
fn round_libm(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((at, key)) = ["threshold=", "makeup="]
        .iter()
        .filter_map(|k| Some((rest.find(k)?, k.len())))
        .min()
    {
        let (head, tail) = rest.split_at(at + key);
        out += head;
        let digits = tail
            .find(|c: char| !(c.is_ascii_digit() || ".eE+-".contains(c)))
            .unwrap_or(tail.len());
        match tail[..digits].parse::<f64>() {
            Ok(v) => out += &format!("{v:.9e}"),
            Err(_) => out += &tail[..digits],
        }
        rest = &tail[digits..];
    }
    out + rest
}

fn repr(args: &Result<Vec<String>>) -> String {
    match args {
        Ok(a) => round_libm(&a.join("\0")),
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
            let (pass, null_sink, passlog) = c.pass;
            let export = build_export_args_phase(&c.timeline, assets, "/golden/out.mp4", &c.opts, pass, null_sink, passlog);
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
            let (start, fps, width, quality, hw) = c.preview.clone();
            text[2] += &repr(&build_preview_args_with(&c.timeline, assets, start, fps, width, quality, hw));
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

/// The families that come from the case's structure rather than from argv text.
const STRUCTURAL: [&str; 11] = [
    "muted-track",
    "solo-track",
    "disabled-clip",
    "no-video",
    "hwaccel-skipped",
    "preview-quality-clamped-low",
    "preview-quality-clamped-high",
    "reframe-one-channel",
    "reframe-single-keyframe",
    "reframe-held",
    "time-negative",
];

fn expected(table: &[(&str, &str)]) -> Vec<String> {
    let mut f: Vec<String> = table.iter().map(|(n, _)| n.to_string()).collect();
    f.extend(STRUCTURAL.iter().chain(&["time-at-overlay-edge"]).map(|s| s.to_string()));
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

/// The families a case hits: its transition branches, the table entries whose text is
/// in the argv they are about (see [`FAMILIES`]), and what only the structure shows.
fn families(c: &Case, text: &[String; 3], table: &[(&str, &str)], transitions: Vec<String>) -> Vec<String> {
    let flat = text.each_ref().map(|t| t.replace(['\0', '\u{1}'], " "));
    let mut f = transitions;
    for (name, needle) in table {
        let scope = if name.starts_with("still-") {
            1
        } else if name.starts_with("preview-") {
            2
        } else {
            0
        };
        if flat[scope].contains(needle) {
            f.push(name.to_string());
        }
    }
    let tl = &c.timeline;
    let rendered = tl.for_render();
    let clips = || {
        rendered
            .tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .flat_map(|t| &t.clips)
    };
    let times = [c.stills[0].0, c.stills[1].0, c.preview.0];
    let channels = |rf: &Reframe| {
        let k = &rf.keyframes;
        [
            k.iter().any(|x| x.yaw != k[0].yaw),
            k.iter().any(|x| x.pitch != k[0].pitch),
            k.iter().any(|x| x.roll != k[0].roll),
            k.iter().any(|x| x.fov != k[0].fov),
        ]
        .iter()
        .filter(|moves| **moves)
        .count()
    };
    let moving: Vec<usize> = clips()
        .filter_map(|k| k.reframe.as_ref().filter(|r| r.keyframes.len() > 1))
        .map(channels)
        .collect();
    for (hit, name) in [
        (tl.tracks.iter().any(|t| t.muted), "muted-track"),
        (tl.tracks.iter().any(|t| t.solo), "solo-track"),
        (tl.tracks.iter().flat_map(|t| &t.clips).any(|k| !k.enabled), "disabled-clip"),
        (clips().next().is_none(), "no-video"),
        (
            matches!(c.opts.hwaccel.as_deref(), Some(h) if h.is_empty() || h.eq_ignore_ascii_case("none")),
            "hwaccel-skipped",
        ),
        (c.preview.3 < 2, "preview-quality-clamped-low"),
        (c.preview.3 > 31, "preview-quality-clamped-high"),
        (moving.contains(&1), "reframe-one-channel"),
        (moving.contains(&0), "reframe-held"),
        (
            clips().any(|k| matches!(&k.reframe, Some(r) if r.keyframes.len() == 1)),
            "reframe-single-keyframe",
        ),
        (times.iter().any(|t| *t < 0.0), "time-negative"),
        (
            tl.overlays.iter().any(|o| times.contains(&o.start) || times.contains(&o.end)),
            "time-at-overlay-edge",
        ),
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
fn round_libm_keeps_the_mapping_and_drops_the_last_digits() {
    let a = "acompressor=threshold=0.031622776601683794:ratio=4:makeup=1.2589254117941673 agate=threshold=0.01";
    // One ulp of the compressor threshold: what another libm's `pow` may return.
    let b = "acompressor=threshold=0.03162277660168380:ratio=4:makeup=1.2589254117941675 agate=threshold=0.01";
    assert_ne!(a, b);
    assert_eq!(round_libm(a), round_libm(b));
    // A different dB mapping (`/ 10` rather than `/ 20`) is not hidden.
    assert_ne!(
        round_libm(a),
        round_libm("acompressor=threshold=0.001:ratio=4:makeup=1.2589254117941673 agate=threshold=0.01")
    );
    assert!(round_libm("a=threshold=0.5:b=1").contains("5.000000000e-1:b=1"));
}

#[test]
#[allow(clippy::print_stderr)]
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

    // The digests first, so a moved block is reported even when coverage fails too.
    let bless = std::env::var("KERF_GOLDEN_BLESS").as_deref() == Ok("1");
    let mut failures = Vec::new();
    for ((kind, cases), blessed) in KINDS.iter().zip(&digests).zip(BLESSED) {
        let blessed = blessed.replace("\r\n", "\n");
        let mut want = format!(
            "# {kind} argv digests (engine/cli/golden.rs): block, then FNV-1a of its {BLOCK} cases.\n\
             # Regenerate after an intended change: KERF_GOLDEN_BLESS=1 cargo test -p kerf-core --no-default-features golden\n"
        );
        for (i, block) in cases.chunks(BLOCK).enumerate() {
            want += &format!("{i} {:016x}\n", fnv1a(&format!("{block:x?}")));
        }
        if bless {
            let file = format!("src/engine/cli/golden/{kind}.txt");
            std::fs::write(Path::new(env!("CARGO_MANIFEST_DIR")).join(&file), &want).unwrap();
            let moved = want != blessed;
            eprintln!(
                "golden: wrote {file} ({} blocks, {})",
                cases.chunks(BLOCK).count(),
                if moved { "changed" } else { "unchanged" }
            );
        } else if want != blessed {
            let bad: Vec<_> = want
                .lines()
                .zip(blessed.lines())
                .filter(|(a, b)| a != b)
                .map(|(a, _)| a.split(' ').next().unwrap().to_string())
                .collect();
            failures.push(format!(
                "the {kind} argv moved in blocks {bad:?} (case = block x {BLOCK} .. +{BLOCK}; an empty list is a stale file). If that is intended, \
                 re-bless (see the module docs) and review `git diff`; otherwise find the case with KERF_GOLDEN_CASES=<file>."
            ));
        }
    }
    if std::env::var_os("KERF_GOLDEN_COVERAGE").is_some() {
        let mut by_count: Vec<_> = seen.iter().collect();
        by_count.sort_by_key(|(_, n)| **n);
        eprintln!("golden: the {} thinnest families: {:?}", 12, &by_count[..12]);
    }
    let thin: Vec<_> = seen.iter().filter(|(_, n)| **n < MIN_PER_FAMILY).collect();
    if !thin.is_empty() {
        failures.push(format!(
            "families under {MIN_PER_FAMILY} cases (the generator stopped covering them, or the argv text changed): {thin:?}"
        ));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
