//! Lining two recordings of one event up by their sound: a camera's scratch track
//! against the recorder's, two cameras on one stage.
//!
//! [`find_offset`] returns the lag `L` (in samples) for which `b[n] ≈ g · a[n + L]`: when `b`
//! started recording two seconds after `a`, `L = 2 · sample_rate`. Two passes:
//!
//! 1. **Coarse.** Both signals are low-pass filtered (windowed sinc), decimated to ≤ 8 kHz,
//!    DC-removed and normalised (further for very long recordings, so the transform stays ≤ 4 M points).
//!    The generalised cross-correlation with partial phase transform weighting (GCC-PHAT-β:
//!    `R = A·B* / |A·B*|^β`) whitens the spectra, so the peak is sharp and independent of the
//!    microphones' gains and frequency responses; β < 1 keeps noise-only bins from dominating.
//! 2. **Refine.** Around the coarse peak, the same correlation is computed at the full rate on
//!    the loudest common window, and the peak is refined by parabolic interpolation to a fraction
//!    of a sample.
//!
//! Two figures say how much to trust the result: `confidence`, the coarse peak's height over the
//! RMS of the correlation, and `distinct`, its height over the highest peak more than 50 ms away
//! (periodic material such as music or a steady hum can match at several lags).
//!
//! Pure: no I/O. Knapp & Carter, "The generalized correlation method for estimation of time
//! delay", IEEE Trans. ASSP 24(4), 1976.

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

/// Tuning of [`find_offset`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SyncOptions {
    /// Largest |lag| considered, in samples (`None`: anything with enough overlap).
    pub max_lag: Option<i64>,
    /// Upper bound of the coarse search's sample rate (Hz).
    pub coarse_rate: u32,
    /// Most samples (both signals together) the coarse transform may hold; longer recordings are
    /// decimated further.
    pub max_coarse_samples: usize,
    /// PHAT weighting exponent (1 = full whitening, 0 = plain cross-correlation).
    pub phat_beta: f32,
    /// Length of the full-rate refinement window (samples).
    pub refine_window: usize,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            max_lag: None,
            coarse_rate: 8000,
            max_coarse_samples: 1 << 22,
            phat_beta: 0.75,
            refine_window: 1 << 16,
        }
    }
}

/// The offset found between two recordings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SyncResult {
    /// `b[n]` lines up with `a[n + lag]` (samples at the input rate).
    pub lag: i64,
    /// The same with sub-sample precision (parabolic interpolation of the refined peak).
    pub lag_fine: f64,
    /// Coarse peak height over the RMS of the correlation.
    pub confidence: f32,
    /// Coarse peak height over the next-highest peak more than 50 ms away.
    pub distinct: f32,
    /// The signals correlate negatively (one recording has inverted polarity).
    pub inverted: bool,
}

/// [`SyncResult::confidence`] a match has to reach to be trusted.
pub const MIN_CONFIDENCE: f32 = 8.0;
/// [`SyncResult::distinct`] a match has to reach to be trusted.
pub const MIN_DISTINCT: f32 = 1.5;

impl SyncResult {
    /// Whether the match is clear enough to trust.
    pub fn reliable(&self) -> bool {
        self.confidence >= MIN_CONFIDENCE && self.distinct >= MIN_DISTINCT
    }
}

/// Low-pass filter (Blackman-windowed sinc, cutoff at 0.42 × the new sample rate) and keep every
/// `d`-th sample.
pub fn decimate(x: &[f32], d: usize) -> Vec<f32> {
    if d <= 1 {
        return x.to_vec();
    }
    use std::f64::consts::PI;
    let half = 8 * d;
    let fc = 0.42 / d as f64;
    let n = 2 * half + 1;
    let mut h: Vec<f64> = (0..n)
        .map(|k| {
            let t = k as f64 - half as f64;
            let sinc = if t == 0.0 {
                2.0 * fc
            } else {
                (2.0 * PI * fc * t).sin() / (PI * t)
            };
            let w =
                0.42 - 0.5 * (2.0 * PI * k as f64 / (n - 1) as f64).cos() + 0.08 * (4.0 * PI * k as f64 / (n - 1) as f64).cos();
            sinc * w
        })
        .collect();
    let s: f64 = h.iter().sum();
    h.iter_mut().for_each(|v| *v /= s);
    let taps: Vec<f32> = h.into_iter().map(|v| v as f32).collect();
    (0..x.len().div_ceil(d))
        .map(|i| {
            let c = (i * d) as isize;
            let lo = (c - half as isize).max(0) as usize;
            let hi = ((c + half as isize + 1) as usize).min(x.len());
            let k0 = (lo as isize - (c - half as isize)) as usize;
            x[lo..hi].iter().zip(&taps[k0..]).map(|(v, t)| v * t).sum()
        })
        .collect()
}

