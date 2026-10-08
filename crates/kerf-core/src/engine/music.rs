//! Bar-level music analysis on decoded PCM: a fixed beat grid, the downbeat, chroma
//! per bar and the repeating phrases between bars. Pure DSP (no ML), so everything
//! but the cache is unit-tested on synthetic signals.
//!
//! The grid is *fitted*, not tracked: an autocorrelation gives a first period, then a
//! brute-force scan of period and phase maximizes the onset strength under the grid.
//! Produced music sits on a fixed grid, and per-beat tracking wobbles around it.

use std::path::{Path, PathBuf};

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use super::cli::{fnv1a, source_key};
use crate::model::{BeatGrid, MusicAnalysis, PhraseMatch};

/// Two bars whose chroma cosine similarity clears this are interchangeable.
pub const SPLICE_SIMILARITY: f32 = 0.95;

const BEATS_PER_BAR: u32 = 4;

const ONSET_WINDOW: usize = 1024;
const ONSET_HOP: usize = 128;
const CHROMA_WINDOW: usize = 4096;
const CHROMA_HOP: usize = 1024;

/// Kick drums live below this; the downbeat is the beat with the most onset energy here.
const KICK_MAX_HZ: f64 = 150.0;
const CHROMA_MIN_HZ: f64 = 55.0;
const CHROMA_MAX_HZ: f64 = 4200.0;

const MIN_BPM: f64 = 60.0;
const MAX_BPM: f64 = 180.0;

/// The grid scan: periods within this fraction of the autocorrelation estimate, at
/// `PERIOD_STEP_S`, and every phase at `PHASE_STEP_S`; then a finer pass around the best.
const PERIOD_SPAN: f64 = 0.01;
const PERIOD_STEP_S: f64 = 0.0002;
const PHASE_STEP_S: f64 = 0.003;

/// A grid whose beats carry less than this multiple of the average onset strength is
/// not a pulse.
const MIN_GRID_CONTRAST: f64 = 1.5;

/// The onset flux of a sharp attack peaks this long *before* the attack: the window
/// gains it fastest while it is still entering from the frame's edge. Measured on
/// clicks (7–10 ms for this window); added back to the fitted phase.
const ONSET_LEAD_S: f64 = 0.0085;

/// The first beat may sit this far before 0, so a beat on the file's first sample that
/// the fit places a few ms early is not pushed a whole period later.
const PHASE_EARLY_S: f64 = 0.02;

/// A shorter file has too few bars to splice.
const MIN_DURATION_S: f64 = 4.0;

/// Fit the bar structure of mono `samples` at `sample_rate`. `None` for audio too short,
/// silent, or without a steady pulse.
pub fn analyze_music(samples: &[f32], sample_rate: u32) -> Option<MusicAnalysis> {
    let sr = sample_rate as f64;
    let duration = samples.len() as f64 / sr;
    if sample_rate == 0 || duration < MIN_DURATION_S {
        return None;
    }
    let onsets = onset_strength(samples, sample_rate);
    let frame_rate = sr / ONSET_HOP as f64;
    let estimate = estimate_period(&onsets.full, frame_rate)?;
    let (period_s, fitted, contrast) = fit_grid(&onsets.full, frame_rate, estimate, duration);
    if contrast < MIN_GRID_CONTRAST {
        return None;
    }
    let phase_s = (fitted + ONSET_LEAD_S + PHASE_EARLY_S).rem_euclid(period_s) - PHASE_EARLY_S;
    let downbeat_offset = pick_downbeat(
        &onsets.low,
        &onsets.full,
        frame_rate,
        period_s,
        phase_s - ONSET_LEAD_S,
        duration,
    );
    let grid = BeatGrid {
        period_s,
        phase_s,
        downbeat_offset,
        beats_per_bar: BEATS_PER_BAR,
    };
    let bar_chroma = bar_chroma(samples, sample_rate, &grid, duration);
    let phrases = phrase_matches(&bar_chroma, SPLICE_SIMILARITY);
    Some(MusicAnalysis {
        grid,
        duration,
        bar_chroma,
        phrases,
    })
}

// ---- STFT ------------------------------------------------------------------

