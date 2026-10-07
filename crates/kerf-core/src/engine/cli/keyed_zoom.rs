//! A keyframed zoom, pinned against rendered pixels (`#[ignore]`d: they drive the real
//! `ffmpeg`, on 6.1 and 9.0 alike).
//!
//! `scale ... eval=frame` is the one filter that changes a picture's size from frame to
//! frame, and almost nothing downstream of it follows: whatever sits after it is
//! pinned to the size of the *first* frame, so a zoom that never shows, an opacity ramp
//! that runs at the first frame's size and a rotation box that does not move all
//! passed every string-level test. These render a lossless 160x120 red field with a
//! blue box at its centre over a green frame and measure, on **every output frame**,
//! where the picture is and how big (the red and the blue together, the blue alone
//! being what survives a zoom past the edge of the frame) against what
//! [`Clip::transform_at`] says for that frame's time, for every combination of the
//! zoom with the other animated channels and with crop, mask, effects, grade, both
//! fits, a still, speed and reverse, at every frame rate, and in the playback
//! stream. Each frame is also compared with the scrubbed still of the same moment,
//! the picture the editor is looking at while cutting.
//!
//! The same harness holds the neighbouring graph bugs to their pictures: how soft a blur's edge
//! is at each zoom in the still and in the file (`a_moving_zoom_blurs_the_still_and_the_export_alike`),
//! a picture zoomed to a fraction of a pixel, HLG footage whose fit is an odd size, and a
//! transparent PNG / FFV1 source over a track below.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored keyed_zoom`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::*;
use crate::clip_timing::ffmpeg_frame_time;
use crate::engine::test_support::{image_stream, make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded};
use crate::model::{Asset, Clip, Keyframe, Mask, MaskShape, Transition, TransitionKind, VideoEffect};

/// The export frame.
const CW: u32 = 320;
const CH: u32 = 180;
/// The source picture: 4:3, so that `Contain` pillarboxes it and `Cover` crops it, a
/// red field with a blue box (`BOX`, in source pixels) dead centre. Every edge is even,
/// so 4:2:0 chroma puts them exactly where they are drawn.
const SW: u32 = 160;
const SH: u32 = 120;
const BOX: [f64; 4] = [60.0, 44.0, 100.0, 76.0];
/// The base the keyed clip is composited over.
const GREEN: [i32; 3] = [0, 128, 0];

/// `(the text the graph carries, numerator, denominator)`, as in `rendered.rs`.
type Rate = (&'static str, u32, u32);
const R30: Rate = ("30", 30, 1);
const RATES: [Rate; 5] = [("24", 24, 1), ("25", 25, 1), ("29.97", 2997, 100), R30, ("60", 60, 1)];

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-keyed-zoom-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn ffmpeg(args: &[String]) {
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success(), "ffmpeg {args:?}");
}

fn s(v: &str) -> String {
    v.to_string()
}

/// The two-tone picture as a lavfi source.
fn picture(fps: &str, secs: f64) -> String {
    let [x0, y0, x1, y1] = BOX;
    format!(
        "color=c=red:s={SW}x{SH}:r={fps}:d={secs},drawbox=x={x0}:y={y0}:w={}:h={}:color=blue:t=fill",
        x1 - x0,
        y1 - y0
    )
}

