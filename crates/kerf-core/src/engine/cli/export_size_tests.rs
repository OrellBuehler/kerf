//! The size estimate, held to its own arithmetic (pure: no ffmpeg). `export_size_rendered` holds the
//! bands to real files.

use super::*;
use crate::engine::test_support::{
    audio_stream, audio_track, av_asset, img_asset, make_clip, single, test_asset, timeline_of, video_stream, video_track,
};
use uuid::Uuid;

fn frame(width: u32, height: u32, fps: f64) -> ExportedFrame {
    ExportedFrame {
        width,
        height,
        fps,
        fps_source: FpsSource::Options,
    }
}

const STEREO: SizeSound = SizeSound {
    sample_rate: 48_000,
    channels: 2,
};

fn video(secs: f64, w: u32, h: u32, fps: f64) -> SizeInputs {
    SizeInputs {
        duration_secs: secs,
        picture: Some(frame(w, h, fps)),
        sound: None,
    }
}

fn x264(crf: u32) -> ExportOptions {
    ExportOptions {
        video_codec: Some("libx264".into()),
        crf: Some(crf),
        include_audio: false,
        ..ExportOptions::default()
    }
}

fn coded(codec: &str, crf: Option<u32>) -> ExportOptions {
    ExportOptions {
        container: if codec == "libvpx-vp9" {
            Container::Webm
        } else {
            Container::Mp4
        },
        video_codec: Some(codec.into()),
        crf,
        include_audio: false,
        ..ExportOptions::default()
    }
}

fn est(inputs: &SizeInputs, opts: &ExportOptions) -> SizeEstimate {
    estimate_size(inputs, opts)
}

/// `high / low` of a bounded estimate.
fn width_of(e: &SizeEstimate) -> f64 {
    e.high_bytes as f64 / e.low_bytes.max(1) as f64
}

// ---- arithmetic ---------------------------------------------------------------------------------

#[test]
fn wav_is_rate_times_channels_times_width_times_length() {
    // Measured: 20 s of stereo 48 kHz s16 is 3_840_078 bytes, s24 5_760_102.
    let sound = SizeInputs {
        duration_secs: 20.0,
        picture: None,
        sound: Some(STEREO),
    };
    for (codec, measured) in [("pcm_s16le", 3_840_078u64), ("pcm_s24le", 5_760_102)] {
        let opts = ExportOptions {
            container: Container::Wav,
            audio_codec: Some(codec.into()),
            ..ExportOptions::default()
        };
        let e = est(&sound, &opts);
        assert_eq!(e.basis, SizeBasis::Exact);
        assert!(e.low_bytes <= measured && measured <= e.high_bytes, "{codec}: {e:?}");
        assert!(e.high_bytes - e.low_bytes < 1_000, "headers only: {e:?}");
    }
    // Mono is half, and the sample rate counts.
    let mono = SizeInputs {
        sound: Some(SizeSound {
            sample_rate: 44_100,
            channels: 1,
        }),
        ..sound
    };
    let opts = ExportOptions {
        container: Container::Wav,
        audio_codec: Some("pcm_s16le".into()),
        ..ExportOptions::default()
    };
    let e = est(&mono, &opts);
    assert!(
        e.low_bytes >= 44_100 * 2 * 20 && e.high_bytes < 44_100 * 2 * 20 + 1_000,
        "{e:?}"
    );
}

#[test]
fn constant_bitrate_mp3_is_its_bitrate_times_the_length() {
    // Measured: 20 s at the 128k default is 320_876 bytes, at 320k 802_604.
    let sound = SizeInputs {
        duration_secs: 20.0,
        picture: None,
        sound: Some(STEREO),
    };
    for (rate, measured) in [(None, 320_876u64), (Some("320k"), 802_604)] {
        let opts = ExportOptions {
            container: Container::Mp3,
            audio_codec: Some("libmp3lame".into()),
            audio_bitrate: rate.map(String::from),
            ..ExportOptions::default()
        };
        let e = est(&sound, &opts);
        assert_eq!(e.basis, SizeBasis::Exact);
        assert!(e.low_bytes <= measured && measured <= e.high_bytes, "{rate:?}: {e:?}");
        assert!(width_of(&e) < 1.05, "{e:?}");
    }
    // A bitrate MP3 does not have gets snapped: honest about it.
    let opts = ExportOptions {
        container: Container::Mp3,
        audio_codec: Some("libmp3lame".into()),
        audio_bitrate: Some("150k".into()),
        ..ExportOptions::default()
    };
    assert_eq!(est(&sound, &opts).basis, SizeBasis::Targeted);
}

#[test]
fn a_target_bitrate_is_arithmetic_within_the_rate_controls_tolerance() {
    let opts = ExportOptions {
        rate_control: RateControl::Bitrate,
        video_bitrate: Some("8M".into()),
        ..x264(23)
    };
    let e = est(&video(60.0, 1920, 1080, 30.0), &opts);
    assert_eq!(e.basis, SizeBasis::Targeted);
    let nominal = 8_000_000.0 * 60.0 / 8.0;
    assert!((e.low_bytes as f64) < nominal && nominal < e.high_bytes as f64, "{e:?}");
    // The top is the target plus the settling overshoot of a minute (about 12%), not a multiple.
    assert!(e.high_bytes as f64 / nominal < 1.2, "{e:?}");
    assert!(e.low_bytes as f64 / nominal > 0.5, "{e:?}");
    // The CRF the options also carry does not enter.
    let other = ExportOptions {
        crf: Some(40),
        ..opts.clone()
    };
    assert_eq!(est(&video(60.0, 1920, 1080, 30.0), &other), e);
    // Two-pass reads the same.
    let two = ExportOptions {
        rate_control: RateControl::TwoPass,
        ..opts
    };
    assert_eq!(est(&video(60.0, 1920, 1080, 30.0), &two), e);
}

