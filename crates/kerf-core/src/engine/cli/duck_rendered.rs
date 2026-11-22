//! A ducked track through the real `ffmpeg` (`#[ignore]`d): the bed plays to its own end however
//! early the dialogue keying it stops, and still dips while the dialogue speaks, on the
//! compressor and on the speech gate, mono and stereo, in a whole export, in a ranged one, in the
//! mix streamed for the agent and in the meters' pass. A sidechain filter ends with the shorter of
//! its two inputs, so a key that stops at 2 s used to take the 6 s bed with it.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored duck_rendered`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build.)

#![allow(clippy::print_stderr)] // what each case measured, for whoever runs the ignored ones

use std::f64::consts::PI;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::*;
use crate::engine::test_support::{
    audio_stream, audio_track, make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded,
};
use crate::engine::{decode_audio, write_wav, AudioBuffer};
use crate::model::TimeRange;

const RATE: u32 = 48_000;
/// The bed's own pitch and the dialogue's, far enough apart that correlating with one ignores the other.
const BED_HZ: f64 = 220.0;
const VOICE_HZ: f64 = 1000.0;
/// What a sine of amplitude 0.2 reads as through [`level`].
const BED_LEVEL: f64 = 0.2;

/// A scratch directory that goes away with the test, however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("kerf-duck-rendered-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        Scratch(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tone(hz: f64, secs: f64) -> Vec<f32> {
    (0..(secs * f64::from(RATE)) as usize)
        .map(|n| (BED_LEVEL * (2.0 * PI * hz * n as f64 / f64::from(RATE)).sin()) as f32)
        .collect()
}

/// A mono wav of `secs` of `hz`, probed as an asset of that length.
fn wav(dir: &Path, name: &str, hz: f64, secs: f64) -> Asset {
    let path = dir.join(name);
    write_wav(&path, &AudioBuffer::new(RATE, 1, tone(hz, secs))).unwrap();
    let mut a = test_asset(vec![audio_stream(RATE, 1)]);
    a.path = path.to_string_lossy().into_owned();
    a.duration = secs;
    a
}

/// The amplitude of the `hz` component of `samples` between `from` and `to` seconds, by correlating
/// with it: what a 0.2 sine reads as 0.2, and any other pitch in the window reads as nothing.
fn level(samples: &[f32], hz: f64, from: f64, to: f64) -> f64 {
    let (a, b) = ((from * f64::from(RATE)) as usize, (to * f64::from(RATE)) as usize);
    let (mut re, mut im) = (0.0, 0.0);
    for (n, x) in samples.iter().enumerate().take(b).skip(a) {
        let w = 2.0 * PI * hz * n as f64 / f64::from(RATE);
        re += f64::from(*x) * w.cos();
        im += f64::from(*x) * w.sin();
    }
    2.0 * (re * re + im * im).sqrt() / (b - a) as f64
}

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

/// The two ways a track ducks: the compressor, and the speech gate at -12 dB.
const MODES: [(&str, Option<f64>); 2] = [("compressor", None), ("speech gate", Some(-12.0))];

/// Two seconds of dialogue on a track of its own and a six-second bed under it on a ducked one:
/// the bed outlives its key by four seconds.
fn dialogue_over_a_bed(dir: &Path, depth: Option<f64>) -> (Timeline, Vec<Asset>) {
    let bed = wav(dir, "bed.wav", BED_HZ, 6.0);
    let voice = wav(dir, "voice.wav", VOICE_HZ, 2.0);
    let mut ducked = audio_track(vec![make_clip(bed.id, 0.0, 6.0, 0.0)]);
    ducked.duck = true;
    let mut tl = timeline_of(vec![audio_track(vec![make_clip(voice.id, 0.0, 2.0, 0.0)]), ducked]);
    tl.master.duck_depth_db = depth;
    (tl, vec![voice, bed])
}

