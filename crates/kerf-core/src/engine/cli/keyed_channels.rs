//! Per-property channels (B5b), pinned against rendered pictures and samples (`#[ignore]`d: they
//! drive the real `ffmpeg`, on 4.4 and 9.0 alike).
//!
//! A string-level test cannot say that `eq ... eval=frame` reads its expressions at the time of
//! the frame it is given, that FFmpeg 4.4 and 9.0 both accept a quoted expression for every
//! number `eq` has, or that `volume ... eval=frame` after `atempo` plays the curve on the
//! clip's own clock. These render the graphs and hold them to the model:
//!
//! * **colour** — a lossless flat-colour clip with each colour number keyed (alone, all five at
//!   once, eased, held), beside a keyed position, under a keyed zoom, at speed 2 and reversed, at
//!   every frame rate, and cut by a range export. Every output frame's centre pixel is held to
//!   the scrubbed still of the same moment, which draws `Clip::color_at(t)` through a *static*
//!   `eq`: two ways to the same number.
//! * **volume** — a 1 kHz sine whose gain is keyed. At speed 1 the render is the unkeyed render
//!   scaled, sample for sample, by the curve at the start of each 128-sample frame (the filter
//!   holds one gain a frame); at other speeds, reversed, late on the timeline and cut by a range
//!   export, the level over every 10 ms is the curve's, to within what its slope moves in a
//!   frame.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored keyed_channels`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::*;
use crate::clip_timing::ffmpeg_frame_time;
use crate::engine::test_support::{make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded};
use crate::model::{Asset, Clip, Easing, Keyframe, Property, PropertyKey, TimeRange};
use crate::project::Project;

const CW: u32 = 64;
const CH: u32 = 36;
/// The flat picture: a light orange (luma about 172, well above the 128 `contrast` pivots on), so that every colour number has something to act on.
const PICTURE: &str = "0xE6A050";

type Rate = (&'static str, u32, u32);
const R30: Rate = ("30", 30, 1);
const RATES: [Rate; 5] = [("24", 24, 1), ("25", 25, 1), ("29.97", 2997, 100), R30, ("60", 60, 1)];

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-keyed-channels-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn ffmpeg(args: &[&str]) {
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success(), "ffmpeg {args:?}");
}

/// A lossless `secs`-long flat clip at `fps`.
fn flat(dir: &Path, fps: &str, secs: f64) -> Asset {
    let path = dir.join(format!("flat-{fps}-{secs}.mp4"));
    if !path.exists() {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("color=c={PICTURE}:s={CW}x{CH}:r={fps}:d={secs}"),
            "-c:v",
            "libx264",
            "-crf",
            "0",
            "-g",
            "1",
            "-pix_fmt",
            "yuv420p",
            &path.to_string_lossy(),
        ]);
    }
    let mut asset = test_asset(vec![video_stream(CW, CH, fps.parse().unwrap())]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = secs;
    asset
}

fn options(rate: Rate) -> ExportOptions {
    ExportOptions {
        resolution: Some((CW, CH)),
        fps: Some(rate.0.parse().unwrap()),
        ..ExportOptions::default()
    }
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
            fps_mode_flag(),
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-y",
        ]
        .map(String::from),
    );
    args.push(out.to_string_lossy().into_owned());
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::read(&out)
        .unwrap()
        .chunks((CW * CH * 3) as usize)
        .map(<[u8]>::to_vec)
        .collect()
}

/// The scrubbed still of `timeline` at `t`: what the editor shows while cutting.
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

/// The centre pixel.
fn centre(frame: &[u8]) -> [i32; 3] {
    let at = ((CH / 2 * CW + CW / 2) * 3) as usize;
    [i32::from(frame[at]), i32::from(frame[at + 1]), i32::from(frame[at + 2])]
}

fn key(time: f64, value: f64, easing: Easing) -> PropertyKey {
    PropertyKey { time, value, easing }
}

/// One colour number moving, in the shape each is given an ease and a hold along.
fn colour_keys(prop: Property) -> Vec<PropertyKey> {
    let (lo, hi) = match prop {
        Property::Brightness => (-0.3, 0.3),
        Property::Contrast => (0.5, 1.8),
        Property::Saturation => (0.0, 2.0),
        Property::Gamma => (0.5, 2.0),
        Property::Temperature => (-1.0, 1.0),
        _ => unreachable!("not a colour number"),
    };
    vec![
        key(0.0, lo, Easing::EaseInOut),
        key(0.9, hi, Easing::Hold),
        key(1.4, lo, Easing::Linear),
        key(2.3, hi, Easing::Linear),
    ]
}