#[test]
fn a_short_cut_may_overshoot_its_target_and_a_long_one_may_not() {
    // x264 on a grainy 1080p source at 8 Mbps landed 1.71x at 5 s, 1.28x at 10 s, 1.10x at 20 s and
    // 1.04x at 40 s of its target; the band keeps every one of them below its top.
    let opts = ExportOptions {
        rate_control: RateControl::Bitrate,
        video_bitrate: Some("8M".into()),
        ..x264(23)
    };
    for (secs, measured) in [(5.0, 1.71), (10.0, 1.28), (20.0, 1.10), (40.0, 1.04)] {
        let e = est(&video(secs, 1920, 1080, 30.0), &opts);
        let nominal = 8_000_000.0 * secs / 8.0;
        assert!(e.high_bytes as f64 >= nominal * measured, "{secs} s: {e:?}");
    }
    let short = est(&video(0.5, 1920, 1080, 30.0), &opts);
    let nominal = 8_000_000.0 * 0.5 / 8.0;
    // Capped at 3x of the video; the container's headers ride on top of a half-second file.
    assert!(short.high_bytes as f64 / nominal <= 3.1, "capped: {short:?}");
    let long = est(&video(3_600.0, 1920, 1080, 30.0), &opts);
    assert!(long.high_bytes as f64 / (8_000_000.0 * 3_600.0 / 8.0) < 1.12, "{long:?}");
}

#[test]
fn a_maxrate_below_the_target_is_the_target() {
    let opts = ExportOptions {
        rate_control: RateControl::Bitrate,
        video_bitrate: Some("8M".into()),
        max_rate: Some("4M".into()),
        ..x264(23)
    };
    let capped = est(&video(60.0, 1920, 1080, 30.0), &opts);
    let plain = est(
        &video(60.0, 1920, 1080, 30.0),
        &ExportOptions {
            max_rate: None,
            video_bitrate: Some("4M".into()),
            ..opts.clone()
        },
    );
    assert_eq!(capped, plain);
    // A cap above the target changes nothing.
    let loose = ExportOptions {
        max_rate: Some("16M".into()),
        ..opts
    };
    assert!(est(&video(60.0, 1920, 1080, 30.0), &loose).high_bytes > capped.high_bytes);
}

#[test]
fn a_bitrate_token_is_read_like_ffmpegs() {
    assert_eq!(bitrate_bps("8M"), Some(8e6));
    assert_eq!(bitrate_bps("2500k"), Some(2.5e6));
    assert_eq!(bitrate_bps("800000"), Some(8e5));
    assert_eq!(bitrate_bps(" 1.5M "), Some(1.5e6));
    for bad in ["", "M", "-3M", "0", "8 M", "fast", "NaN", "inf", "1e400"] {
        assert_eq!(bitrate_bps(bad), None, "{bad:?}");
    }
}

#[test]
fn video_and_sound_add_and_the_loosest_basis_wins() {
    let inputs = SizeInputs {
        sound: Some(STEREO),
        ..video(30.0, 1280, 720, 30.0)
    };
    let both = ExportOptions {
        rate_control: RateControl::Bitrate,
        video_bitrate: Some("4M".into()),
        audio_codec: Some("aac".into()),
        audio_bitrate: Some("192k".into()),
        include_audio: true,
        ..x264(23)
    };
    let e = est(&inputs, &both);
    let video_only = est(
        &video(30.0, 1280, 720, 30.0),
        &ExportOptions {
            include_audio: false,
            ..both.clone()
        },
    );
    let aac = 192_000.0 * 30.0 / 8.0;
    assert!(
        e.high_bytes as f64 >= video_only.high_bytes as f64 + aac,
        "{e:?} {video_only:?}"
    );
    assert_eq!(e.basis, SizeBasis::Targeted);
    // Constant quality is the loosest of the parts.
    let crf = ExportOptions {
        rate_control: RateControl::Crf,
        ..both.clone()
    };
    assert_eq!(est(&inputs, &crf).basis, SizeBasis::Modelled);
    // Exact sound beside exact-less video does not make it exact.
    let pcm = ExportOptions {
        audio_codec: Some("pcm_s16le".into()),
        container: Container::Mov,
        ..both
    };
    assert_eq!(est(&inputs, &pcm).basis, SizeBasis::Targeted);
    // And a muted export drops the sound from the sum.
    assert_eq!(
        est(
            &inputs,
            &ExportOptions {
                include_audio: false,
                ..pcm
            }
        ),
        video_only_in(&inputs)
    );
}

/// The estimate of `inputs` with its sound dropped by the options, for the mov / pcm case above.
fn video_only_in(inputs: &SizeInputs) -> SizeEstimate {
    let opts = ExportOptions {
        container: Container::Mov,
        rate_control: RateControl::Bitrate,
        video_bitrate: Some("4M".into()),
        audio_codec: Some("pcm_s16le".into()),
        include_audio: false,
        ..x264(23)
    };
    est(&SizeInputs { sound: None, ..*inputs }, &opts)
}

// ---- constant quality ---------------------------------------------------------------------------

#[test]
fn constant_quality_is_a_range_that_falls_with_the_crf() {
    let inputs = video(60.0, 1920, 1080, 30.0);
    let mut last: Option<SizeEstimate> = None;
    for crf in (14..=38).step_by(4) {
        let e = est(&inputs, &x264(crf));
        assert_eq!(e.basis, SizeBasis::Modelled);
        assert!(e.low_bytes < e.high_bytes, "{crf}: {e:?}");
        if let Some(prev) = &last {
            assert!(
                e.low_bytes < prev.low_bytes && e.high_bytes < prev.high_bytes,
                "{crf}: {e:?} after {prev:?}"
            );
        }
        last = Some(e);
    }
}

#[test]
fn the_band_is_wide_and_says_what_it_depends_on() {
    // The dialog's Web 1080p default: x264 CRF 20, 1080p30, medium.
    let e = est(&video(60.0, 1920, 1080, 30.0), &x264(20));
    let width = width_of(&e);
    // Wide on purpose: the footage decides. Not unbounded: a band over 200x says nothing.
    assert!((20.0..200.0).contains(&width), "{width}: {e:?}");
    assert!(e.note.contains("detail"), "{}", e.note);
    assert!(
        e.note.contains("bitrate"),
        "points at the predictable alternative: {}",
        e.note
    );
    // The measured 1080p30 CRF 20 classes (soft 0.0189, plain 0.133, grainy 0.775 bpp) are inside.
    let bytes = |bpp: f64| bpp * 1920.0 * 1080.0 * 1800.0 / 8.0;
    for (name, bpp) in [("soft", 0.0189), ("plain", 0.1327), ("grainy", 0.7747)] {
        let b = bytes(bpp);
        assert!(e.low_bytes as f64 <= b && b <= e.high_bytes as f64, "{name}: {b} in {e:?}");
    }
}