/// A lossless `secs`-long two-tone clip at `fps`.
fn two_tone(dir: &Path, fps: &str, secs: f64) -> Asset {
    let path = dir.join(format!("two-tone-{fps}-{secs}.mp4"));
    if !path.exists() {
        ffmpeg(&[
            s("-f"),
            s("lavfi"),
            s("-i"),
            picture(fps, secs),
            s("-c:v"),
            s("libx264"),
            s("-crf"),
            s("0"),
            s("-g"),
            s("1"),
            s("-pix_fmt"),
            s("yuv420p"),
            path.to_string_lossy().into_owned(),
        ]);
    }
    let mut asset = test_asset(vec![video_stream(SW, SH, fps.parse().unwrap())]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = secs;
    asset
}

/// The same picture as a still image.
fn two_tone_still(dir: &Path) -> Asset {
    let path = dir.join("two-tone.png");
    if !path.exists() {
        ffmpeg(&[
            s("-f"),
            s("lavfi"),
            s("-i"),
            picture("1", 1.0),
            s("-frames:v"),
            s("1"),
            path.to_string_lossy().into_owned(),
        ]);
    }
    let mut asset = test_asset(vec![image_stream(SW, SH)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = crate::model::DEFAULT_IMAGE_DURATION;
    asset
}

/// The picture with its left half cut out (alpha 0), as an `rgba` filter graph.
fn picture_with_clear_left(fps: &str, secs: f64) -> String {
    format!(
        "{},format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(lt(X,{}),0,255)'",
        picture(fps, secs),
        SW / 2
    )
}

/// The transparent-left-half picture as a PNG sticker: `rgba`, which the probe records.
fn two_tone_alpha_still(dir: &Path) -> Asset {
    let path = dir.join("two-tone-alpha.png");
    if !path.exists() {
        ffmpeg(&[
            s("-f"),
            s("lavfi"),
            s("-i"),
            picture_with_clear_left("1", 1.0),
            s("-frames:v"),
            s("1"),
            path.to_string_lossy().into_owned(),
        ]);
    }
    let mut asset = test_asset(vec![image_stream(SW, SH)]);
    asset.streams[0].pix_fmt = Some("rgba".into());
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = crate::model::DEFAULT_IMAGE_DURATION;
    asset
}

/// The transparent-left-half picture as a lossless FFV1 clip in `yuva420p`.
fn two_tone_alpha_video(dir: &Path, fps: &str, secs: f64) -> Asset {
    let path = dir.join(format!("two-tone-alpha-{fps}-{secs}.mkv"));
    if !path.exists() {
        ffmpeg(&[
            s("-f"),
            s("lavfi"),
            s("-i"),
            picture_with_clear_left(fps, secs),
            s("-c:v"),
            s("ffv1"),
            s("-pix_fmt"),
            s("yuva420p"),
            path.to_string_lossy().into_owned(),
        ]);
    }
    let mut asset = test_asset(vec![video_stream(SW, SH, fps.parse().unwrap())]);
    asset.streams[0].codec = "ffv1".into();
    asset.streams[0].pix_fmt = Some("yuva420p".into());
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = secs;
    asset
}

/// The two-tone picture as 10-bit HLG BT.2020 footage (the way an iPhone records it), or
/// `None` when this ffmpeg cannot make one (no `zscale` or no `libx265`). FFmpeg 9 wants the
/// source colourspace stated in the graph and FFmpeg 4 wants it on the file, hence both.
fn two_tone_hlg(dir: &Path, fps: &str, secs: f64) -> Option<Asset> {
    let path = dir.join(format!("two-tone-hlg-{fps}-{secs}.mp4"));
    if !path.exists() {
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!(
                "{},format=yuv420p,setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv",
                picture(fps, secs)
            ))
            .args([
                "-vf",
                "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt2020:t=arib-std-b67:m=bt2020nc:r=tv,format=yuv420p10le",
                "-c:v",
                "libx265",
                "-x265-params",
                "log-level=error:lossless=1",
                "-color_primaries",
                "bt2020",
                "-color_trc",
                "arib-std-b67",
                "-colorspace",
                "bt2020nc",
            ])
            .arg(&path)
            .status_bounded()
            .ok()?;
        if !made.success() {
            let _ = std::fs::remove_file(&path);
            return None;
        }
    }
    let mut asset = test_asset(vec![video_stream(SW, SH, fps.parse().unwrap())]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = secs;
    for stream in &mut asset.streams {
        stream.color_transfer = Some("arib-std-b67".into());
        stream.color_primaries = Some("bt2020".into());
    }
    Some(asset)
}

/// A lossless green frame-sized clip, the base.
fn green(dir: &Path, fps: &str, secs: f64) -> Asset {
    let path = dir.join(format!("green-{fps}-{secs}.mp4"));
    if !path.exists() {
        ffmpeg(&[
            s("-f"),
            s("lavfi"),
            s("-i"),
            format!("color=c=green:s={CW}x{CH}:r={fps}:d={secs}"),
            s("-c:v"),
            s("libx264"),
            s("-crf"),
            s("0"),
            s("-g"),
            s("1"),
            s("-pix_fmt"),
            s("yuv420p"),
            path.to_string_lossy().into_owned(),
        ]);
    }
    let mut asset = test_asset(vec![video_stream(CW, CH, fps.parse().unwrap())]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = secs;
    asset
}

fn options(rate: Rate, fit: Fit) -> ExportOptions {
    ExportOptions {
        resolution: Some((CW, CH)),
        fps: Some(rate.0.parse().unwrap()),
        fit,
        ..ExportOptions::default()
    }
}

fn frames_of(raw: &[u8]) -> Vec<Vec<u8>> {
    raw.chunks((CW * CH * 3) as usize).map(<[u8]>::to_vec).collect()
}

/// The export graph of `timeline`, one `CW`x`CH` rgb24 frame per output frame.
fn export_frames(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, dir: &Path, tag: &str) -> Vec<Vec<u8>> {
    let out = dir.join(format!("{tag}.rgb"));
    let mut args = build_export_args(timeline, assets, "unused.mkv", opts).unwrap();
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
        .map(s),
    );
    args.push(out.to_string_lossy().into_owned());
    if std::env::var_os("KERF_ZOOM_KEEP").is_some() {
        std::fs::write(dir.join(format!("{tag}.args")), args.join(" ")).unwrap();
    }
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    frames_of(&std::fs::read(&out).unwrap())
}

/// The scrubbed still of `timeline` at `t`: what the editor shows while cutting, and
/// the picture the export has to agree with.
fn still_frame(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, t: f64) -> Vec<u8> {
    let args = build_still_args(timeline, assets, opts, t, CW, None, &StillOutput::RgbPipe).unwrap();
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout.len(), (CW * CH * 3) as usize, "a still is one frame");
    run.stdout
}

// ---- measuring a frame ------------------------------------------------------

/// A rectangle in frame pixels, edges rather than pixel indices (`x1`, `y1` exclusive).
#[derive(Clone, Copy, Debug)]
struct Rect {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Rect {
    fn clipped(self) -> Option<Rect> {
        let r = Rect {
            x0: self.x0.max(0.0),
            y0: self.y0.max(0.0),
            x1: self.x1.min(f64::from(CW)),
            y1: self.y1.min(f64::from(CH)),
        };
        (r.x1 > r.x0 && r.y1 > r.y0).then_some(r)
    }

    /// The largest disagreement between two rectangles' edges.
    fn off_by(self, other: Rect) -> f64 {
        [self.x0 - other.x0, self.y0 - other.y0, self.x1 - other.x1, self.y1 - other.y1]
            .iter()
            .fold(0.0, |m: f64, d| m.max(d.abs()))
    }

    fn width(self) -> f64 {
        self.x1 - self.x0
    }
}

fn px(frame: &[u8], x: u32, y: u32) -> [i32; 3] {
    let i = ((y * CW + x) * 3) as usize;
    [i32::from(frame[i]), i32::from(frame[i + 1]), i32::from(frame[i + 2])]
}

/// Anything that is not the green base.
fn is_picture(p: [i32; 3]) -> bool {
    p.iter().zip(GREEN).map(|(a, b)| (a - b).abs()).max().unwrap() > 40
}

/// The blue box, at any opacity over green: blue above red is the half-way point of an
/// edge between the two (both fade by the same factor), and blue above green keeps the
/// dim green the chroma subsampling leaves on the corner of a rotated edge out of it.
fn is_blue(p: [i32; 3]) -> bool {
    p[2] > p[0] + 4 && p[2] > p[1]
}

fn bbox(frame: &[u8], hit: fn([i32; 3]) -> bool) -> Option<Rect> {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    let mut any = false;
    for y in 0..CH {
        for x in 0..CW {
            if hit(px(frame, x, y)) {
                any = true;
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    any.then(|| Rect {
        x0: f64::from(x0),
        y0: f64::from(y0),
        x1: f64::from(x1),
        y1: f64::from(y1),
    })
}

/// How opaque the blue box reads: its blue channel over the middle of the box as found.
fn opacity_at(frame: &[u8], blue: Rect) -> f64 {
    let (cx, cy) = (((blue.x0 + blue.x1) / 2.0) as u32, ((blue.y0 + blue.y1) / 2.0) as u32);
    let mut sum = 0.0;
    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        sum += f64::from(px(frame, (cx + dx).min(CW - 1), (cy + dy).min(CH - 1))[2]);
    }
    sum / 4.0 / 255.0
}

// ---- what `transform_at` says -------------------------------------------------

/// Where the picture, the blue box and the opacity are meant to be at one moment.
struct Expected {
    picture: Option<Rect>,
    blue: Option<Rect>,
    opacity: f64,
}

/// The geometry of `clip` at `local` seconds, from `Clip::transform_at` alone: crop the
/// source, fit it into the frame, zoom it about its centre, cut it with the mask,
/// rotate it about its centre, put that centre at the frame's centre plus the offset.
fn expected(clip: &Clip, local: f64, fit: Fit, clear_left: bool) -> Expected {
    let tf = clip.transform_at(local);
    let (sw, sh) = (f64::from(SW), f64::from(SH));
    let (cw, ch) = (f64::from(CW), f64::from(CH));
    let kept = [
        tf.crop_left * sw,
        tf.crop_top * sh,
        (1.0 - tf.crop_right) * sw,
        (1.0 - tf.crop_bottom) * sh,
    ];
    let (kw, kh) = (kept[2] - kept[0], kept[3] - kept[1]);
    let fit_k = match fit {
        Fit::Contain => (cw / kw).min(ch / kh),
        Fit::Cover => (cw / kw).max(ch / kh),
    };
    // `Cover` cuts the overshoot before the zoom, so the picture is the frame, zoomed.
    let (pw, ph) = match fit {
        Fit::Contain => (kw * fit_k * tf.scale, kh * fit_k * tf.scale),
        Fit::Cover => (cw * tf.scale, ch * tf.scale),
    };
    let k = fit_k * tf.scale;
    let theta = tf.rotation.to_radians();
    let centre = (cw / 2.0 + tf.pos_x * cw, ch / 2.0 + tf.pos_y * ch);
    // A rectangle in picture space (origin at the picture's centre, y down) to the
    // frame: rotated clockwise about the origin, moved to the picture's centre, cut to
    // the frame, and boxed. The cut is of the rotated shape, not of its bounding box:
    // a corner outside the frame does not stretch what is drawn inside it.
    let place = |r: [f64; 4]| {
        let corners = [(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])];
        let mut poly: Vec<(f64, f64)> = corners
            .iter()
            .map(|&(x, y)| {
                (
                    centre.0 + x * theta.cos() - y * theta.sin(),
                    centre.1 + x * theta.sin() + y * theta.cos(),
                )
            })
            .collect();
        // Sutherland-Hodgman against the four edges of the frame.
        // (axis, sign, limit): the half-plane `sign * p[axis] >= limit`.
        let edges = [
            (0, 1.0, 0.0),
            (0, -1.0, -f64::from(CW)),
            (1, 1.0, 0.0),
            (1, -1.0, -f64::from(CH)),
        ];
        for (axis, sign, limit) in edges {
            let coord = |p: &(f64, f64)| sign * if axis == 0 { p.0 } else { p.1 };
            let input = std::mem::take(&mut poly);
            for (i, a) in input.iter().enumerate() {
                let b = &input[(i + 1) % input.len()];
                let (ca, cb) = (coord(a) - limit, coord(b) - limit);
                if ca >= 0.0 {
                    poly.push(*a);
                }
                if (ca >= 0.0) != (cb >= 0.0) {
                    let f = ca / (ca - cb);
                    poly.push((a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f));
                }
            }
            if poly.is_empty() {
                return None;
            }
        }
        let fold = |pick: fn(&(f64, f64)) -> f64, f: fn(f64, f64) -> f64, init: f64| poly.iter().map(pick).fold(init, f);
        Some(Rect {
            x0: fold(|p| p.0, f64::min, f64::MAX),
            y0: fold(|p| p.1, f64::min, f64::MAX),
            x1: fold(|p| p.0, f64::max, f64::MIN),
            y1: fold(|p| p.1, f64::max, f64::MIN),
        })
        .and_then(Rect::clipped)
    };
    let mut visible = [-pw / 2.0, -ph / 2.0, pw / 2.0, ph / 2.0];
    if let Some(m) = clip.mask {
        let m = m.normalized();
        visible = [
            -pw / 2.0 + (m.x - m.width / 2.0) * pw,
            -ph / 2.0 + (m.y - m.height / 2.0) * ph,
            -pw / 2.0 + (m.x + m.width / 2.0) * pw,
            -ph / 2.0 + (m.y + m.height / 2.0) * ph,
        ];
    }
    let (ccx, ccy) = ((kept[0] + kept[2]) / 2.0, (kept[1] + kept[3]) / 2.0);
    // A source with its left half cut out shows nothing left of the cut.
    if clear_left {
        visible[0] = visible[0].max((f64::from(SW / 2) - ccx) * k);
    }
    let mut blue = [(BOX[0] - ccx) * k, (BOX[1] - ccy) * k, (BOX[2] - ccx) * k, (BOX[3] - ccy) * k];
    // ... and the box shows only what is inside it.
    blue = [
        blue[0].max(visible[0]),
        blue[1].max(visible[1]),
        blue[2].min(visible[2]),
        blue[3].min(visible[3]),
    ];
    let nothing = |r: [f64; 4]| r[2] <= r[0] || r[3] <= r[1];
    Expected {
        picture: (!nothing(visible)).then(|| place(visible)).flatten(),
        blue: (!nothing(blue)).then(|| place(blue)).flatten(),
        opacity: tf.opacity,
    }
}

// ---- a case -----------------------------------------------------------------

/// The keyframes every case zooms with: in past the frame's edge, then out to a
/// quarter of it — both directions, so a size pinned to the first frame (small) and
/// one pinned to the biggest are both caught.
fn zoom_keys() -> Vec<Keyframe> {
    let key = |time, scale| Keyframe {
        time,
        scale,
        pos_x: 0.0,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
    };
    vec![key(0.0, 0.35), key(0.9, 1.6), key(1.8, 0.5)]
}

/// `zoom_keys` plus the other animated channels, each on its own switch.
fn keys(position: bool, rotation: bool, opacity: bool) -> Vec<Keyframe> {
    let mut k = zoom_keys();
    if position {
        for (key, x) in k.iter_mut().zip([-0.25, 0.25, -0.1]) {
            key.pos_x = x;
            key.pos_y = x / 2.0;
        }
    }
    if rotation {
        for (key, r) in k.iter_mut().zip([0.0, 35.0, -20.0]) {
            key.rotation = r;
        }
    }
    if opacity {
        for (key, o) in k.iter_mut().zip([1.0, 0.6, 1.0]) {
            key.opacity = o;
        }
    }
    k
}

/// What the keyed clip is cut from.
#[derive(Clone, Copy)]
enum Source {
    /// The two-tone clip at this frame rate (the text the graph carries).
    Video(&'static str),
    /// The same picture as a still image.
    Still,
    /// HLG BT.2020 footage at this rate, tone-mapped to SDR by the chain.
    Hlg(&'static str),
    /// The picture with its **left half transparent**, as a PNG sticker (`rgba`).
    AlphaStill,
    /// The same with a transparent left half, as a lossless `yuva420p` clip at this rate.
    AlphaVideo(&'static str),
}

impl Source {
    /// The left half of the picture is cut out.
    fn clear_left(self) -> bool {
        matches!(self, Source::AlphaStill | Source::AlphaVideo(_))
    }
}

/// One keyed clip over the green base, and what to hold it to.
struct Case {
    name: String,
    keys: Vec<Keyframe>,
    /// Static decoration of the clip: crop, mask, effects, grade, speed.
    decorate: Box<dyn Fn(&mut Clip)>,
    fit: Fit,
    rate: Rate,
    source: Source,
    /// Seconds on the timeline the clip is shown for, and where it starts.
    shown: f64,
    start: f64,
    /// Whether the blue box survives the case's effects and can be measured.
    blue: bool,
    /// Whether the picture's opacity is the keyed one (a fade darkens the colour instead).
    opacity: bool,
    /// The most an edge may be off by, in pixels.
    tolerance: f64,
    /// Export only this span of the timeline (`ExportOptions::range`).
    range: Option<(f64, f64)>,
    /// A clip of the same footage ahead of the keyed one, and the transition the keyed
    /// clip comes in with; its frames are measured once the transition is over.
    transition: Option<Transition>,
}

impl Case {
    fn new(name: impl Into<String>, keys: Vec<Keyframe>) -> Self {
        Case {
            name: name.into(),
            keys,
            decorate: Box::new(|_| {}),
            fit: Fit::Contain,
            rate: R30,
            source: Source::Video("30"),
            shown: 2.4,
            start: 0.5,
            blue: true,
            opacity: true,
            tolerance: 3.5,
            range: None,
            transition: None,
        }
    }

    fn decorated(mut self, f: impl Fn(&mut Clip) + 'static) -> Self {
        self.decorate = Box::new(f);
        self
    }

    fn fit(mut self, fit: Fit) -> Self {
        self.fit = fit;
        self
    }

    fn rotated(mut self) -> Self {
        // A rotated edge is soft and its bounding box one more pixel of antialiasing out.
        self.tolerance = self.tolerance.max(4.5);
        self
    }

    fn tolerance(mut self, px: f64) -> Self {
        self.tolerance = px;
        self
    }

    fn without_blue(mut self) -> Self {
        self.blue = false;
        self
    }

    fn without_opacity(mut self) -> Self {
        self.opacity = false;
        self
    }

    fn from(mut self, source: Source) -> Self {
        self.source = source;
        self
    }

    fn ranged(mut self, start: f64, end: f64) -> Self {
        self.range = Some((start, end));
        self
    }

    fn coming_in_with(mut self, kind: TransitionKind, duration: f64) -> Self {
        self.transition = Some(Transition { kind, duration });
        self.start = 1.5;
        self
    }

    /// The timeline: the green base, and the keyed clip on the track above.
    fn timeline(&self, dir: &Path) -> (Timeline, Vec<Asset>) {
        let fps = self.rate.0;
        let total = self.start + self.shown + 0.5;
        let base = green(dir, fps, total + 1.0);
        // A faster clip needs more source for the same time on the timeline.
        let mut probe = Clip::new(base.id, 0.0, 1.0, 0.0);
        (self.decorate)(&mut probe);
        let span = self.shown * probe.speed_mag();
        let (src, speed_span) = match self.source {
            Source::Video(sf) => (two_tone(dir, sf, span + 1.0), span),
            Source::Hlg(sf) => (
                two_tone_hlg(dir, sf, span + 1.0).expect("the test checked that an HLG clip can be made"),
                span,
            ),
            Source::Still => (two_tone_still(dir), self.shown),
            Source::AlphaStill => (two_tone_alpha_still(dir), self.shown),
            Source::AlphaVideo(sf) => (two_tone_alpha_video(dir, sf, span + 1.0), span),
        };
        let mut clip = make_clip(src.id, 0.0, speed_span, self.start);
        (self.decorate)(&mut clip);
        clip.keyframes = self.keys.clone();
        clip.transition_in = self.transition;
        let mut clips = vec![clip];
        if self.transition.is_some() {
            // The outgoing clip: the same footage, untouched, ending where the keyed one starts.
            clips.insert(0, make_clip(src.id, 0.0, self.start - 0.5, 0.5));
        }
        let timeline = timeline_of(vec![
            video_track(vec![make_clip(base.id, 0.0, total, 0.0)]),
            video_track(clips),
        ]);
        (timeline, vec![base, src])
    }
}

/// What a run measured, for the report.
#[derive(Default)]
struct Worst {
    against_transform: f64,
    against_still: f64,
    frames: usize,
}

/// Render `case` and hold every output frame of the keyed clip to `transform_at` and
/// every eighth to the scrubbed still.
fn check(dir: &Path, case: &Case) -> Worst {
    let (timeline, assets) = case.timeline(dir);
    let mut opts = options(case.rate, case.fit);
    opts.range = case.range.map(|(start, end)| crate::model::TimeRange { start, end });
    let tag = format!("{}-{}", case.name.replace([' ', '+'], "-"), case.rate.0);
    let frames = export_frames(&timeline, &assets, &opts, dir, &tag);
    let clip = timeline.tracks[1].clips.last().unwrap();
    let (_, num, den) = case.rate;
    let fps = f64::from(num) / f64::from(den);
    let margin = 1.5 / fps;
    let end = clip.timeline_start + clip.duration();
    // A range export starts its output at `range.start`; the keyed clip still lives at
    // its own timeline time.
    let offset = case.range.map_or(0.0, |r| r.0);
    // A transition mixes the outgoing clip in until it is over.
    let settled = clip.timeline_start + case.transition.map_or(0.0, |t| t.duration) + 0.05;
    let mut worst = Worst::default();
    for (k, frame) in frames.iter().enumerate() {
        let t = ffmpeg_frame_time(k as u64, num, den) + offset;
        if t < settled.max(clip.timeline_start) + margin || t > end - margin {
            continue;
        }
        let want = expected(clip, t - clip.timeline_start, case.fit, case.source.clear_left());
        let got_picture = bbox(frame, is_picture);
        let got_blue = bbox(frame, is_blue);
        let at = format!("{}: frame {k} (t = {t:.4}, local {:.4})", case.name, t - clip.timeline_start);
        match (want.picture, got_picture) {
            (Some(w), Some(g)) => {
                let off = w.off_by(g);
                worst.against_transform = worst.against_transform.max(off);
                assert!(
                    off <= case.tolerance,
                    "{at}: the picture is {g:?}, transform_at puts it at {w:?}"
                );
            }
            (None, None) => {}
            (w, g) => panic!("{at}: the picture is {g:?}, transform_at says {w:?}"),
        }
        if case.blue {
            match (want.blue, got_blue) {
                (Some(w), Some(g)) => {
                    let off = w.off_by(g);
                    worst.against_transform = worst.against_transform.max(off);
                    assert!(
                        off <= case.tolerance,
                        "{at}: the blue box is {g:?}, transform_at puts it at {w:?}"
                    );
                    let opacity = opacity_at(frame, g);
                    assert!(
                        !case.opacity || (opacity - want.opacity).abs() <= 0.08,
                        "{at}: the picture reads {opacity:.3} opaque, transform_at says {:.3}",
                        want.opacity
                    );
                }
                (None, None) => {}
                (w, g) => panic!("{at}: the blue box is {g:?}, transform_at says {w:?}"),
            }
        }
        worst.frames += 1;
        // The scrubbed still of the same moment shows the same picture.
        if worst.frames % 8 == 1 {
            let still = still_frame(&timeline, &assets, &opts, t);
            let (sp, sb) = (bbox(&still, is_picture), bbox(&still, is_blue));
            match (got_picture, sp) {
                (Some(g), Some(s)) => {
                    let off = g.off_by(s);
                    worst.against_still = worst.against_still.max(off);
                    verbose(
                        off > 3.0,
                        &format!("{at}: picture: export {g:?}, still {s:?}, transform_at {:?}", want.picture),
                    );
                    assert!(
                        off <= case.tolerance,
                        "{at}: the export's picture is {g:?}, the still's {s:?}"
                    );
                }
                (None, None) => {}
                (g, s) => panic!("{at}: the export's picture is {g:?}, the still's {s:?}"),
            }
            if case.blue {
                if let (Some(g), Some(s)) = (got_blue, sb) {
                    let off = g.off_by(s);
                    worst.against_still = worst.against_still.max(off);
                    verbose(
                        off > 3.0,
                        &format!("{at}: blue: export {g:?}, still {s:?}, transform_at {:?}", want.blue),
                    );
                    assert!(
                        off <= case.tolerance,
                        "{at}: the export's blue box is {g:?}, the still's {s:?}"
                    );
                }
            }
        }
    }
    assert!(worst.frames >= 24, "{}: only {} frames measured", case.name, worst.frames);
    worst
}

fn run_all(tag: &str, cases: Vec<Case>) {
    let dir = scratch(tag);
    run_in(&dir, cases);
    cleanup(&dir);
}

fn run_in(dir: &Path, cases: Vec<Case>) {
    let mut report = Vec::new();
    for case in &cases {
        let w = check(dir, case);
        report.push(format!(
            "{:<44} {:>3} frames, worst {:.1}px vs transform_at, {:.1}px vs the still",
            case.name, w.frames, w.against_transform, w.against_still
        ));
    }
    report_lines(&report);
}

/// Remove a test's scratch directory, unless `KERF_ZOOM_KEEP` asks to look at what it rendered.
fn cleanup(dir: &Path) {
    if std::env::var_os("KERF_ZOOM_KEEP").is_none() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A diagnostic line under `KERF_ZOOM_VERBOSE`, for the measurements worth a second look.
#[allow(clippy::print_stderr)]
fn verbose(interesting: bool, line: &str) {
    if interesting && std::env::var_os("KERF_ZOOM_VERBOSE").is_some() {
        eprintln!("{line}");
    }
}

/// The first line of `ffmpeg -version`: which build a report was measured on.
fn ffmpeg_version() -> String {
    let out = command(&ffmpeg_bin()).arg("-version").output().expect("run ffmpeg");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

#[allow(clippy::print_stderr)]
fn report_lines(lines: &[String]) {
    eprintln!("[{}]", ffmpeg_version());
    for l in lines {
        eprintln!("{l}");
    }
}

// ---- the tests --------------------------------------------------------------

/// The zoom with each of the other animated channels, singly and all together. These
/// are the combinations that were pinned: a zoom alone ended in a fixed-size format
/// converter, keyed opacity ran its `geq` at the first frame's size, and a keyed
/// rotation rotated a box that never grew.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn keyed_zoom_animates_with_every_other_animated_channel() {
    run_all(
        "channels",
        vec![
            Case::new("scale only", keys(false, false, false)),
            Case::new("scale + position", keys(true, false, false)),
            Case::new("scale + opacity", keys(false, false, true)),
            Case::new("scale + rotation", keys(false, true, false)).rotated(),
            Case::new("scale + position + opacity", keys(true, false, true)),
            Case::new("scale + rotation + opacity", keys(false, true, true)).rotated(),
            Case::new("scale + position + rotation + opacity", keys(true, true, true)).rotated(),
        ],
    );
}

/// The zoom with what is applied to the picture itself: crop, a mask, effects, a grade.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn keyed_zoom_animates_with_crop_mask_effects_and_grade() {
    let mask = Mask {
        shape: MaskShape::Rect,
        x: 0.45,
        y: 0.5,
        width: 0.5,
        height: 0.6,
        feather: 0.0,
        inverted: false,
    };
    run_all(
        "decorated",
        vec![
            Case::new("scale + crop", keys(true, false, false)).decorated(|c| {
                c.transform.crop_left = 0.15;
                c.transform.crop_right = 0.05;
                c.transform.crop_top = 0.1;
            }),
            Case::new("scale + mask", keys(false, false, false))
                .decorated(move |c| c.mask = Some(mask))
                .tolerance(4.0),
            Case::new("scale + mask + opacity + position", keys(true, false, true))
                .decorated(move |c| c.mask = Some(mask))
                .tolerance(4.0),
            Case::new("scale + mask + rotation", keys(false, true, false))
                .decorated(move |c| c.mask = Some(mask))
                .rotated(),
            Case::new("scale + blur + sharpen", keys(true, false, false))
                .decorated(|c| {
                    c.effects = vec![VideoEffect::Blur { sigma: 2.0 }, VideoEffect::Sharpen { amount: 0.8 }];
                })
                .tolerance(4.0),
            Case::new("scale + vignette + chroma key", keys(false, false, true))
                .decorated(|c| {
                    c.effects = vec![
                        VideoEffect::Vignette,
                        VideoEffect::ChromaKey {
                            color: "blue".into(),
                            similarity: 0.3,
                            blend: 0.1,
                        },
                    ];
                })
                .without_blue(),
            Case::new("scale + colour grade", keys(true, false, false)).decorated(|c| {
                c.color.brightness = 0.05;
                c.color.contrast = 1.1;
                c.color.saturation = 1.2;
            }),
            Case::new("scale + fades", keys(true, false, false))
                .decorated(|c| {
                    c.fade_in = 0.3;
                    c.fade_out = 0.3;
                })
                .without_opacity(),
            Case::new("everything", keys(true, true, true))
                .decorated(move |c| {
                    c.transform.crop_left = 0.1;
                    c.transform.crop_bottom = 0.1;
                    c.mask = Some(mask);
                    c.effects = vec![VideoEffect::Blur { sigma: 1.5 }];
                    c.color.contrast = 1.1;
                })
                .rotated()
                .tolerance(5.0),
        ],
    );
}

/// A rotation that moves while the scale holds still. Not a zoom, and not the chain the
/// zoom restructures, but the same bug in the same line: `rotate=...:fillcolor=none`
/// means "leave the corners alone", so a buffer `rotate` reuses still holds every earlier
/// pose and the picture swells into the union of all of them (on 6.1 and 9.0 alike). A
/// constant angle never showed it, because every frame rewrites the same footprint.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn keyed_rotation_alone_leaves_no_earlier_pose_behind() {
    let mut turning = zoom_keys();
    for (key, (scale, angle)) in turning.iter_mut().zip([(0.8, 0.0), (0.8, 40.0), (0.8, -25.0)]) {
        key.scale = scale;
        key.rotation = angle;
    }
    assert!(!Clip::new(uuid::Uuid::nil(), 0.0, 1.0, 0.0).zoom_animated());
    run_all(
        "turning",
        vec![
            Case::new("rotation only, the scale holds", turning.clone()).rotated(),
            Case::new("rotation + opacity, the scale holds", {
                let mut k = turning.clone();
                for (key, o) in k.iter_mut().zip([1.0, 0.6, 1.0]) {
                    key.opacity = o;
                }
                k
            })
            .rotated(),
        ],
    );
}

/// A keyed zoom coming in on a transition: the dissolve and the dips are alpha and
/// colour ramps at the clip's constant pre-zoom size, and a slide or a push moves the
/// overlay by an offset that is added to the keyed position. Once the transition is
/// over the clip is where `transform_at` says.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn keyed_zoom_settles_where_transform_at_says_after_every_transition() {
    let cases = TransitionKind::ALL
        .iter()
        .map(|&kind| Case::new(format!("{kind:?} into a zoom"), keys(true, false, true)).coming_in_with(kind, 0.5))
        .collect();
    run_all("transitions", cases);
}

/// HLG footage tone-maps to SDR in the chain (`zscale` and `tonemap`, which also read the
/// frame size once, when the graph is configured). They ran behind the zoom, pinned like the
/// rest; they run ahead of it now, at the constant fit size and before any colour work.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn keyed_zoom_works_on_hdr_footage() {
    let dir = scratch("hdr");
    if two_tone_hlg(&dir, "30", 3.4).is_none() {
        report_lines(&["skipped: this ffmpeg cannot make the HLG test clip".to_string()]);
        cleanup(&dir);
        return;
    }
    run_in(
        &dir,
        vec![
            // Tone-mapping moves the colours, so the opacity and the edge of the blue box
            // (where the tone-mapped blue meets the tone-mapped red) read differently.
            Case::new("scale + position + opacity, HLG", keys(true, false, true))
                .from(Source::Hlg("30"))
                .without_opacity()
                .tolerance(6.0),
            Case::new("scale + rotation, HLG", keys(false, true, false))
                .from(Source::Hlg("30"))
                .without_opacity()
                .tolerance(6.0),
        ],
    );
    cleanup(&dir);
}

/// A range export slices the timeline (boundary clips retrimmed, keyframes resampled to
/// the new origin) and the zoom must follow the shifted clock.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn keyed_zoom_follows_a_range_export() {
    run_all(
        "range",
        vec![
            Case::new("range 1.1s to 2.7s", keys(true, true, true))
                .rotated()
                .ranged(1.1, 2.7),
            Case::new("range 0.0s to 1.7s", keys(true, false, false)).ranged(0.0, 1.7),
        ],
    );
}

/// Both fits, a still image and a retimed or reversed clip: the keys are clip-local
/// timeline seconds, so speed and reverse must not move them.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn keyed_zoom_animates_under_either_fit_for_stills_and_retimed_clips() {
    let mut still = Case::new("still image + position", keys(true, false, false));
    still.source = Source::Still;
    let mut still_rot = Case::new("still image + rotation + opacity", keys(false, true, true)).rotated();
    still_rot.source = Source::Still;
    run_all(
        "fits",
        vec![
            Case::new("cover", keys(false, false, false)).fit(Fit::Cover),
            Case::new("cover + position + rotation", keys(true, true, false))
                .fit(Fit::Cover)
                .rotated(),
            Case::new("cover + crop + opacity", keys(false, false, true))
                .fit(Fit::Cover)
                .decorated(|c| c.transform.crop_left = 0.2),
            still,
            still_rot,
            Case::new("speed 2x", keys(true, false, false)).decorated(|c| c.speed = 2.0),
            Case::new("speed 0.5x + opacity", keys(false, false, true)).decorated(|c| c.speed = 0.5),
            Case::new("reverse + rotation", keys(false, true, false))
                .decorated(|c| c.speed = -1.0)
                .rotated(),
            Case::new("reverse 0.5x + position + opacity", keys(true, false, true)).decorated(|c| c.speed = -0.5),
        ],
    );
}

/// The frame times the zoom is evaluated at are the output's, at every export rate —
/// including the ones where FFmpeg's own time for a frame is not `k / fps`, and a clip
/// that starts between two frames.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn keyed_zoom_is_evaluated_at_the_output_frames_of_every_rate() {
    let cases = RATES
        .iter()
        .map(|&rate| {
            let mut case = Case::new(format!("{} fps", rate.0), keys(true, false, true));
            case.rate = rate;
            case.source = Source::Video(rate.0);
            case.start = 0.37;
            case
        })
        .collect();
    run_all("rates", cases);
}

/// A 10 fps clip in a 30 fps export zooms on every *output* frame. The zoom used to sit
/// before `fps`, so it was read at the source frame's time and the picture grew in
/// three-frame steps: the widest run of equal sizes across the zoom-in is one frame now.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_slow_source_zooms_smoothly_on_every_output_frame() {
    let dir = scratch("slow");
    let mut case = Case::new("10 fps source in a 30 fps export", keys(true, false, false));
    case.source = Source::Video("10");
    let w = check(&dir, &case);

    // The blue box's width over the zoom-in (clip-local 0 to 0.9 s), frame by frame.
    let (timeline, assets) = case.timeline(&dir);
    let frames = export_frames(&timeline, &assets, &options(R30, Fit::Contain), &dir, "slow-widths");
    let start = (case.start * 30.0).round() as usize;
    let widths: Vec<f64> = frames[start + 2..start + 26]
        .iter()
        .map(|f| bbox(f, is_blue).expect("the blue box").width())
        .collect();
    // The widths come in even numbers (4:2:0), so a 2.8 px a frame is sometimes two
    // equal frames in a row; a source-frame step is three equal ones and then a jump of
    // three frames' growth (8 px).
    let mut run = 1;
    let mut longest = 1;
    for pair in widths.windows(2) {
        assert!(pair[1] >= pair[0], "the zoom-in never shrinks: {widths:?}");
        assert!(pair[1] - pair[0] <= 6.0, "the zoom-in grows in steps: {widths:?}");
        run = if pair[1] == pair[0] { run + 1 } else { 1 };
        longest = longest.max(run);
    }
    assert!(longest <= 2, "the blue box grows on every frame, not in steps: {widths:?}");
    assert!(widths[widths.len() - 1] - widths[0] > 40.0, "{widths:?}");
    report_lines(&[format!(
        "10 fps source, 30 fps export: {} frames, worst {:.1}px vs transform_at; blue widths {:?}",
        w.frames, w.against_transform, widths
    )]);
    cleanup(&dir);
}