/// A Hann-windowed STFT over centred frames: frame `i` is centred on sample `i * hop`
/// (zero-padded at the ends), so its time is `i * hop / sample_rate`. Calls `f` with
/// each frame's index and its magnitude spectrum (`window / 2 + 1` bins, scaled so a
/// full-scale sine peaks near 0.5), reusing one set of buffers throughout.
fn stft(samples: &[f32], window: usize, hop: usize, mut f: impl FnMut(usize, &[f32])) {
    let fft: std::sync::Arc<dyn RealToComplex<f32>> = RealFftPlanner::<f32>::new().plan_fft_forward(window);
    let hann: Vec<f32> = (0..window)
        .map(|n| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / window as f32).cos())
        .collect();
    let norm = 1.0 / hann.iter().sum::<f32>();
    let mut input = fft.make_input_vec();
    let mut spectrum: Vec<Complex<f32>> = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let mut mags = vec![0.0_f32; spectrum.len()];
    let frames = samples.len() / hop + 1;
    let half = window as isize / 2;
    for i in 0..frames {
        let start = (i * hop) as isize - half;
        for (n, x) in input.iter_mut().enumerate() {
            let s = start + n as isize;
            *x = if s >= 0 && (s as usize) < samples.len() {
                samples[s as usize] * hann[n]
            } else {
                0.0
            };
        }
        if fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch).is_err() {
            return;
        }
        for (m, c) in mags.iter_mut().zip(&spectrum) {
            *m = c.norm() * norm;
        }
        f(i, &mags);
    }
}

// ---- onset strength ----------------------------------------------------------

struct Onsets {
    /// Log-magnitude spectral flux over every bin, one value per onset frame.
    full: Vec<f32>,
    /// The same over the kick band only.
    low: Vec<f32>,
}

/// Half-wave-rectified log-magnitude spectral flux, summed over frequency bins (and
/// separately over the kick band).
fn onset_strength(samples: &[f32], sample_rate: u32) -> Onsets {
    let bins = ONSET_WINDOW / 2 + 1;
    let kick_bins = ((KICK_MAX_HZ * ONSET_WINDOW as f64 / sample_rate as f64).ceil() as usize).clamp(2, bins);
    let mut prev = vec![0.0_f32; bins];
    let mut log = vec![0.0_f32; bins];
    let mut full = Vec::new();
    let mut low = Vec::new();
    stft(samples, ONSET_WINDOW, ONSET_HOP, |i, mags| {
        for (l, m) in log.iter_mut().zip(mags) {
            *l = (1.0 + 100.0 * m).ln();
        }
        let (mut f, mut lo) = (0.0_f32, 0.0_f32);
        if i > 0 {
            for (k, (l, p)) in log.iter().zip(&prev).enumerate() {
                let d = (l - p).max(0.0);
                f += d;
                if (1..kick_bins).contains(&k) {
                    lo += d;
                }
            }
        }
        full.push(f);
        low.push(lo);
        prev.copy_from_slice(&log);
    });
    Onsets { full, low }
}

/// `env` sampled at fractional frame `x` by linear interpolation; 0 outside.
fn sample_at(env: &[f32], x: f64) -> f64 {
    if x < 0.0 {
        return 0.0;
    }
    let i = x.floor() as usize;
    if i + 1 >= env.len() {
        return if i < env.len() { env[i] as f64 } else { 0.0 };
    }
    let t = x - i as f64;
    env[i] as f64 * (1.0 - t) + env[i + 1] as f64 * t
}

// ---- tempo -------------------------------------------------------------------