#[test]
fn a_bigger_frame_and_a_slower_rate_cost_more() {
    let at = |w, h, fps| est(&video(60.0, w, h, fps), &x264(23));
    let hd = at(1920, 1080, 30.0);
    let uhd = at(3840, 2160, 30.0);
    let small = at(1280, 720, 30.0);
    assert!(small.high_bytes < hd.high_bytes && hd.high_bytes < uhd.high_bytes);
    assert!(small.low_bytes < hd.low_bytes && hd.low_bytes < uhd.low_bytes);
    // 4K soft footage costs fewer bits per pixel than 1080p's, so the floor rises less than 4x.
    assert!((uhd.low_bytes as f64) < 4.0 * hd.low_bytes as f64 * 0.75, "{uhd:?} {hd:?}");
    // Twice the frames of the same picture is not twice the bytes.
    let fast = at(1920, 1080, 60.0);
    assert!(fast.high_bytes > hd.high_bytes && (fast.high_bytes as f64) < 2.0 * hd.high_bytes as f64);
    assert!(fast.low_bytes > hd.low_bytes && (fast.low_bytes as f64) < 2.0 * hd.low_bytes as f64);
    // Longer is proportionally bigger, to within the container's fixed headers.
    let short = est(&video(10.0, 1920, 1080, 30.0), &x264(23));
    let long = est(&video(100.0, 1920, 1080, 30.0), &x264(23));
    let ratio = long.high_bytes as f64 / short.high_bytes as f64;
    assert!((9.0..10.1).contains(&ratio), "{ratio}");
}

#[test]
fn the_measured_classes_sit_inside_every_codecs_band_at_the_dialogs_qualities() {
    // (codec, CRF, soft, plain, grainy) in bits per pixel per frame, 1080p30, as measured.
    let table = [
        ("libx264", 18, 0.0226, 0.1550, 0.9550),
        ("libx264", 23, 0.0140, 0.0984, 0.5809),
        ("libx264", 28, 0.0087, 0.0464, 0.3620),
        ("libx264", 32, 0.0064, 0.0276, 0.1186),
        ("libx265", 18, 0.0113, 0.1939, 0.9014),
        ("libx265", 23, 0.0061, 0.1293, 0.5421),
        ("libx265", 28, 0.0038, 0.0732, 0.1656),
        ("libx265", 38, 0.0023, 0.0154, 0.0092),
        ("libsvtav1", 24, 0.0064, 0.2004, 1.0614),
        ("libsvtav1", 30, 0.0045, 0.1434, 0.5361),
        ("libsvtav1", 40, 0.0026, 0.0585, 0.0958),
        ("libsvtav1", 50, 0.0015, 0.0181, 0.0247),
        ("libvpx-vp9", 24, 0.0088, 0.1684, 0.9451),
        ("libvpx-vp9", 31, 0.0051, 0.1229, 0.6378),
        ("libvpx-vp9", 38, 0.0032, 0.0783, 0.3825),
        ("libvpx-vp9", 45, 0.0022, 0.0407, 0.1142),
    ];
    // 150 frames, as the measurements were: the container's headers are in the figures.
    let inputs = video(5.0, 1920, 1080, 30.0);
    for (codec, crf, soft, plain, grainy) in table {
        let e = est(&inputs, &coded(codec, Some(crf)));
        for (name, bpp) in [("soft", soft), ("plain", plain), ("grainy", grainy)] {
            let bytes = bpp * 1920.0 * 1080.0 * 150.0 / 8.0;
            assert!(
                e.low_bytes as f64 <= bytes && bytes <= e.high_bytes as f64,
                "{codec} CRF {crf} {name}: {bytes} outside {} .. {}",
                e.low_bytes,
                e.high_bytes
            );
        }
    }
}

#[test]
fn no_crf_is_the_encoders_own_default() {
    let inputs = video(30.0, 1920, 1080, 30.0);
    for (codec, default) in [("libx264", 23), ("libx265", 28), ("libsvtav1", 35), ("libvpx-vp9", 32)] {
        assert_eq!(
            est(&inputs, &coded(codec, None)),
            est(&inputs, &coded(codec, Some(default))),
            "{codec}"
        );
    }
    // No encoder named: ffmpeg's pick for the container.
    let bare = |container| ExportOptions {
        container,
        include_audio: false,
        ..ExportOptions::default()
    };
    assert_eq!(est(&inputs, &bare(Container::Mp4)), est(&inputs, &x264(23)));
    assert_eq!(est(&inputs, &bare(Container::Mkv)).low_bytes, {
        let mkv = ExportOptions {
            container: Container::Mkv,
            ..x264(23)
        };
        est(&inputs, &mkv).low_bytes
    });
    let webm = est(&inputs, &bare(Container::Webm));
    assert_eq!(webm.basis, SizeBasis::Modelled);
    assert_eq!(webm.low_bytes, est(&inputs, &coded("libvpx-vp9", Some(32))).low_bytes);
    // A .mov takes x264 as an .mp4 does (FFmpeg 9.0.2); a GIF-less, codec-less audio container has no picture.
    assert_eq!(est(&inputs, &bare(Container::Mov)).basis, SizeBasis::Modelled);
    assert_eq!(
        est(&inputs, &bare(Container::Mov)).low_bytes,
        est(&inputs, &x264(23)).low_bytes
    );
}

#[test]
fn a_crf_past_the_scale_reads_as_the_scales_end() {
    let inputs = video(30.0, 1920, 1080, 30.0);
    assert_eq!(est(&inputs, &x264(51)), est(&inputs, &x264(9_999)));
    assert_eq!(est(&inputs, &x264(51)), est(&inputs, &x264(u32::MAX)));
    let vp9 = |crf| coded("libvpx-vp9", Some(crf));
    assert_eq!(est(&inputs, &vp9(63)), est(&inputs, &vp9(1_000)));
}