/// Remove the mean and scale to unit RMS (silence stays silence).
fn normalise(x: &mut [f32]) {
    if x.is_empty() {
        return;
    }
    let mean = (x.iter().map(|&v| f64::from(v)).sum::<f64>() / x.len() as f64) as f32;
    x.iter_mut().for_each(|v| *v -= mean);
    let rms = (x.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / x.len() as f64).sqrt();
    if rms > 1e-12 {
        let k = (1.0 / rms) as f32;
        x.iter_mut().for_each(|v| *v *= k);
    }
}

/// GCC-PHAT-β of `a` and `b`, zero-padded to `n` (a power of two ≥ `a.len() + b.len()`):
/// `r[L mod n] = Σ a[i + L]·b[i]` with whitened spectra. Only bins whose frequency (cycles per
/// sample) lies in `band` contribute.
fn gcc(a: &[f32], b: &[f32], n: usize, beta: f32, band: (f32, f32)) -> Vec<f32> {
    let mut planner = RealFftPlanner::<f32>::new();
    let forward = planner.plan_fft_forward(n);
    let spectrum = |x: &[f32]| {
        let mut input = vec![0.0f32; n];
        input[..x.len()].copy_from_slice(x);
        let mut out = forward.make_output_vec();
        forward.process(&mut input, &mut out).expect("sizes from the plan");
        out
    };
    let (fa, fb) = (spectrum(a), spectrum(b));
    let mut r: Vec<Complex<f32>> = fa
        .iter()
        .zip(&fb)
        .enumerate()
        .map(|(k, (x, y))| {
            let f = k as f32 / n as f32;
            if f < band.0 || f > band.1 {
                return Complex::default();
            }
            let c = x * y.conj();
            let mag = c.norm();
            if mag <= 1e-30 {
                return Complex::default();
            }
            c * mag.powf(-beta)
        })
        .collect();
    // DC and Nyquist of a real signal's spectrum are real.
    let last = r.len() - 1;
    r[0].im = 0.0;
    r[last].im = 0.0;
    let inverse = planner.plan_fft_inverse(n);
    let mut out = inverse.make_output_vec();
    if inverse.process(&mut r, &mut out).is_err() {
        return vec![0.0; n];
    }
    out
}

fn corr_at(r: &[f32], lag: i64) -> f32 {
    r[lag.rem_euclid(r.len() as i64) as usize]
}

