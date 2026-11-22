//! The size estimate held to real files (`#[ignore]`d: they drive the real `ffmpeg`, the pinned 9.0.2).
//!
//! `export_size` is a model, and a model nobody measured is a guess. Each case here renders a short cut
//! through the real export (`render_with_progress`, the dialog's own options) and asserts that the file's
//! size is inside what `estimate_size` said before it was written, for three kinds of picture the model is
//! built on:
//!
//! * **soft**: a blurred test pattern, the screen / slide / plain-background end (0.014 bpp at x264 CRF 23);
//! * **plain**: `testsrc2` as it comes (0.098);
//! * **grainy**: `testsrc2` with temporal noise, pre-encoded at the 0.405 bits per pixel per frame a
//!   camera hands over (0.58), so the source is compressed footage and not raw noise.
//!
//! A bitrate-targeted encode that needs the bits (plain, grainy) must also land above the low end, and
//! no encode of any class may pass the top. Raw white noise and sources richer than the grainy one are
//! outside the model on purpose (see `export_size`); this does not test them.
//!
//! Every case prints a row (what was measured, the band, how wide it is) for whoever runs it; the table
//! in `.claude/docs/engine-cli.md` is that output.
//!
//! `KERF_FFMPEG=<pinned ffmpeg> KERF_FFPROBE=<pinned ffprobe> cargo test -p kerf-core --no-default-features \
//!  -- --ignored export_size_rendered`

#![allow(clippy::print_stderr)] // what each case measured, for whoever runs the ignored ones

use std::path::{Path, PathBuf};

use super::*;
use crate::engine::test_support::{
    audio_stream, audio_track, make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded,
};

/// A scratch directory removed when it goes out of scope, pass or fail: the sources are big.
pub(super) struct Scratch(pub(super) PathBuf);

impl Scratch {
    pub(super) fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("kerf-export-size-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Class {
    Soft,
    Plain,
    Grainy,
}

impl Class {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Soft => "soft",
            Self::Plain => "plain",
            Self::Grainy => "grainy",
        }
    }
}

pub(super) const ALL: [Class; 3] = [Class::Soft, Class::Plain, Class::Grainy];
/// The classes that need a target bitrate's bits: a soft picture is allowed to come out far under it.
const NEEDY: [Class; 2] = [Class::Plain, Class::Grainy];

pub(super) fn run(args: &[String]) {
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success(), "ffmpeg {args:?}");
}

/// `secs` of a `class` picture at `w`x`h`, `fps`, as a camera-like file (and a 192 kbps tone when
/// `sound`), described as the asset the engine would have probed.
pub(super) fn source(dir: &Path, class: Class, (w, h): (u32, u32), fps: f64, secs: f64, sound: bool) -> Asset {
    let path = dir.join(format!("{}-{w}x{h}-{fps}-{secs}-{sound}.mp4", class.name()));
    if !path.exists() {
        let graph = match class {
            Class::Soft => format!(
                "testsrc2=s={w}x{h}:r={fps}:d={secs},gblur=sigma={:.2}",
                6.0 * f64::from(h) / 1080.0
            ),
            Class::Plain => format!("testsrc2=s={w}x{h}:r={fps}:d={secs}"),
            Class::Grainy => format!("testsrc2=s={w}x{h}:r={fps}:d={secs},noise=alls=20:allf=t"),
        };
        let mut args: Vec<String> = ["-f", "lavfi", "-i"].map(String::from).to_vec();
        args.push(graph);
        if sound {
            args.extend(["-f", "lavfi", "-i"].map(String::from));
            args.push(format!("sine=f=440:sample_rate=48000:d={secs}"));
        }
        args.extend(["-c:v", "libx264", "-preset", "fast", "-pix_fmt", "yuv420p"].map(String::from));
        if class == Class::Grainy {
            let bps = ((0.405 * f64::from(w) * f64::from(h) * fps) as u64).to_string();
            for (flag, value) in [("-b:v", &bps), ("-maxrate", &bps), ("-bufsize", &bps)] {
                args.push(flag.to_string());
                args.push(value.clone());
            }
        } else {
            args.extend(["-crf", "18"].map(String::from));
        }
        if sound {
            args.extend(["-c:a", "aac", "-b:a", "192k", "-ac", "2"].map(String::from));
        }
        args.push(path.to_string_lossy().into_owned());
        run(&args);
    }
    let mut streams = vec![video_stream(w, h, fps)];
    if sound {
        streams.push(audio_stream(48_000, 2));
    }
    let mut asset = test_asset(streams);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = secs;
    asset
}