#[test]
fn lossless_is_large_and_svt_av1_does_not_have_one() {
    let inputs = video(10.0, 1920, 1080, 30.0);
    let lossless = |codec: &str| ExportOptions {
        rate_control: RateControl::Lossless,
        ..coded(codec, None)
    };
    let raw_bytes = 12.0 * 1920.0 * 1080.0 * 300.0 / 8.0;
    for codec in ["libx264", "libx265", "libvpx-vp9"] {
        let e = est(&inputs, &lossless(codec));
        assert_eq!(e.basis, SizeBasis::Modelled);
        // Measured 0.15..8.4 bpp over the three classes and white noise.
        assert!(e.low_bytes as f64 <= 0.15 * 1920.0 * 1080.0 * 300.0 / 8.0, "{codec}: {e:?}");
        assert!(e.high_bytes as f64 >= 8.4 * 1920.0 * 1080.0 * 300.0 / 8.0, "{codec}: {e:?}");
        assert!((e.high_bytes as f64) < raw_bytes, "{codec} is bounded by raw video");
        // `-crf 0` is the lossless of x264 / x265 / VP9 too.
        assert_eq!(est(&inputs, &coded(codec, Some(0))), e, "{codec}");
    }
    // libsvtav1 `-crf 0` measured the bytes of no `-crf` at all: ffmpeg reads 0 as unset.
    let svt = est(&inputs, &lossless("libsvtav1"));
    assert_eq!(svt, est(&inputs, &coded("libsvtav1", Some(35))));
    assert_eq!(svt, est(&inputs, &coded("libsvtav1", Some(0))));
}

#[test]
fn a_preset_moves_the_band_the_way_it_was_measured() {
    let inputs = video(10.0, 1920, 1080, 30.0);
    let with = |preset: &str| {
        est(
            &inputs,
            &ExportOptions {
                preset: Some(preset.into()),
                ..x264(23)
            },
        )
    };
    let medium = with("medium");
    assert_eq!(medium, est(&inputs, &x264(23)));
    // Ultrafast costs 2.7x on soft and 1.7x on grainy pictures; veryslow 0.76x on soft ones.
    assert!(with("ultrafast").high_bytes as f64 >= 1.7 * medium.high_bytes as f64 / 1.25 * 0.99);
    assert!(with("ultrafast").low_bytes as f64 > 1.9 * medium.low_bytes as f64);
    assert!(with("veryslow").low_bytes < medium.low_bytes);
    assert_eq!(with("veryslow").high_bytes, medium.high_bytes);
    // An unknown word is the default, never an error.
    assert_eq!(with("nonsense"), medium);
    // SVT-AV1's numeric presets and VP9's `-cpu-used` are read; garbage is the default.
    let av1 = |preset: Option<&str>| {
        est(
            &inputs,
            &ExportOptions {
                preset: preset.map(String::from),
                ..coded("libsvtav1", Some(30))
            },
        )
    };
    assert!(av1(Some("2")).low_bytes < av1(Some("8")).low_bytes);
    assert_eq!(av1(Some("x")), av1(Some("8")));
}

#[test]
fn a_tune_a_profile_and_a_wide_pixel_format_only_raise_the_top() {
    let inputs = video(10.0, 1920, 1080, 30.0);
    let plain = est(&inputs, &x264(23));
    let grain = est(
        &inputs,
        &ExportOptions {
            tune: Some("grain".into()),
            ..x264(23)
        },
    );
    assert_eq!(grain.low_bytes, plain.low_bytes);
    assert!(grain.high_bytes as f64 > 1.5 * plain.high_bytes as f64);
    let wide = est(
        &inputs,
        &ExportOptions {
            pix_fmt: Some("yuv422p".into()),
            ..x264(23)
        },
    );
    assert!(wide.high_bytes > plain.high_bytes && wide.low_bytes == plain.low_bytes);
    let baseline = est(
        &inputs,
        &ExportOptions {
            profile_v: Some("baseline".into()),
            ..x264(23)
        },
    );
    assert!(baseline.high_bytes > plain.high_bytes);
    // The stack is capped: every extra at once is not 5x.
    let everything = est(
        &inputs,
        &ExportOptions {
            tune: Some("grain".into()),
            profile_v: Some("baseline".into()),
            pix_fmt: Some("yuv444p".into()),
            ..x264(23)
        },
    );
    assert!((everything.high_bytes as f64) < 2.1 * plain.high_bytes as f64);
}

#[test]
fn hardware_encoders_read_the_software_curve_with_a_wider_band() {
    let inputs = video(30.0, 1920, 1080, 30.0);
    let soft = est(&inputs, &x264(23));
    for (codec, spread) in [
        ("h264_amf", (0.6, 1.8)),
        ("h264_vaapi", (0.6, 1.8)),
        ("h264_nvenc", (0.5, 2.5)),
        ("h264_videotoolbox", (0.4, 3.0)),
    ] {
        let e = est(&inputs, &coded(codec, Some(23)));
        assert_eq!(e.basis, SizeBasis::Modelled);
        assert!(
            e.low_bytes < soft.low_bytes && e.high_bytes > soft.high_bytes,
            "{codec}: {e:?} vs {soft:?}"
        );
        let mid = |e: &SizeEstimate| (e.low_bytes as f64 * e.high_bytes as f64).sqrt();
        assert!(mid(&e) > 0.5 * mid(&soft) && mid(&e) < 2.0 * mid(&soft), "{codec}");
        assert!(e.note.contains("hardware encoder"), "{}", e.note);
        let _ = spread;
    }
    // Wider where there is less to go on.
    let amf = est(&inputs, &coded("h264_amf", Some(23)));
    let nvenc = est(&inputs, &coded("h264_nvenc", Some(23)));
    assert!(nvenc.high_bytes > amf.high_bytes && nvenc.low_bytes < amf.low_bytes);
    // HEVC hardware is read on the x264 curve, AV1 on SVT-AV1's.
    let hevc_hw = est(&inputs, &coded("hevc_amf", Some(23)));
    let h264_hw = est(&inputs, &coded("h264_amf", Some(23)));
    assert_eq!(hevc_hw.low_bytes, h264_hw.low_bytes);
    let av1_hw = est(&inputs, &coded("av1_amf", Some(30)));
    let av1 = est(&inputs, &coded("libsvtav1", Some(30)));
    assert!(av1_hw.low_bytes < av1.low_bytes && av1_hw.high_bytes > av1.high_bytes);
    // A hardware encoder with a bitrate is a target with a looser hold.
    let bitrate = |codec: &str| {
        est(
            &video(60.0, 1920, 1080, 30.0),
            &ExportOptions {
                rate_control: RateControl::Bitrate,
                video_bitrate: Some("8M".into()),
                ..coded(codec, None)
            },
        )
    };
    assert_eq!(bitrate("libx264").basis, SizeBasis::Targeted);
    assert_eq!(bitrate("h264_nvenc").basis, SizeBasis::Modelled);
    assert!(bitrate("h264_nvenc").high_bytes > bitrate("libx264").high_bytes);
    assert!(bitrate("h264_nvenc").low_bytes < bitrate("libx264").low_bytes);
}