/// What `tl` exports as a wav of `channels` (over `range`), decoded to `channels` interleaved.
fn export_wav(dir: &Path, tl: &Timeline, assets: &[Asset], channels: u16, range: Option<TimeRange>, tag: &str) -> AudioBuffer {
    let out = dir.join(format!("{tag}.wav"));
    let opts = ExportOptions {
        container: Container::Wav,
        video_codec: None,
        audio_codec: Some("pcm_s16le".into()),
        audio_sample_rate: Some(RATE),
        audio_channels: Some(channels),
        range,
        ..Default::default()
    };
    render_with(tl, assets, &out, &opts).unwrap();
    let mix = decode_audio(&out, RATE, channels).unwrap();
    let _ = std::fs::remove_file(&out);
    mix
}

fn channel(mix: &AudioBuffer, ch: usize) -> Vec<f32> {
    mix.samples
        .iter()
        .skip(ch)
        .step_by(usize::from(mix.channels))
        .copied()
        .collect()
}

/// The bed on its own, on a track that does not duck and with nothing to key it: what the bed is
/// at in a delivery of `channels` (a mono source folds up to stereo a few dB down, so its level is
/// measured, not assumed).
fn bed_alone(dir: &Path, channels: u16, range: Option<TimeRange>) -> AudioBuffer {
    let (tl, assets) = dialogue_over_a_bed(dir, None);
    let mut alone = timeline_of(vec![tl.tracks[1].clone()]);
    alone.tracks[0].duck = false;
    export_wav(dir, &alone, &assets, channels, range, "alone")
}

/// The bed's level over a span the dialogue is on (`during`) and one it has left (`after`), on
/// every channel of `mix`, against `alone`: `during` sits at least `min_dip_db` under the bed's own
/// level, `after` is the bed at its own level again (within half a dB).
fn assert_dips_then_recovers(
    label: &str,
    mix: &AudioBuffer,
    alone: &AudioBuffer,
    during: (f64, f64),
    after: (f64, f64),
    min_dip_db: f64,
) {
    for ch in 0..usize::from(mix.channels) {
        let (ducked, own) = (channel(mix, ch), channel(alone, ch));
        let dip = level(&ducked, BED_HZ, during.0, during.1) / level(&own, BED_HZ, during.0, during.1);
        let back = level(&ducked, BED_HZ, after.0, after.1) / level(&own, BED_HZ, after.0, after.1);
        eprintln!(
            "{label} ch{ch}: the bed is {:.2} dB under the dialogue ({:.1} to {:.1} s) and {:.2} dB off its own level after it ({:.1} to {:.1} s)",
            db(dip),
            during.0,
            during.1,
            db(back),
            after.0,
            after.1
        );
        assert!(
            db(back).abs() < 0.5,
            "{label} ch{ch}: the bed is back at its own level once the dialogue stops ({} dB off)",
            db(back)
        );
        assert!(
            db(dip) < -min_dip_db,
            "{label} ch{ch}: the bed dips under the dialogue ({} dB)",
            db(dip)
        );
    }
}

#[test]
#[ignore = "drives the ffmpeg binary"]
fn a_ducked_bed_plays_to_its_own_end_after_its_key_stops() {
    let scratch = Scratch::new();
    for (mode, depth) in MODES {
        for channels in [1u16, 2] {
            let (tl, assets) = dialogue_over_a_bed(scratch.path(), depth);
            let mix = export_wav(scratch.path(), &tl, &assets, channels, None, "whole");
            let label = format!("{mode}, {channels} ch");
            eprintln!(
                "{label}: {:.3} s ({} frames; the timeline is {:.3} s)",
                mix.frames() as f64 / f64::from(RATE),
                mix.frames(),
                tl.duration()
            );
            assert_eq!(mix.frames(), 6 * RATE as usize, "{label}: the bed plays to its end");
            // 4 s to 5 s: two seconds after the dialogue, past the compressor's 400 ms release; and the
            // last half second is the bed, not silence.
            let alone = bed_alone(scratch.path(), channels, None);
            assert_dips_then_recovers(&label, &mix, &alone, (0.5, 1.5), (4.0, 5.0), 6.0);
            assert_dips_then_recovers(&label, &mix, &alone, (0.5, 1.5), (5.5, 5.95), 6.0);
        }
    }
}

