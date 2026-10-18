//! Mel-band energies of 16 kHz mono audio, Whisper's framing: a 400-sample periodic Hann
//! window, hop 160 (100 frames a second), reflect padding of 200 samples so frame `f` is
//! centred on sample `f * 160`, the power spectrum through a Slaney mel filterbank
//! (0–8 kHz, area-normalized). `n` samples give `n / 160` frames. The filterbank is
//! computed, not loaded. Diarization's front end ([`super::diarize`]).

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// The sample rate the transcript refinement decodes at.
pub const RATE: u32 = 16_000;
/// FFT size (25 ms).
pub const N_FFT: usize = 400;
/// Hop length (10 ms).
pub const HOP: usize = 160;
/// Frequency bins of the one-sided spectrum.
pub const N_BINS: usize = N_FFT / 2 + 1;

const F_SP: f64 = 200.0 / 3.0;
const MIN_LOG_HZ: f64 = 1000.0;

fn log_step() -> f64 {
    6.4f64.ln() / 27.0
}

/// Slaney: linear below 1 kHz, logarithmic above.
fn hz_to_mel(f: f64) -> f64 {
    let min_log_mel = MIN_LOG_HZ / F_SP;
    if f >= MIN_LOG_HZ {
        min_log_mel + (f / MIN_LOG_HZ).ln() / log_step()
    } else {
        f / F_SP
    }
}

fn mel_to_hz(m: f64) -> f64 {
    let min_log_mel = MIN_LOG_HZ / F_SP;
    if m >= min_log_mel {
        MIN_LOG_HZ * (log_step() * (m - min_log_mel)).exp()
    } else {
        F_SP * m
    }
}

/// Mel filterbank, `n_mels × N_BINS` row-major, for 16 kHz audio and a 400-point FFT.
pub fn mel_filters(n_mels: usize) -> Vec<f32> {
    let sr = f64::from(RATE);
    let fftfreqs: Vec<f64> = (0..N_BINS).map(|i| i as f64 * sr / N_FFT as f64).collect();
    let (lo, hi) = (hz_to_mel(0.0), hz_to_mel(sr / 2.0));
    let mel_f: Vec<f64> = (0..n_mels + 2)
        .map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (n_mels + 1) as f64))
        .collect();
    let mut w = vec![0f32; n_mels * N_BINS];
    for m in 0..n_mels {
        let enorm = 2.0 / (mel_f[m + 2] - mel_f[m]);
        for (k, &f) in fftfreqs.iter().enumerate() {
            let lower = (f - mel_f[m]) / (mel_f[m + 1] - mel_f[m]);
            let upper = (mel_f[m + 2] - f) / (mel_f[m + 2] - mel_f[m + 1]);
            w[m * N_BINS + k] = (lower.min(upper).max(0.0) * enorm) as f32;
        }
    }
    w
}

/// Calls `each(frame, energies)` with every frame's `n_mels` mel energies (before the log),
/// one frame at a time so a long file never holds its spectrogram.
pub fn mel_frames(audio: &[f32], n_mels: usize, mut each: impl FnMut(usize, &[f32])) {
    let n = audio.len();
    let frames = n / HOP;
    if frames == 0 {
        return;
    }
    // numpy's "reflect": the edge sample is not repeated.
    let at = |i: isize| -> f32 {
        if n == 1 {
            return audio[0];
        }
        let p = 2 * (n as isize - 1);
        let mut j = i.rem_euclid(p);
        if j >= n as isize {
            j = p - j;
        }
        audio[j as usize]
    };
    let window: Vec<f32> = (0..N_FFT)
        .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N_FFT as f64).cos()) as f32)
        .collect();
    // Each band's non-zero stretch of bins, so a frame costs its triangles, not the matrix.
    let filters = mel_filters(n_mels);
    let bands: Vec<(usize, Vec<f32>)> = (0..n_mels)
        .map(|m| {
            let row = &filters[m * N_BINS..(m + 1) * N_BINS];
            let first = row.iter().position(|v| *v > 0.0).unwrap_or(0);
            let last = row.iter().rposition(|v| *v > 0.0).map_or(first, |l| l + 1);
            (first, row[first..last].to_vec())
        })
        .collect();
    let fft: std::sync::Arc<dyn RealToComplex<f32>> = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let mut input = fft.make_input_vec();
    let mut spectrum: Vec<Complex<f32>> = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut power = vec![0f32; N_BINS];
    let mut mel = vec![0f32; n_mels];
    let pad = (N_FFT / 2) as isize;
    for f in 0..frames {
        let base = (f * HOP) as isize - pad;
        for (i, x) in input.iter_mut().enumerate() {
            *x = at(base + i as isize) * window[i];
        }
        if fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch).is_err() {
            return;
        }
        for (p, c) in power.iter_mut().zip(&spectrum) {
            *p = c.norm_sqr();
        }
        for (m, (first, w)) in mel.iter_mut().zip(&bands) {
            *m = w.iter().zip(&power[*first..]).map(|(a, b)| a * b).sum();
        }
        each(f, &mel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filterbank_shape_and_coverage() {
        let w = mel_filters(80);
        assert_eq!(w.len(), 80 * N_BINS);
        for m in 0..80 {
            let row = &w[m * N_BINS..(m + 1) * N_BINS];
            assert!(row.iter().all(|v| *v >= 0.0));
            assert!(row.iter().sum::<f32>() > 0.0, "band {m}");
        }
        // Slaney scale is linear below 1 kHz.
        assert!((hz_to_mel(500.0) - 7.5).abs() < 1e-9);
        assert!((mel_to_hz(hz_to_mel(3000.0)) - 3000.0).abs() < 1e-6);
    }

    #[test]
    fn a_tone_lands_in_its_band() {
        let audio: Vec<f32> = (0..16_000)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 16_000.0).sin())
            .collect();
        let mut frames = 0;
        let mut col = Vec::new();
        mel_frames(&audio, 40, |f, m| {
            frames += 1;
            if f == 50 {
                col = m.to_vec();
            }
        });
        assert_eq!(frames, 100);
        let peak = col.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        let w = mel_filters(40);
        let k = 1000 * N_FFT / 16_000;
        let best = (0..40)
            .max_by(|a, b| w[a * N_BINS + k].total_cmp(&w[b * N_BINS + k]))
            .unwrap();
        assert_eq!(peak, best);
    }
}
