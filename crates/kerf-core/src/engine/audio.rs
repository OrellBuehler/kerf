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

// ---- onset / transient detection -------------------------------------------

/// Detect onset (transient) timestamps in seconds — the moments where new sound
/// energy arrives (a drum hit, a note attack, a hard edit point). An agent can
/// snap cut points to these so edits land on the beat rather than mid-phrase.
/// `sensitivity` is the adaptive-threshold std-dev multiplier; higher = fewer,
/// stronger onsets.
pub fn detect_onsets(path: &Path, sensitivity: f64) -> Result<Vec<f64>> {
    const SR: u32 = 22_050;
    let samples = decode_audio_mono_f32(path, SR)?;
    let (env, frame_rate) = onset_envelope(&samples, SR);
    Ok(pick_onsets(&env, frame_rate, sensitivity))
}

/// Frame hop for the onset / tempo envelopes. At 22.05 kHz this is a ~43 Hz
/// envelope (~23 ms per frame) — fine enough for transients, cheap to compute.
pub(super) const ONSET_HOP: usize = 512;

/// Build an onset-strength envelope: per-frame positive change in log energy
/// (half-wave-rectified energy flux). Returns the envelope and its frame rate in
/// Hz. Pure (no I/O), so it is unit-tested and shared with tempo estimation.
pub(super) fn onset_envelope(samples: &[f32], sample_rate: u32) -> (Vec<f32>, f64) {
    let frame_rate = sample_rate as f64 / ONSET_HOP as f64;
    if samples.len() < ONSET_HOP * 2 {
        return (Vec::new(), frame_rate);
    }
    let n_frames = samples.len() / ONSET_HOP;
    let mut log_energy = Vec::with_capacity(n_frames);
    for f in 0..n_frames {
        let frame = &samples[f * ONSET_HOP..(f + 1) * ONSET_HOP];
        let energy: f64 = frame.iter().map(|s| (*s as f64) * (*s as f64)).sum();
        log_energy.push((1.0 + energy).ln());
    }
    let mut env = Vec::with_capacity(n_frames);
    env.push(0.0);
    for i in 1..log_energy.len() {
        env.push((log_energy[i] - log_energy[i - 1]).max(0.0) as f32);
    }
    (env, frame_rate)
}

/// Pick onset times (seconds) from an envelope: local maxima that clear an
/// adaptive threshold (local mean + `sensitivity`·std), debounced by ~30 ms.
/// Pure, so it is unit-tested.
fn pick_onsets(env: &[f32], frame_rate: f64, sensitivity: f64) -> Vec<f64> {
    if env.is_empty() || frame_rate <= 0.0 {
        return Vec::new();
    }
    let win = ((frame_rate * 0.1).round() as usize).max(1); // ~100 ms half-window
    let min_gap = ((frame_rate * 0.03).round() as usize).max(1); // ~30 ms debounce
    let mut onsets = Vec::new();
    let mut last: Option<usize> = None;
    for i in 0..env.len() {
        let lo = i.saturating_sub(win);
        let hi = (i + win + 1).min(env.len());
        let slice = &env[lo..hi];
        let mean = slice.iter().copied().sum::<f32>() / slice.len() as f32;
        let var = slice.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / slice.len() as f32;
        let threshold = mean + sensitivity as f32 * var.sqrt();
        let is_peak = env[i] > threshold
            && (i == 0 || env[i] >= env[i - 1])
            && (i + 1 >= env.len() || env[i] >= env[i + 1]);
        if is_peak && last.map_or(true, |p| i - p >= min_gap) {
            onsets.push(i as f64 / frame_rate);
            last = Some(i);
        }
    }
    onsets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_onsets_finds_isolated_spikes() {
        // Flat low background with three strong spikes 1 s apart at 100 Hz.
        let mut env = vec![0.01_f32; 300];
        for &i in &[50usize, 150, 250] {
            env[i] = 1.0;
        }
        let onsets = pick_onsets(&env, 100.0, 1.5);
        assert_eq!(onsets.len(), 3, "got {onsets:?}");
        for (got, want) in onsets.iter().zip([0.5, 1.5, 2.5]) {
            assert!((got - want).abs() < 0.02, "onset {got} ~ {want}");
        }
        assert!(pick_onsets(&[], 100.0, 1.5).is_empty());
    }

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