struct Case {
    name: &'static str,
    rate: Rate,
    /// How the clip is decorated beyond the colour keys.
    decorate: fn(&mut Clip),
    props: &'static [Property],
    range: Option<(f64, f64)>,
    start: f64,
    /// Longest the render may differ from the still, in levels.
    tolerance: i32,
    /// The least the centre pixel has to move over the clip, in levels: the case animates.
    moves: i32,
}

impl Case {
    fn new(name: &'static str, props: &'static [Property]) -> Self {
        Self {
            name,
            rate: R30,
            decorate: |_| {},
            props,
            range: None,
            start: 0.0,
            tolerance: 2,
            moves: 12,
        }
    }
}

const ALL_COLOUR: &[Property] = &Property::COLOR;

fn check(dir: &Path, case: &Case) -> i32 {
    let fps = case.rate.0;
    let shown = 3.0;
    let source = flat(dir, fps, 8.0);
    let speed_span = {
        let mut probe = Clip::new(source.id, 0.0, 1.0, 0.0);
        (case.decorate)(&mut probe);
        shown * probe.speed_mag()
    };
    let mut clip = make_clip(source.id, 1.0, 1.0 + speed_span, case.start);
    (case.decorate)(&mut clip);
    for p in case.props {
        clip.set_property_keys(*p, colour_keys(*p));
    }
    let timeline = timeline_of(vec![video_track(vec![clip.clone()])]);
    let assets = vec![source];
    let mut opts = options(case.rate);
    opts.range = case.range.map(|(start, end)| TimeRange { start, end });
    let tag = format!("{}-{fps}", case.name.replace([' ', '+', ','], "-"));
    let frames = export_frames(&timeline, &assets, &opts, dir, &tag);
    let offset = case.range.map_or(0.0, |r| r.0);
    let (_, num, den) = case.rate;
    let (mut worst, mut low, mut high) = (0, [255; 3], [0; 3]);
    let mut compared = 0;
    for (k, frame) in frames.iter().enumerate() {
        let t = ffmpeg_frame_time(k as u64, num, den) + offset;
        if t < clip.timeline_start || t >= clip.timeline_end() {
            continue;
        }
        let got = centre(frame);
        for c in 0..3 {
            low[c] = low[c].min(got[c]);
            high[c] = high[c].max(got[c]);
        }
        // The still of every fifth frame (and the first): each one is a process.
        if k % 5 != 0 {
            continue;
        }
        let want = centre(&still_frame(&timeline, &assets, &opts_unranged(&opts), t));
        let off = (0..3).map(|c| (got[c] - want[c]).abs()).max().unwrap();
        worst = worst.max(off);
        compared += 1;
        assert!(
            off <= case.tolerance,
            "{}: frame {k} (t = {t:.4}, local {:.4}) is {got:?}, the still of that moment {want:?}",
            case.name,
            t - clip.timeline_start
        );
    }
    let moved = (0..3).map(|c| high[c] - low[c]).max().unwrap();
    assert!(
        moved >= case.moves,
        "{}: the picture only moved {moved} levels ({low:?} .. {high:?}): the keys did nothing",
        case.name
    );
    assert!(compared >= 6, "{}: only {compared} frames compared", case.name);
    worst
}

/// A still is judged on the full timeline: the range only picks what the file renders.
fn opts_unranged(opts: &ExportOptions) -> ExportOptions {
    ExportOptions {
        range: None,
        ..opts.clone()
    }
}