#[test]
#[ignore = "drives the ffmpeg binary"]
fn a_ranged_export_keeps_the_ducked_bed_to_the_end_of_the_range() {
    let scratch = Scratch::new();
    // 1 s to 5.5 s: the range opens one second into the dialogue and ends in the bed.
    let range = TimeRange { start: 1.0, end: 5.5 };
    for (mode, depth) in MODES {
        let (tl, assets) = dialogue_over_a_bed(scratch.path(), depth);
        let mix = export_wav(scratch.path(), &tl, &assets, 2, Some(range), "ranged");
        let alone = bed_alone(scratch.path(), 2, Some(range));
        let label = format!("{mode}, ranged");
        eprintln!(
            "{label}: {:.3} s ({} frames; the range is 4.5 s)",
            mix.frames() as f64 / f64::from(RATE),
            mix.frames()
        );
        assert_eq!(
            mix.frames(),
            (4.5 * f64::from(RATE)) as usize,
            "{label}: the range plays to its end"
        );
        // The dialogue's second second is the range's first; the bed is clear of it from 1 s on.
        assert_dips_then_recovers(&label, &mix, &alone, (0.2, 0.8), (2.5, 3.5), 6.0);
    }
}

#[test]
#[ignore = "drives the ffmpeg binary"]
fn the_mix_the_agent_hears_and_the_meters_read_run_to_the_end_of_the_bed() {
    let scratch = Scratch::new();
    for (mode, depth) in MODES {
        let (tl, assets) = dialogue_over_a_bed(scratch.path(), depth);
        let mut pcm: Vec<f32> = Vec::new();
        let secs = stream_mix_pcm(&tl, &assets, None, RATE, 1, &|| false, &mut |f| pcm.extend_from_slice(f)).unwrap();
        eprintln!("{mode}: the streamed mix is {} frames for {secs:.3} s", pcm.len());
        assert_eq!(
            pcm.len(),
            6 * RATE as usize,
            "{mode}: the streamed mix plays the bed to its end"
        );
        let tail = level(&pcm, BED_HZ, 4.0, 5.0);
        assert!(db(tail / BED_LEVEL).abs() < 0.5, "{mode}: the bed after its key ({tail})");

        // `short_term_max_lufs` is `None` for a span under three seconds: a mix the sidechain cut
        // off at the dialogue's 2 s has no full window to read.
        let levels = mix_levels(&tl, &assets, None, false, &|| false).unwrap();
        let master = levels.master.expect("a mix with sound has a master reading");
        eprintln!("{mode}: master {master:?}");
        assert!(
            master.short_term_max_lufs.is_some(),
            "{mode}: the meters read past 3 s of mix"
        );
    }
}

/// The duration ffprobe reads off the first stream of `kind` (`v` / `a`) of `path`.
fn stream_secs(path: &Path, kind: &str) -> f64 {
    let run = command(&ffprobe_bin())
        .args(["-v", "error", "-select_streams", &format!("{kind}:0")])
        .args(["-show_entries", "stream=duration", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    String::from_utf8_lossy(&run.stdout)
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("no {kind} duration: {}", String::from_utf8_lossy(&run.stderr)))
}

/// Two seconds of picture with its own sound, 64x36 at 30 fps: the dialogue, to go on a video track.
fn talking_head(dir: &Path) -> Asset {
    let talk = dir.join("talk.mp4");
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "color=c=gray:s=64x36:r=30:d=2"])
        .args(["-f", "lavfi", "-i", &format!("sine=f={VOICE_HZ}:r={RATE}:d=2")])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(&talk)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());
    let mut voice = test_asset(vec![video_stream(64, 36, 30.0), audio_stream(RATE, 1)]);
    voice.path = talk.to_string_lossy().into_owned();
    voice.duration = 2.0;
    voice
}