/// A first beat period (seconds) from the autocorrelation of the onset envelope, in
/// 60–180 BPM, weighted towards 120 BPM to settle tempo octaves, and refined between
/// lags by a parabola through the peak.
fn estimate_period(env: &[f32], frame_rate: f64) -> Option<f64> {
    let min_lag = ((frame_rate * 60.0 / MAX_BPM).floor() as usize).max(1);
    let max_lag = (frame_rate * 60.0 / MIN_BPM).ceil() as usize;
    if env.len() <= max_lag * 2 {
        return None;
    }
    let mean = env.iter().map(|x| *x as f64).sum::<f64>() / env.len() as f64;
    let centred: Vec<f64> = env.iter().map(|x| *x as f64 - mean).collect();
    let acf = |lag: usize| -> f64 { (0..centred.len() - lag).map(|i| centred[i] * centred[i + lag]).sum::<f64>() };
    let weight = |lag: f64| -> f64 {
        let bpm = 60.0 * frame_rate / lag;
        (-0.5 * (bpm / 120.0).log2().powi(2)).exp()
    };
    let raw: Vec<f64> = (min_lag - 1..=max_lag + 1).map(acf).collect();
    let mut best: Option<(usize, f64)> = None;
    for lag in min_lag..=max_lag {
        let v = raw[lag - (min_lag - 1)] * weight(lag as f64);
        if v > 0.0 && best.is_none_or(|(_, b)| v > b) {
            best = Some((lag, v));
        }
    }
    let (lag, _) = best?;
    let (y0, y1, y2) = (raw[lag - min_lag], raw[lag - (min_lag - 1)], raw[lag + 1 - (min_lag - 1)]);
    let denom = y0 - 2.0 * y1 + y2;
    let shift = if denom.abs() > 1e-12 {
        (0.5 * (y0 - y2) / denom).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    Some((lag as f64 + shift) / frame_rate)
}

/// Mean onset strength under the grid `phase + k * period` across `[0, duration)`.
fn grid_score(env: &[f32], frame_rate: f64, period: f64, phase: f64, duration: f64) -> f64 {
    let (mut sum, mut n) = (0.0, 0usize);
    let mut t = phase;
    while t < duration {
        sum += sample_at(env, t * frame_rate);
        n += 1;
        t += period;
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f64
    }
}

/// The fixed grid that best explains the onsets: a brute-force scan of period around
/// `estimate` and of phase across one period, then a ten-times finer pass around the
/// winner. Returns `(period, phase in [0, period), contrast)`, where contrast is the
/// grid's score over the envelope's mean.
fn fit_grid(env: &[f32], frame_rate: f64, estimate: f64, duration: f64) -> (f64, f64, f64) {
    let scan = |periods: (f64, f64, f64), phases: &dyn Fn(f64) -> (f64, f64, f64)| -> (f64, f64, f64) {
        let (p_lo, p_hi, p_step) = periods;
        let mut best = (estimate, 0.0, f64::MIN);
        let mut p = p_lo;
        while p <= p_hi + 1e-12 {
            let (ph_lo, ph_hi, ph_step) = phases(p);
            let mut ph = ph_lo;
            while ph < ph_hi {
                let s = grid_score(env, frame_rate, p, ph.rem_euclid(p), duration);
                if s > best.2 {
                    best = (p, ph.rem_euclid(p), s);
                }
                ph += ph_step;
            }
            p += p_step;
        }
        best
    };
    let coarse = scan(
        (estimate * (1.0 - PERIOD_SPAN), estimate * (1.0 + PERIOD_SPAN), PERIOD_STEP_S),
        &|p| (0.0, p, PHASE_STEP_S),
    );
    let fine = scan(
        (coarse.0 - PERIOD_STEP_S, coarse.0 + PERIOD_STEP_S, PERIOD_STEP_S / 10.0),
        &|_| (coarse.1 - PHASE_STEP_S, coarse.1 + PHASE_STEP_S, PHASE_STEP_S / 10.0),
    );
    let mean = env.iter().map(|x| *x as f64).sum::<f64>() / env.len().max(1) as f64;
    let contrast = if mean > 0.0 { fine.2 / mean } else { 0.0 };
    (fine.0, fine.1, contrast)
}

/// Which beat (mod 4) is the downbeat: the one with the most kick-band onset energy,
/// or the most onset energy overall when the music has no low end.
fn pick_downbeat(low: &[f32], full: &[f32], frame_rate: f64, period: f64, phase: f64, duration: f64) -> u32 {
    let per_slot = |env: &[f32]| -> [f64; BEATS_PER_BAR as usize] {
        let mut sums = [0.0; BEATS_PER_BAR as usize];
        let mut k = 0usize;
        loop {
            let t = phase + k as f64 * period;
            if t >= duration {
                break;
            }
            sums[k % BEATS_PER_BAR as usize] += sample_at(env, t * frame_rate);
            k += 1;
        }
        sums
    };
    let mut sums = per_slot(low);
    if sums.iter().all(|s| *s <= 1e-6) {
        sums = per_slot(full);
    }
    let mut best = 0;
    for (i, s) in sums.iter().enumerate() {
        if *s > sums[best] + 1e-9 {
            best = i;
        }
    }
    best as u32
}

// ---- chroma ------------------------------------------------------------------

/// Pitch class (C = 0 … B = 11) of frequency `hz`, A4 = 440 Hz.
fn pitch_class(hz: f64) -> usize {
    let semis = (12.0 * (hz / 440.0).log2()).round() as i64;
    (semis + 9).rem_euclid(12) as usize
}

/// L2-normalized chroma of every whole bar of `grid` in `samples`: each STFT frame's
/// magnitude folded onto the 12 pitch classes, summed over the frames centred in the bar.
fn bar_chroma(samples: &[f32], sample_rate: u32, grid: &BeatGrid, duration: f64) -> Vec<[f32; 12]> {
    let bars = grid.whole_bars(duration);
    let mut out = vec![[0.0_f32; 12]; bars];
    if bars == 0 {
        return out;
    }
    let bin_hz = sample_rate as f64 / CHROMA_WINDOW as f64;
    let classes: Vec<Option<usize>> = (0..CHROMA_WINDOW / 2 + 1)
        .map(|k| {
            let hz = k as f64 * bin_hz;
            (CHROMA_MIN_HZ..=CHROMA_MAX_HZ).contains(&hz).then(|| pitch_class(hz))
        })
        .collect();
    let first = grid.first_downbeat();
    let bar_s = grid.bar_s();
    stft(samples, CHROMA_WINDOW, CHROMA_HOP, |i, mags| {
        let t = (i * CHROMA_HOP) as f64 / sample_rate as f64;
        if t < first {
            return;
        }
        let b = ((t - first) / bar_s).floor() as usize;
        if b >= bars {
            return;
        }
        for (m, pc) in mags.iter().zip(&classes) {
            if let Some(pc) = pc {
                out[b][*pc] += m;
            }
        }
    });
    for c in &mut out {
        let norm = c.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 1e-6 {
            for x in c.iter_mut() {
                *x /= norm;
            }
        } else {
            *c = [0.0; 12];
        }
    }
    out
}

/// Cosine similarity of two L2-normalized chroma vectors (0 when either is silent).
pub fn chroma_similarity(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Every pair of phrases — 8 bars, then 4 — that start on a 4-bar boundary and whose
/// bars all match above `threshold`. A 4-bar match inside an 8-bar one is still listed:
/// the planner prefers the longer one but may need the shorter.
pub fn phrase_matches(chroma: &[[f32; 12]], threshold: f32) -> Vec<PhraseMatch> {
    let mut out = Vec::new();
    for bars in [8usize, 4] {
        if chroma.len() < bars {
            continue;
        }
        let starts: Vec<usize> = (0..=chroma.len() - bars).step_by(4).collect();
        for (i, &a) in starts.iter().enumerate() {
            for &b in &starts[i + 1..] {
                if (0..bars).all(|k| chroma_similarity(&chroma[a + k], &chroma[b + k]) > threshold) {
                    out.push(PhraseMatch { a, b, bars });
                }
            }
        }
    }
    out
}

// ---- cache -------------------------------------------------------------------

const CACHE_VERSION: u32 = 1;

/// Where `src`'s music analysis is cached: `<cache>/kerf/music/<hash>.json`, keyed by
/// the file's identity (path, size, mtime) like the waveform cache.
fn cache_path(src: &Path) -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("kerf").join("music");
    let key = format!("{}|music-v{CACHE_VERSION}", source_key(src));
    Some(dir.join(format!("{:016x}.json", fnv1a(&key))))
}

/// The cached analysis of `src`, if there is a readable one. The outer `Option` is
/// "cached at all"; the inner one is the analysis (a file without a pulse caches `None`).
pub(super) fn cached(src: &Path) -> Option<Option<MusicAnalysis>> {
    let entry: CacheEntry = serde_json::from_slice(&std::fs::read(cache_path(src)?).ok()?).ok()?;
    Some(entry.music)
}

/// Wrapped so that a cached "no pulse" (`null`) is not read back as "not cached".
#[derive(serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    music: Option<MusicAnalysis>,
}

