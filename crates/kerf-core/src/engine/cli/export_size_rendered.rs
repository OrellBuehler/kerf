//! The size estimate held to real files (`#[ignore]`d: they drive the real `ffmpeg`).
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
//! This module is the helpers every size test shares, and the **smoke tests** CI runs on every engine lane (the distro FFmpeg 6.1,
//! the pinned build on Linux, Windows and macOS): one real export of each kind whose size the model or the arithmetic states,
//! held to its band, with nothing that depends on the machine's speed. The calibration suites (`export_size_calibration`,
//! `export_size_measure_calibration`) are `--skip`ped there and gated at run time to the FFmpeg they were tuned on.
//!
//! Every case prints a row (what was measured, the band, how wide it is) for whoever runs it; the table
//! in `.claude/docs/engine-cli.md` is that output.
//!
//! `KERF_FFMPEG=<ffmpeg> KERF_FFPROBE=<ffprobe> cargo test -p kerf-core --no-default-features -- --ignored export_size_rendered`

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
pub(super) const NEEDY: [Class; 2] = [Class::Plain, Class::Grainy];

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
pub(super) struct Row {
    pub(super) what: String,
    pub(super) class: &'static str,
    pub(super) real: u64,
    pub(super) estimate: SizeEstimate,
    /// Whether the real size is required to be over the low end (it is for a picture that needs the bits).
    pub(super) holds_low: bool,
}

impl Row {
    pub(super) fn inside(&self) -> bool {
        self.real <= self.estimate.high_bytes && (!self.holds_low || self.real >= self.estimate.low_bytes)
    }

    pub(super) fn print(&self) {
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
pub(super) fn measure(dir: &Path, what: &str, class: &'static str, asset: &Asset, opts: &ExportOptions, holds_low: bool) -> Row {
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

pub(super) fn finish(rows: &[Row]) {
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
pub(super) struct Shot {
    pub(super) what: &'static str,
    pub(super) frame: (u32, u32),
    pub(super) fps: f64,
    pub(super) secs: f64,
    pub(super) opts: ExportOptions,
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

/// The major version of this machine's ffmpeg (`9` for `n9.0.2-...`, `6` for `6.1.1-3ubuntu5`).
pub(super) fn ffmpeg_major() -> u32 {
    let out = command(&ffmpeg_bin()).arg("-version").output().expect("run ffmpeg");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .nth(2)
        .map(|v| {
            v.trim_start_matches('n')
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Whether this ffmpeg lists every one of `encoders`.
pub(super) fn has_encoders(encoders: &[&str]) -> bool {
    let out = command(&ffmpeg_bin())
        .args(["-hide_banner", "-encoders"])
        .output()
        .expect("run ffmpeg");
    let listed = String::from_utf8_lossy(&out.stdout);
    encoders
        .iter()
        .all(|e| listed.lines().any(|l| l.split_whitespace().nth(1) == Some(e)))
}

/// Whether a calibration suite runs on this machine: the bands were tuned on the pinned FFmpeg 9 and hold there, where the encoders'
/// sizes are the ones measured. Any other build skips (and says so); `KERF_SIZE_CALIBRATION=any` runs them anyway to see how
/// far another build is.
pub(super) fn calibrated_here(suite: &str) -> bool {
    if std::env::var("KERF_SIZE_CALIBRATION").as_deref() == Ok("any") {
        return true;
    }
    let major = ffmpeg_major();
    if major == 9 {
        return true;
    }
    eprintln!(
        "{suite}: skipped, its bands were tuned on FFmpeg 9 and this is {major} (KERF_SIZE_CALIBRATION=any runs it anyway)"
    );
    false
}

// ---- smoke tests: what CI runs on every engine lane --------------------------------------------------

/// A real x264 export lands inside the model's band, and the sizes that are arithmetic land on it: nothing here depends on how fast the
/// machine is or on which build of an encoder wrote the file.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn smoke_a_real_export_is_inside_the_estimate_it_was_given() {
    let scratch = Scratch::new("smoke");
    let dir = &scratch.0;
    let mut rows = Vec::new();
    if has_encoders(&["libx264"]) {
        for class in ALL {
            let asset = source(dir, class, (320, 180), 30.0, 3.0, false);
            rows.push(measure(
                dir,
                "smoke x264 CRF 23",
                class.name(),
                &asset,
                &crf_case("libx264", Container::Mp4, 23, Some("medium")),
                true,
            ));
        }
    } else {
        eprintln!("no libx264 in this ffmpeg: the picture half of the smoke test is skipped");
    }
    // PCM is arithmetic: the file is the estimate to within a header.
    let tone = sound_source(dir, "tone", 3.0);
    let wav = ExportOptions {
        container: Container::Wav,
        audio_codec: Some("pcm_s16le".into()),
        ..ExportOptions::default()
    };
    let row = measure(dir, "smoke wav s16", "tone", &tone, &wav, true);
    assert_eq!(row.estimate.basis, SizeBasis::Exact);
    rows.push(row);
    finish(&rows);
}
