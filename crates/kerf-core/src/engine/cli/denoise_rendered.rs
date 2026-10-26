//! A clip's noise reduction through the real `ffmpeg` (`#[ignore]`d): the denoised sound lands on
//! the sample the dry one does, in the export and in the preview's decode, at 44.1 and 48 kHz and on a
//! sped-up clip, keeps its length, its loud head and tail and its loud middle, and the hiss under it is
//! taken down. The chain written for the other kind of `afftdn` is held to the delay the probe says this
//! one has (late by it where the filter delays, early by it where it does not), so the probe is measured,
//! not assumed.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored denoise_rendered`
//! (`KERF_FFMPEG` / `KERF_FFPROBE` pick the build: run it on a 4.4 or 5.0 too.)

#![allow(clippy::print_stderr)] // what each case measured, for whoever runs the ignored ones

use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::denoise::afftdn_delays;
use super::*;
use crate::engine::test_support::{audio_track, make_clip, timeline_of, StatusBounded};
use crate::model::AudioEffect;
use crate::project::Project;

/// The rate the export is read back at.
const OUT: usize = 48_000;
/// Where the loud burst sits in the source, and how long it lasts.
const BURST: (f64, f64) = (1.0, 0.1);
/// How far either way the sound is searched for, in output samples (over 30 ms).
const SEARCH: isize = 1500;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-denoise-rendered-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Four seconds of mono hiss at -45 dBFS with a loud burst of noise at [`BURST`] and over the first
/// 0.2 s and the last 0.1 s, at `rate`, probed like an import.
fn source(dir: &Path, rate: u32) -> Asset {
    let path = dir.join(format!("hiss-{rate}.wav"));
    let (at, len) = BURST;
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!(
            "aevalsrc='0.01*(random(0)*2-1)+if(lt(t,0.2)+between(t,{at},{end})+gt(t,3.9),0.5*(random(1)*2-1),0)':s={rate}:d=4",
            end = at + len
        ))
        .args(["-c:a", "pcm_s16le"])
        .arg(&path)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());
    Project::probe_asset(&path).expect("probe the hiss")
}