/// A tone, noise or silence as a 48 kHz stereo wav.
pub(super) fn sound_source(dir: &Path, kind: &str, secs: f64) -> Asset {
    let path = dir.join(format!("{kind}-{secs}.wav"));
    if !path.exists() {
        let graph = match kind {
            "silence" => format!("anullsrc=r=48000:cl=stereo:d={secs}"),
            "tone" => format!("sine=f=440:sample_rate=48000:d={secs},aformat=channel_layouts=stereo"),
            _ => format!("anoisesrc=r=48000:c=white:a=0.5:d={secs},aformat=channel_layouts=stereo"),
        };
        run(&["-f", "lavfi", "-i", &graph, "-c:a", "pcm_s16le", &path.to_string_lossy()].map(String::from));
    }
    let mut asset = test_asset(vec![audio_stream(48_000, 2)]);
    asset.path = path.to_string_lossy().into_owned();
    asset.duration = secs;
    asset
}

/// What a case measured, and what the estimate said about it before.
struct Row {
    what: String,
    class: &'static str,
    real: u64,
    estimate: SizeEstimate,
    /// Whether the real size is required to be over the low end (it is for a picture that needs the bits).
    holds_low: bool,
}

impl Row {
    fn inside(&self) -> bool {
        self.real <= self.estimate.high_bytes && (!self.holds_low || self.real >= self.estimate.low_bytes)
    }

    fn print(&self) {
        let e = &self.estimate;
        let width = e.high_bytes as f64 / e.low_bytes.max(1) as f64;
        eprintln!(
            "{:<44} {:<6} real {:>11} in {:>11} .. {:>11}  x{:>6.1}  at {:>5.2} of the way  {:?} {}",
            self.what,
            self.class,
            self.real,
            e.low_bytes,
            e.high_bytes,
            width,
            (self.real as f64 / e.low_bytes.max(1) as f64).ln() / width.ln().max(1e-9),
            e.basis,
            if self.inside() { "ok" } else { "OUTSIDE" }
        );
    }
}

/// Export `asset`'s whole length with `opts` and read the file's size next to the estimate made
/// before it was written.
fn measure(dir: &Path, what: &str, class: &'static str, asset: &Asset, opts: &ExportOptions, holds_low: bool) -> Row {
    let clip = make_clip(asset.id, 0.0, asset.duration, 0.0);
    let tl = if asset.streams.iter().any(|s| s.kind == StreamKind::Video) {
        timeline_of(vec![video_track(vec![clip])])
    } else {
        timeline_of(vec![audio_track(vec![clip])])
    };
    let assets = std::slice::from_ref(asset);
    let estimate = estimate_size(&export_size_inputs(&tl, assets, opts), opts);
    let out = dir.join(format!("out-{}.{}", what.replace([' ', '/', ':'], "_"), opts.container.ext()));
    let status = render_with_progress(&tl, assets, &out, opts, &mut |_| {}, &|| false).expect("export");
    assert_eq!(status, RenderStatus::Completed, "{what}");
    let real = std::fs::metadata(&out).expect("the export wrote a file").len();
    let _ = std::fs::remove_file(&out);
    Row {
        what: what.to_string(),
        class,
        real,
        estimate,
        holds_low,
    }
}

fn finish(rows: &[Row]) {
    for row in rows {
        row.print();
    }
    let outside: Vec<String> = rows
        .iter()
        .filter(|r| !r.inside())
        .map(|r| {
            format!(
                "{} {}: {} outside {}..{}",
                r.what, r.class, r.real, r.estimate.low_bytes, r.estimate.high_bytes
            )
        })
        .collect();
    assert!(
        outside.is_empty(),
        "the estimate missed {} of {} files:\n{}",
        outside.len(),
        rows.len(),
        outside.join("\n")
    );
}

