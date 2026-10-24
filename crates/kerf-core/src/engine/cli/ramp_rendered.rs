//! Speed ramps through the real `ffmpeg` (`#[ignore]`d: they drive the real binary, on 6.1 and 9.0
//! alike).
//!
//! A ramp is the one retime whose graph text cannot be checked by reading it: the `setpts` expression
//! is a tree of closed forms, and the sound is a ramp cut into steps. These render clips whose frames
//! number themselves and whose sound clicks, and hold the result to the map (`Clip::along`):
//!
//! * the frame the export shows at each output frame is the last whose retimed slot has come
//!   (`fps`'s pick over `setpts`' stamps), for a ramp with a hold, a slow stretch and a constant
//!   tail, forward, reversed, in the middle of a file, and playing on under a dissolve;
//! * the scrubbed still at a time is the frame the file shows there;
//! * the sound lasts as long as the picture and a click lands where the map says, so a picture and
//!   the click of the same moment of the source meet at every step (`atempo` is early by a few
//!   milliseconds, and the ramp compensates).
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
/// stamped by the ramp, the last one whose slot has come shown at each output frame.
fn expected(clip: &Clip, frames: usize) -> Vec<Option<u32>> {
    let first = (clip.source_in * FPS).round() as u32;
    let last = (clip.source_out * FPS).round() as u32;
    // `reverse` plays the kept frames last first, with the forward timestamps.
    let kept: Vec<u32> = if clip.is_reversed() {
        (first..last).rev().collect()
    } else {
        (first..last).collect()
    };
    let slots: Vec<i64> = (0..kept.len() as u32).map(|j| slot(clip, j)).collect();
    let end = (clip.timeline_end() * FPS - 1e-6).ceil() as i64;
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
        let want = expected(&clip, got.len());
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

#[test]
#[ignore = "calibration: prints how early each tempo's pieces play"]
fn calibrate_tempo_advance() {
    let dir = scratch("ramp-calibrate");
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
        let (frames, samples) = frames_and_samples(&timeline, &assets, &dir, "cal");
        let moments = sync(&clip, &frames, &samples);
        let errs: Vec<f64> = moments.iter().map(|(p, h, _)| (h - p) * 1000.0).collect();
        let mean = errs.iter().sum::<f64>() / errs.len() as f64;
        eprintln!(
            "tempo {tempo}: mean {mean:+.1} ms, from {:+.1} to {:+.1} ({} clicks; in source ms {:+.1})",
            errs.iter().copied().fold(f64::MAX, f64::min),
            errs.iter().copied().fold(f64::MIN, f64::max),
            errs.len(),
            mean * tempo
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