#[test]
fn prores_is_set_by_its_profile_not_by_a_crf_or_a_rate_control() {
    let inputs = video(10.0, 1920, 1080, 30.0);
    let prores = |profile: Option<u8>| ExportOptions {
        container: Container::Mov,
        video_codec: Some("prores_ks".into()),
        prores_profile: profile,
        include_audio: false,
        ..ExportOptions::default()
    };
    let mut last = 0;
    for p in 0..=3 {
        let e = est(&inputs, &prores(Some(p)));
        assert_eq!(e.basis, SizeBasis::Modelled);
        assert!(e.low_bytes > last, "profile {p}: {e:?}");
        last = e.low_bytes;
    }
    // The default profile is HQ; the measured 220 Mbps of Apple's own table is inside it.
    let hq = est(&inputs, &prores(None));
    assert_eq!(hq, est(&inputs, &prores(Some(3))));
    let apple_hq = 220e6 * 10.0 / 8.0;
    assert!(hq.low_bytes as f64 <= apple_hq && apple_hq <= hq.high_bytes as f64, "{hq:?}");
    // A CRF or a bitrate mode is not read.
    let noisy = ExportOptions {
        crf: Some(40),
        rate_control: RateControl::Bitrate,
        video_bitrate: Some("1M".into()),
        ..prores(Some(3))
    };
    assert_eq!(est(&inputs, &noisy), hq);
    // A profile past the table is the table's end.
    assert_eq!(est(&inputs, &prores(Some(200))), est(&inputs, &prores(Some(5))));
}

#[test]
fn gif_is_bytes_per_pixel_per_frame() {
    let gif = ExportOptions {
        container: Container::Gif,
        video_codec: Some("gif".into()),
        include_audio: false,
        ..ExportOptions::default()
    };
    // The dialog's preset: 480p, 15 fps. Measured over five seconds: 0.056 bytes per pixel per frame
    // on plain content, 0.10 on blur, 0.66 on grain.
    let e = est(&video(5.0, 854, 480, 15.0), &gif);
    assert_eq!(e.basis, SizeBasis::Modelled);
    let per_frame = 854.0 * 480.0 * 75.0;
    for measured in [0.056, 0.10, 0.66] {
        let bytes = measured * per_frame;
        assert!(
            e.low_bytes as f64 <= bytes && bytes <= e.high_bytes as f64,
            "{measured}: {e:?}"
        );
    }
    assert!(e.note.contains("GIF"), "{}", e.note);
    // The container decides, not the codec field (a GIF with a stale `libx264` is still a GIF).
    let stale = ExportOptions {
        video_codec: Some("libx264".into()),
        ..gif.clone()
    };
    assert_eq!(est(&video(5.0, 854, 480, 15.0), &stale), e);
    // And no audio in it, whatever the cut has.
    let with_sound = SizeInputs {
        sound: Some(STEREO),
        ..video(5.0, 854, 480, 15.0)
    };
    assert_eq!(
        est(
            &with_sound,
            &ExportOptions {
                include_audio: true,
                ..gif
            }
        ),
        e
    );
}