/// Find the lag `L` with `b[n] ≈ g·a[n + L]` (see the module docs). `None` when either signal is
/// too short or silent, or no lag leaves enough overlap.
pub fn find_offset(a: &[f32], b: &[f32], sample_rate: u32, opts: &SyncOptions) -> Option<SyncResult> {
    let sr = sample_rate.max(1) as usize;
    let min_len = (sr / 10).max(64);
    if a.len() < min_len || b.len() < min_len {
        return None;
    }
    let energy = |x: &[f32]| x.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>();
    if energy(a) < 1e-12 || energy(b) < 1e-12 {
        return None;
    }
    // ---- coarse
    let d = sr
        .div_ceil(opts.coarse_rate.max(500) as usize)
        .max((a.len() + b.len()).div_ceil(opts.max_coarse_samples.max(1 << 12)))
        .max(1);
    // Decimated before they are normalised, so each side is copied once (the filter passes DC,
    // which the normalisation then removes).
    let (mut ca, mut cb) = (decimate(a, d), decimate(b, d));
    normalise(&mut ca);
    normalise(&mut cb);
    let (la, lb) = (ca.len() as i64, cb.len() as i64);
    let n = (ca.len() + cb.len()).next_power_of_two();
    let rate_c = sr as f32 / d as f32;
    let band = ((40.0 / rate_c).min(0.05), if d > 1 { 0.40 } else { 0.5 });
    let r = gcc(&ca, &cb, n, opts.phat_beta, band);
    drop((ca, cb));
    // Valid lags: b[0..lb) against a[L..L+lb) must overlap by a minimum amount.
    let min_overlap = ((rate_c as i64) / 2).min(la.min(lb) / 4).max(8);
    let max_lag_c = opts.max_lag.map(|m| m.abs() / d as i64 + 1);
    let (lo, hi) = (-(lb - min_overlap), la - min_overlap);
    if hi < lo {
        return None;
    }
    let in_range = |l: i64| max_lag_c.is_none_or(|m| l.abs() <= m);
    let mut best = (0i64, 0.0f32);
    let (mut sum2, mut count) = (0.0f64, 0usize);
    for l in (lo..=hi).filter(|l| in_range(*l)) {
        let v = corr_at(&r, l);
        sum2 += f64::from(v) * f64::from(v);
        count += 1;
        if v.abs() > best.1.abs() {
            best = (l, v);
        }
    }
    if count == 0 || best.1 == 0.0 {
        return None;
    }
    let rms = (sum2 / count as f64).sqrt().max(1e-30) as f32;
    let confidence = best.1.abs() / rms;
    let guard = ((rate_c * 0.05) as i64).max(2);
    let second = (lo..=hi)
        .filter(|l| (l - best.0).abs() > guard && in_range(*l))
        .map(|l| corr_at(&r, l).abs())
        .fold(0.0f32, f32::max);
    drop(r);
    let distinct = if second > 0.0 { best.1.abs() / second } else { f32::INFINITY };
    let inverted = best.1 < 0.0;
    let coarse = best.0 * d as i64;
    let coarse_only = SyncResult {
        lag: coarse,
        lag_fine: coarse as f64,
        confidence,
        distinct,
        inverted,
    };
    // ---- refine at the full rate
    let margin = (2 * d + 4) as i64;
    let (fa, fb) = (a.len() as i64, b.len() as i64);
    let ov0 = (-coarse).max(0);
    let ov1 = fb.min(fa - coarse);
    if ov1 - ov0 < 16 {
        return Some(coarse_only);
    }
    let w = ((ov1 - ov0) as usize).min(opts.refine_window.max(256)) as i64;
    // The window of b with the most energy on both sides (a lag-shifted), judged on the energies
    // of `hop`-long blocks (a window is `per` of them), so nothing the length of the input is kept.
    let start = {
        let hop = (w / 4).max(1);
        let per = (w / hop) as usize;
        let blocks = |x: &[f32], from: i64| -> Vec<f64> {
            (0..(ov1 - ov0) / hop)
                .map(|k| {
                    let s = (from + k * hop) as usize;
                    x[s..s + hop as usize].iter().map(|&v| f64::from(v) * f64::from(v)).sum()
                })
                .collect()
        };
        let (ea, eb) = (blocks(a, ov0 + coarse), blocks(b, ov0));
        let (mut k, mut best_s, mut best_e) = (0usize, ov0, -1.0f64);
        while ov0 + k as i64 * hop + w <= ov1 {
            let sum = |e: &[f64]| e[k..k + per].iter().sum::<f64>();
            let e = sum(&ea).min(sum(&eb));
            if e > best_e {
                best_e = e;
                best_s = ov0 + k as i64 * hop;
            }
            k += 1;
        }
        best_s
    };
    let mut bw: Vec<f32> = b[start as usize..(start + w) as usize].to_vec();
    let mut aw: Vec<f32> = (start + coarse - margin..start + coarse + w + margin)
        .map(|i| if (0..fa).contains(&i) { a[i as usize] } else { 0.0 })
        .collect();
    normalise(&mut aw);
    normalise(&mut bw);
    let n2 = (aw.len() + bw.len()).next_power_of_two();
    let r2 = gcc(&aw, &bw, n2, opts.phat_beta, (20.0 / sr as f32, 0.5));
    // r2[L'] = Σ aw[i + L']·bw[i]; L' = margin + δ.
    let sign = if inverted { -1.0 } else { 1.0 };
    let (mut bl, mut bv) = (0i64, f32::MIN);
    for l in 0..=2 * margin {
        let v = corr_at(&r2, l) * sign;
        if v > bv {
            bv = v;
            bl = l;
        }
    }
    let (y0, y1, y2) = (corr_at(&r2, bl - 1) * sign, bv, corr_at(&r2, bl + 1) * sign);
    let den = y0 - 2.0 * y1 + y2;
    let frac = if den.abs() > 1e-20 {
        (0.5 * (y0 - y2) / den).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    let lag = coarse + bl - margin;
    Some(SyncResult {
        lag,
        lag_fine: lag as f64 + f64::from(frac),
        ..coarse_only
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift, uniform in `[-1, 1)`.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
        }

        fn uniform(&mut self) -> f32 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            ((x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
        }
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    /// Speech-like test signal: voiced syllables (harmonics of a gliding pitch under an envelope),
    /// noise bursts (consonants) and pauses.
    fn speechy(sr: f64, secs: f64, seed: u64) -> Vec<f32> {
        use std::f64::consts::PI;
        let n = (sr * secs) as usize;
        let mut rng = Rng::new(seed);
        let mut out = vec![0.0f32; n];
        let mut i = 0usize;
        while i < n {
            let len = ((0.08 + 0.25 * f64::from(rng.uniform() * 0.5 + 0.5)) * sr) as usize;
            let kind = rng.uniform();
            let f0 = 110.0 + 120.0 * f64::from(rng.uniform() * 0.5 + 0.5);
            let glide = 1.0 + 0.3 * f64::from(rng.uniform());
            let amp = 0.2 + 0.3 * (rng.uniform() * 0.5 + 0.5);
            for k in 0..len.min(n - i) {
                let t = k as f64 / sr;
                let env = (PI * k as f64 / len as f64).sin() as f32;
                let v = if kind > 0.3 {
                    let f = f0 * (1.0 + (glide - 1.0) * k as f64 / len as f64);
                    (1..8)
                        .map(|h| ((2.0 * PI * f * f64::from(h) * t).sin() / f64::from(h)) as f32)
                        .sum::<f32>()
                } else if kind > -0.4 {
                    rng.uniform()
                } else {
                    0.0
                };
                out[i + k] = v * env * amp;
            }
            i += len + ((rng.uniform() * 0.5 + 0.5) * 0.15 * sr as f32) as usize;
        }
        out
    }

    /// `x` delayed by `delay` samples (fractional: windowed-sinc interpolation), scaled by `gain`,
    /// optionally coloured by a zero-phase low-pass (`[k, 1-2k, k]`, so the delay stays exact),
    /// with white noise `snr_db` below the signal.
    fn mic(x: &[f32], delay: f64, gain: f32, lowpass: Option<f32>, snr_db: f32, len: usize, seed: u64) -> Vec<f32> {
        use std::f64::consts::PI;
        let mut out = vec![0.0f32; len];
        let di = delay.floor() as i64;
        let fr = delay - di as f64;
        for (n, o) in out.iter_mut().enumerate() {
            let src = n as i64 - di;
            if fr == 0.0 {
                if (0..x.len() as i64).contains(&src) {
                    *o = x[src as usize];
                }
            } else {
                let mut acc = 0.0f64;
                for k in -16i64..=16 {
                    let j = src - k;
                    if j < 0 || j as usize >= x.len() {
                        continue;
                    }
                    let t = k as f64 - fr;
                    let s = (PI * t).sin() / (PI * t);
                    let w = 0.5 + 0.5 * (PI * t / 17.0).cos();
                    acc += f64::from(x[j as usize]) * s * w;
                }
                *o = acc as f32;
            }
        }
        if let Some(k) = lowpass {
            let src = out.clone();
            for i in 1..len.saturating_sub(1) {
                out[i] = k * src[i - 1] + (1.0 - 2.0 * k) * src[i] + k * src[i + 1];
            }
        }
        let nrms = rms(&out) as f32 * 10f32.powf(-snr_db / 20.0) * 3f32.sqrt();
        let mut rng = Rng::new(seed);
        for v in &mut out {
            *v = *v * gain + rng.uniform() * nrms * gain;
        }
        out
    }

    fn sine(freq: f64, sr: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / sr).sin() as f32)
            .collect()
    }

    const SR: u32 = 16_000;

    #[test]
    fn decimate_keeps_low_frequencies_and_removes_high_ones() {
        let (dl, dh) = (
            decimate(&sine(300.0, 16_000.0, 16_000), 2),
            decimate(&sine(7_000.0, 16_000.0, 16_000), 2),
        );
        assert_eq!(dl.len(), 8000);
        assert!((rms(&dl[100..7900]) - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01);
        assert!(rms(&dh[100..7900]) < 0.01);
    }

    #[test]
    fn a_pure_delay_is_found_exactly_either_way() {
        let s = speechy(f64::from(SR), 10.0, 1);
        for delay in [0i64, 1, 7, 4_321, 32_001, -1_234, -17_000] {
            // b[n] = s[n + delay]
            let b: Vec<f32> = (0..120_000i64)
                .map(|n| s.get((n + delay) as usize).copied().filter(|_| n + delay >= 0).unwrap_or(0.0))
                .collect();
            let r = find_offset(&s, &b, SR, &SyncOptions::default()).unwrap();
            assert_eq!(r.lag, delay, "{r:?}");
            assert!(r.reliable(), "{r:?}");
            assert!(!r.inverted);
        }
    }

    /// Microphones recording one talker: different gains, noise (SNR 25 and 6 dB), one coloured
    /// by a low-pass, fractional delays. Every offset within one sample.
    #[test]
    fn noisy_coloured_microphones_line_up_within_a_sample() {
        let src = speechy(f64::from(SR), 15.0, 7);
        let len = src.len();
        let cases: [(f64, f32, Option<f32>, f32); 3] = [
            (0.0, 1.0, None, 40.0),
            (2_345.0, 0.1, None, 25.0),
            (-11_337.4, 3.0, Some(0.3), 6.0),
        ];
        let mics: Vec<Vec<f32>> = cases
            .iter()
            .enumerate()
            .map(|(i, c)| mic(&src, c.0, c.1, c.2, c.3, len, 100 + i as u64))
            .collect();
        for (k, c) in cases.iter().enumerate().skip(1) {
            // mic_k[n] = src[n - d_k] = mic_0[n - d_k + d_0] → lag = d_0 - d_k
            let expect = cases[0].0 - c.0;
            let r = find_offset(&mics[0], &mics[k], SR, &SyncOptions::default()).unwrap();
            assert!((r.lag as f64 - expect).abs() <= 1.0, "mic {k}: {r:?} vs {expect}");
            assert!((r.lag_fine - expect).abs() < 0.75, "mic {k}: {r:?} vs {expect}");
            assert!(r.reliable(), "{r:?}");
        }
    }

    /// At 48 kHz the coarse pass decimates by six; a long recording is decimated further still.
    #[test]
    fn a_heavily_decimated_search_still_lands_on_the_sample() {
        let sr = 48_000u32;
        let s = speechy(f64::from(sr), 8.0, 21);
        for (delay, opts) in [
            (12_345i64, SyncOptions::default()),
            (
                -54_321,
                SyncOptions {
                    max_coarse_samples: 1 << 16,
                    ..Default::default()
                },
            ),
        ] {
            let b: Vec<f32> = (0..300_000i64)
                .map(|n| s.get((n + delay) as usize).copied().filter(|_| n + delay >= 0).unwrap_or(0.0))
                .collect();
            let r = find_offset(&s, &b, sr, &opts).unwrap();
            assert_eq!(r.lag, delay, "{r:?}");
            assert!(r.reliable(), "{r:?}");
        }
    }

    #[test]
    fn inverted_polarity_is_found() {
        let s = speechy(f64::from(SR), 6.0, 3);
        let b: Vec<f32> = (0..80_000usize).map(|n| s.get(n + 1000).map_or(0.0, |v| -0.5 * v)).collect();
        let r = find_offset(&s, &b, SR, &SyncOptions::default()).unwrap();
        assert_eq!(r.lag, 1000);
        assert!(r.inverted);
    }

    #[test]
    fn unrelated_or_silent_signals_are_not_trusted() {
        let a = speechy(f64::from(SR), 8.0, 11);
        let b = speechy(f64::from(SR), 8.0, 12);
        let r = find_offset(&a, &b, SR, &SyncOptions::default()).unwrap();
        assert!(!r.reliable(), "{r:?}");
        assert!(find_offset(&a, &[0.0; 16_000], SR, &SyncOptions::default()).is_none());
        assert!(find_offset(&a[..100], &a, SR, &SyncOptions::default()).is_none());
    }

    #[test]
    fn max_lag_limits_the_search() {
        let s = speechy(f64::from(SR), 8.0, 5);
        let b = s[16_000..].to_vec();
        let near = SyncOptions {
            max_lag: Some(20_000),
            ..Default::default()
        };
        assert_eq!(find_offset(&s, &b, SR, &near).unwrap().lag, 16_000);
        let far = SyncOptions {
            max_lag: Some(4_000),
            ..Default::default()
        };
        let r = find_offset(&s, &b, SR, &far).unwrap();
        assert!(!r.reliable() || r.lag != 16_000);
    }
}
