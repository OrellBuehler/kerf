//! Whether this ffmpeg's SVT-AV1 can write lossless, and the refusal when it cannot.
//!
//! FFmpeg's `libsvtav1` has no lossless mode of its own: `-crf 0` is "no CRF given" and writes the default quality. Lossless is the
//! SVT-AV1 parameter `lossless=1` (`-svtav1-params`), which the pinned FFmpeg 9.0.2 (SVT-AV1 4.2.0) takes and decodes bit-exact. An
//! SVT-AV1 too old to know the key does not fail: FFmpeg logs `Error parsing option lossless` and carries on, writing a lossy file
//! the export would then call lossless. Windows ships the pinned build; Linux and macOS run the user's own, so the export asks once
//! per ffmpeg binary (a one-frame-sized encode with the parameter, read for the encoder's own complaint) and **refuses** a lossless
//! SVT-AV1 export where the answer is no, with a sentence saying what to pick instead.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::*;

/// How long the probe may take: a 64x64 encode of three pictures, milliseconds on any machine; a wedged one is killed.
const PROBE_LIMIT: Duration = Duration::from_secs(20);

/// Whether `opts` ask for SVT-AV1 lossless: its Lossless mode, or a CRF of 0 (which writes it as lossless, as x264's does).
pub(super) fn wants_svt_lossless(opts: &ExportOptions) -> bool {
    opts.container != Container::Gif
        && opts.video_codec.as_deref() == Some("libsvtav1")
        && (opts.rate_control == RateControl::Lossless || (opts.rate_control == RateControl::Crf && opts.crf == Some(0)))
}

/// The sentence a lossless SVT-AV1 export is refused with where the encoder cannot write one (pure).
pub(super) fn svt_lossless_refusal() -> String {
    "This FFmpeg's SVT-AV1 cannot encode lossless (it ignores the request and would write a lossy file). \
     Choose lossless with x264, x265 or VP9, or an SVT-AV1 CRF of 1 or more, or update FFmpeg."
        .to_string()
}

/// The argv of the probe (pure): three pictures of a 64x64 test card through `libsvtav1 -svtav1-params lossless=1` to a null sink, at
/// the warning level so that the encoder's complaint about a key it does not know is printed.
pub(super) fn probe_args() -> Vec<String> {
    [
        "-hide_banner",
        "-loglevel",
        "warning",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=64x64:r=5:d=1",
        "-frames:v",
        "3",
        "-c:v",
        "libsvtav1",
        "-preset",
        "8",
        "-svtav1-params",
        "lossless=1",
        "-pix_fmt",
        "yuv420p",
        "-f",
        "null",
        "-",
    ]
    .map(String::from)
    .to_vec()
}

/// What the probe's run says (pure): it ran to the end and the encoder did not complain about the key.
pub(super) fn probe_says_supported(success: bool, stderr: &str) -> bool {
    success && !stderr.contains("Error parsing option")
}

/// `Some(answer)` when the probe could say; `None` when it could not run at all (the binary would not start, or it wedged): no answer to keep.
fn probe(bin: &str, limit: Duration) -> Option<bool> {
    let mut cmd = command(bin);
    cmd.args(probe_args());
    let run = output_bounded_cancellable(cmd, limit, None).ok().flatten()?;
    Some(probe_says_supported(
        run.status.success(),
        &String::from_utf8_lossy(&run.stderr),
    ))
}