#[test]
fn lossy_sound_lands_near_its_bitrate_and_lossless_sound_is_a_share_of_raw() {
    let sound = SizeInputs {
        duration_secs: 20.0,
        picture: None,
        sound: Some(STEREO),
    };
    let audio = |container, codec: &str, rate: Option<&str>| {
        est(
            &sound,
            &ExportOptions {
                container,
                audio_codec: Some(codec.into()),
                audio_bitrate: rate.map(String::from),
                ..ExportOptions::default()
            },
        )
    };
    // Measured over 20 s of silence, a tone, a chord and noise: AAC 4..284 kbps of 128k / 320k asked,
    // Opus 4..178, FLAC 9..1112, ALAC 4..1495.
    let aac = audio(Container::M4a, "aac", None);
    for kbps in [4.1, 130.0, 133.9] {
        let bytes = kbps * 1000.0 * 20.0 / 8.0;
        assert!(
            aac.low_bytes as f64 <= bytes && bytes <= aac.high_bytes as f64,
            "aac {kbps}: {aac:?}"
        );
    }
    let aac320 = audio(Container::M4a, "aac", Some("320k"));
    for kbps in [4.1, 233.1, 283.8] {
        let bytes = kbps * 1000.0 * 20.0 / 8.0;
        assert!(
            aac320.low_bytes as f64 <= bytes && bytes <= aac320.high_bytes as f64,
            "aac 320k {kbps}: {aac320:?}"
        );
    }
    let opus = audio(Container::Webm, "libopus", Some("128k"));
    for kbps in [3.9, 90.2, 177.7] {
        let bytes = kbps * 1000.0 * 20.0 / 8.0;
        assert!(
            opus.low_bytes as f64 <= bytes && bytes <= opus.high_bytes as f64,
            "opus {kbps}: {opus:?}"
        );
    }
    for (container, codec, kbps_list) in [
        (Container::Flac, "flac", [8.9, 857.1, 1112.6]),
        (Container::M4a, "alac", [3.7, 1105.3, 1494.9]),
    ] {
        let e = audio(container, codec, None);
        assert_eq!(e.basis, SizeBasis::Modelled);
        for kbps in kbps_list {
            let bytes = kbps * 1000.0 * 20.0 / 8.0;
            assert!(
                e.low_bytes as f64 <= bytes && bytes <= e.high_bytes as f64,
                "{codec} {kbps}: {e:?}"
            );
        }
    }
    // A container's own default encoder when none is named.
    assert_eq!(
        est(
            &sound,
            &ExportOptions {
                container: Container::M4a,
                ..ExportOptions::default()
            }
        ),
        aac
    );
    // The defaults ffmpeg takes by container: Vorbis for matroska, Opus for webm.
    let default_in = |container| {
        est(
            &sound,
            &ExportOptions {
                container,
                ..ExportOptions::default()
            },
        )
    };
    let vorbis = default_in(Container::Mkv);
    assert_eq!(vorbis.basis, SizeBasis::Modelled);
    for kbps in [6.4, 30.1, 34.6, 83.2] {
        let bytes = kbps * 1000.0 * 20.0 / 8.0;
        assert!(
            vorbis.low_bytes as f64 <= bytes && bytes <= vorbis.high_bytes as f64,
            "vorbis {kbps}: {vorbis:?}"
        );
    }
    let opus_default = default_in(Container::Webm);
    for kbps in [3.9, 121.4, 138.9, 66.7] {
        let bytes = kbps * 1000.0 * 20.0 / 8.0;
        assert!(
            opus_default.low_bytes as f64 <= bytes && bytes <= opus_default.high_bytes as f64,
            "opus {kbps}: {opus_default:?}"
        );
    }
    // A mov takes AAC as an m4a does (its own container's headers differ).
    let mov = default_in(Container::Mov);
    assert_eq!((mov.basis, mov.note.as_str()), (aac.basis, aac.note.as_str()));
    assert!(mov.high_bytes > aac.high_bytes - 20_000 && mov.high_bytes < aac.high_bytes + 20_000);
    // Mono AAC defaults to half the bits.
    let mono = SizeInputs {
        sound: Some(SizeSound {
            sample_rate: 48_000,
            channels: 1,
        }),
        ..sound
    };
    let m = est(
        &mono,
        &ExportOptions {
            container: Container::M4a,
            ..ExportOptions::default()
        },
    );
    assert!(m.high_bytes < aac.high_bytes);
}

#[test]
fn the_container_adds_headers_that_grow_with_the_pictures() {
    let inputs = video(60.0, 1280, 720, 30.0);
    let rate = |container| ExportOptions {
        container,
        rate_control: RateControl::Bitrate,
        video_bitrate: Some("2M".into()),
        ..x264(23)
    };
    let mp4 = est(&inputs, &rate(Container::Mp4));
    let mkv = est(&inputs, &rate(Container::Mkv));
    let nominal = 2_000_000.0 * 60.0 / 8.0;
    for e in [&mp4, &mkv] {
        assert!(e.low_bytes as f64 > 0.6 * nominal && (e.high_bytes as f64) < 3.0 * nominal);
    }
    // Captions muxed in are only ever the top of the band.
    let captioned = est(
        &inputs,
        &ExportOptions {
            embed_captions: true,
            ..rate(Container::Mp4)
        },
    );
    assert_eq!(captioned.low_bytes, mp4.low_bytes);
    assert!(captioned.high_bytes > mp4.high_bytes);
}

// ---- what cannot be estimated ---------------------------------------------------------------------

#[test]
fn what_cannot_be_estimated_says_why_and_has_no_numbers() {
    let ok = video(30.0, 1920, 1080, 30.0);
    let cases: Vec<(&str, SizeInputs, ExportOptions)> = vec![
        ("an empty cut", video(0.0, 1920, 1080, 30.0), x264(23)),
        ("a negative length", video(-3.0, 1920, 1080, 30.0), x264(23)),
        ("an infinite length", video(f64::INFINITY, 1920, 1080, 30.0), x264(23)),
        ("a nan length", video(f64::NAN, 1920, 1080, 30.0), x264(23)),
        ("nothing to write", SizeInputs { picture: None, ..ok }, x264(23)),
        (
            "an unknown encoder",
            ok,
            ExportOptions {
                video_codec: Some("libxvid".into()),
                ..x264(23)
            },
        ),
        (
            "a bitrate mode with no bitrate",
            ok,
            ExportOptions {
                rate_control: RateControl::Bitrate,
                ..x264(23)
            },
        ),
        (
            "a bitrate that is not one",
            ok,
            ExportOptions {
                rate_control: RateControl::TwoPass,
                video_bitrate: Some("fast".into()),
                ..x264(23)
            },
        ),
        (
            "an unknown sound encoder",
            SizeInputs {
                sound: Some(STEREO),
                ..ok
            },
            ExportOptions {
                audio_codec: Some("wmav2".into()),
                include_audio: true,
                ..x264(23)
            },
        ),
    ];
    for (what, inputs, opts) in cases {
        let e = est(&inputs, &opts);
        assert_eq!(e.basis, SizeBasis::Unknown, "{what}: {e:?}");
        assert_eq!((e.low_bytes, e.high_bytes), (0, 0), "{what}");
        assert!(!e.note.is_empty(), "{what} says why");
        assert_eq!(e.bounds(), None, "{what}");
    }
    // An audio-only container with only a picture in the cut writes nothing.
    let mp3 = ExportOptions {
        container: Container::Mp3,
        audio_codec: Some("libmp3lame".into()),
        ..ExportOptions::default()
    };
    assert_eq!(est(&ok, &mp3).basis, SizeBasis::Unknown);
}