/// Two layers fed from one decoded input (`split`) each zoom: the chain follows the
/// split's own output pad, so this is the label flow the other cases do not reach.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn keyed_zoom_works_on_layers_that_share_an_input() {
    let dir = scratch("shared");
    let case = Case::new("shared input", keys(true, false, false));
    let (mut timeline, assets) = case.timeline(&dir);
    // The same clip again on a third track: same asset, same window, same seek.
    let twin = timeline.tracks[1].clips[0].clone();
    timeline.tracks.push(video_track(vec![twin]));
    let opts = options(R30, Fit::Contain);
    let args = build_export_args(&timeline, &assets, "unused.mkv", &opts).unwrap();
    assert!(args.join(" ").contains("split=2"), "the two layers share one input");
    let frames = export_frames(&timeline, &assets, &opts, &dir, "shared");
    let clip = &timeline.tracks[1].clips[0];
    let mut measured = 0;
    for (k, frame) in frames.iter().enumerate() {
        let t = ffmpeg_frame_time(k as u64, 30, 1);
        if t < clip.timeline_start + 0.05 || t > clip.timeline_start + clip.duration() - 0.05 {
            continue;
        }
        let want = expected(clip, t - clip.timeline_start, Fit::Contain, false);
        let (got, want) = (bbox(frame, is_blue).expect("blue"), want.blue.expect("blue expected"));
        assert!(got.off_by(want) <= 3.5, "frame {k}: {got:?} against {want:?}");
        measured += 1;
    }
    assert!(measured > 40);
    cleanup(&dir);
}

