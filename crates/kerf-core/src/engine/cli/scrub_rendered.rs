//! The scrub window held to the export and to the still (`#[ignore]`d: they drive the real
//! `ffmpeg`, on 6.1 and 9.0 alike).
//!
//! A scrubbed frame comes from a window, one process drawing the export graph over a slice of
//! the cut, where it used to come from a one-shot still of its own. The cache is keyed by the
//! still's argv, so a window frame is filed as the still of the same frame. What these hold:
//!
//! * a window frame is **the still's picture** (its JPEG byte for byte, or through the encoder's
//!   rounding) **or the export's frame**, never another picture, for every kind of cut a window
//!   may draw (`Snapshot::plain`), from a window started at any frame: a clip at the cut's rate,
//!   at a whole multiple of it (60 in 30, 2x), at 29.97 in 30, with its start or its in-point off
//!   the frame grid, reversed, over another track, from a still image, under a title, graded,
//!   blurred, with a keyed zoom, delivered as Cover. On 9.0 a plain cut is one JPEG both ways; a
//!   reversed clip is the export's frame (the still is a frame off it); an inset, a still image,
//!   Cover, a grade and a blur are the still's picture through the rounding (up to 3.3 levels on
//!   the mean under a grade). On 6.1 the still is the odd one out on every cut (its bare `color`
//!   base, with no `format=yuv420p` as the window's has, lets the overlay settle on another pixel
//!   format and the chroma of `testsrc2`'s one-pixel lines comes out 14 levels off on the mean):
//!   the window is the export there. A start exactly half a frame off the grid is a tie between
//!   two source frames: the window and the still take one on 6.1 and the export its neighbour;
//! * a window never spans a frame `plain` refuses: inside a fade, a dissolve, a slide or a tail, a
//!   source that is not a whole multiple of the cut's rate, the last frame of a title;
//!   `refusals` prints what a window would have drawn there, which is why.
//!
//! `KERF_SCRUB_VERBOSE=1` prints every frame, `KERF_SCRUB_CASE=<name>` runs one case.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored scrub_rendered`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build).

#![allow(clippy::print_stderr)] // what each case drew, for whoever runs the suite

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use super::rendered::export_frames_sized;
use super::scrub::Snapshot;
use super::*;
use crate::engine::test_support::{image_stream, make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded};
use crate::model::{Asset, Clip, Delivery, Keyframe, TextOverlay};

const W: u32 = 320;
const H: u32 = 180;
const QUALITY: u8 = 4;

