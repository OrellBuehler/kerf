//! The size model held to real files, in depth (`#[ignore]`d: they drive the real `ffmpeg`, and run only on the build the bands were tuned on).
//!
//! CI does not run this suite (`--skip export_size_calibration`): every test starts at `calibrated_here`, which is true on FFmpeg 9 (the pinned
//! build) or with `KERF_SIZE_CALIBRATION=any`, because the sizes an encoder writes are a property of its version. The cheap smoke tests of
//! `export_size_rendered` are what runs everywhere. Rows and the soft / plain / grainy classes are described there.
//!
//! `KERF_FFMPEG=<pinned ffmpeg> KERF_FFPROBE=<pinned ffprobe> cargo test -p kerf-core --no-default-features \
//!  -- --ignored export_size_calibration --test-threads=1`

#![allow(clippy::print_stderr)] // what each case measured, for whoever runs the ignored ones

use super::export_size_rendered::{
    calibrated_here, crf_case, finish, measure, sound_source, source, Class, Scratch, Shot, ALL, NEEDY,
};
use super::*;

/// Constant quality, every codec the dialog offers, at the qualities it offers, for every class: 360p30.
#[test]
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn crf_bands_hold_for_every_codec_and_class() {
    if !calibrated_here("crf_bands_hold_for_every_codec_and_class") {
        return;
    }
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
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn preset_frames_and_rates_hold() {
    if !calibrated_here("preset_frames_and_rates_hold") {
        return;
    }
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
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn bitrate_bands_hold() {
    if !calibrated_here("bitrate_bands_hold") {
        return;
    }
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
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn prores_gif_lossless_and_sound_with_picture_hold() {
    if !calibrated_here("prores_gif_lossless_and_sound_with_picture_hold") {
        return;
    }
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
        // SVT-AV1: `lossless=1` (its `-crf 0` is no lossless), by the mode and by CRF 0, where this build's SVT-AV1 knows it.
        let svt_lossless = svt_av1_lossless_supported();
        for (what, rate_control) in [
            ("svt-av1 lossless", RateControl::Lossless),
            ("svt-av1 CRF 0", RateControl::Crf),
        ] {
            let svt = ExportOptions {
                rate_control,
                ..crf_case("libsvtav1", Container::Mkv, 0, Some("8"))
            };
            if svt_lossless {
                rows.push(measure(dir, what, class.name(), &asset, &svt, true));
            }
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
#[ignore = "needs the ffmpeg binary the bands were tuned on"]
fn sound_bands_hold() {
    if !calibrated_here("sound_bands_hold") {
        return;
    }
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
#[ignore = "needs a hardware encoder, and KERF_SIZE_HARDWARE=1"]
fn a_hardware_encoders_band_holds_when_there_is_one() {
    // Hardware sessions are not for a test to take unasked (a machine's encoder may be in use, and consumer cards count sessions).
    if std::env::var("KERF_SIZE_HARDWARE").as_deref() != Ok("1") {
        eprintln!("a_hardware_encoders_band_holds_when_there_is_one: skipped, set KERF_SIZE_HARDWARE=1 to run it on this machine's encoder");
        return;
    }
    if !calibrated_here("a_hardware_encoders_band_holds_when_there_is_one") {
        return;
    }
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