/// Whether the SVT-AV1 of the ffmpeg this process runs writes lossless, asked of it once (per binary: a test's stand-in is its own
/// answer) and kept. A probe that cannot run at all is not kept, and is taken as yes: an ffmpeg that will not start fails the export loudly,
/// which is better than refusing for a reason that is not the encoder's. Never called under a lock; two threads asking at once each run
/// it, the first to finish sets the answer.
pub fn svt_av1_lossless_supported() -> bool {
    static KNOWN: OnceLock<Mutex<HashMap<String, Arc<OnceLock<bool>>>>> = OnceLock::new();
    let bin = ffmpeg_bin();
    let cell = {
        let mut known = KNOWN.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(known.entry(bin.clone()).or_default())
    };
    if let Some(answer) = cell.get() {
        return *answer;
    }
    match probe(&bin, PROBE_LIMIT) {
        Some(answer) => *cell.get_or_init(|| answer),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{av_asset, make_clip, single};
    use uuid::Uuid;

    fn opts(codec: &str, rate_control: RateControl, crf: Option<u32>) -> ExportOptions {
        ExportOptions {
            container: Container::Mkv,
            video_codec: Some(codec.into()),
            rate_control,
            crf,
            ..ExportOptions::default()
        }
    }

    #[test]
    fn what_asks_for_svt_av1_lossless_is_its_lossless_mode_and_a_crf_of_zero() {
        assert!(wants_svt_lossless(&opts("libsvtav1", RateControl::Lossless, None)));
        assert!(wants_svt_lossless(&opts("libsvtav1", RateControl::Lossless, Some(30))));
        assert!(wants_svt_lossless(&opts("libsvtav1", RateControl::Crf, Some(0))));
        for not in [
            opts("libsvtav1", RateControl::Crf, Some(1)),
            opts("libsvtav1", RateControl::Crf, None),
            opts("libsvtav1", RateControl::Bitrate, Some(0)),
            opts("libsvtav1", RateControl::TwoPass, Some(0)),
            opts("libx264", RateControl::Lossless, None),
            opts("libx265", RateControl::Crf, Some(0)),
            opts("av1_nvenc", RateControl::Lossless, None),
        ] {
            assert!(!wants_svt_lossless(&not), "{not:?}");
        }
        // A GIF is paletted: the codec field is ignored.
        let gif = ExportOptions {
            container: Container::Gif,
            ..opts("libsvtav1", RateControl::Lossless, None)
        };
        assert!(!wants_svt_lossless(&gif));
    }

    #[test]
    fn the_probe_asks_for_the_parameter_and_reads_the_encoders_complaint() {
        let args = probe_args();
        let at = args.iter().position(|a| a == "-svtav1-params").expect("the parameter");
        assert_eq!(args[at + 1], "lossless=1");
        assert!(args.windows(2).any(|w| w == ["-c:v", "libsvtav1"]) && args.windows(2).any(|w| w == ["-f", "null"]));
        assert!(
            args.windows(2).any(|w| w == ["-loglevel", "warning"]),
            "the complaint is a warning"
        );
        // Ran, and said nothing about the key: supported. A complaint, or a failed run: not.
        assert!(probe_says_supported(true, "Svt[warn]: TPL is disabled for aq_mode 0\n"));
        assert!(!probe_says_supported(
            true,
            "[libsvtav1 @ 0x55c7ec832640] Error parsing option lossless: 1.\n"
        ));
        assert!(!probe_says_supported(false, ""));
        assert!(!probe_says_supported(false, "Unknown encoder 'libsvtav1'"));
    }

    #[test]
    fn the_refusal_says_what_to_pick_instead() {
        let why = svt_lossless_refusal();
        assert!(
            why.contains("SVT-AV1 cannot encode lossless") && why.contains("lossy"),
            "{why}"
        );
        for alternative in ["x264", "x265", "VP9", "CRF of 1"] {
            assert!(why.contains(alternative), "{alternative}: {why}");
        }
    }

    #[cfg(unix)]
    fn stand_in(tag: &str, body: &str) -> (std::path::PathBuf, String) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kerf-svt-probe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ffmpeg");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let bin = path.to_string_lossy().into_owned();
        (dir, bin)
    }

    #[cfg(unix)]
    #[test]
    fn a_build_that_ignores_the_key_is_unsupported_one_that_takes_it_is_supported_and_each_is_asked_once() {
        for (tag, says, expected) in [
            (
                "ignores",
                "echo '[libsvtav1 @ 0x1] Error parsing option lossless: 1.' >&2",
                false,
            ),
            ("takes", "echo 'Svt[warn]: TPL is disabled' >&2", true),
            ("missing", "echo \"Unknown encoder 'libsvtav1'\" >&2\nexit 1", false),
        ] {
            let (dir, bin) = stand_in(tag, &format!("echo run >> \"{0}/asked\"\n{says}", "$(dirname \"$0\")"));
            let asked = || std::fs::read_to_string(dir.join("asked")).map_or(0, |t| t.lines().count());
            with_ffmpeg(&bin, || {
                assert_eq!(svt_av1_lossless_supported(), expected, "{tag}");
                assert_eq!(svt_av1_lossless_supported(), expected, "{tag} again");
            });
            assert_eq!(asked(), 1, "{tag}: the answer is kept, not asked again");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_probe_that_cannot_run_is_no_answer_and_is_not_kept() {
        // A binary that wedges is killed at the limit: no answer.
        let (dir, bin) = stand_in("wedged", "exec sleep 60");
        let started = std::time::Instant::now();
        assert_eq!(probe(&bin, Duration::from_millis(500)), None);
        assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
        let _ = std::fs::remove_dir_all(&dir);
        // One that is not there is no answer either, and the export takes it as yes (it will fail on its own, loudly).
        assert_eq!(probe("/nonexistent/ffmpeg", PROBE_LIMIT), None);
        with_ffmpeg("/nonexistent/ffmpeg", || {
            assert!(svt_av1_lossless_supported());
            assert!(svt_av1_lossless_supported());
        });
    }

    #[cfg(unix)]
    #[test]
    fn a_lossless_svt_av1_export_is_refused_where_the_build_cannot_and_nothing_else_is() {
        let (dir, ignores) = stand_in(
            "preflight-ignores",
            "echo '[libsvtav1 @ 0x1] Error parsing option lossless: 1.' >&2",
        );
        let (dir2, takes) = stand_in("preflight-takes", "exit 0");
        let asset = av_asset(Uuid::new_v4(), 30.0);
        let tl = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
        let assets = [asset];
        let preflight = |bin: &str, o: &ExportOptions| with_ffmpeg(bin, || export_preflight(&tl, &assets, o));
        for lossless in [
            opts("libsvtav1", RateControl::Lossless, None),
            opts("libsvtav1", RateControl::Crf, Some(0)),
        ] {
            let refused = preflight(&ignores, &lossless).unwrap_err();
            assert!(matches!(refused, Error::InvalidArgument(_)), "{refused:?}");
            assert!(refused.to_string().contains("cannot encode lossless"), "{refused}");
            assert!(
                preflight(&takes, &lossless).is_ok(),
                "a build that takes the key is not refused"
            );
        }
        // The same build, anything that is not lossless SVT-AV1: untouched.
        for fine in [
            opts("libsvtav1", RateControl::Crf, Some(30)),
            opts("libsvtav1", RateControl::Crf, Some(1)),
            opts("libx264", RateControl::Lossless, None),
            ExportOptions {
                container: Container::Mp3,
                audio_codec: Some("libmp3lame".into()),
                ..opts("libsvtav1", RateControl::Lossless, None)
            },
        ] {
            assert!(preflight(&ignores, &fine).is_ok(), "{fine:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