#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn a_keyed_colour_is_the_still_of_the_same_moment_on_every_output_frame() {
    let dir = scratch("colour");
    let mut cases = vec![
        Case::new("brightness", &[Property::Brightness]),
        Case::new("contrast", &[Property::Contrast]),
        Case::new("saturation", &[Property::Saturation]),
        Case::new("gamma", &[Property::Gamma]),
        Case::new("temperature", &[Property::Temperature]),
        Case::new("all five", ALL_COLOUR),
        Case {
            start: 1.25,
            ..Case::new("late on the timeline", ALL_COLOUR)
        },
        Case {
            decorate: |c| c.speed = 2.0,
            start: 0.5,
            ..Case::new("double speed", ALL_COLOUR)
        },
        Case {
            decorate: |c| c.speed = -1.0,
            ..Case::new("reversed", ALL_COLOUR)
        },
        Case {
            decorate: |c| c.speed = 0.5,
            ..Case::new("half speed", &[Property::Brightness, Property::Temperature])
        },
        // The grade is keyed and the picture moves beside it: the chain keeps both.
        Case {
            decorate: |c| {
                c.set_property_keys(
                    Property::PosX,
                    vec![key(0.0, -0.05, Easing::Linear), key(2.0, 0.05, Easing::EaseOut)],
                );
                c.set_property_keys(
                    Property::Opacity,
                    vec![key(0.0, 1.0, Easing::Linear), key(2.0, 0.99, Easing::Linear)],
                );
            },
            // The still takes a constant opacity through an RGB round trip and the file a `geq`.
            tolerance: 3,
            ..Case::new("beside a keyed position and opacity", ALL_COLOUR)
        },
        // A moving zoom runs last, after the grade.
        Case {
            decorate: |c| {
                c.keyframes = vec![
                    Keyframe {
                        time: 0.0,
                        scale: 0.7,
                        pos_x: 0.0,
                        pos_y: 0.0,
                        rotation: 0.0,
                        opacity: 1.0,
                        easing: Easing::Linear,
                    },
                    Keyframe {
                        time: 3.0,
                        scale: 1.4,
                        pos_x: 0.0,
                        pos_y: 0.0,
                        rotation: 0.0,
                        opacity: 1.0,
                        easing: Easing::Linear,
                    },
                ];
            },
            ..Case::new("under a moving zoom", ALL_COLOUR)
        },
        // The static grade beside a keyed one: the keyed number is the only thing that moves.
        Case {
            decorate: |c| {
                c.color.contrast = 1.3;
                c.color.saturation = 0.8;
            },
            ..Case::new("beside a static grade", &[Property::Brightness, Property::Gamma])
        },
        Case {
            range: Some((1.2, 3.0)),
            start: 0.4,
            ..Case::new("range export", ALL_COLOUR)
        },
    ];
    for rate in RATES {
        if rate != R30 {
            cases.push(Case {
                rate,
                ..Case::new("frame rate", ALL_COLOUR)
            });
        }
    }
    for case in &cases {
        let worst = check(&dir, case);
        eprintln!("{} ({} fps): at worst {worst} levels from the still", case.name, case.rate.0);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- volume -------------------------------------------------------------------

/// Four seconds of a 1 kHz sine (lossless PCM, so the level measured is the level made) with a
/// tiny picture, probed like an import.
fn tone(dir: &Path) -> Asset {
    let path = dir.join("tone.mkv");
    if !path.exists() {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc=s=32x18:r=25:d=4",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=48000:duration=4",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "pcm_s16le",
            "-ac",
            "1",
            &path.to_string_lossy(),
        ]);
    }
    Project::probe_asset(&path).expect("probe the test media")
}