/// One cut at its own frame size, rate and length.
struct Shot {
    what: &'static str,
    frame: (u32, u32),
    fps: f64,
    secs: f64,
    opts: ExportOptions,
}

pub(super) fn crf_case(codec: &str, container: Container, crf: u32, preset: Option<&str>) -> ExportOptions {
    ExportOptions {
        container,
        video_codec: Some(codec.into()),
        rate_control: RateControl::Crf,
        crf: Some(crf),
        preset: preset.map(String::from),
        pix_fmt: Some("yuv420p".into()),
        include_audio: false,
        ..ExportOptions::default()
    }
}

/// Constant quality, every codec the dialog offers, at the qualities it offers, for every class: 360p30.
#[test]
#[ignore = "needs the pinned ffmpeg binary"]
fn crf_bands_hold_for_every_codec_and_class() {
    let scratch = Scratch::new("crf");
    let dir = &scratch.0;
    let mut rows = Vec::new();
    let small = (640, 360);
    let cases: Vec<(String, ExportOptions)> = vec![
        (
            "x264 medium CRF 18".into(),
            crf_case("libx264", Container::Mp4, 18, Some("medium")),
        ),
        (
            "x264 medium CRF 23".into(),
            crf_case("libx264", Container::Mp4, 23, Some("medium")),
        ),
        (
            "x264 medium CRF 28".into(),
            crf_case("libx264", Container::Mp4, 28, Some("medium")),
        ),
        (
            "x264 medium CRF 32".into(),
            crf_case("libx264", Container::Mp4, 32, Some("medium")),
        ),
        (
            "x264 no CRF no preset".into(),
            ExportOptions {
                crf: None,
                ..crf_case("libx264", Container::Mp4, 0, None)
            },
        ),
        (
            "x264 ultrafast CRF 23".into(),
            crf_case("libx264", Container::Mp4, 23, Some("ultrafast")),
        ),
        (
            "x264 veryfast CRF 23".into(),
            crf_case("libx264", Container::Mp4, 23, Some("veryfast")),
        ),
        (
            "x264 veryslow CRF 23".into(),
            crf_case("libx264", Container::Mp4, 23, Some("veryslow")),
        ),
        (
            "x264 grain tune CRF 23".into(),
            ExportOptions {
                tune: Some("grain".into()),
                ..crf_case("libx264", Container::Mp4, 23, Some("medium"))
            },
        ),
        (
            "x264 zerolatency tune CRF 23".into(),
            ExportOptions {
                tune: Some("zerolatency".into()),
                ..crf_case("libx264", Container::Mp4, 23, Some("medium"))
            },
        ),
        (
            "x265 medium CRF 18".into(),
            crf_case("libx265", Container::Mp4, 18, Some("medium")),
        ),
        (
            "x265 medium CRF 23".into(),
            crf_case("libx265", Container::Mp4, 23, Some("medium")),
        ),
        (
            "x265 medium CRF 28".into(),
            crf_case("libx265", Container::Mp4, 28, Some("medium")),
        ),
        (
            "x265 ultrafast CRF 23".into(),
            crf_case("libx265", Container::Mp4, 23, Some("ultrafast")),
        ),
        (
            "svt-av1 8 CRF 24".into(),
            crf_case("libsvtav1", Container::Mkv, 24, Some("8")),
        ),
        (
            "svt-av1 8 CRF 30".into(),
            crf_case("libsvtav1", Container::Mkv, 30, Some("8")),
        ),
        (
            "svt-av1 8 CRF 40".into(),
            crf_case("libsvtav1", Container::Mkv, 40, Some("8")),
        ),
        (
            "vp9 cpu-used 4 CRF 24".into(),
            crf_case("libvpx-vp9", Container::Webm, 24, Some("4")),
        ),
        (
            "vp9 cpu-used 4 CRF 31".into(),
            crf_case("libvpx-vp9", Container::Webm, 31, Some("4")),
        ),
        (
            "vp9 cpu-used 8 CRF 38".into(),
            crf_case("libvpx-vp9", Container::Webm, 38, Some("8")),
        ),
    ];
    for class in ALL {
        let asset = source(dir, class, small, 30.0, 4.0, false);
        for (what, opts) in &cases {
            rows.push(measure(dir, what, class.name(), &asset, opts, true));
        }
    }
    finish(&rows);
}