/// Playback is the same graph, so it zooms the same way: stream a keyed clip through
/// `stream_preview`, decode the JPEGs it sends and hold each to `transform_at` at the
/// time it carries.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn the_playback_stream_zooms_too() {
    let dir = scratch("stream");
    let case = Case::new("playback", keys(true, true, true)).rotated();
    let (timeline, assets) = case.timeline(&dir);
    let clip = timeline.tracks[1].clips[0].clone();

    let mut frames: Vec<PreviewFrame> = Vec::new();
    // From a little before the keyed clip, so the stream's own slice starts mid-timeline.
    stream_preview(&timeline, &assets, 0.2, 24.0, &mut |f| {
        frames.push(f);
        frames.len() < 54
    })
    .expect("stream");
    assert_eq!(frames.len(), 54);

    let mjpeg = dir.join("stream.mjpeg");
    std::fs::write(
        &mjpeg,
        frames.iter().flat_map(|f| f.jpeg.iter().copied()).collect::<Vec<u8>>(),
    )
    .unwrap();
    let raw = dir.join("stream.rgb");
    ffmpeg(&[
        s("-f"),
        s("mjpeg"),
        s("-i"),
        mjpeg.to_string_lossy().into_owned(),
        s("-f"),
        s("rawvideo"),
        s("-pix_fmt"),
        s("rgb24"),
        raw.to_string_lossy().into_owned(),
    ]);
    let decoded = frames_of(&std::fs::read(&raw).unwrap());
    assert_eq!(decoded.len(), frames.len());

    let mut measured = 0;
    let mut worst = 0.0f64;
    for (f, pixels) in frames.iter().zip(&decoded) {
        let local = f.time - clip.timeline_start;
        if !(0.1..clip.duration() - 0.1).contains(&local) {
            continue;
        }
        let want = expected(&clip, local, Fit::Contain, false);
        let (got, want_blue) = (bbox(pixels, is_blue), want.blue);
        let at = format!("playback frame at t = {:.3} (local {local:.3})", f.time);
        match (got, want_blue) {
            (Some(g), Some(w)) => {
                worst = worst.max(g.off_by(w));
                // JPEG at the stream's quality, bilinear scaling: a little looser.
                assert!(
                    g.off_by(w) <= 5.0,
                    "{at}: the blue box is {g:?}, transform_at puts it at {w:?}"
                );
                let opacity = opacity_at(pixels, g);
                assert!(
                    (opacity - want.opacity).abs() <= 0.12,
                    "{at}: reads {opacity:.3} opaque, transform_at says {:.3}",
                    want.opacity
                );
            }
            (None, None) => {}
            (g, w) => panic!("{at}: the blue box is {g:?}, transform_at says {w:?}"),
        }
        measured += 1;
    }
    assert!(measured >= 30, "{measured} frames measured");
    report_lines(&[format!(
        "playback stream: {measured} frames, worst {worst:.1}px vs transform_at"
    )]);
    cleanup(&dir);
}