/// The sound the export of `timeline` renders, mono f32 at [`OUT`].
fn export_audio(timeline: &Timeline, assets: &[Asset], dir: &Path, tag: &str) -> Vec<f32> {
    let opts = ExportOptions {
        container: Container::Wav,
        ..ExportOptions::default()
    };
    let out = dir.join(format!("{tag}.f32"));
    let mut args = build_export_args(timeline, assets, "unused.wav", &opts).unwrap();
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
    let bytes = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// The lag (in samples) that best lines `b` up with `a` over a window around `a`'s loudest sample:
/// positive when `b` is late.
fn lag(a: &[f32], b: &[f32]) -> isize {
    let peak = (0..a.len()).max_by(|&i, &j| a[i].abs().total_cmp(&a[j].abs())).unwrap();
    let (from, to) = (peak.saturating_sub(4800), (peak + 4800).min(a.len()));
    (-SEARCH..=SEARCH)
        .max_by(|&x, &y| {
            let score = |l: isize| -> f64 {
                (from..to)
                    .filter_map(|i| Some(f64::from(a[i]) * f64::from(*b.get(usize::try_from(i as isize + l).ok()?)?)))
                    .sum()
            };
            score(x).total_cmp(&score(y))
        })
        .unwrap()
}

fn rms_db(s: &[f32]) -> f64 {
    10.0 * (s.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / s.len() as f64).log10()
}

fn denoised(mut clip: Clip, track_noise: bool) -> Clip {
    clip.audio = vec![
        AudioEffect::Highpass { hz: 40.0 },
        AudioEffect::Denoise {
            reduction_db: 24.0,
            floor_db: -40.0,
            track_noise,
        },
    ];
    clip
}

#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn a_denoised_clip_keeps_its_sync_and_its_length_and_loses_its_hiss() {
    let dir = scratch("export");
    let delays = afftdn_delays();
    for rate in [44_100, 48_000] {
        let asset = source(&dir, rate);
        let assets = vec![asset.clone()];
        for speed in [1.0, 1.5] {
            let mut clip = make_clip(asset.id, 0.0, 4.0, 0.25);
            clip.audio = vec![AudioEffect::Highpass { hz: 40.0 }];
            clip.speed = speed;
            let tag = format!("{rate}-{speed}");
            let dry = export_audio(
                &timeline_of(vec![audio_track(vec![clip.clone()])]),
                &assets,
                &dir,
                &format!("{tag}-dry"),
            );
            let wet_tl = timeline_of(vec![audio_track(vec![denoised(clip.clone(), false)])]);
            let wet = export_audio(&wet_tl, &assets, &dir, &format!("{tag}-wet"));
            let other = with_afftdn_delay(!delays, || export_audio(&wet_tl, &assets, &dir, &format!("{tag}-other")));
            let tracked = export_audio(
                &timeline_of(vec![audio_track(vec![denoised(clip, true)])]),
                &assets,
                &dir,
                &format!("{tag}-tracked"),
            );

            let (wet_lag, tracked_lag, other_lag) = (lag(&dry, &wet), lag(&dry, &tracked), lag(&dry, &other));
            let hops = (2 * (rate / 80)) as f64 / f64::from(rate) * OUT as f64;
            let delay = if delays { hops } else { -hops };
            // The burst on the timeline, a stretch of hiss well after it, and the loud ends of the clip.
            let burst_at = 0.25 + BURST.0 / speed;
            let end = 0.25 + 4.0 / speed;
            let at = |t: f64| (t * OUT as f64) as usize;
            let head = |s: &[f32]| rms_db(&s[at(0.25)..at(0.262)]);
            let tail = |s: &[f32]| rms_db(&s[at(end - 0.02)..at(end)]);
            let burst = |s: &[f32]| {
                let from = ((burst_at + 0.03) * OUT as f64) as usize;
                rms_db(&s[from..from + (0.04 / speed * OUT as f64) as usize])
            };
            let hiss = |s: &[f32]| {
                let from = ((burst_at + 0.4) * OUT as f64) as usize;
                rms_db(&s[from..from + OUT / 2])
            };
            eprintln!(
                "{rate} Hz at {speed}x: lag {wet_lag} (tracked {tracked_lag}), the other chain {other_lag} \
                 (delay {delay:.1}); burst {:.2} / {:.2} dB, head {:.2} / {:.2}, tail {:.2} / {:.2}, \
                 hiss {:.2} -> {:.2} dB (tracked {:.2})",
                burst(&dry),
                burst(&wet),
                head(&dry),
                head(&wet),
                tail(&dry),
                tail(&wet),
                hiss(&dry),
                hiss(&wet),
                hiss(&tracked)
            );
            // Where the filter does not delay the chain is cut at the clip's length, which 4.4's `atempo`
            // runs a few ms past when the sound is dry.
            let length = if delays { dry.len() } else { at(end) };
            assert_eq!(wet.len(), length, "{tag}: the clip keeps its length");
            assert_eq!(wet_lag, 0, "{tag}: the denoised sound lands where the dry one does");
            assert_eq!(tracked_lag, 0, "{tag}: and with the floor tracked");
            assert!(
                (other_lag as f64 - delay).abs() <= 1.0,
                "{tag}: the other chain is {other_lag} samples late, the probe says {delay:.1}"
            );
            assert!((burst(&dry) - burst(&wet)).abs() < 0.5, "{tag}: the loud part passes");
            assert!((head(&dry) - head(&wet)).abs() < 1.5, "{tag}: the head is not faded in");
            assert!((tail(&dry) - tail(&wet)).abs() < 0.5, "{tag}: the tail is not lost");
            // About 21 dB on 6.1, 12 to 14 on 4.4 and 5.0.
            assert!(hiss(&wet) < hiss(&dry) - 10.0, "{tag}: the hiss is taken down");
            assert!(hiss(&tracked) < hiss(&dry), "{tag}: a tracked floor takes some down too");
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
#[ignore = "needs the ffmpeg and ffprobe binaries"]
fn the_preview_decode_of_a_denoised_clip_keeps_its_sync() {
    let dir = scratch("preview");
    let asset = source(&dir, 44_100);
    let path = Path::new(&asset.path);
    let chain = audio_effects_filter(&[AudioEffect::Denoise {
        reduction_db: 24.0,
        floor_db: -40.0,
        track_noise: false,
    }]);
    let decode = |filters: Option<&str>| -> Vec<f32> {
        audio_pcm(path, 0.5, 3.0, 48_000, filters)
            .unwrap()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.0)
            .collect()
    };
    let (dry, wet) = (decode(None), decode(chain.as_deref()));
    let found = lag(&dry, &wet);
    eprintln!("preview: {} / {} samples, lag {found}", dry.len(), wet.len());
    assert_eq!(wet.len(), dry.len(), "the window keeps its length");
    assert_eq!(found, 0, "the denoised window lands where the dry one does");
    std::fs::remove_dir_all(&dir).ok();
}