/// The dialog's presets at their own sizes and rates, and the frame rate and size factors on their own.
#[test]
#[ignore = "needs the pinned ffmpeg binary"]
fn preset_frames_and_rates_hold() {
    let scratch = Scratch::new("frames");
    let dir = &scratch.0;
    let mut rows = Vec::new();
    // Web 1080p, Small share 720p, Reels 9:16, AV1 1080p, and the 24 / 15 / 60 fps factors.
    let shot = |what, frame, fps, secs, opts| Shot {
        what,
        frame,
        fps,
        secs,
        opts,
    };
    let medium = |codec: &str, container, crf| crf_case(codec, container, crf, Some("medium"));
    let cases = vec![
        shot(
            "web 1080p x264 CRF 20",
            (1920, 1080),
            30.0,
            3.0,
            medium("libx264", Container::Mp4, 20),
        ),
        shot(
            "share 720p x264 CRF 26 slower",
            (1280, 720),
            30.0,
            4.0,
            crf_case("libx264", Container::Mp4, 26, Some("slower")),
        ),
        shot(
            "reels 1080x1920 x264 CRF 23",
            (1080, 1920),
            30.0,
            2.0,
            medium("libx264", Container::Mp4, 23),
        ),
        shot(
            "x264 CRF 23 at 24 fps",
            (640, 360),
            24.0,
            4.0,
            medium("libx264", Container::Mp4, 23),
        ),
        shot(
            "x264 CRF 23 at 60 fps",
            (640, 360),
            60.0,
            4.0,
            medium("libx264", Container::Mp4, 23),
        ),
        shot(
            "x264 CRF 23 at 15 fps",
            (640, 360),
            15.0,
            4.0,
            medium("libx264", Container::Mp4, 23),
        ),
        shot(
            "x265 CRF 23 at 1080p",
            (1920, 1080),
            30.0,
            3.0,
            medium("libx265", Container::Mp4, 23),
        ),
        shot(
            "svt-av1 CRF 30 at 1080p",
            (1920, 1080),
            30.0,
            3.0,
            crf_case("libsvtav1", Container::Mkv, 30, Some("8")),
        ),
    ];
    for class in ALL {
        for case in &cases {
            let asset = source(dir, class, case.frame, case.fps, case.secs, false);
            rows.push(measure(dir, case.what, class.name(), &asset, &case.opts, true));
        }
    }
    // The export resized away from the footage: 360p footage into a 720p frame. The picture is the
    // footage scaled up (softer than the same picture natively), so only the top is held to it.
    let asset = source(dir, Class::Plain, (640, 360), 30.0, 4.0, false);
    let upscaled = ExportOptions {
        resolution: Some((1280, 720)),
        ..crf_case("libx264", Container::Mp4, 23, Some("medium"))
    };
    rows.push(measure(dir, "x264 CRF 23 360p into 720p", "plain", &asset, &upscaled, false));
    finish(&rows);
}