/// The 10 to 90 per cent width of the blue box's left edge, on the row through its middle: how
/// soft the edge is. Red is 0 and blue is 1, read off the two channels that tell them apart.
fn edge_width(frame: &[u8], blue: Rect) -> f64 {
    let y = ((blue.y0 + blue.y1) / 2.0) as u32;
    let blueness = |x: u32| {
        let p = px(frame, x, y);
        (f64::from(p[2] - p[0]) / 255.0 + 1.0) / 2.0
    };
    let mut x = ((blue.x0 + blue.x1) / 2.0) as u32;
    while x > 0 && blueness(x) >= 0.9 {
        x -= 1;
    }
    let at_90 = x;
    while x > 0 && blueness(x) >= 0.1 {
        x -= 1;
    }
    f64::from(at_90 - x)
}

/// A blur is part of the picture, and a moving zoom magnifies the picture: its edges are as soft
/// as the zoom is large (the chain runs the zoom last, and so does the still). The still used to
/// zoom first and blur after, so its edge was the same softness at every zoom while the export's
/// grew with it: at a zoom of 0.5 the still was twice as soft as the file, at 1.6 a third sharper.
/// Four moments of one zoom, a blur of sigma 6 (a strong one, so the edge is wide enough to
/// measure): the export's edge width tracks the zoom, and the still's is the export's.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_moving_zoom_blurs_the_still_and_the_export_alike() {
    let dir = scratch("softness");
    let case = Case::new("blur on a moving zoom", zoom_keys()).decorated(|c| {
        c.effects = vec![VideoEffect::Blur { sigma: 6.0 }];
    });
    let (timeline, assets) = case.timeline(&dir);
    let opts = options(R30, Fit::Contain);
    let frames = export_frames(&timeline, &assets, &opts, &dir, "softness");
    let clip = &timeline.tracks[1].clips[0];
    let mut widths = Vec::new();
    for local in [0.11, 0.47, 0.9, 1.5] {
        let k = ((clip.timeline_start + local) * 30.0).round() as u64;
        let t = ffmpeg_frame_time(k, 30, 1);
        let zoom = clip.transform_at(t - clip.timeline_start).scale;
        let still = still_frame(&timeline, &assets, &opts, t);
        let export = &frames[k as usize];
        let (eb, sb) = (bbox(export, is_blue).expect("blue"), bbox(&still, is_blue).expect("blue"));
        let (we, ws) = (edge_width(export, eb), edge_width(&still, sb));
        assert!(
            (we - ws).abs() <= (0.15 * we).max(2.0),
            "zoom {zoom:.2}: the export's edge is {we} px wide, the still's {ws}"
        );
        widths.push((zoom, we, ws));
    }
    // The edge grows with the zoom: 3.2 times between 0.5 and 1.6, give or take the chroma.
    let (small, large) = (widths[0], widths[2]);
    let ratio = large.1 / small.1;
    assert!((2.0..4.5).contains(&ratio), "{widths:?}");
    report_lines(&[format!(
        "blur edge widths (zoom, export px, still px): {}",
        widths
            .iter()
            .map(|(z, e, s)| format!("({z:.2}, {e}, {s})"))
            .collect::<Vec<_>>()
            .join(" ")
    )]);
    cleanup(&dir);
}

