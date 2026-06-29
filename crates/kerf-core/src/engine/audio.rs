//! Audio analysis driven by the `ffmpeg` binary plus light DSP on decoded PCM.
//!
//! Like [`super::cli`], everything here needs only the FFmpeg *binaries* (it
//! reuses cli's process helpers and PCM decode), so it compiles and runs in the
//! `--no-default-features` build — no dev libraries, no extra system deps.

use std::path::Path;
use std::process::Stdio;

use super::cli::{command, decode_audio_mono_f32, ffmpeg_bin, launch_err};
use crate::error::{Error, Result};
use crate::model::Loudness;

// ---- loudness (EBU R128) ---------------------------------------------------

/// Measure EBU R128 loudness of the first audio stream in a single `loudnorm`
/// analysis pass. `loudnorm=print_format=json` prints a JSON object to stderr
/// with the measured `input_i` / `input_lra` / `input_tp` / `input_thresh`.
pub fn measure_loudness(path: &Path) -> Result<Loudness> {
    let bin = ffmpeg_bin();
    let output = command(&bin)
        .args(["-hide_banner", "-nostats"])
        .arg("-i")
        .arg(path)
        .args(["-map", "0:a:0", "-af", "loudnorm=print_format=json", "-f", "null", "-"])
        .stdout(Stdio::null())
        .output()
        .map_err(|e| launch_err(&bin, e))?;
    // loudnorm prints to stderr regardless of exit status.
    parse_loudness(&String::from_utf8_lossy(&output.stderr))
        .ok_or_else(|| Error::Engine("could not parse loudnorm measurement".to_string()))
}

/// Pull the trailing JSON object `loudnorm=print_format=json` prints and read the
/// measured input values out of it. Pure (no I/O) so it is unit-tested.
fn parse_loudness(stderr: &str) -> Option<Loudness> {
    // The JSON object is the last `{ ... }` block on stderr; its values carry no
    // nested braces, so the first `}` after the last `{` closes it.
    let start = stderr.rfind('{')?;
    let end = start + stderr[start..].find('}')?;
    let json: serde_json::Value = serde_json::from_str(&stderr[start..=end]).ok()?;
    let field = |key: &str| -> Option<f64> { json.get(key)?.as_str()?.trim().parse().ok() };
    Some(Loudness {
        integrated_lufs: field("input_i")?,
        loudness_range: field("input_lra")?,
        true_peak_dbtp: field("input_tp")?,
        threshold_lufs: field("input_thresh")?,
    })
}

// ---- energy / RMS envelope -------------------------------------------------

/// Decode the first audio stream and reduce it to `buckets` RMS magnitudes in
/// `0.0..=1.0` — a perceptual loudness-over-time curve. Peaks (see
/// [`super::cli::waveform`]) overstate brief transients; RMS tracks how loud each
/// slice actually feels, so an agent can match cut pacing to musical energy or
/// find the quiet/loud passages. Same shape as the waveform so both render alike.
pub fn energy_envelope(path: &Path, buckets: usize, sample_rate: u32) -> Result<Vec<f32>> {
    let samples = decode_audio_mono_f32(path, sample_rate)?;
    Ok(rms_buckets(&samples, buckets.max(1)))
}

/// Root-mean-square magnitude per evenly divided bucket. Pure, so it is tested.
fn rms_buckets(samples: &[f32], buckets: usize) -> Vec<f32> {
    if samples.is_empty() {
        return vec![0.0; buckets];
    }
    let mut out = Vec::with_capacity(buckets);
    for b in 0..buckets {
        let lo = b * samples.len() / buckets;
        let hi = ((b + 1) * samples.len() / buckets).max(lo + 1).min(samples.len());
        let sum_sq: f64 = samples[lo..hi].iter().map(|s| (*s as f64) * (*s as f64)).sum();
        let rms = (sum_sq / (hi - lo) as f64).sqrt();
        out.push((rms as f32).clamp(0.0, 1.0));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_buckets_have_requested_length_and_track_amplitude() {
        // A constant-amplitude signal has RMS equal to that amplitude.
        let samples = vec![0.5_f32; 1000];
        let buckets = rms_buckets(&samples, 8);
        assert_eq!(buckets.len(), 8);
        for b in buckets {
            assert!((b - 0.5).abs() < 1e-4, "constant 0.5 signal should give RMS 0.5, got {b}");
        }
        assert_eq!(rms_buckets(&[], 4), vec![0.0; 4]);
    }

    #[test]
    fn parses_loudnorm_json_block() {
        let stderr = "\
[Parsed_loudnorm_0 @ 0x55] \n\
{\n\
\t\"input_i\" : \"-14.42\",\n\
\t\"input_tp\" : \"-2.45\",\n\
\t\"input_lra\" : \"7.60\",\n\
\t\"input_thresh\" : \"-24.86\",\n\
\t\"output_i\" : \"-16.00\",\n\
\t\"normalization_type\" : \"dynamic\",\n\
\t\"target_offset\" : \"0.34\"\n\
}\n";
        let l = parse_loudness(stderr).expect("should parse");
        assert_eq!(l.integrated_lufs, -14.42);
        assert_eq!(l.true_peak_dbtp, -2.45);
        assert_eq!(l.loudness_range, 7.60);
        assert_eq!(l.threshold_lufs, -24.86);
    }

    #[test]
    fn parse_loudness_rejects_non_json() {
        assert!(parse_loudness("no json here").is_none());
    }
}