fn scratch() -> PathBuf {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let n = MADE.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kerf-scrub-rendered-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A lossless all-intra clip of `testsrc2` (it counts frames and moves, so a frame off by one
/// shows) at `fps` (the text ffmpeg is given), `secs` long.
fn source(dir: &Path, name: &str, fps: &str, secs: u32) -> Asset {
    let path = dir.join(format!("{name}-{}.mp4", fps.replace('/', "-")));
    if !path.exists() {
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=size={W}x{H}:rate={fps}:duration={secs},format=yuv420p"))
            .args(["-c:v", "libx264", "-crf", "0", "-g", "1", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());
    }
    let rate: f64 = match fps {
        "30000/1001" => 30000.0 / 1001.0,
        other => other.parse().unwrap(),
    };
    let mut asset = test_asset(vec![video_stream(W, H, rate)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = f64::from(secs);
    asset
}

fn picture(dir: &Path, video: &Asset) -> Asset {
    let path = dir.join("picture.png");
    if !path.exists() {
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", "0.5", "-i"])
            .arg(&video.path)
            .args(["-frames:v", "1"])
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());
    }
    let mut asset = test_asset(vec![image_stream(W, H)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = crate::model::DEFAULT_IMAGE_DURATION;
    asset
}

/// The frames of `[a, b)` as the window draws them: JPEGs, in order.
fn window(snap: &Snapshot<'_>, a: u64, b: u64) -> Vec<Vec<u8>> {
    let args = snap.window_args(a, b, None).expect("a window");
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    let mut frames = Vec::new();
    let mut rest = &run.stdout[..];
    while let Some((s, e)) = next_jpeg(rest) {
        frames.push(rest[s..e].to_vec());
        rest = &rest[e..];
    }
    frames
}

/// The one-shot still of frame `k`.
fn still(snap: &Snapshot<'_>, k: u64) -> Vec<u8> {
    let args = snap.still_args(k).expect("a still");
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    run.stdout
}

/// A JPEG as rgb24.
fn rgb(dir: &Path, jpeg: &[u8], tag: &str) -> Vec<u8> {
    static DECODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = DECODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = dir.join(format!("decode-{tag}-{n}.jpg"));
    std::fs::write(&path, jpeg).unwrap();
    let run = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&path)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(run.status.success());
    run.stdout
}

/// How far two rgb24 frames are apart: the worst channel and the mean over all of them.
fn distance(a: &[u8], b: &[u8]) -> (u8, f64) {
    assert_eq!(a.len(), b.len(), "frames of another size");
    let worst = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    let sum: u64 = a.iter().zip(b).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    (worst, sum as f64 / a.len() as f64)
}

/// What a case is: a cut, and the windows to draw over it.
struct Case {
    name: &'static str,
    build: fn(&Rig) -> (Timeline, Vec<Asset>),
    /// Windows `(from, to)` in frames of the cut; every frame must be plain.
    windows: &'static [(u64, u64)],
    /// How many of the window frames may be neither the still's JPEG nor the export's frame. A
    /// keyed zoom that lands exactly on a whole number of pixels (`320 * 1.1`) rounds either way on
    /// the last bit of a key the slice recomputed, and the frame is a pixel larger or smaller.
    strays: usize,
    /// The size the export is rendered and read back at: the cut's, or a delivery's.
    size: (u32, u32),
    /// Every frame is a tie between two source frames (a start half a frame off the grid): the
    /// window and the still may round to the one and the export to its neighbour.
    tie: bool,
}

impl Case {
    fn new(name: &'static str, build: fn(&Rig) -> (Timeline, Vec<Asset>), windows: &'static [(u64, u64)]) -> Self {
        Self {
            name,
            build,
            windows,
            strays: 0,
            size: (W, H),
            tie: false,
        }
    }

    fn tie(self) -> Self {
        Self { tie: true, ..self }
    }

    fn strays(self, frames: usize) -> Self {
        Self { strays: frames, ..self }
    }

    fn delivered_at(self, width: u32, height: u32) -> Self {
        Self {
            size: (width, height),
            ..self
        }
    }
}

/// Whether a frame is another picture than the export's, not the same one through a JPEG: the
/// JPEG at this quality is up to ~65 levels off on `testsrc2`'s one-pixel lines, 1.6 on the mean.
fn is_another_picture(d: (u8, f64)) -> bool {
    d.0 > 100 || d.1 > 4.0
}

struct Rig {
    dir: PathBuf,
    a30: Asset,
    a24: Asset,
    a25: Asset,
    a60: Asset,
    a2997: Asset,
    image: Asset,
}

impl Rig {
    /// The sources are made once for the tests that run side by side, and removed with the last of
    /// them, a panic included.
    fn shared() -> Arc<Rig> {
        static HELD: Mutex<Weak<Rig>> = Mutex::new(Weak::new());
        let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(rig) = held.upgrade() {
            return rig;
        }
        let rig = Arc::new(Rig::new());
        *held = Arc::downgrade(&rig);
        rig
    }

    fn new() -> Self {
        let dir = scratch();
        let a30 = source(&dir, "testsrc", "30", 6);
        let image = picture(&dir, &a30);
        Self {
            a24: source(&dir, "testsrc", "24", 6),
            a25: source(&dir, "testsrc", "25", 6),
            a60: source(&dir, "testsrc", "60", 6),
            a2997: source(&dir, "testsrc", "30000/1001", 6),
            a30,
            image,
            dir,
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn verbose() -> bool {
    std::env::var("KERF_SCRUB_VERBOSE").is_ok()
}

fn run(case: &Case, rig: &Rig) {
    let (timeline, assets) = (case.build)(rig);
    let snap = Snapshot::new(&timeline, &assets, W, QUALITY);
    let export = Some(export_frames_sized(
        snap.rendered(),
        &assets,
        snap.fps(),
        case.size,
        &rig.dir,
        case.name,
    ));
    let (mut the_same, mut the_still, mut the_export, mut the_tie, mut strays, mut total) = (0, 0, 0, 0, 0, 0);
    let mut worst_still = (0u8, 0.0f64);
    for &(a, b) in case.windows {
        for k in a..b {
            assert!(
                snap.plain(k),
                "{}: frame {k} of a window is not one a window may draw",
                case.name
            );
        }
        let frames = window(&snap, a, b);
        assert_eq!(frames.len() as u64, b - a, "{}: a window of {a}..{b}", case.name);
        for (i, jpeg) in frames.iter().enumerate() {
            let k = a + i as u64;
            total += 1;
            let one_shot = still(&snap, k);
            let w = rgb(&rig.dir, jpeg, "w");
            let to_still = if *jpeg == one_shot {
                (0, 0.0)
            } else {
                distance(&w, &rgb(&rig.dir, &one_shot, "s"))
            };
            let to_export = export.as_ref().map(|frames| distance(&w, &frames[k as usize]));
            // The picture, not the bytes: 6.1 writes the same pictures as 9.0 through other
            // roundings of the last conversion, a level or two apart.
            let like_the_still = !is_another_picture(to_still);
            if *jpeg == one_shot {
                the_same += 1;
            } else if like_the_still {
                the_still += 1;
                worst_still = (worst_still.0.max(to_still.0), worst_still.1.max(to_still.1));
            } else if to_export.is_some_and(|d| !is_another_picture(d)) {
                the_export += 1;
            } else if case.tie
                && export.as_ref().is_some_and(|frames| {
                    [k.wrapping_sub(1), k + 1]
                        .into_iter()
                        .filter_map(|n| frames.get(n as usize))
                        .any(|f| !is_another_picture(distance(&w, f)))
                })
            {
                the_tie += 1;
            } else {
                strays += 1;
            }
            if verbose() {
                eprintln!(
                    "{:>12} frame {k:>4}: window~still {to_still:?} window~export {to_export:?}",
                    case.name
                );
            }
        }
    }
    eprintln!(
        "{:>12}: {total} window frames: {the_same} are the still's JPEG, {the_still} are the still's picture through another rounding (worst {worst_still:?}), {the_export} are the export's frame where the still is not, {the_tie} are the export's neighbour where the tie rounds the other way, {strays} are neither",
        case.name
    );
    assert!(
        strays <= case.strays,
        "{}: {strays} window frames are neither the still's picture nor the export's frame (allowed {})",
        case.name,
        case.strays
    );
}

fn plain_cut(rig: &Rig) -> (Timeline, Vec<Asset>) {
    let a = &rig.a30;
    (
        timeline_of(vec![video_track(vec![make_clip(a.id, 0.5, 5.5, 0.0)])]),
        vec![a.clone()],
    )
}

fn into_30(rig: &Rig, other: &Asset) -> (Timeline, Vec<Asset>) {
    let lead = make_clip(rig.a30.id, 0.0, 1.0, 0.0);
    let main = make_clip(other.id, 0.5, 5.0, 1.0);
    (
        timeline_of(vec![video_track(vec![lead, main])]),
        vec![rig.a30.clone(), other.clone()],
    )
}

/// One clip of the 30 fps source, `edit`ed.
fn one(rig: &Rig, src_in: f64, src_out: f64, start: f64, edit: impl FnOnce(&mut Clip)) -> (Timeline, Vec<Asset>) {
    let mut clip = make_clip(rig.a30.id, src_in, src_out, start);
    edit(&mut clip);
    (timeline_of(vec![video_track(vec![clip])]), vec![rig.a30.clone()])
}

/// A fraction of a 30 fps frame, in seconds.
fn frac(f: f64) -> f64 {
    f / 30.0
}

fn zoom(c: &mut Clip) {
    c.keyframes = vec![
        Keyframe {
            time: 0.0,
            scale: 1.0,
            ..Keyframe::from_transform(0.0, &Default::default())
        },
        Keyframe {
            time: 3.0,
            scale: 1.6,
            pos_x: 0.1,
            ..Keyframe::from_transform(3.0, &Default::default())
        },
    ];
}

fn cases() -> Vec<Case> {
    vec![
        Case::new("cfr30", plain_cut, &[(0, 12), (7, 31), (61, 85), (100, 124)]),
        Case::new("r60-in-30", |r| into_30(r, &r.a60), &[(32, 56), (60, 84)]),
        Case::new("r2997-in-30", |r| into_30(r, &r.a2997), &[(32, 56), (60, 84)]),
        Case::new(
            "start+.25f",
            |r| one(r, 0.5, 5.0, 0.5 + frac(0.25), |_| {}),
            &[(20, 44), (61, 85)],
        ),
        Case::new(
            "start+.5f",
            |r| one(r, 0.5, 5.0, 0.5 + frac(0.5), |_| {}),
            &[(20, 44), (61, 85)],
        )
        .tie(),
        Case::new("in+.5f", |r| one(r, 0.5 + frac(0.5), 5.0, 0.0, |_| {}), &[(1, 25), (61, 85)]),
        Case::new("speed2", |r| one(r, 0.5, 4.5, 0.0, |c| c.speed = 2.0), &[(1, 25), (30, 54)]),
        // The still is a frame off the export on a reversed clip; the window is not.
        Case::new(
            "reversed",
            |r| one(r, 0.5, 4.5, 0.0, |c| c.speed = -1.0),
            &[(1, 25), (40, 64)],
        ),
        Case::new(
            "pip",
            |r| {
                let base = make_clip(r.a30.id, 0.5, 5.0, 0.0);
                let mut inset = make_clip(r.a30.id, 1.0, 4.0, 0.0);
                inset.transform.scale = 0.4;
                inset.transform.pos_x = 0.25;
                inset.transform.pos_y = -0.2;
                let mut tl = timeline_of(vec![video_track(vec![base]), video_track(vec![inset])]);
                tl.tracks[1].name = "V2".to_string();
                (tl, vec![r.a30.clone()])
            },
            &[(1, 25), (40, 64)],
        ),
        Case::new(
            "graded",
            |r| {
                one(r, 0.5, 5.0, 0.0, |c| {
                    c.color.contrast = 1.3;
                    c.color.saturation = 1.5;
                    c.effects.push(crate::model::VideoEffect::Blur { sigma: 3.0 });
                })
            },
            &[(1, 25), (40, 64)],
        ),
        Case::new(
            "brightness",
            |r| one(r, 0.5, 5.0, 0.0, |c| c.color.brightness = 0.1),
            &[(1, 25)],
        ),
        // Frames 5 and 15 of a window started at frame 1 or 4 land on a whole number of pixels.
        Case::new(
            "zoom-keyed",
            |r| one(r, 0.5, 5.0, 0.0, zoom),
            &[(0, 24), (1, 25), (2, 26), (3, 27), (40, 64)],
        )
        .strays(4),
        Case::new(
            "title",
            |r| {
                let (mut tl, assets) = plain_cut(r);
                tl.overlays.push(TextOverlay::new("scrub", 0.5, 1.5));
                (tl, assets)
            },
            &[(1, 25), (30, 45)],
        ),
        Case::new(
            "cover-9x16",
            |r| {
                let (mut tl, assets) = plain_cut(r);
                tl.format = Some(Delivery::new(180, 320, Fit::Cover));
                (tl, assets)
            },
            &[(1, 25), (40, 64)],
        )
        .delivered_at(180, 320),
        Case::new(
            "image",
            |r| {
                let base = make_clip(r.a30.id, 0.5, 5.0, 0.0);
                let mut sticker = make_clip(r.image.id, 0.0, 3.0, 0.0);
                sticker.transform.scale = 0.3;
                sticker.transform.pos_x = -0.3;
                let mut tl = timeline_of(vec![video_track(vec![base]), video_track(vec![sticker])]);
                tl.tracks[1].name = "V2".to_string();
                (tl, vec![r.a30.clone(), r.image.clone()])
            },
            &[(1, 25), (40, 64)],
        ),
    ]
}

#[test]
#[ignore = "drives the real ffmpeg"]
fn a_scrub_window_frame_is_the_exports_frame() {
    let rig = Rig::shared();
    let only = std::env::var("KERF_SCRUB_CASE").ok();
    for case in cases() {
        if only.as_deref().is_some_and(|name| name != case.name) {
            continue;
        }
        run(&case, &rig);
    }
}

/// The sources a window is refused: a 24 or 25 fps clip in a 30 fps cut, a clip at 1.5x or 0.5x.
/// `plain` says no for every frame of them; what a window would have drawn is printed, which is
/// the evidence: its frames follow the slice it starts at, and are neither the export's nor the
/// still's on some frames of each cycle.
#[test]
#[ignore = "drives the real ffmpeg"]
fn a_source_that_is_not_a_whole_multiple_of_the_rate_is_refused() {
    let rig = Rig::shared();
    type Build = Box<dyn Fn(&Rig) -> (Timeline, Vec<Asset>)>;
    let refused: Vec<(&str, Build, (u64, u64))> = vec![
        ("r24-in-30", Box::new(|r| into_30(r, &r.a24)), (32, 56)),
        ("r25-in-30", Box::new(|r| into_30(r, &r.a25)), (32, 56)),
        ("speed.5", Box::new(|r| one(r, 0.5, 2.5, 0.0, |c| c.speed = 0.5)), (1, 25)),
        ("speed1.5", Box::new(|r| one(r, 0.5, 4.5, 0.0, |c| c.speed = 1.5)), (1, 25)),
    ];
    for (name, build, (a, b)) in refused {
        let (timeline, assets) = build(&rig);
        let snap = Snapshot::new(&timeline, &assets, W, QUALITY);
        assert!(!(a..b).any(|k| snap.plain(k)), "{name}: a window may not draw this clip");
        let export = export_frames_sized(snap.rendered(), &assets, snap.fps(), (W, H), &rig.dir, name);
        let frames = window(&snap, a, b);
        let off = frames
            .iter()
            .enumerate()
            .filter(|(i, jpeg)| is_another_picture(distance(&rgb(&rig.dir, jpeg, "w"), &export[a as usize + i])))
            .count();
        eprintln!(
            "{name:>12}: {off} of {} window frames are another picture than the export's",
            frames.len()
        );
    }
}

/// A dissolve between two clips: no window spans its frames, and the windows either side of it
/// are the export's.
#[test]
#[ignore = "drives the real ffmpeg"]
fn a_window_stops_short_of_a_transition_and_starts_after_it() {
    let rig = Rig::shared();
    let a = make_clip(rig.a30.id, 0.0, 1.0, 0.0);
    let mut b = make_clip(rig.a30.id, 2.0, 4.0, 1.0);
    b.transition_in = Some(crate::model::Transition {
        kind: crate::model::TransitionKind::Crossfade,
        duration: 0.5,
    });
    let timeline = timeline_of(vec![video_track(vec![a, b])]);
    let assets = vec![rig.a30.clone()];
    let snap = Snapshot::new(&timeline, &assets, W, QUALITY);
    // The dissolve covers frames 30..45.
    let hazard: Vec<u64> = (0..60).filter(|k| !snap.plain(*k)).collect();
    eprintln!("not plain: {hazard:?}");
    assert_eq!(hazard.first(), Some(&30), "the dissolve starts at the incoming clip");
    assert!(hazard.iter().all(|k| (30..46).contains(k)), "and nowhere else: {hazard:?}");
    assert!(hazard.len() >= 15, "all of it: {hazard:?}");

    let config = super::scrub::ScrubConfig::default();
    assert_eq!(snap.window_range(33, false, &config), None, "a frame inside it is a one-shot");
    let (lo, hi) = snap.window_range(20, false, &config).expect("a window before it");
    assert!(hi <= 30, "ends at its first frame: {lo}..{hi}");
    let (lo, hi) = snap.window_range(50, false, &config).expect("a window after it");
    assert!(lo > hazard[hazard.len() - 1], "starts after its last: {lo}..{hi}");

    let export = export_frames_sized(snap.rendered(), &assets, snap.fps(), (W, H), &rig.dir, "dissolve");
    for (from, to) in [(20, 30), (50, 74)] {
        let frames = window(&snap, from, to);
        assert_eq!(frames.len() as u64, to - from);
        for (i, jpeg) in frames.iter().enumerate() {
            let d = distance(&rgb(&rig.dir, jpeg, "w"), &export[from as usize + i]);
            if verbose() {
                eprintln!("dissolve frame {}: {d:?}", from as usize + i);
            }
            assert!(!is_another_picture(d), "frame {}: {d:?} from the export", from as usize + i);
        }
    }
}