#[test]
fn every_option_and_every_input_gives_a_sane_range_and_never_panics() {
    let codecs = [
        None,
        Some("libx264"),
        Some("libx265"),
        Some("libvpx-vp9"),
        Some("libsvtav1"),
        Some("prores_ks"),
        Some("gif"),
        Some("h264_nvenc"),
        Some("hevc_qsv"),
        Some("av1_vaapi"),
        Some("h264_videotoolbox"),
        Some("nonsense"),
    ];
    let containers = [
        Container::Mp4,
        Container::Mov,
        Container::Mkv,
        Container::Webm,
        Container::Gif,
        Container::Mp3,
        Container::M4a,
        Container::Wav,
        Container::Flac,
    ];
    let rates = [
        RateControl::Crf,
        RateControl::Bitrate,
        RateControl::TwoPass,
        RateControl::Lossless,
    ];
    let frames = [
        frame(1920, 1080, 30.0),
        frame(2, 2, 1.0),
        frame(0, 0, 0.0),
        frame(u32::MAX, u32::MAX, f64::NAN),
        frame(7680, 4320, 240.0),
        frame(640, 360, f64::INFINITY),
    ];
    let secs = [0.001, 1.0, 59.9, 86_400.0, 1e12];
    for container in containers {
        for codec in codecs {
            for rate in rates {
                for f in frames {
                    for s in secs {
                        for crf in [None, Some(0), Some(51), Some(u32::MAX)] {
                            let inputs = SizeInputs {
                                duration_secs: s,
                                picture: Some(f),
                                sound: Some(STEREO),
                            };
                            let opts = ExportOptions {
                                container,
                                video_codec: codec.map(String::from),
                                rate_control: rate,
                                crf,
                                video_bitrate: Some("8M".into()),
                                audio_codec: Some("aac".into()),
                                ..ExportOptions::default()
                            };
                            let e = est(&inputs, &opts);
                            assert!(
                                e.low_bytes <= e.high_bytes,
                                "{container:?} {codec:?} {rate:?} {f:?} {s}: {e:?}"
                            );
                            if e.basis == SizeBasis::Unknown {
                                assert_eq!((e.low_bytes, e.high_bytes), (0, 0));
                            }
                        }
                    }
                }
            }
        }
    }
}

// ---- the estimate as a file's --------------------------------------------------------------------

#[test]
fn the_estimate_serializes_the_way_the_frontend_reads_it() {
    let e = est(&video(10.0, 1920, 1080, 30.0), &x264(23));
    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["basis"], "modelled");
    assert!(json["low_bytes"].as_u64().unwrap() < json["high_bytes"].as_u64().unwrap());
    assert!(json["note"].as_str().unwrap().len() > 10);
    let unknown = serde_json::to_value(est(&video(0.0, 2, 2, 1.0), &x264(23))).unwrap();
    assert_eq!(unknown["basis"], "unknown");
    assert_eq!(unknown["low_bytes"], 0);
    for (basis, text) in [
        (SizeBasis::Exact, "exact"),
        (SizeBasis::Targeted, "targeted"),
        (SizeBasis::Modelled, "modelled"),
        (SizeBasis::Unknown, "unknown"),
    ] {
        assert_eq!(serde_json::to_value(basis).unwrap(), text);
    }
}

// ---- resolving the inputs from a cut -------------------------------------------------------------

fn av_cut(secs: f64) -> (Timeline, Vec<Asset>) {
    let asset = av_asset(Uuid::new_v4(), 100.0);
    let clip = make_clip(asset.id, 0.0, secs, 0.0);
    (single(vec![clip]), vec![asset])
}

#[test]
fn the_inputs_are_the_frame_the_export_renders_and_the_length_of_the_range() {
    let (tl, assets) = av_cut(20.0);
    let opts = ExportOptions::default();
    let i = export_size_inputs(&tl, &assets, &opts);
    assert_eq!(i.duration_secs, 20.0);
    let p = i.picture.expect("a picture");
    assert_eq!((p.width, p.height, p.fps), (1920, 1080, 30.0));
    assert_eq!(p.fps_source, FpsSource::Footage);
    assert_eq!(
        i.sound,
        Some(SizeSound {
            sample_rate: 48_000,
            channels: 2
        })
    );
    // The dialog's size and rate win, like the export's; odd sizes are even-clamped.
    let resized = export_size_inputs(
        &tl,
        &assets,
        &ExportOptions {
            resolution: Some((1281, 721)),
            fps: Some(24.0),
            ..opts.clone()
        },
    );
    let p = resized.picture.unwrap();
    assert_eq!(
        (p.width, p.height, p.fps, p.fps_source),
        (1280, 720, 24.0, FpsSource::Options)
    );
    // A range is the length of the file, clamped to the cut.
    let ranged = |start, end| {
        export_size_inputs(
            &tl,
            &assets,
            &ExportOptions {
                range: Some(crate::model::TimeRange { start, end }),
                ..opts.clone()
            },
        )
        .duration_secs
    };
    assert!((ranged(5.0, 12.5) - 7.5).abs() < 1e-9);
    assert!((ranged(10.0, 500.0) - 10.0).abs() < 1e-9);
    // Empty or inverted: the whole cut, as the export does.
    assert_eq!(ranged(12.0, 12.0), 20.0);
    assert_eq!(ranged(15.0, 3.0), 20.0);
}

#[test]
fn the_inputs_drop_what_the_container_or_the_options_do() {
    let (tl, assets) = av_cut(10.0);
    let audio_only = export_size_inputs(
        &tl,
        &assets,
        &ExportOptions {
            container: Container::Mp3,
            audio_codec: Some("libmp3lame".into()),
            ..ExportOptions::default()
        },
    );
    assert!(audio_only.picture.is_none() && audio_only.sound.is_some());
    let gif = export_size_inputs(
        &tl,
        &assets,
        &ExportOptions {
            container: Container::Gif,
            ..ExportOptions::default()
        },
    );
    assert!(gif.picture.is_some() && gif.sound.is_none());
    let muted = export_size_inputs(
        &tl,
        &assets,
        &ExportOptions {
            include_audio: false,
            ..ExportOptions::default()
        },
    );
    assert!(muted.picture.is_some() && muted.sound.is_none());
    // A clip whose sound was detached carries none; a video-only asset never did.
    let mut silent_tl = tl.clone();
    silent_tl.tracks[0].clips[0].source_audio = false;
    assert!(export_size_inputs(&silent_tl, &assets, &ExportOptions::default())
        .sound
        .is_none());
    let mut video_only = av_asset(Uuid::new_v4(), 100.0);
    video_only.streams.retain(|s| s.kind == StreamKind::Video);
    let vtl = single(vec![make_clip(video_only.id, 0.0, 10.0, 0.0)]);
    assert!(
        export_size_inputs(&vtl, std::slice::from_ref(&video_only), &ExportOptions::default())
            .sound
            .is_none()
    );
    // A muted track is as absent here as in the file.
    let mut muted_tl = tl;
    muted_tl.tracks[0].muted = true;
    let none = export_size_inputs(&muted_tl, &assets, &ExportOptions::default());
    assert!(none.picture.is_none() && none.sound.is_none() && none.duration_secs == 0.0);
    // A sound-only cut has no picture whatever the container.
    let asset = test_asset(vec![audio_stream(44_100, 1)]);
    let atl = timeline_of(vec![audio_track(vec![make_clip(asset.id, 0.0, 8.0, 0.0)])]);
    let a = export_size_inputs(&atl, std::slice::from_ref(&asset), &ExportOptions::default());
    assert!(a.picture.is_none());
    assert_eq!(
        a.sound,
        Some(SizeSound {
            sample_rate: 44_100,
            channels: 1
        })
    );
}