#[test]
#[ignore = "drives the ffmpeg binary"]
fn a_cut_with_a_picture_keeps_its_bed_as_long_as_its_picture() {
    let scratch = Scratch::new();
    let dir = scratch.path();
    let voice = talking_head(dir);
    let bed = wav(dir, "bed.wav", BED_HZ, 6.0);

    for (mode, depth) in MODES {
        let mut ducked = audio_track(vec![make_clip(bed.id, 0.0, 6.0, 0.0)]);
        ducked.duck = true;
        let mut tl = timeline_of(vec![video_track(vec![make_clip(voice.id, 0.0, 2.0, 0.0)]), ducked]);
        tl.master.duck_depth_db = depth;
        let out = dir.join("cut.mp4");
        let opts = ExportOptions {
            resolution: Some((64, 36)),
            fps: Some(30.0),
            ..ExportOptions::default()
        };
        render_with(&tl, &[voice.clone(), bed.clone()], &out, &opts).unwrap();
        let (picture, sound) = (stream_secs(&out, "v"), stream_secs(&out, "a"));
        eprintln!("{mode}: the picture is {picture:.3} s, the sound {sound:.3} s");
        let _ = std::fs::remove_file(&out);
        assert!(
            (picture - 6.0).abs() < 0.1,
            "{mode}: the picture runs the timeline ({picture})"
        );
        assert!(
            (sound - picture).abs() < 0.1,
            "{mode}: the sound ({sound} s) plays as long as the picture ({picture} s)"
        );
    }
}

#[test]
#[ignore = "drives the ffmpeg binary"]
fn every_file_of_a_multi_format_export_keeps_its_bed_as_long_as_its_picture() {
    let scratch = Scratch::new();
    let dir = scratch.path();
    let voice = talking_head(dir);
    let bed = wav(dir, "bed.wav", BED_HZ, 6.0);
    let shapes = [Delivery::new(64, 36, Fit::Contain), Delivery::new(36, 36, Fit::Cover)];
    for (mode, depth) in MODES {
        let mut ducked = audio_track(vec![make_clip(bed.id, 0.0, 6.0, 0.0)]);
        ducked.duck = true;
        let mut tl = timeline_of(vec![video_track(vec![make_clip(voice.id, 0.0, 2.0, 0.0)]), ducked]);
        tl.master.duck_depth_db = depth;
        let assets = [voice.clone(), bed.clone()];
        for together in [false, true] {
            let tag = if together { "together" } else { "apart" };
            let variants: Vec<ExportVariant> = shapes
                .iter()
                .map(|d| ExportVariant::beside(&dir.join(format!("{tag}.mp4")), *d))
                .collect();
            let run = render_variants(
                &tl,
                &assets,
                &variants,
                &ExportOptions::default(),
                together,
                &mut |_| {},
                &|| false,
            )
            .expect("render the variants");
            assert_eq!(
                (run.status, run.completed, run.together),
                (RenderStatus::Completed, 2, together)
            );
            for v in &variants {
                let (picture, sound) = (stream_secs(&v.output, "v"), stream_secs(&v.output, "a"));
                eprintln!(
                    "{mode}, {tag}, {}: the picture is {picture:.3} s, the sound {sound:.3} s",
                    v.output.display()
                );
                assert!((picture - 6.0).abs() < 0.1, "{mode}, {tag}: the picture ({picture})");
                assert!(
                    (sound - picture).abs() < 0.1,
                    "{mode}, {tag}: the sound ({sound} s) plays as long as the picture ({picture} s)"
                );
                let _ = std::fs::remove_file(&v.output);
            }
        }
    }
}
