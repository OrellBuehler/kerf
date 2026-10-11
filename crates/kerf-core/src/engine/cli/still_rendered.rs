//! The scrubbed still inside a fade, a dip, a dissolve, a slide or a push, held to the export
//! (`#[ignore]`d: they drive the real `ffmpeg`, on 6.1 and 9.0 alike).
//!
//! A paused frame is `build_still_args`'s: one decoded frame per visible clip, no timeline clock.
//! The export is the filter graph the file is rendered through, and a frame inside a transition
//! used to differ between them wholesale: the still drew the incoming clip at full strength and
//! nothing of the outgoing one, no fade, no dip, no slide. These render a lossless 64x36 clip of
//! four one-second stripes (each half of the picture another colour) through both and hold every
//! frame of the stretch the transition covers to the export's:
//!
//! * a dissolve (the outgoing clip on its tail under the incoming one's alpha ramp), also with the
//!   outgoing clip reversed and sped up, with the incoming one transformed, and out of black;
//! * a dip to black and to white, a clip's own fade-in and fade-out, and an incoming clip that fades
//!   or dips out again after its dissolve (its chain stays on an alpha plane, where black is 0);
//! * every slide and push;
//! * each at 24 / 25 / 29.97 / 30 / 60 fps (the transition is counted in output frames);
//! * a transition 18.3 frames long at 30 fps, the still asked for at the start, the middle and the
//!   end of every frame (the frame it shows is the one its time falls in, and the outgoing tail's
//!   window closes inside one of them).
//!
//! The rendered pictures are compared pixel by pixel after the same conversion to rgb24. Set
//! `KERF_STILL_DUMP=<dir>` to keep the raw frames of both (`<case>-<fps>-<k>-<phase>-export.rgb` / `-still.rgb`)
//! and `KERF_STILL_VERBOSE=1` to print every case's worst frame.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored still_rendered`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use uuid::Uuid;

use super::rendered::export_frames_sized;
use super::*;
use crate::clip_timing::ffmpeg_frame_time;
use crate::engine::test_support::{image_stream, make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded};
use crate::model::{Asset, Clip, Keyframe, Transition, TransitionKind};

const W: u32 = 64;
const H: u32 = 36;

/// The stripes: second `s` of the source is `LEFT[s]` on the left half and `RIGHT[s]` on the right.
const LEFT: [&str; 4] = ["0xE03030", "0x30B030", "0x3030E0", "0xE0A020"];
const RIGHT: [&str; 4] = ["0x30D0D0", "0xC030C0", "0xE0E030", "0xF0F0F0"];