/// Remember `analysis` for `src`, through a temp file and a rename. Failing is not fatal.
pub(super) fn store(src: &Path, analysis: &Option<MusicAnalysis>) {
    let Some(file) = cache_path(src) else { return };
    let Ok(json) = serde_json::to_vec(&CacheEntry { music: analysis.clone() }) else {
        return;
    };
    let tmp = file.with_extension(format!("{}.part", std::process::id()));
    let written = file
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&tmp, json))
        .and_then(|()| std::fs::rename(&tmp, &file));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!("could not cache the music analysis of {}: {e}", src.display());
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const SR: u32 = 22_050;

    /// A short decaying low thump (a kick) and a high click, mixed at `t`.
    fn add_hit(out: &mut [f32], sr: u32, t: f64, kick: bool) {
        let start = (t * sr as f64).round() as usize;
        for n in 0..(sr as usize / 20) {
            let Some(s) = out.get_mut(start + n) else { break };
            let tt = n as f32 / sr as f32;
            let env = (-tt * 60.0).exp();
            *s += 0.3 * env * (2.0 * std::f32::consts::PI * 3000.0 * tt).sin();
            if kick {
                *s += 0.6 * (-tt * 25.0).exp() * (2.0 * std::f32::consts::PI * 60.0 * tt).sin();
            }
        }
    }

    /// A click on every beat of a `bpm` grid starting at `phase`, with a kick on every
    /// beat `k` where `k % 4 == downbeat`.
    pub(crate) fn click_track(bpm: f64, phase: f64, downbeat: usize, seconds: f64) -> Vec<f32> {
        click_track_at(SR, bpm, phase, downbeat, seconds)
    }

    fn click_track_at(sr: u32, bpm: f64, phase: f64, downbeat: usize, seconds: f64) -> Vec<f32> {
        let mut out = vec![0.0_f32; (seconds * sr as f64) as usize];
        let period = 60.0 / bpm;
        let mut k = 0;
        loop {
            let t = phase + k as f64 * period;
            if t >= seconds {
                break;
            }
            add_hit(&mut out, sr, t, k % 4 == downbeat);
            k += 1;
        }
        out
    }

    fn chord(freqs: &[f64], seconds: f64) -> Vec<f32> {
        chord_at(SR, freqs, seconds)
    }

    fn chord_at(sr: u32, freqs: &[f64], seconds: f64) -> Vec<f32> {
        (0..(seconds * sr as f64).round() as usize)
            .map(|n| {
                let t = n as f64 / sr as f64;
                freqs.iter().map(|f| (2.0 * std::f64::consts::PI * f * t).sin()).sum::<f64>() as f32 * 0.2
            })
            .collect()
    }

    const C_MAJOR: [f64; 3] = [261.63, 329.63, 392.00];
    const A_MINOR: [f64; 3] = [220.00, 261.63, 329.63];
    const F_MAJOR: [f64; 3] = [174.61, 220.00, 261.63];
    const G_MAJOR: [f64; 3] = [196.00, 246.94, 293.66];

    /// An 8-bar loop at `bpm` (C Am F G | C Am F G with a different last two bars:
    /// Dm Em), clicked on every beat and kicked on the downbeat, repeated `times`.
    pub(crate) fn chord_loop(bpm: f64, times: usize) -> Vec<f32> {
        chord_loop_at(SR, bpm, times)
    }

    fn chord_loop_at(sr: u32, bpm: f64, times: usize) -> Vec<f32> {
        const D_MINOR: [f64; 3] = [146.83, 174.61, 220.00];
        const E_MINOR: [f64; 3] = [164.81, 196.00, 246.94];
        let bars: [&[f64]; 8] = [&C_MAJOR, &A_MINOR, &F_MAJOR, &G_MAJOR, &C_MAJOR, &A_MINOR, &D_MINOR, &E_MINOR];
        let bar_s = 4.0 * 60.0 / bpm;
        let mut out = Vec::new();
        for _ in 0..times {
            for b in bars {
                out.extend(chord_at(sr, b, bar_s));
            }
        }
        let seconds = out.len() as f64 / sr as f64;
        for (i, s) in click_track_at(sr, bpm, 0.0, 0, seconds).into_iter().enumerate() {
            out[i] += s;
        }
        out
    }

    #[test]
    fn a_click_track_recovers_period_phase_and_downbeat() {
        for (bpm, phase, downbeat) in [(90.0, 0.123, 1), (120.0, 0.4, 3), (128.0, 0.0, 0), (75.0, 0.61, 2)] {
            let samples = click_track(bpm, phase, downbeat, 45.0);
            let m = analyze_music(&samples, SR).expect("a grid");
            let period = 60.0 / bpm;
            assert!(
                (m.grid.period_s - period).abs() < 0.0005,
                "{bpm} BPM: period {} vs {period}",
                m.grid.period_s
            );
            assert!(
                (m.grid.phase_s - phase).abs() < 0.005,
                "{bpm} BPM: phase {} vs {phase}",
                m.grid.phase_s
            );
            assert_eq!(m.grid.downbeat_offset as usize, downbeat, "{bpm} BPM");
        }
    }

    #[test]
    fn silence_and_noise_have_no_grid() {
        assert!(analyze_music(&vec![0.0; SR as usize * 20], SR).is_none());
        assert!(analyze_music(&vec![0.0; SR as usize], SR).is_none(), "too short");
        let mut x: u32 = 1;
        let noise: Vec<f32> = (0..SR as usize * 20)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 - 0.5) * 0.5
            })
            .collect();
        assert!(analyze_music(&noise, SR).is_none());
    }

    #[test]
    fn sine_chords_peak_on_their_pitch_classes() {
        let grid = BeatGrid {
            period_s: 0.5,
            phase_s: 0.0,
            downbeat_offset: 0,
            beats_per_bar: 4,
        };
        let mut samples = chord(&C_MAJOR, 2.0);
        samples.extend(chord(&A_MINOR, 2.0));
        let c = bar_chroma(&samples, SR, &grid, 4.0);
        assert_eq!(c.len(), 2);
        let top3 = |v: &[f32; 12]| {
            let mut idx: Vec<usize> = (0..12).collect();
            idx.sort_by(|a, b| v[*b].total_cmp(&v[*a]));
            let mut t = idx[..3].to_vec();
            t.sort();
            t
        };
        assert_eq!(top3(&c[0]), vec![0, 4, 7], "C E G: {:?}", c[0]);
        assert_eq!(top3(&c[1]), vec![0, 4, 9], "A C E: {:?}", c[1]);
        let norm: f32 = c[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
        assert!(chroma_similarity(&c[0], &c[0]) > 0.999);
        assert!(chroma_similarity(&c[0], &c[1]) < SPLICE_SIMILARITY);
    }

    #[test]
    fn pitch_classes_follow_a440() {
        assert_eq!(pitch_class(440.0), 9);
        assert_eq!(pitch_class(261.63), 0);
        assert_eq!(pitch_class(523.25), 0);
        assert_eq!(pitch_class(110.0), 9);
        assert_eq!(pitch_class(493.88), 11);
    }

    #[test]
    fn a_looped_progression_finds_its_repeats() {
        let samples = chord_loop(90.0, 2);
        let m = analyze_music(&samples, SR).expect("a grid");
        assert!((m.grid.bpm() - 90.0).abs() < 0.1, "{}", m.grid.bpm());
        assert_eq!(m.grid.downbeat_offset, 0);
        assert!(m.grid.first_downbeat() < 0.01, "{}", m.grid.first_downbeat());
        assert_eq!(m.bar_chroma.len(), 16);
        assert!(m.phrases.contains(&PhraseMatch { a: 0, b: 8, bars: 8 }), "{:?}", m.phrases);
        // Bars 0-3 and 4-7 differ only in their last two bars.
        assert!(!m.phrases.iter().any(|p| p.a == 0 && p.b == 4), "{:?}", m.phrases);
        assert!(m.phrases.contains(&PhraseMatch { a: 4, b: 12, bars: 4 }));
    }

    #[test]
    fn phrase_matches_need_every_bar_to_match() {
        let mut a = [0.0_f32; 12];
        a[0] = 1.0;
        let mut b = [0.0_f32; 12];
        b[7] = 1.0;
        let chroma = vec![a, a, a, a, a, a, a, b, a, a, a, a];
        let m = phrase_matches(&chroma, SPLICE_SIMILARITY);
        assert!(m.contains(&PhraseMatch { a: 0, b: 8, bars: 4 }));
        assert!(!m.iter().any(|p| p.b == 4), "bar 7 breaks 4..8: {m:?}");
        assert!(!m.iter().any(|p| p.bars == 8), "{m:?}");
        assert!(phrase_matches(&[a, a], SPLICE_SIMILARITY).is_empty());
    }

    #[test]
    fn beat_grid_lists_beats_bars_and_whole_bars() {
        let g = BeatGrid {
            period_s: 0.5,
            phase_s: 0.25,
            downbeat_offset: 1,
            beats_per_bar: 4,
        };
        assert_eq!(g.first_downbeat(), 0.75);
        assert_eq!(g.bar_s(), 2.0);
        assert_eq!(g.beats(1.5), vec![0.25, 0.75, 1.25]);
        assert_eq!(g.downbeats(5.0), vec![0.75, 2.75, 4.75]);
        assert_eq!(g.whole_bars(4.75), 2);
        assert_eq!(g.whole_bars(4.74), 1);
        assert_eq!(g.whole_bars(0.5), 0);
        assert_eq!(g.bpm(), 120.0);
    }

    // ---- rendered through the export graph --------------------------------------

    use crate::engine::test_support::{audio_stream, audio_track, test_asset, timeline_of};
    use crate::engine::{decode_audio, render_with, write_wav, AudioBuffer, Container, ExportOptions};
    use crate::model::{music_fit_clips, plan_music_fit, Asset, Clip, Timeline};

    const RENDER_SR: u32 = 48_000;

    /// A 16-bar song at 90 BPM (the 8-bar loop twice) with half a second of silence ahead
    /// and a second of the last chord ringing out after, on the 16-bit grid so a WAV
    /// export can reproduce it exactly.
    fn fixture(dir: &Path) -> (Asset, AudioBuffer) {
        let mut samples = vec![0.0_f32; RENDER_SR as usize / 2];
        samples.extend(chord_loop_at(RENDER_SR, 90.0, 2));
        let ring = chord_at(RENDER_SR, &C_MAJOR, 1.0);
        let n = ring.len() as f32;
        samples.extend(ring.iter().enumerate().map(|(i, s)| s * (1.0 - i as f32 / n)));
        for s in &mut samples {
            *s = ((*s * 0.8).clamp(-1.0, 1.0) * 32768.0).round().min(32767.0) / 32768.0;
        }
        let buf = AudioBuffer::new(RENDER_SR, 1, samples);
        let path = dir.join("song.wav");
        write_wav(&path, &buf).unwrap();
        let mut asset = test_asset(vec![audio_stream(RENDER_SR, 1)]);
        asset.path = path.to_string_lossy().into_owned();
        asset.duration = buf.duration();
        (asset, buf)
    }

    fn render(dir: &Path, name: &str, timeline: &Timeline, asset: &Asset) -> AudioBuffer {
        let out = dir.join(name);
        let opts = ExportOptions {
            container: Container::Wav,
            video_codec: None,
            audio_codec: Some("pcm_s16le".into()),
            audio_sample_rate: Some(RENDER_SR),
            audio_channels: Some(1),
            ..Default::default()
        };
        render_with(timeline, std::slice::from_ref(asset), &out, &opts).unwrap();
        decode_audio(&out, RENDER_SR, 1).unwrap()
    }

    fn samples(t: f64) -> usize {
        (t * RENDER_SR as f64).round() as usize
    }

    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn a_fitted_loop_renders_to_length_without_new_peaks_and_with_untouched_runs() {
        let dir = std::env::temp_dir().join(format!("kerf-music-fit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (asset, src) = fixture(&dir);
        let analysed = decode_audio(Path::new(&asset.path), SR, 1).unwrap();
        let m = analyze_music(&analysed.samples, SR).expect("a grid");
        assert!((m.grid.bpm() - 90.0).abs() < 0.05, "{}", m.grid.bpm());
        assert!(!m.phrases.is_empty());
        let whole = Clip::new(asset.id, 0.0, asset.duration, 0.0);
        for target in [60.0, 90.0, 150.0] {
            let fit = plan_music_fit(&m, target, RENDER_SR);
            assert!(fit.splices >= 1, "{target}: {fit:?}");
            let clips = music_fit_clips(&whole, &fit, RENDER_SR, true);
            let timeline = timeline_of(vec![audio_track(clips.clone())]);
            let out = render(&dir, &format!("fit-{target}.wav"), &timeline, &asset);
            let want = if fit.remainder < 0.0 { target } else { fit.duration };

            assert!(
                (out.duration() - want).abs() < 0.002,
                "{target}: {} s vs {want} s",
                out.duration()
            );
            assert!(
                out.peak() <= src.peak() + 1e-6,
                "{target}: peak {} over {}",
                out.peak(),
                src.peak()
            );
            // Away from the crossfades and the final fade, every segment is its source,
            // sample for sample.
            let guard = samples(0.005) + 2;
            let fade_from = if fit.remainder < 0.0 {
                samples(target - crate::model::FIT_FADE_S)
            } else {
                usize::MAX
            };
            for seg in &fit.segments {
                let (o, s, n) = (samples(seg.output_start), samples(seg.source_start), samples(seg.len()));
                let worst = (guard..n.saturating_sub(guard))
                    .take_while(|k| o + k < fade_from.min(out.samples.len()))
                    .map(|k| (out.samples[o + k] - src.samples[s + k]).abs())
                    .fold(0.0_f32, f32::max);
                assert_eq!(worst, 0.0, "{target}: segment {seg:?} differs from its source");
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn a_splice_between_continuous_bars_is_sample_identical() {
        let dir = std::env::temp_dir().join(format!("kerf-music-cont-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (asset, src) = fixture(&dir);
        // Bar 8 on a grid that is not a whole millisecond (0.5 s + 8 bars of 8/3 s).
        let cut = crate::model::to_samples(0.5 + 8.0 * 4.0 * 60.0 / 90.0, RENDER_SR);
        assert!((cut * 1000.0).fract().abs() > 1e-6);
        let a = Clip::new(asset.id, 0.0, cut, 0.0);
        let b = Clip::new(asset.id, cut, asset.duration, cut);
        let hard = render(
            &dir,
            "hard.wav",
            &timeline_of(vec![audio_track(vec![a, b])]),
            &asset,
        );
        // The same join crossfaded the way a fit splices: the window centred on the cut.
        let fit = crate::model::MusicFit {
            segments: vec![
                crate::model::MusicSegment {
                    source_start: 0.0,
                    source_end: cut,
                    output_start: 0.0,
                },
                crate::model::MusicSegment {
                    source_start: cut,
                    source_end: asset.duration,
                    output_start: cut,
                },
            ],
            target: asset.duration,
            duration: asset.duration,
            remainder: 0.0,
            bars: 16,
            splices: 1,
        };
        let faded = music_fit_clips(&Clip::new(asset.id, 0.0, asset.duration, 0.0), &fit, RENDER_SR, false);
        let soft = render(&dir, "soft.wav", &timeline_of(vec![audio_track(faded)]), &asset);
        std::fs::remove_dir_all(&dir).unwrap();
        for (name, out) in [("hard cut", hard), ("crossfade", soft)] {
            assert_eq!(out.samples.len(), src.samples.len(), "{name}");
            let worst = out
                .samples
                .iter()
                .zip(&src.samples)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0_f32, f32::max);
            assert_eq!(worst, 0.0, "{name}: max abs diff");
        }
    }
}