#[test]
fn a_still_image_cut_is_a_picture_without_sound() {
    let still = img_asset(Uuid::new_v4());
    let tl = single(vec![make_clip(still.id, 0.0, 4.0, 0.0)]);
    let i = export_size_inputs(&tl, std::slice::from_ref(&still), &ExportOptions::default());
    assert_eq!(i.duration_secs, 4.0);
    assert!(i.picture.is_some() && i.sound.is_none());
    assert_eq!(i.picture.unwrap().fps_source, FpsSource::Default);
}

#[test]
fn an_empty_timeline_cannot_be_estimated() {
    let tl = timeline_of(vec![video_track(vec![])]);
    let all = estimate_export(&tl, &[], &ExportOptions::default(), &[]);
    assert_eq!(all.files.len(), 1);
    assert_eq!(all.files[0].estimate.basis, SizeBasis::Unknown);
    assert_eq!(all.total, None);
}

#[test]
fn a_multi_format_export_has_one_estimate_per_delivery_and_a_total() {
    let (tl, assets) = av_cut(30.0);
    let opts = ExportOptions {
        video_codec: Some("libx264".into()),
        crf: Some(23),
        audio_codec: Some("aac".into()),
        audio_bitrate: Some("128k".into()),
        ..ExportOptions::default()
    };
    let tall = Delivery::new(1080, 1920, Fit::Cover);
    let square = Delivery::new(1080, 1080, Fit::Cover);
    let wide = Delivery::new(1920, 1080, Fit::Contain);
    let all = estimate_export(&tl, &assets, &opts, &[tall, square, wide]);
    assert_eq!(all.files.len(), 3);
    for (file, d) in all.files.iter().zip([tall, square, wide]) {
        assert_eq!(file.delivery, Some(d));
        let f = file.frame.expect("a frame");
        assert_eq!((f.width, f.height), (d.width, d.height));
        assert_eq!(file.duration_secs, 30.0);
        assert_eq!(file.estimate.basis, SizeBasis::Modelled);
        // The same as the frame the variant really renders.
        let variant = ExportVariant {
            delivery: d,
            output: "x.mp4".into(),
        };
        assert_eq!(Some(f), variant_frame(&tl, &assets, &variant, &opts));
    }
    // The 9:16 and 16:9 frames have the same pixels, so the same band.
    assert_eq!(all.files[0].estimate, all.files[2].estimate);
    assert!(
        all.files[1].estimate.high_bytes < all.files[0].estimate.high_bytes,
        "1080x1080 is fewer pixels"
    );
    let total = all.total.expect("a total");
    assert_eq!(total.low_bytes, all.files.iter().map(|f| f.estimate.low_bytes).sum::<u64>());
    assert_eq!(total.high_bytes, all.files.iter().map(|f| f.estimate.high_bytes).sum::<u64>());
    // No deliveries is the one file at the project's frame.
    let one = estimate_export(&tl, &assets, &opts, &[]);
    assert_eq!(one.files.len(), 1);
    assert_eq!(one.files[0].delivery, None);
    assert_eq!(one.files[0].estimate, all.files[2].estimate);
    // One file that cannot be estimated takes the total with it.
    let unknown = ExportOptions {
        video_codec: Some("nonsense".into()),
        ..opts
    };
    let none = estimate_export(&tl, &assets, &unknown, &[tall, wide]);
    assert!(none.files.iter().all(|f| f.estimate.basis == SizeBasis::Unknown));
    assert_eq!(none.total, None);
    // JSON: the delivery and the frame ride along, the total is left out when there is none.
    let json = serde_json::to_value(&all).unwrap();
    assert_eq!(json["files"][0]["delivery"]["width"], 1080);
    assert_eq!(json["files"][0]["frame"]["height"], 1920);
    assert!(json["total"]["high_bytes"].as_u64().unwrap() > 0);
    assert!(serde_json::to_value(&none).unwrap().get("total").is_none());
}

#[test]
fn a_project_frame_and_rate_are_the_ones_the_estimate_is_made_for() {
    let (mut tl, assets) = av_cut(10.0);
    tl.format = Some(Delivery::new(1080, 1920, Fit::Cover).with_fps(Some(25.0)).unwrap());
    let i = export_size_inputs(&tl, &assets, &ExportOptions::default());
    let p = i.picture.unwrap();
    assert_eq!(
        (p.width, p.height, p.fps, p.fps_source),
        (1080, 1920, 25.0, FpsSource::Delivery)
    );
    // Footage of another rate than the project's: the project's wins, as in the export.
    let mut odd = av_asset(Uuid::new_v4(), 100.0);
    odd.streams[0] = video_stream(1920, 1080, 59.94);
    let mut tl2 = single(vec![make_clip(odd.id, 0.0, 10.0, 0.0)]);
    assert_eq!(
        export_size_inputs(&tl2, std::slice::from_ref(&odd), &ExportOptions::default())
            .picture
            .unwrap()
            .fps,
        59.94
    );
    tl2.format = Some(Delivery::new(1280, 720, Fit::Contain).with_fps(Some(30.0)).unwrap());
    assert_eq!(
        export_size_inputs(&tl2, std::slice::from_ref(&odd), &ExportOptions::default())
            .picture
            .unwrap()
            .fps,
        30.0
    );
}