type Rate = (&'static str, u32, u32);
const R24: Rate = ("24", 24, 1);
const RATES: [Rate; 5] = [R24, ("25", 25, 1), ("29.97", 2997, 100), ("30", 30, 1), ("60", 60, 1)];

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-still-rendered-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A lossless four second clip of the stripes at `fps` (the text ffmpeg is given).
fn stripes(dir: &Path, fps: &str) -> Asset {
    let path = dir.join(format!("stripes-{fps}.mp4"));
    if !path.exists() {
        let mut chain = format!("color=c=0x303030:s={W}x{H}:r={fps}:d=4");
        for s in 0..4 {
            for (x, color) in [(0, LEFT[s]), (W / 2, RIGHT[s])] {
                chain += &format!(
                    ",drawbox=x={x}:y=0:w={}:h={H}:color={color}:t=fill:enable='gte(t,{s})*lt(t,{})'",
                    W / 2,
                    s + 1
                );
            }
        }
        chain += ",format=yuv420p";
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

/// A still image of the first stripe (a PNG, flagged as an image on the asset).
fn picture(dir: &Path, video: &Asset) -> Asset {
    let path = dir.join("picture.png");
    if !path.exists() {
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", "0.25", "-i"])
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

fn options(rate: Rate) -> ExportOptions {
    ExportOptions {
        resolution: Some((W, H)),
        fps: Some(rate.0.parse().unwrap()),
        ..ExportOptions::default()
    }
}

/// The scrubbed still of `timeline` at `t`, as raw rgb24.
fn still_frame(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, t: f64) -> Vec<u8> {
    let args = build_still_args(timeline, assets, opts, t, W, None, &StillOutput::RgbPipe).unwrap();
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout.len(), (W * H * 3) as usize, "a still is one frame");
    run.stdout
}

/// How far two rgb24 frames are apart: the worst channel and the mean over all of them.
fn distance(a: &[u8], b: &[u8]) -> (u8, f64) {
    assert_eq!(a.len(), b.len());
    let worst = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
    let sum: u64 = a.iter().zip(b).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    (worst, sum as f64 / a.len() as f64)
}

/// The assets a cut is built from: the stripes, and a picture (a still image, one frame of the
/// first stripe).
#[derive(Clone, Copy)]
struct Ids {
    video: Uuid,
    image: Uuid,
}

/// What a case renders: a cut, and the frames of it to compare.
struct Case {
    name: &'static str,
    rate: Rate,
    build: fn(Ids) -> Timeline,
    /// The output frames to compare, as seconds on the timeline: `(from, to)`, both ends in.
    span: (f64, f64),
    /// The worst channel the still may be off the export, and the worst mean.
    tolerance: (u8, f64),
    /// The least the export itself changes over the span (worst channel): the case does something.
    moves: u8,
    /// Where in each output frame the still is asked for, in frames after its start: a still is
    /// asked for any time, and the frame it shows is the one the time falls in.
    phases: &'static [f64],
}

impl Case {
    fn new(name: &'static str, build: fn(Ids) -> Timeline, span: (f64, f64)) -> Self {
        Self {
            name,
            rate: R24,
            build,
            span,
            tolerance: (1, 0.1),
            moves: 40,
            phases: &[0.0],
        }
    }

    fn sampled_at(self, phases: &'static [f64]) -> Self {
        Self { phases, ..self }
    }

    fn at(self, rate: Rate) -> Self {
        Self { rate, ..self }
    }

    fn within(self, channel: u8, mean: f64) -> Self {
        Self {
            tolerance: (channel, mean),
            ..self
        }
    }
}

fn entering(clip: &mut Clip, kind: TransitionKind, secs: f64) {
    clip.transition_in = Some(Transition { kind, duration: secs });
}

/// One cut: clip `A` (stripe 0) then clip `B` (stripe 2) from the one-second mark, `B` entering
/// through `kind` over `secs`.
fn then(ids: Ids, kind: TransitionKind, secs: f64) -> Timeline {
    let a = make_clip(ids.video, 0.0, 1.0, 0.0);
    let mut b = make_clip(ids.video, 2.0, 3.0, 1.0);
    entering(&mut b, kind, secs);
    timeline_of(vec![video_track(vec![a, b])])
}

fn dissolve(ids: Ids) -> Timeline {
    then(ids, TransitionKind::Crossfade, 0.5)
}

fn dip_black(ids: Ids) -> Timeline {
    then(ids, TransitionKind::DipToBlack, 1.0)
}

fn dip_white(ids: Ids) -> Timeline {
    then(ids, TransitionKind::DipToWhite, 1.0)
}

fn slide_left(ids: Ids) -> Timeline {
    then(ids, TransitionKind::SlideLeft, 0.5)
}

fn slide_right(ids: Ids) -> Timeline {
    then(ids, TransitionKind::SlideRight, 0.5)
}

fn slide_up(ids: Ids) -> Timeline {
    then(ids, TransitionKind::SlideUp, 0.5)
}

fn slide_down(ids: Ids) -> Timeline {
    then(ids, TransitionKind::SlideDown, 0.5)
}

fn push_left(ids: Ids) -> Timeline {
    then(ids, TransitionKind::PushLeft, 0.5)
}

fn push_right(ids: Ids) -> Timeline {
    then(ids, TransitionKind::PushRight, 0.5)
}

fn push_up(ids: Ids) -> Timeline {
    then(ids, TransitionKind::PushUp, 0.5)
}

fn push_down(ids: Ids) -> Timeline {
    then(ids, TransitionKind::PushDown, 0.5)
}

fn fade_in(ids: Ids) -> Timeline {
    let mut clip = make_clip(ids.video, 0.0, 2.0, 0.0);
    clip.fade_in = 0.5;
    timeline_of(vec![video_track(vec![clip])])
}

fn fade_out(ids: Ids) -> Timeline {
    let mut clip = make_clip(ids.video, 0.0, 2.0, 0.0);
    clip.fade_out = 0.5;
    timeline_of(vec![video_track(vec![clip])])
}

/// A dissolve whose incoming clip then fades out on its own: the export's chain for that clip is on
/// an alpha plane from the dissolve on, and so is the fade, long after the ramp is over.
fn crossfade_then_fade_out(ids: Ids) -> Timeline {
    let a = make_clip(ids.video, 0.0, 1.0, 0.0);
    let mut b = make_clip(ids.video, 2.0, 4.0, 1.0);
    entering(&mut b, TransitionKind::Crossfade, 0.5);
    b.fade_out = 1.0;
    timeline_of(vec![video_track(vec![a, b])])
}

/// The same incoming clip dipping out to the clip after it.
fn crossfade_then_dip_out(ids: Ids) -> Timeline {
    let a = make_clip(ids.video, 0.0, 1.0, 0.0);
    let mut b = make_clip(ids.video, 2.0, 4.0, 1.0);
    entering(&mut b, TransitionKind::Crossfade, 0.5);
    let mut c = make_clip(ids.video, 0.0, 1.0, 3.0);
    entering(&mut c, TransitionKind::DipToBlack, 2.0);
    timeline_of(vec![video_track(vec![a, b, c])])
}

/// A transition whose length is not a whole number of frames at 30 fps (18.3), over an incoming
/// clip that leaves a border: the outgoing clip's tail window closes inside a frame, and that frame
/// still draws it.
fn off_grid(ids: Ids, kind: TransitionKind) -> Timeline {
    let mut tl = then(ids, kind, 0.61);
    tl.tracks[0].clips[1].transform.scale = 0.5;
    tl
}

fn dissolve_off_grid(ids: Ids) -> Timeline {
    off_grid(ids, TransitionKind::Crossfade)
}

fn slide_off_grid(ids: Ids) -> Timeline {
    off_grid(ids, TransitionKind::SlideLeft)
}

fn dissolve_from_black(ids: Ids) -> Timeline {
    let mut clip = make_clip(ids.video, 1.0, 3.0, 0.0);
    entering(&mut clip, TransitionKind::Crossfade, 0.5);
    timeline_of(vec![video_track(vec![clip])])
}

/// The outgoing clip plays backwards, so its tail borrows the source *below* its in point (the
/// stripe before the first second the clip plays, which keeps the tail clear of a stripe edge,
/// where a still of a reversed clip can be a frame off the file's with or without a transition).
fn dissolve_reversed(ids: Ids) -> Timeline {
    let mut a = make_clip(ids.video, 1.5, 2.5, 0.0);
    a.speed = -1.0;
    let mut b = make_clip(ids.video, 2.0, 3.0, 1.0);
    entering(&mut b, TransitionKind::Crossfade, 0.5);
    timeline_of(vec![video_track(vec![a, b])])
}

/// The outgoing clip runs at twice the speed: half a second of tail is a second of source (the
/// stripe after the one the incoming clip shows, or the dissolve would mix a colour with itself).
fn dissolve_fast(ids: Ids) -> Timeline {
    let mut a = make_clip(ids.video, 0.0, 2.0, 0.0);
    a.speed = 2.0;
    let mut b = make_clip(ids.video, 3.0, 4.0, 1.0);
    entering(&mut b, TransitionKind::Crossfade, 0.5);
    timeline_of(vec![video_track(vec![a, b])])
}

/// The incoming clip is smaller, moved and translucent, and has a grade: the ramp acts on that
/// picture, over the outgoing one.
fn dissolve_transformed(ids: Ids) -> Timeline {
    let mut tl = then(ids, TransitionKind::Crossfade, 0.5);
    let b = &mut tl.tracks[0].clips[1];
    b.transform.scale = 0.6;
    b.transform.pos_x = 0.15;
    b.transform.opacity = 0.8;
    b.color.brightness = 0.1;
    tl
}

/// A dissolve between clips on a track above another clip: the lower track shows through the
/// ramp of the incoming one where the outgoing one does not reach.
fn dissolve_over_a_track(ids: Ids) -> Timeline {
    let mut tl = then(ids, TransitionKind::Crossfade, 0.5);
    tl.tracks[0].clips[0].transform.scale = 0.5;
    tl.tracks[0].clips[1].transform.scale = 0.5;
    let over = tl.tracks.remove(0);
    timeline_of(vec![video_track(vec![make_clip(ids.video, 2.0, 4.0, 0.0)]), over])
}

/// A dissolve whose clips also fade on their own.
fn dissolve_with_own_fades(ids: Ids) -> Timeline {
    let mut tl = then(ids, TransitionKind::Crossfade, 0.5);
    tl.tracks[0].clips[1].fade_in = 0.25;
    tl.tracks[0].clips[0].fade_out = 0.75;
    tl
}

/// The outgoing clip is a still image, which loops for as long as the tail needs.
fn dissolve_from_an_image(ids: Ids) -> Timeline {
    let a = make_clip(ids.image, 0.0, 1.0, 0.0);
    let mut b = make_clip(ids.video, 2.0, 3.0, 1.0);
    entering(&mut b, TransitionKind::Crossfade, 0.5);
    timeline_of(vec![video_track(vec![a, b])])
}

/// The incoming clip has a moving zoom, which the chain runs last, after the fades.
fn dissolve_into_a_zoom(ids: Ids) -> Timeline {
    let mut tl = then(ids, TransitionKind::Crossfade, 0.5);
    let b = &mut tl.tracks[0].clips[1];
    b.keyframes = vec![
        Keyframe {
            time: 0.0,
            scale: 0.5,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        },
        Keyframe {
            time: 1.0,
            scale: 1.0,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        },
    ];
    tl
}

fn cases() -> Vec<Case> {
    let mut all = vec![
        Case::new("dissolve", dissolve, (0.9, 1.7)),
        Case::new("dip-black", dip_black, (0.4, 1.6)),
        // A fade at full strength through white is not quite the identity (it rounds by a level
        // or two), and the still leaves the fades that are not changing the picture out.
        Case::new("dip-white", dip_white, (0.4, 1.6)).within(2, 1.6),
        Case::new("fade-in", fade_in, (0.0, 0.7)),
        Case::new("fade-out", fade_out, (1.3, 1.95)),
        Case::new("slide-left", slide_left, (0.9, 1.7)),
        Case::new("slide-right", slide_right, (0.9, 1.7)),
        Case::new("slide-up", slide_up, (0.9, 1.7)),
        Case::new("slide-down", slide_down, (0.9, 1.7)),
        Case::new("push-left", push_left, (0.9, 1.7)),
        Case::new("push-right", push_right, (0.9, 1.7)),
        Case::new("push-up", push_up, (0.9, 1.7)),
        Case::new("push-down", push_down, (0.9, 1.7)),
        Case::new("dissolve-from-black", dissolve_from_black, (0.0, 0.7)),
        Case::new("dissolve-reversed", dissolve_reversed, (0.9, 1.7)),
        Case::new("dissolve-fast", dissolve_fast, (0.9, 1.7)),
        Case::new("dissolve-transformed", dissolve_transformed, (0.9, 1.7)),
        Case::new("dissolve-over-a-track", dissolve_over_a_track, (0.9, 1.7)),
        Case::new("dissolve-own-fades", dissolve_with_own_fades, (0.2, 1.7)),
        Case::new("dissolve-from-an-image", dissolve_from_an_image, (0.9, 1.7)),
        Case::new("crossfade-then-fade-out", crossfade_then_fade_out, (1.9, 3.1)),
        Case::new("crossfade-then-fade-out", crossfade_then_fade_out, (1.9, 3.1)).at(RATES[3]),
        Case::new("crossfade-then-dip-out", crossfade_then_dip_out, (1.9, 3.9)),
        Case::new("crossfade-then-dip-out", crossfade_then_dip_out, (1.9, 3.9)).at(RATES[3]),
        Case::new("dissolve-off-grid", dissolve_off_grid, (1.4, 1.75))
            .at(RATES[3])
            .sampled_at(&[0.0, 0.5, 0.95]),
        Case::new("slide-off-grid", slide_off_grid, (1.4, 1.75))
            .at(RATES[3])
            .sampled_at(&[0.0, 0.5, 0.95]),
        Case::new("dissolve-into-a-zoom", dissolve_into_a_zoom, (0.9, 1.7)),
    ];
    for rate in &RATES[1..] {
        all.push(Case::new("dissolve", dissolve, (0.9, 1.7)).at(*rate));
        all.push(Case::new("dip-black", dip_black, (0.4, 1.6)).at(*rate));
        all.push(Case::new("slide-left", slide_left, (0.9, 1.7)).at(*rate));
    }
    all
}

/// A case's verdict: the worst frame it found.
struct Worst {
    frames: usize,
    channel: u8,
    mean: f64,
    at: f64,
    moved: u8,
}

fn run(dir: &Path, case: &Case, dump: Option<&Path>) -> Worst {
    let fps = case.rate.0;
    let video = stripes(dir, fps);
    let image = picture(dir, &video);
    let timeline = (case.build)(Ids {
        video: video.id,
        image: image.id,
    });
    let assets = [video, image];
    let opts = options(case.rate);
    let tag = format!("{}-{fps}", case.name);
    let frames = export_frames_sized(&timeline, &assets, fps.parse().unwrap(), (W, H), dir, &tag);
    let (_, num, den) = case.rate;
    let mut worst = Worst {
        frames: 0,
        channel: 0,
        mean: 0.0,
        at: 0.0,
        moved: 0,
    };
    let mut first: Option<&Vec<u8>> = None;
    for (k, exported) in frames.iter().enumerate() {
        let slot = ffmpeg_frame_time(k as u64, num, den);
        if slot < case.span.0 || slot > case.span.1 {
            continue;
        }
        if let Some(f) = first {
            worst.moved = worst.moved.max(distance(f, exported).0);
        }
        first.get_or_insert(exported);
        for &phase in case.phases {
            let t = slot + phase * f64::from(den) / f64::from(num);
            let still = still_frame(&timeline, &assets, &opts, t);
            let (channel, mean) = distance(exported, &still);
            worst.frames += 1;
            if channel > worst.channel || (channel == worst.channel && mean > worst.mean) {
                (worst.channel, worst.mean, worst.at) = (channel, mean, t);
            }
            if let Some(out) = dump {
                let stem = format!("{}-{fps}-{k:03}-{phase}", case.name);
                std::fs::write(out.join(format!("{stem}-export.rgb")), exported).unwrap();
                std::fs::write(out.join(format!("{stem}-still.rgb")), &still).unwrap();
            }
        }
    }
    worst
}

#[allow(clippy::print_stderr)]
fn report(case: &Case, w: &Worst) {
    if std::env::var_os("KERF_STILL_VERBOSE").is_some() {
        eprintln!(
            "{:<24} {:>5} fps  {:>3} frames  worst channel {:>3}, mean {:>6.2} at {:.3}s  (export moves {})",
            case.name, case.rate.0, w.frames, w.channel, w.mean, w.at, w.moved
        );
    }
}

fn check(names: &[&str]) {
    // One directory per test: they run side by side, and each removes its own when it is done.
    let dir = scratch(names[0]);
    let dump = std::env::var_os("KERF_STILL_DUMP").map(PathBuf::from);
    if let Some(d) = &dump {
        std::fs::create_dir_all(d).unwrap();
    }
    let mut failures = Vec::new();
    for case in cases().iter().filter(|c| names.contains(&c.name)) {
        let w = run(&dir, case, dump.as_deref());
        report(case, &w);
        assert!(w.frames >= 5, "{}: only {} frames compared", case.name, w.frames);
        assert!(
            w.moved >= case.moves,
            "{}: the export only moves {} levels over the span: the case does nothing",
            case.name,
            w.moved
        );
        if w.channel > case.tolerance.0 || w.mean > case.tolerance.1 {
            failures.push(format!(
                "{} at {} fps: the still is {} levels off the export (mean {:.2}) at {:.3}s",
                case.name, case.rate.0, w.channel, w.mean, w.at
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_still_inside_a_dissolve_is_the_exports_frame() {
    check(&[
        "dissolve",
        "dissolve-from-black",
        "dissolve-reversed",
        "dissolve-fast",
        "dissolve-transformed",
        "dissolve-over-a-track",
        "dissolve-own-fades",
        "dissolve-from-an-image",
        "dissolve-into-a-zoom",
        "dissolve-off-grid",
        "crossfade-then-fade-out",
        "crossfade-then-dip-out",
    ]);
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_still_inside_a_dip_or_a_fade_is_the_exports_frame() {
    check(&["dip-black", "dip-white", "fade-in", "fade-out"]);
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_still_inside_a_slide_or_a_push_is_the_exports_frame() {
    check(&[
        "slide-left",
        "slide-right",
        "slide-up",
        "slide-down",
        "push-left",
        "push-right",
        "push-up",
        "push-down",
        "slide-off-grid",
    ]);
}