/// The audio the export of `timeline` renders, as mono f32 at 48 kHz.
fn export_audio(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, dir: &Path, tag: &str) -> Vec<f32> {
    // An audio container: the graph carries only the mix, so nothing is left unmapped.
    let opts = ExportOptions {
        container: Container::Wav,
        ..opts.clone()
    };
    let out = dir.join(format!("{tag}.f32"));
    let mut args = build_export_args(timeline, assets, "unused.mkv", &opts).unwrap();
    let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
    args.truncate(graph + 1);
    args.extend(["-map", "[outa]", "-f", "f32le", "-ac", "1", "-ar", "48000", "-y"].map(String::from));
    args.push(out.to_string_lossy().into_owned());
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    std::fs::read(&out)
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

fn volume_keys(easing: Easing) -> Vec<PropertyKey> {
    vec![
        key(0.2, 0.2, easing),
        key(1.1, 1.8, Easing::Linear),
        key(1.7, 0.4, Easing::Hold),
        // Off the 128-sample frame grid (1/375 s): a hold's step lands in one frame or the next
        // by a rounding of the frame's time, and nothing here is about which.
        key(2.2013, 1.0, Easing::Linear),
    ]
}

/// The times a hold in `keys` steps at (the next key's).
fn steps(keys: &[PropertyKey]) -> Vec<f64> {
    keys.windows(2)
        .filter(|w| w[0].easing == Easing::Hold)
        .map(|w| w[1].time)
        .collect()
}

/// The sample rate everything here is rendered at.
const SR: f64 = 48_000.0;

#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn a_keyed_volume_is_the_unkeyed_render_scaled_by_the_curve_a_frame_at_a_time() {
    let dir = scratch("volume-exact");
    let asset = tone(&dir);
    let assets = vec![asset.clone()];
    let opts = ExportOptions::default();
    let plain = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, 3.0, 0.0)])]);
    let reference = export_audio(&plain, &assets, &opts, &dir, "reference");
    assert!(reference.len() >= 3 * 48_000 - 64, "{} samples", reference.len());
    let peak = reference.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.05, "the sine rendered: {peak}");
    for easing in [
        Easing::Linear,
        Easing::EaseIn,
        Easing::EaseInOut,
        Easing::Hold,
        Easing::Bezier {
            x1: 0.2,
            y1: 0.9,
            x2: 0.3,
            y2: 1.0,
        },
    ] {
        let mut clip = make_clip(asset.id, 0.0, 3.0, 0.0);
        // A neighbour of the keyed gain: the static one is replaced, not multiplied in.
        clip.volume = 0.123;
        clip.set_property_keys(Property::Volume, volume_keys(easing));
        let keyed = timeline_of(vec![video_track(vec![clip.clone()])]);
        let got = export_audio(&keyed, &assets, &opts, &dir, "keyed");
        assert_eq!(got.len(), reference.len(), "{easing:?}: the length is the clip's");
        let mut worst = 0.0_f64;
        for (i, (g, r)) in got.iter().zip(&reference).enumerate() {
            // One gain for a whole 128-sample frame: the curve where the frame starts.
            let frame_start = (i / 128 * 128) as f64 / SR;
            let want = f64::from(*r) * clip.volume_at(frame_start);
            let off = (f64::from(*g) - want).abs();
            worst = worst.max(off);
            assert!(
                off < 3e-4,
                "{easing:?}: sample {i} (t = {:.5}) is {g}, the curve says {want}",
                i as f64 / SR
            );
        }
        eprintln!("{easing:?}: the render is the scaled reference to within {worst:.1e}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The level over every `window` seconds of `got` against `reference`, as a gain.
fn gains(got: &[f32], reference: &[f32], window: f64) -> Vec<(f64, f64)> {
    let n = (window * SR) as usize;
    let rms = |s: &[f32]| (s.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
    got.chunks_exact(n)
        .zip(reference.chunks_exact(n))
        .enumerate()
        .filter(|(_, (_, r))| rms(r) > 0.01)
        .map(|(i, (g, r))| ((i as f64 + 0.5) * window, rms(g) / rms(r)))
        .collect()
}

#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn a_keyed_volume_plays_on_the_clips_own_clock() {
    let dir = scratch("volume-clock");
    let asset = tone(&dir);
    let assets = vec![asset.clone()];
    let window = 0.01;
    // (name, speed, timeline start, range)
    type Clocked = (&'static str, f64, f64, Option<(f64, f64)>);
    let cases: [Clocked; 6] = [
        ("speed 1", 1.0, 0.0, None),
        ("double speed", 2.0, 0.0, None),
        ("half speed", 0.5, 0.0, None),
        ("reversed", -1.0, 0.0, None),
        ("late on the timeline", 1.0, 1.3, None),
        ("range export", 1.0, 0.6, Some((1.4, 3.1))),
    ];
    for (name, speed, start, range) in cases {
        let span = 2.8 * speed.abs();
        let mut clip = make_clip(asset.id, 0.5, 0.5 + span, start);
        clip.speed = speed;
        let reference_timeline = timeline_of(vec![video_track(vec![clip.clone()])]);
        clip.set_property_keys(Property::Volume, volume_keys(Easing::EaseInOut));
        let keyed = timeline_of(vec![video_track(vec![clip.clone()])]);
        let opts = ExportOptions {
            range: range.map(|(start, end)| TimeRange { start, end }),
            ..ExportOptions::default()
        };
        let reference = export_audio(&reference_timeline, &assets, &opts, &dir, "clock-reference");
        let got = export_audio(&keyed, &assets, &opts, &dir, "clock-keyed");
        let n = got.len().min(reference.len());
        let offset = range.map_or(0.0, |r| r.0);
        let mut checked = 0;
        let mut worst = 0.0_f64;
        for (mid, gain) in gains(&got[..n], &reference[..n], window) {
            let local = mid + offset - clip.timeline_start;
            // Edges are where the mix ramps (`adelay`, the clip's end); inside, the gain is the curve.
            if local < 0.15 || local > clip.duration() - 0.15 {
                continue;
            }
            // A window a hold steps inside holds both gains.
            if steps(&clip.property_keys(Property::Volume))
                .iter()
                .any(|s| (s - local).abs() < window + 0.01)
            {
                continue;
            }
            // The gain is held for a frame, so it lags the curve by up to a frame's worth of its slope.
            let slope = (clip.volume_at(local + 0.005) - clip.volume_at(local - 0.005)).abs() / 0.01;
            let slack = 0.02 + slope * (window / 2.0 + 128.0 / SR);
            let want = clip.volume_at(local);
            let off = (gain - want).abs();
            worst = worst.max(off);
            assert!(
                off <= slack,
                "{name}: at clip time {local:.3} the level is {gain:.3} of the unkeyed render, the curve says {want:.3} (slack {slack:.3})"
            );
            checked += 1;
        }
        assert!(checked > 100, "{name}: only {checked} windows compared");
        eprintln!("{name}: {checked} windows within the curve, at worst {worst:.3}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