/// A target bitrate: single and two-pass, with and without sound, and a hard cap.
#[test]
#[ignore = "needs the pinned ffmpeg binary"]
fn bitrate_bands_hold() {
    let scratch = Scratch::new("bitrate");
    let dir = &scratch.0;
    let mut rows = Vec::new();
    let small = (640, 360);
    let bitrate = |rate: RateControl, target: &str| ExportOptions {
        rate_control: rate,
        video_bitrate: Some(target.into()),
        ..crf_case("libx264", Container::Mp4, 23, Some("medium"))
    };
    let cases: Vec<(String, f64, ExportOptions)> = vec![
        ("x264 ABR 600k 20 s".into(), 20.0, bitrate(RateControl::Bitrate, "600k")),
        ("x264 ABR 1M 20 s".into(), 20.0, bitrate(RateControl::Bitrate, "1M")),
        ("x264 ABR 1M 8 s".into(), 8.0, bitrate(RateControl::Bitrate, "1M")),
        ("x264 ABR 1M 40 s".into(), 40.0, bitrate(RateControl::Bitrate, "1M")),
        (
            "x264 two-pass 1M 20 s".into(),
            20.0,
            ExportOptions {
                preset: Some("slow".into()),
                ..bitrate(RateControl::TwoPass, "1M")
            },
        ),
        (
            "x264 ABR 2M capped at 1M 20 s".into(),
            20.0,
            ExportOptions {
                max_rate: Some("1M".into()),
                buf_size: Some("1M".into()),
                ..bitrate(RateControl::Bitrate, "2M")
            },
        ),
        (
            "x265 ABR 1M 20 s".into(),
            20.0,
            ExportOptions {
                video_codec: Some("libx265".into()),
                ..bitrate(RateControl::Bitrate, "1M")
            },
        ),
    ];
    for class in ALL {
        for (what, secs, opts) in &cases {
            let asset = source(dir, class, small, 30.0, *secs, false);
            rows.push(measure(dir, what, class.name(), &asset, opts, NEEDY.contains(&class)));
        }
    }
    // The bitrate of the video plus the sound beside it.
    for class in NEEDY {
        let asset = source(dir, class, small, 30.0, 20.0, true);
        let opts = ExportOptions {
            audio_codec: Some("aac".into()),
            audio_bitrate: Some("192k".into()),
            include_audio: true,
            ..bitrate(RateControl::Bitrate, "1M")
        };
        rows.push(measure(dir, "x264 ABR 1M + aac 192k 20 s", class.name(), &asset, &opts, true));
    }
    finish(&rows);
}

/// ProRes by profile, GIF, lossless, and the constant-quality encoders with sound beside the picture.
#[test]
#[ignore = "needs the pinned ffmpeg binary"]
fn prores_gif_lossless_and_sound_with_picture_hold() {
    let scratch = Scratch::new("other");
    let dir = &scratch.0;
    let mut rows = Vec::new();
    let small = (640, 360);
    for class in ALL {
        let asset = source(dir, class, small, 30.0, 4.0, false);
        for profile in [0u8, 2, 3, 4] {
            let opts = ExportOptions {
                container: Container::Mov,
                video_codec: Some("prores_ks".into()),
                prores_profile: Some(profile),
                audio_codec: Some("pcm_s16le".into()),
                include_audio: false,
                ..ExportOptions::default()
            };
            rows.push(measure(
                dir,
                &format!("prores profile {profile}"),
                class.name(),
                &asset,
                &opts,
                true,
            ));
        }
        let gif = ExportOptions {
            container: Container::Gif,
            video_codec: Some("gif".into()),
            resolution: Some((480, 270)),
            fps: Some(15.0),
            gif_dither: Some("bayer".into()),
            include_audio: false,
            ..ExportOptions::default()
        };
        rows.push(measure(dir, "gif 480x270 15 fps bayer", class.name(), &asset, &gif, true));
        for codec in ["libx264", "libvpx-vp9"] {
            let (container, preset) = if codec == "libvpx-vp9" {
                (Container::Webm, "4")
            } else {
                (Container::Mp4, "medium")
            };
            let opts = ExportOptions {
                rate_control: RateControl::Lossless,
                ..crf_case(codec, container, 0, Some(preset))
            };
            rows.push(measure(dir, &format!("{codec} lossless"), class.name(), &asset, &opts, true));
        }
        // SVT-AV1: `lossless=1` (its `-crf 0` is no lossless), by the mode and by CRF 0.
        for (what, rate_control) in [
            ("svt-av1 lossless", RateControl::Lossless),
            ("svt-av1 CRF 0", RateControl::Crf),
        ] {
            let svt = ExportOptions {
                rate_control,
                ..crf_case("libsvtav1", Container::Mkv, 0, Some("8"))
            };
            rows.push(measure(dir, what, class.name(), &asset, &svt, true));
        }
    }
    // Picture and sound in one file: the audio is on top of the video's band, headers and all.
    for class in ALL {
        let asset = source(dir, class, small, 30.0, 10.0, true);
        for (what, rate) in [("aac 128k", Some("128k")), ("aac 320k", Some("320k")), ("aac default", None)] {
            let opts = ExportOptions {
                audio_codec: Some("aac".into()),
                audio_bitrate: rate.map(String::from),
                include_audio: true,
                ..crf_case("libx264", Container::Mp4, 23, Some("medium"))
            };
            rows.push(measure(
                dir,
                &format!("x264 CRF 23 + {what}"),
                class.name(),
                &asset,
                &opts,
                true,
            ));
        }
        let mkv = ExportOptions {
            audio_codec: Some("libopus".into()),
            audio_bitrate: Some("128k".into()),
            include_audio: true,
            ..crf_case("libsvtav1", Container::Mkv, 30, Some("8"))
        };
        rows.push(measure(dir, "svt-av1 CRF 30 + opus 128k", class.name(), &asset, &mkv, true));
        // No encoder named at all: whatever ffmpeg takes for the container (x264 / AAC, Vorbis, VP9 / Opus).
        for container in [Container::Mp4, Container::Mov, Container::Mkv, Container::Webm] {
            let bare = ExportOptions {
                container,
                ..ExportOptions::default()
            };
            let what = format!("bare .{} (ffmpeg's own encoders)", container.ext());
            rows.push(measure(dir, &what, class.name(), &asset, &bare, true));
        }
    }
    finish(&rows);
}