/// A picture zoomed to a fraction of a pixel is 1 px, not full size: `scale` reads a width that
/// evaluates to 0 as "unset" and keeps the input's. A scale of 0.0004 on a 240 px picture snapped
/// to 240 px, at a constant zoom and at the first frames of a keyed one. The picture here is a
/// speck (the blue box is not measurable), held to where `transform_at` puts it, in the export
/// and in the still.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_picture_zoomed_to_a_fraction_of_a_pixel_does_not_snap_to_full_size() {
    let held_then_up = |last: f64| {
        let key = |time, scale| Keyframe {
            time,
            scale,
            pos_x: 0.1,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
        };
        vec![key(0.0, 0.0004), key(0.8, 0.0004), key(1.4, last), key(2.0, last)]
    };
    run_all(
        "tiny",
        vec![
            Case::new("a constant scale of 0.0004", vec![])
                .decorated(|c| {
                    c.transform.scale = 0.0004;
                    c.transform.pos_x = 0.1;
                })
                .without_blue(),
            Case::new("keys from 0.0004 to 0.8 (a moving zoom)", held_then_up(0.8)).without_blue(),
            Case::new("keys held at 0.0004 (no zoom to move)", held_then_up(0.0004)).without_blue(),
        ],
    );
}

/// HLG footage is tone-mapped by `zscale`, which aborts the graph on a size that is not a
/// multiple of the chroma subsampling: 4:3 footage Contain-fitted into a 9:16 frame is 180 x 135,
/// and a constant zoom of 0.45 of that is 81 x 60. Rendered at a transform (no pad, so the fit's
/// size is what `zscale` sees) for a constant zoom and a moving one.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn hdr_footage_with_an_odd_fit_size_renders() {
    let (w, h) = (180u32, 320u32);
    let dir = scratch("hdr-odd");
    let Some(hlg) = two_tone_hlg(&dir, "30", 3.0) else {
        report_lines(&["skipped: this ffmpeg cannot make the HLG test clip".to_string()]);
        cleanup(&dir);
        return;
    };
    let opts = ExportOptions {
        resolution: Some((w, h)),
        fps: Some(30.0),
        ..ExportOptions::default()
    };
    // The picture's box in this frame, from the fit (a 4:3 picture is `w` wide and 3/4 of that
    // high), the zoom and the offset: where it is expected within a couple of pixels.
    let expect = |scale: f64, pos_x: f64| {
        let (pw, ph) = (f64::from(w) * scale, f64::from(w) * 0.75 * scale);
        let (cx, cy) = (f64::from(w) / 2.0 + pos_x * f64::from(w), f64::from(h) / 2.0);
        // (cut at the frame's edges, which a moved picture runs into)
        (
            (cx - pw / 2.0).max(0.0),
            (cy - ph / 2.0).max(0.0),
            (cx + pw / 2.0).min(f64::from(w)),
            (cy + ph / 2.0).min(f64::from(h)),
        )
    };
    let drawn = |frame: &[u8]| {
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 3) as usize;
                if frame[i..i + 3].iter().any(|v| *v > 60) {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
                }
            }
        }
        (f64::from(x0), f64::from(y0), f64::from(x1), f64::from(y1))
    };
    let run = |name: &str, clip: Clip| {
        let tl = timeline_of(vec![video_track(vec![clip.clone()])]);
        let out = dir.join(format!("{name}.rgb"));
        let mut args = build_export_args(&tl, std::slice::from_ref(&hlg), "unused.mkv", &opts).unwrap();
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
            .map(s),
        );
        args.push(out.to_string_lossy().into_owned());
        let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
        assert!(
            run.status.success(),
            "{name}: {}\n{}",
            args.join(" "),
            String::from_utf8_lossy(&run.stderr)
        );
        let raw = std::fs::read(&out).unwrap();
        let frames: Vec<&[u8]> = raw.chunks((w * h * 3) as usize).collect();
        assert!(frames.len() >= 60, "{name}: {} frames", frames.len());
        (tl, frames.iter().map(|f| f.to_vec()).collect::<Vec<_>>())
    };
    let near = |got: (f64, f64, f64, f64), want: (f64, f64, f64, f64), what: &str| {
        let off = [got.0 - want.0, got.1 - want.1, got.2 - want.2, got.3 - want.3]
            .iter()
            .fold(0.0f64, |m, d| m.max(d.abs()));
        assert!(off <= 4.0, "{what}: the picture is {got:?}, expected {want:?}");
    };
    // A constant zoom: 81 x 60.75, an odd size to the scaler.
    let mut constant = make_clip(hlg.id, 0.0, 2.0, 0.0);
    constant.transform.scale = 0.45;
    let (_, frames) = run("constant", constant);
    near(drawn(&frames[15]), expect(0.45, 0.0), "a constant zoom of 0.45");
    // The same size with a transform that is not a zoom (no pad either).
    let mut moved = make_clip(hlg.id, 0.0, 2.0, 0.0);
    moved.transform.pos_x = 0.05;
    let (_, frames) = run("moved", moved);
    near(drawn(&frames[15]), expect(1.0, 0.05), "a moved picture");
    // A moving zoom: the fit is even now, and the zoom runs after the tone-map.
    let mut moving = make_clip(hlg.id, 0.0, 2.0, 0.0);
    moving.keyframes = vec![
        Keyframe {
            time: 0.0,
            scale: 0.45,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
        },
        Keyframe {
            time: 1.8,
            scale: 0.9,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
        },
    ];
    let (tl, frames) = run("moving", moving);
    let clip = &tl.tracks[0].clips[0];
    for k in [3u64, 15, 30, 45] {
        let scale = clip.transform_at(ffmpeg_frame_time(k, 30, 1)).scale;
        near(
            drawn(&frames[k as usize]),
            expect(scale, 0.0),
            &format!("a moving zoom at frame {k}"),
        );
    }
    cleanup(&dir);
}

