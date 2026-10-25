//! Speed ramps through the real `ffmpeg` (`#[ignore]`d: they drive the real binary, on 6.1 and 9.0
//! alike).
//!
//! A ramp is the one retime whose graph text cannot be checked by reading it: the `setpts` expression
//! is a tree of closed forms, and the sound is a ramp cut into steps. These render clips whose frames
//! number themselves and whose sound clicks, and hold the result to the map (`Clip::along`):
//!
//! * the frame the export shows at each output frame is the last whose retimed slot has come
//!   (`fps`'s pick over `setpts`' stamps), for a ramp with a hold, a slow stretch and a constant
//!   tail, forward, reversed, in the middle of a file, and playing on under a dissolve (through the rest
//!   of its ramp, as far as its handle goes);
//! * the scrubbed still at a time is the frame the file shows there, or a neighbour at the ramp's pace;
//! * a keyed opacity is the curve's value at the frame's timeline time, ramp or none;
//! * a clip cut from inside a late-starting file's lead keeps its picture and click together;
//! * the sound lasts as long as the picture and a click lands where the map says, so a picture and
//!   the click of the same moment of the source meet at every step (`atempo` plays its content
//!   early, and the ramp makes it up), at every tempo from 0.25x to 8x;
//! * the whole export, encoder and all (a ten-minute stereo AAC file, sixteen keys, forward and
//!   reversed): picture and sound as long as the cut, which the raw-sample tests cannot see (`adelay`
//!   stamps its first silence with no time, and the muxer refuses the file);
//! * a steady tone keeps its pitch through the seams between the sound's steps: its residual stays under
//!   -15 dB (-24 to -30 measured, where one `atempo` stream is -38).
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored ramp_rendered`
//! (and again with `KERF_FFMPEG` / `KERF_FFPROBE` at the pinned build.)

#![allow(clippy::print_stderr)]

use std::path::Path;

use super::picked::{code, filler, numbered, run, H, MP4, W};
use super::rendered::{export_frames_sized, scratch};
use super::*;
use crate::clip_timing::ramp::retime_expr;
use crate::engine::test_support::{make_clip, timeline_of, video_track};
use crate::model::{RampEasing, SpeedKey};
use crate::project::Project;
use uuid::Uuid;

const FPS: f64 = 30.0;
/// The pixel row the frame number is read from.
const ROW: u32 = 16;

fn key(time: f64, speed: f64) -> SpeedKey {
    SpeedKey::new(time, speed)
}

fn hold(time: f64, speed: f64) -> SpeedKey {
    SpeedKey {
        time,
        speed,
        easing: RampEasing::Hold,
    }
}

/// 1x to 2.5x over 1.5 s, 2.5x held for a second, down to 0.5x over a second, then 0.5x.
fn ramp() -> Vec<SpeedKey> {
    vec![key(0.0, 1.0), hold(1.5, 2.5), key(2.5, 0.5), key(3.5, 0.5)]
}

/// The mp4's ticks per second: `setpts` truncates its result to one, and `fps` then rounds that to its
/// slots (half away from zero).
const TICKS: f64 = 15360.0;

/// The slot of the source frame `j` frames into the clip's kept frames, as `setpts` and `fps` work it
/// out: the frame's seconds into the window are `ticks * TB` (a double), the graph's own expression
/// takes them to the timeline, the result is truncated to a tick and rounded to a slot.
fn slot(clip: &Clip, j: u32) -> i64 {
    let tb = 1.0 / TICKS;
    let r = f64::from(j * (TICKS / FPS) as u32) * tb;
    let expr = retime_expr(clip).expect("a ramp");
    let ticks = ((expr.eval(r) + clip.timeline_start) / tb).trunc();
    (ticks * FPS / TICKS).round() as i64
}

/// The number on each output frame the export draws for `clip` (frame `n` of the source carries
/// `n`; `None` where nothing is drawn), over `frames` output frames: the frames of the clip's window
/// stamped by the ramp, the last one whose slot has come shown at each output frame. `tail` is how long
/// a transition keeps the clip on after its end, at the speed the ramp ends on.
fn expected(clip: &Clip, tail: f64, frames: usize) -> Vec<Option<u32>> {
    let handle = clip.along(clip.duration() + tail) - clip.along(clip.duration());
    let (lo, hi) = if clip.is_reversed() {
        (clip.source_in - handle, clip.source_out)
    } else {
        (clip.source_in, clip.source_out + handle)
    };
    let first = (lo * FPS).round() as u32;
    let last = (hi * FPS).round() as u32;
    // `reverse` plays the kept frames last first, with the forward timestamps.
    let kept: Vec<u32> = if clip.is_reversed() {
        (first..last).rev().collect()
    } else {
        (first..last).collect()
    };
    let slots: Vec<i64> = (0..kept.len() as u32).map(|j| slot(clip, j)).collect();
    let end = ((clip.timeline_end() + tail) * FPS - 1e-6).ceil() as i64;
    (0..frames as i64)
        .map(|k| {
            if k < slots[0] || k >= end {
                return None;
            }
            let j = slots.partition_point(|s| *s <= k) - 1;
            Some(kept[j])
        })
        .collect()
}

/// A numbered source of 8 s at [`FPS`], a black filler beneath it, and the clip of `source_in..source_out`
/// ramped by `keys`, `speed` signed, on its own track from `start`.
struct Rig {
    dir: std::path::PathBuf,
    src: Asset,
    filler: Asset,
}

fn rig(tag: &str) -> Rig {
    let dir = scratch(tag);
    let source = numbered(&dir, "30", 8, MP4);
    Rig {
        src: source.asset,
        filler: filler(&dir),
        dir,
    }
}

impl Rig {
    fn clip(&self, source_in: f64, source_out: f64, start: f64, speed: f64, keys: Vec<SpeedKey>) -> Clip {
        let mut clip = make_clip(self.src.id, source_in, source_out, start);
        clip.speed = speed;
        clip.speed_ramp = keys;
        clip
    }

    fn timeline(&self, clips: Vec<Clip>) -> Timeline {
        let end = clips.iter().map(Clip::timeline_end).fold(0.0, f64::max);
        let base = make_clip(self.filler.id, 0.0, end + 0.6, 0.0);
        timeline_of(vec![video_track(vec![base]), video_track(clips)])
    }

    fn assets(&self) -> Vec<Asset> {
        vec![self.src.clone(), self.filler.clone()]
    }

    /// The numbers the export draws on each output frame.
    fn shown(&self, timeline: &Timeline, tag: &str) -> Vec<Option<u32>> {
        export_frames_sized(timeline, &self.assets(), FPS, (W, H), &self.dir, tag)
            .iter()
            .map(|f| code(f, ROW).checked_sub(1))
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// How many output frames of `got` differ from `want`, and by how much at most.
fn compare(label: &str, got: &[Option<u32>], want: &[Option<u32>]) -> (usize, i64) {
    assert_eq!(got.len(), want.len(), "{label}: frames");
    let wrong: Vec<(usize, Option<u32>, Option<u32>)> = got
        .iter()
        .zip(want)
        .enumerate()
        .filter(|(_, (g, w))| g != w)
        .map(|(k, (g, w))| (k, *g, *w))
        .collect();
    let worst = wrong
        .iter()
        .map(|(_, g, w)| match (g, w) {
            (Some(g), Some(w)) => (i64::from(*g) - i64::from(*w)).abs(),
            _ => 1000,
        })
        .max()
        .unwrap_or(0);
    eprintln!(
        "{label}: {} of {} frames differ, by {worst} at most; first {:?}",
        wrong.len(),
        got.len(),
        wrong.first()
    );
    (wrong.len(), worst)
}

#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn a_ramped_clip_shows_the_frames_the_map_says() {
    let rig = rig("ramp-frames");
    for (label, speed, start, source_in, source_out) in [
        ("forward", 1.0, 0.5, 1.0, 7.5),
        ("forward at the head", 1.0, 0.0, 0.0, 6.0),
        ("reversed", -1.0, 0.5, 1.0, 7.5),
    ] {
        let clip = rig.clip(source_in, source_out, start, speed, ramp());
        assert!(clip.is_ramped());
        let timeline = rig.timeline(vec![clip.clone()]);
        let got = rig.shown(&timeline, label);
        let want = expected(&clip, 0.0, got.len());
        let (differ, worst) = compare(label, &got, &want);
        // Every frame the clip's window leaves is drawn, in the order the ramp plays them.
        assert_eq!(differ, 0, "{label}: {worst} frames off at most");
        let drawn = got.iter().flatten().count();
        assert!(
            drawn as f64 >= clip.duration() * FPS - 2.0,
            "{label}: drawn on {drawn} frames of {}",
            clip.duration() * FPS
        );
    }
}

#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn a_ramped_clip_plays_on_under_a_dissolve_through_the_rest_of_its_ramp() {
    use crate::model::{Transition, TransitionKind};
    let rig = rig("ramp-tail");
    for (label, speed) in [("forward", 1.0), ("reversed", -1.0)] {
        // 2..7 of the source: the clip ends inside the ramp's 2.5x hold, and its tail plays on through the
        // rest of the ramp (the hold's last 0.05 s, then 0.5x). A second of handle above the out point
        // bounds it forward at 1.8 s; reversed the two seconds below the in point bound it at the
        // transition's own 2 s.
        let a = rig.clip(2.0, 7.0, 0.5, speed, ramp());
        let mut b = make_clip(rig.filler.id, 0.0, 3.0, a.timeline_end());
        b.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 2.0,
        });
        let timeline = rig.timeline(vec![a.clone(), b]);
        let assets = rig.assets();
        let tail = crate::clip_timing::transition_fx(&timeline.for_render(), &assets)[1].tail;
        let handle = if speed < 0.0 { 2.0 } else { 1.0 };
        let want_tail = (a.local_at_along(a.source_duration() + handle) - a.duration()).min(2.0);
        assert!(
            (tail - want_tail).abs() < 1e-9,
            "{label}: the tail is {tail}, the map says {want_tail}"
        );
        assert!((a.speed_at(a.duration()) - 2.5).abs() < 1e-9 && a.speed_at(a.duration() + tail) < 0.6);
        let got = rig.shown(&timeline, label);
        let want = expected(&a, tail, got.len());
        // The black clip dissolves in over a linear ramp of the tail: the picture's bars stay readable
        // (over half their level) through the first third of it.
        let readable = ((a.timeline_end() + tail / 3.0) * FPS) as usize;
        let (got, want) = (&got[..readable], &want[..readable]);
        let (differ, worst) = compare(label, got, want);
        assert_eq!(differ, 0, "{label}: {worst} frames off at most");
        // It reached into the tail: frames numbered past the clip's own last one are on screen.
        let frames_into_tail = (a.timeline_end() * FPS).ceil() as usize;
        assert!(got[frames_into_tail..].iter().all(Option::is_some) && got.len() > frames_into_tail + 3);
    }
}

#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn a_ramp_leaves_a_keyed_opacity_on_the_timeline_clock() {
    use crate::model::{Property, PropertyKey};
    let rig = rig("ramp-keyed");
    let white = rig.dir.join("white.mp4");
    run(&[
        "-f",
        "lavfi",
        "-i",
        &format!("color=c=white:s={W}x{H}:r=30:d=8,format=yuv420p"),
        "-c:v",
        "libx264",
        "-qp",
        "0",
        "-pix_fmt",
        "yuv420p",
        &white.to_string_lossy(),
    ]);
    let mut asset = rig.filler.clone();
    asset.id = Uuid::new_v4();
    asset.path = white.to_string_lossy().into_owned();
    // The same opacity curve on the same white clip, at a constant speed and ramped: the picture is
    // the curve's value at the frame's timeline time, whatever the clip plays at.
    let luma = |keys: Vec<SpeedKey>, speed: f64| -> Vec<u8> {
        let mut clip = make_clip(asset.id, 0.0, 6.0, 0.5);
        clip.speed = speed;
        clip.speed_ramp = keys;
        clip.set_property_keys(
            Property::Opacity,
            vec![PropertyKey::new(0.0, 0.1), PropertyKey::new(2.0, 1.0)],
        );
        let timeline = timeline_of(vec![
            video_track(vec![make_clip(rig.filler.id, 0.0, 8.0, 0.0)]),
            video_track(vec![clip]),
        ]);
        export_frames_sized(
            &timeline,
            &[rig.filler.clone(), asset.clone()],
            FPS,
            (W, H),
            &rig.dir,
            "keyed",
        )
        .iter()
        .map(|f| f[(ROW * W) as usize * 3])
        .collect()
    };
    let plain = luma(Vec::new(), 1.0);
    let ramped = luma(ramp(), 1.0);
    let reversed = luma(ramp(), -1.0);
    // 0.5 s in to 2.5 s in: the ramp of the opacity, a frame for a frame in all three.
    let (from, to) = (16, 74);
    assert_eq!(&plain[from..to], &ramped[from..to]);
    assert_eq!(&plain[from..to], &reversed[from..to]);
    assert!(plain[from + 2] < 60 && plain[to - 2] > 200, "{:?}", &plain[from..to]);
}

#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn a_ramped_clip_cut_from_inside_a_late_starting_files_lead_keeps_its_picture_and_click_together() {
    use super::late_start::{flash_and_click, late_source, FPS as LATE_FPS};
    let dir = scratch("ramp-late");
    let lead = 0.5;
    let asset = late_source(&dir, lead, "mov");
    let assets = vec![asset.clone()];
    // The flash and the click are a second into the file's video, `lead + 1` into the file.
    let at = lead + 1.0;
    let mut failures = Vec::new();
    for source_in in [0.0, lead / 2.0, lead + 0.3] {
        for keys in [
            vec![key(0.0, 1.0), key(1.0, 2.0)],
            vec![key(0.0, 0.5), hold(0.8, 2.0), key(2.0, 1.0)],
        ] {
            let mut clip = make_clip(asset.id, source_in, 3.5, 0.0);
            clip.speed_ramp = keys.clone();
            let want = clip.source_to_timeline(at);
            let tag = format!("{source_in}-{}", keys.len());
            let (flash, click) = flash_and_click(&single_clip(clip), &assets, &dir, &tag);
            eprintln!("in {source_in} {keys:?}: flash {flash:?} click {click:?} want {want:.4}");
            let near = |got: Option<f64>, tol: f64| got.is_some_and(|t| (t - want).abs() <= tol);
            // A frame of the picture, and the click within the sound's own early start.
            if !near(flash, 1.0 / LATE_FPS) || !near(click, 0.03) {
                failures.push(format!(
                    "in {source_in} {keys:?}: flash {flash:?} click {click:?} want {want:.4}"
                ));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "out of sync:\n{}", failures.join("\n"));
}

fn single_clip(clip: Clip) -> Timeline {
    crate::engine::test_support::single(vec![clip])
}

/// The scrubbed still of `timeline` at `t`: the number on its picture.
fn still_number(timeline: &Timeline, assets: &[Asset], t: f64) -> Option<u32> {
    let opts = ExportOptions {
        resolution: Some((W, H)),
        fps: Some(FPS),
        ..ExportOptions::default()
    };
    let mut args = build_still_args(timeline, assets, &opts, t, W, None, &StillOutput::RgbPipe).unwrap();
    let _script = externalize_filter_complex(&mut args, "ramp-still").unwrap();
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout.len(), (W * H * 3) as usize, "a still is one frame");
    code(&run.stdout, ROW).checked_sub(1)
}

#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn the_scrubbed_still_shows_the_frame_the_map_names_where_the_export_shows_its_slots_frame() {
    let rig = rig("ramp-still");
    for (label, speed) in [("forward", 1.0), ("reversed", -1.0)] {
        let clip = rig.clip(1.0, 7.5, 0.5, speed, ramp());
        let timeline = rig.timeline(vec![clip.clone()]);
        let exported = rig.shown(&timeline, label);
        let (mut exact, mut off, mut worst, mut compared) = (0, 0, 0i64, 0);
        for k in (0..exported.len()).step_by(3) {
            let t = k as f64 / FPS;
            if t < clip.timeline_start + 0.05 || t > clip.timeline_end() - 0.1 {
                continue;
            }
            let (Some(still), Some(file)) = (still_number(&timeline, &rig.assets(), t), exported[k]) else {
                panic!(
                    "{label}: nothing at {t}: still {:?}, export {:?}",
                    still_number(&timeline, &rig.assets(), t),
                    exported[k]
                );
            };
            // The still decodes the frame the map names (the first at or after its source second); the
            // file shows the last whose slot has come. They are the same picture, or neighbours the
            // ramp's pace apart: never further than the source frames one output frame covers.
            let named = (clip.timeline_to_source(t) * FPS).ceil() as i64;
            assert!(
                (i64::from(still) - named).abs() <= 1,
                "{label} at {t}: still {still}, the map names {named}"
            );
            // A reversed clip's still is a frame past the export's whatever its speed (`-ss` lands on the
            // first frame at or after the time, where the reversed file has played down to the one
            // before): that is a constant speed's behaviour too, and the ramp keeps it.
            let gap = i64::from(still) - i64::from(file) - i64::from(speed < 0.0);
            let pace = clip.speed_at(t - clip.timeline_start).ceil() as i64;
            assert!(
                gap.abs() <= pace,
                "{label} at {t}: still {still}, export {file}, speed {}",
                clip.speed_at(t - clip.timeline_start)
            );
            let gap = gap.abs();
            compared += 1;
            worst = worst.max(gap);
            if gap == 0 {
                exact += 1;
            } else {
                off += 1;
            }
        }
        eprintln!("{label}: {compared} stills, {exact} the export's frame, {off} a neighbour, at most {worst} frames off");
        assert!(compared > 20 && exact * 3 > compared, "{label}");
    }
}

// ---- picture and sound together --------------------------------------------------------------------

const RATE: usize = 48_000;
/// Source seconds between two clicks, and where the first one's middle is.
const CLICK_EVERY: f64 = 0.5;
const CLICK_AT: f64 = 0.25;

/// A source of 8 s whose frames number themselves and whose sound clicks (30 ms of 1.5 kHz under a
/// Hann window) every [`CLICK_EVERY`] seconds from [`CLICK_AT`]. Probed like an import.
fn clicking_source(dir: &Path) -> Asset {
    let video = numbered(dir, "30", 8, MP4).asset.path;
    let audio = dir.join("clicks.wav");
    let out = dir.join("clicking.mov");
    let arg = |p: &Path| p.to_string_lossy().into_owned();
    if !out.exists() {
        let at = CLICK_AT - 0.015;
        let phase = format!("st(0,mod(t-{at},{CLICK_EVERY}))");
        run(&[
            "-f",
            "lavfi",
            "-i",
            &format!(
                "aevalsrc='{phase};if(gte(t,{at})*lt(ld(0),0.03),0.8*sin(2*PI*1500*t)*pow(sin(PI*ld(0)/0.03),2),0)':s={RATE}:d=8"
            ),
            "-c:a",
            "pcm_s16le",
            &arg(&audio),
        ]);
        run(&[
            "-i",
            &video,
            "-i",
            &arg(&audio),
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c",
            "copy",
            &arg(&out),
        ]);
    }
    Project::probe_asset(&out).expect("probe the clicking source")
}

/// The export of `timeline` as `W`x`H` rgb frames at [`FPS`] and mono samples at [`RATE`].
fn frames_and_samples(timeline: &Timeline, assets: &[Asset], dir: &Path, tag: &str) -> (Vec<Option<u32>>, Vec<f32>) {
    let opts = ExportOptions {
        resolution: Some((W, H)),
        fps: Some(FPS),
        ..ExportOptions::default()
    };
    let (v, a) = (dir.join(format!("{tag}.rgb")), dir.join(format!("{tag}.f32")));
    let mut args = build_export_args(timeline, assets, "unused.mkv", &opts).unwrap();
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
        ]
        .map(String::from),
    );
    args.push(v.to_string_lossy().into_owned());
    args.extend(["-map", "[outa]", "-f", "f32le", "-ac", "1", "-ar", "48000"].map(String::from));
    args.push(a.to_string_lossy().into_owned());
    let _script = externalize_filter_complex(&mut args, "ramp-frames").unwrap();
    let run = command(&ffmpeg_bin())
        .arg("-y")
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    let frames: Vec<Option<u32>> = std::fs::read(&v)
        .unwrap()
        .chunks((W * H * 3) as usize)
        .map(|f| code(f, ROW).checked_sub(1))
        .collect();
    let samples: Vec<f32> = std::fs::read(&a)
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    let _ = std::fs::remove_file(&v);
    let _ = std::fs::remove_file(&a);
    (frames, samples)
}

/// The middles of the clicks in `samples`, in seconds: runs of the 2 ms-smoothed envelope over a tenth
/// of its peak (a click stretched by `atempo` breaks into several, so runs closer than half a click
/// at the `slowest` speed are one), each at the envelope's centre of mass.
fn clicks(samples: &[f32], slowest: f64) -> Vec<f64> {
    let width = RATE / 500;
    let mut env = vec![0.0f32; samples.len()];
    let mut acc = 0.0f32;
    for (i, s) in samples.iter().enumerate() {
        acc += s.abs();
        if i >= width {
            acc -= samples[i - width].abs();
        }
        env[i] = acc / width as f32;
    }
    let peak = env.iter().copied().fold(0.0, f32::max);
    let gap = (0.015 / slowest * RATE as f64) as usize;
    let (mut found, mut i) = (Vec::new(), 0);
    while i < env.len() {
        if env[i] > peak * 0.1 {
            let start = i;
            let mut end = i;
            while i < env.len() && i <= end + gap {
                if env[i] > peak * 0.1 {
                    end = i;
                }
                i += 1;
            }
            let (mut weight, mut moment) = (0.0f64, 0.0f64);
            for (j, e) in env.iter().enumerate().take(end + 1).skip(start) {
                weight += f64::from(*e);
                moment += f64::from(*e) * j as f64;
            }
            // The envelope trails the samples by half its width.
            found.push((moment / weight - width as f64 / 2.0) / RATE as f64);
        } else {
            i += 1;
        }
    }
    found
}

/// For each click of the source inside the clip's window: where the map says it plays on the timeline,
/// where the sound has it, and where the picture has the frame it is on (the first output frame that
/// shows that source frame or a later one, when the ramp skips it).
fn sync(clip: &Clip, frames: &[Option<u32>], samples: &[f32]) -> Vec<(f64, f64, Option<f64>)> {
    let slowest = (0..=40)
        .map(|i| clip.speed_at(clip.duration() * f64::from(i) / 40.0))
        .fold(f64::MAX, f64::min);
    let heard = clicks(samples, slowest);
    let mut moments: Vec<f64> = (0..16)
        .map(|i| CLICK_AT + CLICK_EVERY * f64::from(i))
        .filter(|c| *c > clip.source_in + 0.05 && *c < clip.source_out - 0.05)
        .collect();
    // They are heard in the order the clip plays them.
    if clip.is_reversed() {
        moments.reverse();
    }
    assert_eq!(heard.len(), moments.len(), "clicks heard: {heard:?}");
    moments
        .iter()
        .zip(&heard)
        .map(|(c, heard)| {
            let planned = clip.source_to_timeline(*c);
            let n = (c * FPS).floor() as u32;
            let reached = |f: &Option<u32>| f.is_some_and(|f| if clip.is_reversed() { f <= n } else { f >= n });
            let seen = frames.iter().position(reached).map(|k| k as f64 / FPS);
            (planned, *heard, seen)
        })
        .collect()
}

#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn a_ramped_clips_sound_meets_its_picture_at_every_step() {
    let dir = scratch("ramp-sync");
    let src = clicking_source(&dir);
    let filler = filler(&dir);
    let assets = vec![src.clone(), filler.clone()];
    let mut worst = 0.0f64;
    for (label, speed, start, source_in, source_out) in [
        ("forward", 1.0, 0.5, 0.5, 7.5),
        ("forward at the head", 1.0, 0.0, 0.0, 6.0),
        ("reversed", -1.0, 0.5, 0.5, 7.5),
    ] {
        let mut clip = make_clip(src.id, source_in, source_out, start);
        clip.speed = speed;
        clip.speed_ramp = ramp();
        let base = make_clip(filler.id, 0.0, clip.timeline_end() + 0.6, 0.0);
        let timeline = timeline_of(vec![video_track(vec![base]), video_track(vec![clip.clone()])]);
        let (frames, samples) = frames_and_samples(&timeline, &assets, &dir, label);
        let length = samples.len() as f64 / RATE as f64;
        eprintln!(
            "{label}: the clip lasts {:.4} s, the export {length:.4} s, {} frames",
            clip.duration(),
            frames.len()
        );
        let moments = sync(&clip, &frames, &samples);
        for (planned, heard, seen) in &moments {
            let off = (heard - planned) * 1000.0;
            eprintln!(
                "  planned {planned:.3} heard {heard:.3} ({off:+.1} ms) seen {}",
                seen.map_or("-".into(), |s| format!("{s:.3}"))
            );
            worst = worst.max(off.abs());
        }
        for (planned, heard, _) in &moments {
            assert!(
                (heard - planned).abs() < 1.0 / FPS,
                "{label}: a click planned at {planned:.3} is heard at {heard:.3}"
            );
        }
    }
    eprintln!("worst sound offset {worst:.1} ms");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `timeline` exported whole to an mp4, encoder and all: how long the file's picture and sound are, or
/// what ffmpeg said.
fn export_whole(timeline: &Timeline, assets: &[Asset], dir: &Path, tag: &str) -> std::result::Result<(f64, f64, f64), String> {
    let out = dir.join(format!("{tag}.mp4"));
    let opts = ExportOptions {
        resolution: Some((160, 90)),
        fps: Some(10.0),
        ..ExportOptions::default()
    };
    let mut args = build_export_args(timeline, assets, &out.to_string_lossy(), &opts).unwrap();
    let _script = externalize_filter_complex(&mut args, "ramp-export").unwrap();
    let began = std::time::Instant::now();
    let made = command(&ffmpeg_bin())
        .arg("-y")
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    if !made.status.success() {
        let said = String::from_utf8_lossy(&made.stderr);
        let lines: Vec<&str> = said
            .lines()
            .filter(|l| !l.starts_with("[libx264") && !l.starts_with("[aac"))
            .collect();
        return Err(format!(
            "{} ... {}",
            lines.first().copied().unwrap_or_default(),
            lines.iter().rev().take(4).copied().collect::<Vec<_>>().join(" / ")
        ));
    }
    let took = began.elapsed().as_secs_f64();
    let probed = command(&ffprobe_bin())
        .args(["-v", "error", "-show_entries", "stream=codec_type,duration", "-of", "json"])
        .arg(&out)
        .output()
        .unwrap();
    let probed: serde_json::Value = serde_json::from_slice(&probed.stdout).expect("ffprobe json");
    let seconds = |kind: &str| -> f64 {
        probed["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["codec_type"] == kind)
            .and_then(|s| s["duration"].as_str()?.parse().ok())
            .expect("a stream with a duration")
    };
    Ok((seconds("video"), seconds("audio"), took))
}

/// The export written out whole, encoder and all: a ramped clip's file is as long as the cut, picture and
/// sound alike, from a stereo AAC source too, and a ramp of sixteen keys over a long file is not slow.
#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn a_ramped_clip_exports_a_file_as_long_as_the_cut_picture_and_sound() {
    let dir = scratch("ramp-export");
    let long = dir.join("long.mp4");
    run(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=160x90:r=10:d=600",
        "-f",
        "lavfi",
        "-i",
        "aevalsrc='0.3*sin(2*PI*(220+110*floor(t/5))*t)|0.3*sin(2*PI*330*t)':s=44100:d=600",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-c:a",
        "aac",
        "-b:a",
        "96k",
        &long.to_string_lossy(),
    ]);
    let asset = Project::probe_asset(&long).expect("probe the long source");
    let assets = std::slice::from_ref(&asset);
    // The control: a clip at a constant speed that starts a second in, as the ramp does.
    let mut steady = make_clip(asset.id, 2.0, 90.0, 1.0);
    steady.speed = 1.5;
    let timeline = crate::engine::test_support::single(vec![steady]);
    let steady_file = export_whole(&timeline, assets, &dir, "steady");
    eprintln!("a constant 1.5x clip at 1 s: {steady_file:?}");
    // Sixteen keys over a ten-minute window: the most the export nests, and the most steps of sound.
    let mut clip = make_clip(asset.id, 2.0, 590.0, 1.0);
    clip.speed_ramp = (0..16)
        .map(|i| SpeedKey {
            time: i as f64 * 9.0,
            speed: [0.5, 3.0, 1.0, 6.0][i as usize % 4],
            easing: if i % 3 == 1 { RampEasing::Hold } else { RampEasing::Linear },
        })
        .collect();
    let timeline = crate::engine::test_support::single(vec![clip.clone()]);
    let (picture, sound, took) = export_whole(&timeline, assets, &dir, "ramped").expect("a ramped export");
    let cut = clip.timeline_end();
    eprintln!("exported {cut:.1} s of cut in {took:.1} s: picture {picture:.3} s, sound {sound:.3} s");
    assert!((picture - cut).abs() < 0.2, "picture");
    assert!((sound - cut).abs() < 0.2, "sound");
    assert!(took < 120.0, "{took} s");
    // The same ramp played backwards over four minutes of the file: `areverse` holds the window whole and
    // every step reads from it.
    let mut back = make_clip(asset.id, 300.0, 540.0, 0.5);
    back.speed = -1.0;
    back.speed_ramp = clip.speed_ramp;
    let timeline = crate::engine::test_support::single(vec![back.clone()]);
    let (picture, sound, took) = export_whole(&timeline, assets, &dir, "reversed").expect("a reversed ramped export");
    let cut = back.timeline_end();
    eprintln!("reversed: {cut:.1} s of cut in {took:.1} s: picture {picture:.3} s, sound {sound:.3} s");
    assert!((picture - cut).abs() < 0.2 && (sound - cut).abs() < 0.2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// How much of a 440 Hz tone's power is not the tone, in dB, over 50 ms blocks of `samples` (a tone the
/// clip kept at its pitch is a sine of 440 Hz whatever it was stretched by).
fn tone_residual_db(samples: &[f32]) -> f64 {
    let block = RATE / 20;
    let (mut residual, mut power) = (0.0f64, 0.0f64);
    let w = 2.0 * std::f64::consts::PI * 440.0 / RATE as f64;
    for chunk in samples.chunks_exact(block) {
        let (mut c, mut s) = (0.0f64, 0.0f64);
        for (i, x) in chunk.iter().enumerate() {
            c += f64::from(*x) * (w * i as f64).cos();
            s += f64::from(*x) * (w * i as f64).sin();
        }
        let (a, q) = (2.0 * c / block as f64, 2.0 * s / block as f64);
        for (i, x) in chunk.iter().enumerate() {
            let fit = a * (w * i as f64).cos() + q * (w * i as f64).sin();
            residual += (f64::from(*x) - fit).powi(2);
            power += f64::from(*x).powi(2);
        }
    }
    10.0 * (residual / power + 1e-12).log10()
}

/// A steady tone through a ramp and through the same clip at one speed: the pieces' seams against one
/// `atempo` stream (a note keeps its pitch through both; the residual says how clean it is).
#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn a_steady_tone_keeps_its_pitch_through_a_ramps_seams() {
    let dir = scratch("ramp-tone");
    let video = numbered(&dir, "30", 8, MP4).asset.path;
    let tone = dir.join("tone.wav");
    let src = dir.join("tone.mov");
    run(&[
        "-f",
        "lavfi",
        "-i",
        &format!("sine=f=440:r={RATE}:d=8"),
        "-c:a",
        "pcm_s16le",
        &tone.to_string_lossy(),
    ]);
    run(&[
        "-i",
        &video,
        "-i",
        &tone.to_string_lossy(),
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c",
        "copy",
        &src.to_string_lossy(),
    ]);
    let asset = Project::probe_asset(&src).expect("probe");
    let filler = filler(&dir);
    let assets = vec![asset.clone(), filler.clone()];
    let mut results = Vec::new();
    for (label, keys, speed) in [
        ("one atempo at 1.4x", Vec::new(), 1.4),
        ("a gentle ramp 1x to 1.8x over 4 s", vec![key(0.0, 1.0), key(4.0, 1.8)], 1.0),
        ("a ramp 1x to 3x over 2 s", vec![key(0.0, 1.0), key(2.0, 3.0)], 1.0),
        (
            "a hold at 1.4x between 1x ends",
            vec![key(0.0, 1.0), hold(1.0, 1.4), key(3.0, 1.0)],
            1.0,
        ),
    ] {
        let mut clip = make_clip(asset.id, 0.5, 6.5, 0.0);
        clip.speed = speed;
        clip.speed_ramp = keys;
        let base = make_clip(filler.id, 0.0, clip.timeline_end() + 0.6, 0.0);
        let timeline = timeline_of(vec![video_track(vec![base]), video_track(vec![clip.clone()])]);
        let (_, samples) = frames_and_samples(&timeline, &assets, &dir, "tone");
        // Away from the clip's first and last fifth of a second.
        let from = (0.2 * RATE as f64) as usize;
        let to = samples.len() - from;
        let db = tone_residual_db(&samples[from..to]);
        eprintln!("{label}: {db:.1} dB of the tone is not 440 Hz");
        results.push(db);
    }
    // One stream of `atempo` is the floor; the seams stay within a few dB of nothing worse than noise.
    assert!(results[0] < -25.0, "{results:?}");
    assert!(results.iter().all(|db| *db < -15.0), "{results:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A clip of four steps all at one `tempo` (the pieces of the sound without a change of pace), a click
/// every half second of the source: how far from the map each is heard. This is what `tempo_advance` is
/// measured and held to.
#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn a_click_is_heard_where_the_map_says_at_every_tempo() {
    let dir = scratch("ramp-tempos");
    let src = clicking_source(&dir);
    let filler = filler(&dir);
    let assets = vec![src.clone(), filler.clone()];
    for tempo in [0.1, 0.25, 0.5, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 2.0, 3.0, 5.0, 8.0] {
        let (source_in, source_out) = (0.5, if tempo >= 3.0 { 7.5 } else { 4.5 });
        let mut clip = make_clip(src.id, source_in, source_out, 0.0);
        let span = (source_out - source_in) / tempo;
        clip.speed_ramp = (0..4).map(|i| key(span * f64::from(i) / 4.0, tempo)).collect();
        let base = make_clip(filler.id, 0.0, clip.timeline_end() + 0.6, 0.0);
        let timeline = timeline_of(vec![video_track(vec![base]), video_track(vec![clip.clone()])]);
        let (frames, samples) = frames_and_samples(&timeline, &assets, &dir, "tempos");
        let errs: Vec<f64> = sync(&clip, &frames, &samples)
            .iter()
            .map(|(planned, heard, _)| (heard - planned) * 1000.0)
            .collect();
        let (least, most) = (
            errs.iter().copied().fold(f64::MAX, f64::min),
            errs.iter().copied().fold(f64::MIN, f64::max),
        );
        eprintln!(
            "tempo {tempo}: {} clicks, heard {least:+.1} to {most:+.1} ms from the map, mean {:+.1}",
            errs.len(),
            errs.iter().sum::<f64>() / errs.len() as f64
        );
        // Within half a frame of 30 fps from 0.25x to 8x; the crawl of 0.1x is a moment of the source
        // however far it is stretched.
        let tolerance = if tempo < 0.25 { 40.0 } else { 16.0 };
        assert!(
            least.abs() < tolerance && most.abs() < tolerance,
            "tempo {tempo}: {least} to {most}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