/// Sound alone: exact for PCM and constant-bitrate MP3, near for the lossy and the lossless codecs.
#[test]
#[ignore = "needs the pinned ffmpeg binary"]
fn sound_bands_hold() {
    let scratch = Scratch::new("sound");
    let dir = &scratch.0;
    let mut rows = Vec::new();
    let audio = |container, codec: Option<&str>, rate: Option<&str>| ExportOptions {
        container,
        audio_codec: codec.map(String::from),
        audio_bitrate: rate.map(String::from),
        ..ExportOptions::default()
    };
    let cases: Vec<(&str, ExportOptions)> = vec![
        ("wav s16", audio(Container::Wav, Some("pcm_s16le"), None)),
        ("wav s24", audio(Container::Wav, Some("pcm_s24le"), None)),
        (
            "wav s24 44.1k",
            ExportOptions {
                audio_sample_rate: Some(44_100),
                ..audio(Container::Wav, Some("pcm_s24le"), None)
            },
        ),
        ("mp3 default", audio(Container::Mp3, Some("libmp3lame"), None)),
        ("mp3 320k", audio(Container::Mp3, Some("libmp3lame"), Some("320k"))),
        ("mp3 192k", audio(Container::Mp3, Some("libmp3lame"), Some("192k"))),
        ("m4a aac default", audio(Container::M4a, Some("aac"), None)),
        ("m4a aac 192k", audio(Container::M4a, Some("aac"), Some("192k"))),
        ("m4a aac 320k", audio(Container::M4a, Some("aac"), Some("320k"))),
        ("m4a alac", audio(Container::M4a, Some("alac"), None)),
        ("flac", audio(Container::Flac, Some("flac"), None)),
        ("bare mp3 container", audio(Container::Mp3, None, None)),
    ];
    for kind in ["silence", "tone", "noise"] {
        let asset = sound_source(dir, kind, 20.0);
        for (what, opts) in &cases {
            rows.push(measure(dir, what, kind, &asset, opts, true));
        }
    }
    finish(&rows);
}

/// A hardware encoder, when this machine has a verified one: the software curve with the wider band
/// has to hold the file, on the three classes. (Nothing runs on a machine without one.)
#[test]
#[ignore = "needs the pinned ffmpeg binary and a hardware encoder"]
fn a_hardware_encoders_band_holds_when_there_is_one() {
    let found = hw_encoders();
    let Some(h264) = ["h264_nvenc", "h264_qsv", "h264_videotoolbox", "h264_amf", "h264_vaapi"]
        .into_iter()
        .find(|id| found.iter().any(|f| f == id))
    else {
        eprintln!("no hardware H.264 encoder verified here; nothing measured");
        return;
    };
    let scratch = Scratch::new("hardware");
    let dir = &scratch.0;
    let mut rows = Vec::new();
    for class in ALL {
        let asset = source(dir, class, (1280, 720), 30.0, 4.0, false);
        for crf in [20, 23, 28] {
            let opts = crf_case(h264, Container::Mp4, crf, None);
            rows.push(measure(dir, &format!("{h264} CRF {crf}"), class.name(), &asset, &opts, true));
        }
    }
    finish(&rows);
}