/// Footage with an alpha channel keeps it to the end of the chain. The terminal `format=` of a
/// chain with no alpha plane of its own flattened a sticker's cut-out onto black (the picture drew
/// as a whole rectangle where the still, which has no terminal format, cut it out and showed the
/// track below), and a moving zoom, whose chain ends in `yuva420p`, flipped it back: the same sticker
/// was opaque or transparent depending on whether its scale moved. The left half of the picture
/// is transparent here (a PNG and an FFV1 clip), over the green frame.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_source_with_alpha_keeps_its_cut_out_in_the_export_and_the_still() {
    let steady = |scale: f64| {
        move |c: &mut Clip| {
            c.transform.scale = scale;
            c.transform.pos_x = 0.1;
        }
    };
    run_all(
        "alpha",
        vec![
            Case::new("transparent PNG, a constant zoom", vec![])
                .from(Source::AlphaStill)
                .decorated(steady(0.8)),
            Case::new("transparent PNG, keys that hold the zoom", keys_holding_the_zoom()).from(Source::AlphaStill),
            Case::new("transparent PNG, a moving zoom", keys(true, false, false)).from(Source::AlphaStill),
            Case::new("transparent PNG, a moving zoom + rotation", keys(false, true, true))
                .from(Source::AlphaStill)
                .rotated(),
            Case::new("transparent FFV1, a constant zoom", vec![])
                .from(Source::AlphaVideo("30"))
                .decorated(steady(0.8)),
            Case::new("transparent FFV1, keys that hold the zoom", keys_holding_the_zoom()).from(Source::AlphaVideo("30")),
            Case::new("transparent FFV1, a moving zoom", keys(true, false, false)).from(Source::AlphaVideo("30")),
            Case::new("transparent FFV1, a moving zoom + opacity", keys(false, false, true)).from(Source::AlphaVideo("30")),
            Case::new("transparent FFV1, cover", vec![])
                .from(Source::AlphaVideo("30"))
                .fit(Fit::Cover)
                .decorated(steady(0.8)),
        ],
    );
}

/// Position keys over a zoom that holds still (0.8 throughout).
fn keys_holding_the_zoom() -> Vec<Keyframe> {
    let mut k = keys(true, false, false);
    for key in &mut k {
        key.scale = 0.8;
    }
    k
}

// ---- what it costs ----------------------------------------------------------

/// Wall time of the export graph at 1080p30 for a clip with each kind of keyframes, to the
/// null muxer (no encoder: the graph is what is being timed), best of two. Measures,
/// asserts nothing: `KERF_ZOOM_COST=1 cargo test -p kerf-core --no-default-features -- --ignored
/// keyed_zoom_cost --nocapture`. The filters that used to follow the zoom now run at the
/// constant fit size instead of at the zoomed one, which is dearer for a picture that shrinks
/// (a clip at a quarter of the frame used to hand `geq` or `rotate` a quarter of the pixels)
/// and cheaper for one that grows past the frame.
#[test]
#[ignore = "a measurement, not a check; needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn keyed_zoom_cost() {
    // Minutes of ffmpeg, so a plain `--ignored` run (CI's) leaves it alone.
    if std::env::var_os("KERF_ZOOM_COST").is_none() {
        eprintln!("skipped: set KERF_ZOOM_COST=1 to time it");
        return;
    }
    let dir = scratch("cost");
    let path = dir.join("hd.mp4");
    ffmpeg(&[
        s("-f"),
        s("lavfi"),
        s("-i"),
        s("testsrc2=size=1920x1080:rate=30:duration=3,format=yuv420p"),
        s("-c:v"),
        s("libx264"),
        s("-preset"),
        s("ultrafast"),
        s("-crf"),
        s("18"),
        path.to_string_lossy().into_owned(),
    ]);
    let mut asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = 3.0;
    let keyed = |scale: [f64; 2], rotation: f64, opacity: f64, position: bool| {
        let mut clip = make_clip(asset.id, 0.0, 3.0, 0.0);
        let key = |time, scale, rotation, opacity, pos_x| Keyframe {
            time,
            scale,
            pos_x,
            pos_y: 0.0,
            rotation,
            opacity,
        };
        clip.keyframes = vec![
            key(0.0, scale[0], 0.0, 1.0, if position { -0.2 } else { 0.0 }),
            key(3.0, scale[1], rotation, opacity, if position { 0.2 } else { 0.0 }),
        ];
        clip
    };
    let mut still = make_clip(asset.id, 0.0, 3.0, 0.0);
    still.transform.scale = 0.5;
    let rows: Vec<(&str, Clip)> = vec![
        ("static scale 0.5 (unkeyed)", still),
        ("keyed position only", keyed([0.5, 0.5], 0.0, 1.0, true)),
        ("keyed zoom 0.5 -> 1.5", keyed([0.5, 1.5], 0.0, 1.0, false)),
        ("keyed zoom 0.25 -> 0.5 (shrinking)", keyed([0.25, 0.5], 0.0, 1.0, false)),
        ("keyed opacity only (scale holds 0.5)", keyed([0.5, 0.5], 0.0, 0.5, false)),
        ("keyed rotation only (scale holds 0.5)", keyed([0.5, 0.5], 40.0, 1.0, false)),
        ("keyed zoom + opacity (geq)", keyed([0.5, 1.5], 0.0, 0.5, false)),
        ("keyed zoom + rotation", keyed([0.5, 1.5], 40.0, 1.0, false)),
        ("keyed zoom + opacity + rotation", keyed([0.5, 1.5], 40.0, 0.5, false)),
        ("keyed zoom 0.25 -> 0.5 + opacity", keyed([0.25, 0.5], 0.0, 0.5, false)),
    ];
    let mut report = Vec::new();
    for (name, clip) in rows {
        let timeline = timeline_of(vec![video_track(vec![clip])]);
        let mut args = build_export_args(
            &timeline,
            std::slice::from_ref(&asset),
            "unused.mkv",
            &ExportOptions::default(),
        )
        .unwrap();
        let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
        args.truncate(graph + 1);
        args.extend(["-map", "[outv]", "-f", "null", "-"].map(s));
        let best = (0..2)
            .map(|_| {
                let started = std::time::Instant::now();
                let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
                assert!(run.status.success(), "{name}: {}", String::from_utf8_lossy(&run.stderr));
                started.elapsed().as_secs_f64()
            })
            .fold(f64::MAX, f64::min);
        report.push(format!("{name:<40} {best:>6.2} s for 90 frames ({:.0} fps)", 90.0 / best));
    }
    report_lines(&report);
    cleanup(&dir);
}
