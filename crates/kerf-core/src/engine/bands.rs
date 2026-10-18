//! Band balance: where a stretch of sound keeps its energy.
//!
//! Six bands — sub 20–60 Hz, bass 60–250, low-mid 250–500, mid 500–2 k, high-mid 2–6 k, air
//! 6–20 k — measured per **window** (a section of a song, a phrase, an eighth of a clip): the
//! window's RMS level, each band's energy in dB and its share of the window's in-band energy,
//! and an ungated K-weighted loudness (ITU-R BS.1770, so it reads like the LUFS of the mix
//! meter) of the window. It answers "is the bass drowning the voice, is there any top end, does
//! the drop sit heavier than the verse" without listening.
//!
//! The analysis is a Hann-windowed STFT (8192 samples, hop 4096; 5.4 Hz bins at 44.1 kHz so
//! the sub band is a handful of bins) on the mono mix, scaled by Parseval so a band's energy is
//! the mean square it contributes: a full-scale sine reads −3.0 dBFS in its band. A frame
//! belongs to the window its centre falls in. [`BandMeter`] takes the sound as it streams off a
//! decoder (memory is a frame, never the file); [`band_balance`] is the same over a slice.
//! Pure, apart from the decode helpers at the end.

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};

use super::cli::{bg_command, ffmpeg_bin, launch_err};
use super::cpu;
use super::peaks::pump_pcm_cancellable;
use crate::error::{Error, Result};

/// The bands: name and edges in Hz.
pub const BANDS: [(&str, f64, f64); 6] = [
    ("sub", 20.0, 60.0),
    ("bass", 60.0, 250.0),
    ("low_mid", 250.0, 500.0),
    ("mid", 500.0, 2_000.0),
    ("high_mid", 2_000.0, 6_000.0),
    ("air", 6_000.0, 20_000.0),
];

/// The rate the decoders resample to: the air band reaches 20 kHz.
pub const SAMPLE_RATE: u32 = 44_100;

const FRAME: usize = 8192;
const HOP: usize = 4096;
/// A band quieter than this (mean square) is silence: −120 dBFS.
const SILENCE_MS: f64 = 1e-12;
/// A window shorter than this has no frame to measure.
pub const MIN_WINDOW_S: f64 = 0.25;

/// A stretch of the measured sound, seconds from its start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BandWindow {
    pub start: f64,
    pub end: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One band in one window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BandLevel {
    pub name: &'static str,
    pub low_hz: f64,
    pub high_hz: f64,
    /// The band's energy as a level (its mean square, in dBFS); `None` when there is none.
    pub energy_dbfs: Option<f64>,
    /// The band's share of the window's energy across the six bands, percent.
    pub share_pct: f64,
}

/// What one window sounds like.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowBalance {
    pub start: f64,
    pub end: f64,
    pub label: Option<String>,
    /// The whole window's RMS, dBFS (`None` for silence).
    pub rms_dbfs: Option<f64>,
    /// Its K-weighted loudness, LUFS, ungated (`None` for silence).
    pub loudness_lufs: Option<f64>,
    /// The six bands, low to high. Empty when the window held no frame to measure.
    pub bands: Vec<BandLevel>,
    /// The band with the most energy.
    pub dominant: Option<&'static str>,
}

#[derive(Default, Clone)]
struct Acc {
    samples: u64,
    square: f64,
    /// The K-weighted square per channel, summed.
    k_square: f64,
    frames: u64,
    band_ms: [f64; 6],
}

/// A second-order section, direct form I.
#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self {
            b,
            a,
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1] - self.a[0] * self.y[0] - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// BS.1770's two filters — the head-related high shelf and the 38 Hz high-pass — for `rate`.
fn k_weighting(rate: f64) -> [Biquad; 2] {
    // The shelf: +4 dB above ~1.7 kHz.
    let (g, f0, q) = (
        3.999_843_853_973_347_f64,
        1_681.974_450_955_533_f64,
        0.707_175_236_955_419_6_f64,
    );
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad::new(
        [
            (vh + vb * k / q + k * k) / a0,
            2.0 * (k * k - vh) / a0,
            (vh - vb * k / q + k * k) / a0,
        ],
        [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
    );
    // The high-pass: the revised low-frequency B-curve.
    let (f0, q) = (38.135_470_876_024_44_f64, 0.500_327_037_323_877_3_f64);
    let k = (std::f64::consts::PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    let high_pass = Biquad::new([1.0, -2.0, 1.0], [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0]);
    [shelf, high_pass]
}

/// Measures windows of a stream of interleaved samples as it arrives.
pub struct BandMeter {
    rate: f64,
    channels: usize,
    /// `(start sample, end sample)` per window, as given.
    ranges: Vec<(u64, u64)>,
    meta: Vec<(f64, f64, Option<String>)>,
    acc: Vec<Acc>,
    fft: Arc<dyn RealToComplex<f32>>,
    hann: Vec<f32>,
    hann_power: f64,
    ring: Vec<f32>,
    /// Samples (frames of all channels) received.
    pos: u64,
    filters: Vec<[Biquad; 2]>,
    band_bins: [(usize, usize); 6],
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    frames_done: u64,
}

impl BandMeter {
    /// A meter for `windows` (seconds into the stream) over a stream of `channels` interleaved
    /// channels at `rate` Hz.
    pub fn new(rate: u32, channels: usize, windows: &[BandWindow]) -> Result<Self> {
        if rate < 8_000 || channels == 0 || channels > 2 {
            return Err(Error::InvalidArgument(
                "band balance needs a sample rate of 8 kHz or more and one or two channels".to_string(),
            ));
        }
        let r = rate as f64;
        let mut ranges = Vec::with_capacity(windows.len());
        for w in windows {
            if !w.start.is_finite() || !w.end.is_finite() || w.start < 0.0 || w.end - w.start < MIN_WINDOW_S {
                return Err(Error::InvalidArgument(format!(
                    "a band window needs 0 <= start and at least {MIN_WINDOW_S} s (got {}..{})",
                    w.start, w.end
                )));
            }
            ranges.push(((w.start * r).round() as u64, (w.end * r).round() as u64));
        }
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FRAME);
        let hann: Vec<f32> = (0..FRAME)
            .map(|n| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / FRAME as f32).cos())
            .collect();
        let hann_power: f64 = hann.iter().map(|w| (*w as f64) * (*w as f64)).sum();
        let mut band_bins = [(0, 0); 6];
        for (i, (_, lo, hi)) in BANDS.iter().enumerate() {
            let first = (lo * FRAME as f64 / r).ceil() as usize;
            let last = ((hi * FRAME as f64 / r).ceil() as usize).min(FRAME / 2 + 1);
            band_bins[i] = (first.max(1), last);
        }
        Ok(Self {
            rate: r,
            channels,
            ranges,
            meta: windows.iter().map(|w| (w.start, w.end, w.label.clone())).collect(),
            acc: vec![Acc::default(); windows.len()],
            input: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
            hann,
            hann_power,
            ring: vec![0.0; FRAME],
            pos: 0,
            filters: vec![k_weighting(r); channels],
            band_bins,
            frames_done: 0,
        })
    }

    /// Feed interleaved samples (whole frames; a trailing partial frame is ignored).
    pub fn push(&mut self, samples: &[f32]) {
        for frame in samples.chunks_exact(self.channels) {
            let mono = frame.iter().sum::<f32>() / self.channels as f32;
            let at = self.pos;
            for (a, (lo, hi)) in self.acc.iter_mut().zip(&self.ranges) {
                if at >= *lo && at < *hi {
                    a.samples += 1;
                    a.square += (mono as f64) * (mono as f64);
                }
            }
            let mut k_square = 0.0;
            for (c, s) in frame.iter().enumerate() {
                let [shelf, high_pass] = &mut self.filters[c];
                let y = high_pass.run(shelf.run(*s as f64));
                k_square += y * y;
            }
            for (a, (lo, hi)) in self.acc.iter_mut().zip(&self.ranges) {
                if at >= *lo && at < *hi {
                    a.k_square += k_square;
                }
            }
            self.ring[(at % FRAME as u64) as usize] = mono;
            self.pos += 1;
            if self.pos >= FRAME as u64 && (self.pos - FRAME as u64).is_multiple_of(HOP as u64) {
                self.frame(false);
            }
        }
    }

    /// Transform the last `FRAME` samples and give their band energies to the windows the frame's
    /// centre is in. `partial` pads what has arrived with zeros (a stream shorter than a frame).
    fn frame(&mut self, partial: bool) {
        let n = FRAME as u64;
        let (first, centre) = if partial {
            (0, self.pos / 2)
        } else {
            (self.pos - n, self.pos - n / 2)
        };
        if !self.ranges.iter().any(|(lo, hi)| centre >= *lo && centre < *hi) {
            return;
        }
        for i in 0..FRAME {
            let at = first + i as u64;
            let s = if at < self.pos { self.ring[(at % n) as usize] } else { 0.0 };
            self.input[i] = s * self.hann[i];
        }
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .is_err()
        {
            return;
        }
        let norm = 1.0 / (FRAME as f64 * self.hann_power);
        let mut ms = [0.0_f64; 6];
        for (band, (lo, hi)) in self.band_bins.iter().enumerate() {
            for k in *lo..*hi {
                let two = if k == FRAME / 2 { 1.0 } else { 2.0 };
                ms[band] += two * self.spectrum[k].norm_sqr() as f64 * norm;
            }
        }
        self.frames_done += 1;
        for (a, (lo, hi)) in self.acc.iter_mut().zip(&self.ranges) {
            if centre >= *lo && centre < *hi {
                a.frames += 1;
                for (sum, m) in a.band_ms.iter_mut().zip(&ms) {
                    *sum += m;
                }
            }
        }
    }

    /// The windows' balances.
    pub fn finish(mut self) -> Vec<WindowBalance> {
        if self.frames_done == 0 && self.pos > 0 {
            self.frame(true);
        }
        let _ = self.rate;
        self.acc
            .iter()
            .zip(&self.meta)
            .map(|(a, (start, end, label))| {
                let db = |ms: f64| (ms > SILENCE_MS).then(|| 10.0 * ms.log10());
                let rms = (a.samples > 0).then(|| a.square / a.samples as f64).and_then(db);
                let lufs = (a.samples > 0)
                    .then(|| a.k_square / a.samples as f64)
                    .and_then(|ms| (ms > SILENCE_MS).then(|| -0.691 + 10.0 * ms.log10()));
                let bands = if a.frames == 0 {
                    Vec::new()
                } else {
                    let mean: Vec<f64> = a.band_ms.iter().map(|m| m / a.frames as f64).collect();
                    let total: f64 = mean.iter().sum();
                    BANDS
                        .iter()
                        .zip(&mean)
                        .map(|((name, lo, hi), m)| BandLevel {
                            name,
                            low_hz: *lo,
                            high_hz: *hi,
                            energy_dbfs: db(*m),
                            share_pct: if total > SILENCE_MS { 100.0 * m / total } else { 0.0 },
                        })
                        .collect()
                };
                let dominant = bands
                    .iter()
                    .filter(|b| b.share_pct > 0.0)
                    .max_by(|x, y| x.share_pct.total_cmp(&y.share_pct))
                    .map(|b| b.name);
                WindowBalance {
                    start: *start,
                    end: *end,
                    label: label.clone(),
                    rms_dbfs: rms,
                    loudness_lufs: lufs,
                    bands,
                    dominant,
                }
            })
            .collect()
    }
}

/// The balance of `windows` over mono `samples` at `rate`. Errors on a window shorter than
/// [`MIN_WINDOW_S`] or a rate under 8 kHz.
pub fn band_balance(samples: &[f32], rate: u32, windows: &[BandWindow]) -> Result<Vec<WindowBalance>> {
    let mut meter = BandMeter::new(rate, 1, windows)?;
    meter.push(samples);
    Ok(meter.finish())
}

// ---- decoding ------------------------------------------------------------------------------

/// A decode silent this long is killed.
const STALL: Duration = Duration::from_secs(60);

/// Stream seconds `start..start + secs` of `path`'s first audio stream (all of it from `start`
/// with `None`) to `on_frames` as interleaved stereo f32 at [`SAMPLE_RATE`]. Ungated like every
/// read an agent looks at, but capped to a share of the machine and niced; a stall is killed and
/// `cancel` ends it with [`Error::Cancelled`].
pub fn stream_file(
    path: &Path,
    start: f64,
    secs: Option<f64>,
    cancel: &dyn Fn() -> bool,
    on_frames: &mut dyn FnMut(&[f32]),
) -> Result<()> {
    if cancel() {
        return Err(Error::Cancelled);
    }
    let bin = ffmpeg_bin();
    let mut cmd = bg_command(&bin);
    cpu::limit_cmd(&mut cmd, cpu::shared_threads(cpu::budget_threads(), 2));
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    if start > 0.0 {
        cmd.args(["-ss", &format!("{start:.6}")]);
    }
    cmd.arg("-i").arg(path);
    if let Some(secs) = secs {
        cmd.args(["-t", &format!("{secs:.6}")]);
    }
    cmd.args([
        "-map",
        "0:a:0",
        "-ac",
        "2",
        "-ar",
        &SAMPLE_RATE.to_string(),
        "-f",
        "f32le",
        "pipe:1",
    ]);
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    pump_pcm_cancellable(child, 8, STALL, cancel, on_frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    fn sine(hz: f64, amp: f64, secs: f64) -> Vec<f32> {
        (0..(secs * RATE as f64) as usize)
            .map(|i| (amp * (2.0 * std::f64::consts::PI * hz * i as f64 / RATE as f64).sin()) as f32)
            .collect()
    }

    fn window(start: f64, end: f64) -> BandWindow {
        BandWindow { start, end, label: None }
    }

    fn share(w: &WindowBalance, name: &str) -> f64 {
        w.bands.iter().find(|b| b.name == name).unwrap().share_pct
    }

    #[test]
    fn a_sine_lands_in_its_band_at_its_level() {
        for (hz, band) in [
            (40.0, "sub"),
            (100.0, "bass"),
            (350.0, "low_mid"),
            (1000.0, "mid"),
            (3000.0, "high_mid"),
            (10000.0, "air"),
        ] {
            let w = band_balance(&sine(hz, 1.0, 4.0), RATE, &[window(0.5, 3.5)])
                .unwrap()
                .remove(0);
            assert_eq!(w.dominant, Some(band), "{hz} Hz");
            assert!(share(&w, band) > 98.0, "{hz} Hz: {w:?}");
            // A full-scale sine is −3.01 dBFS RMS, and all of it is in the band.
            let level = w.bands.iter().find(|b| b.name == band).unwrap().energy_dbfs.unwrap();
            assert!((level - -3.01).abs() < 0.35, "{hz} Hz band level {level}");
            assert!((w.rms_dbfs.unwrap() - -3.01).abs() < 0.05, "{hz} Hz rms {:?}", w.rms_dbfs);
            assert_eq!(w.bands.len(), 6);
        }
    }

    #[test]
    fn shares_add_to_a_hundred_and_levels_follow_the_amplitude() {
        let a = band_balance(&sine(1000.0, 0.5, 3.0), RATE, &[window(0.5, 2.5)])
            .unwrap()
            .remove(0);
        let b = band_balance(&sine(1000.0, 0.25, 3.0), RATE, &[window(0.5, 2.5)])
            .unwrap()
            .remove(0);
        let sum: f64 = a.bands.iter().map(|b| b.share_pct).sum();
        assert!((sum - 100.0).abs() < 1e-6);
        assert!((a.rms_dbfs.unwrap() - b.rms_dbfs.unwrap() - 6.02).abs() < 0.05);
        let band = |w: &WindowBalance| w.bands[3].energy_dbfs.unwrap();
        assert!((band(&a) - band(&b) - 6.02).abs() < 0.1);
    }

    #[test]
    fn white_noise_spreads_by_bandwidth() {
        let mut state = 0x1234_5678_u64;
        let noise: Vec<f32> = (0..RATE as usize * 6)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 11) as f32 / (1u64 << 53) as f32 * 2.0 - 1.0
            })
            .collect();
        let w = band_balance(&noise, RATE, &[window(0.5, 5.5)]).unwrap().remove(0);
        // Bandwidth of the six bands: 40, 190, 250, 1500, 4000, 14000 Hz of 20 kHz-ish total.
        let total = 40.0 + 190.0 + 250.0 + 1500.0 + 4000.0 + 14000.0;
        for (name, width) in [
            ("sub", 40.0),
            ("bass", 190.0),
            ("low_mid", 250.0),
            ("mid", 1500.0),
            ("high_mid", 4000.0),
            ("air", 14000.0),
        ] {
            let want = 100.0 * width / total;
            let got = share(&w, name);
            assert!((got - want).abs() < want * 0.3 + 0.3, "{name}: {got:.2}% vs {want:.2}%");
        }
        assert_eq!(w.dominant, Some("air"));
    }

    #[test]
    fn windows_see_only_their_own_stretch() {
        let mut x = sine(100.0, 0.5, 3.0);
        x.extend(sine(5000.0, 0.5, 3.0));
        let ws = band_balance(
            &x,
            RATE,
            &[
                BandWindow {
                    start: 0.5,
                    end: 2.5,
                    label: Some("low".to_string()),
                },
                window(3.5, 5.5),
                window(0.0, 6.0),
            ],
        )
        .unwrap();
        assert_eq!(ws[0].label.as_deref(), Some("low"));
        assert_eq!(ws[0].dominant, Some("bass"));
        assert_eq!(ws[1].dominant, Some("high_mid"));
        assert!(share(&ws[0], "bass") > 98.0 && share(&ws[1], "high_mid") > 98.0);
        // The whole span has both, half and half.
        assert!((share(&ws[2], "bass") - 50.0).abs() < 6.0, "{}", share(&ws[2], "bass"));
        assert!((share(&ws[2], "high_mid") - 50.0).abs() < 6.0);
    }

    #[test]
    fn silence_has_no_levels() {
        let w = band_balance(&vec![0.0; RATE as usize * 2], RATE, &[window(0.0, 2.0)])
            .unwrap()
            .remove(0);
        assert_eq!(w.rms_dbfs, None);
        assert_eq!(w.loudness_lufs, None);
        assert_eq!(w.dominant, None);
        assert!(w.bands.iter().all(|b| b.energy_dbfs.is_none() && b.share_pct == 0.0));
    }

    #[test]
    fn k_weighted_loudness_reads_like_a_loudness_meter() {
        // BS.1770: a 1 kHz full-scale sine in one channel is −3.01 LUFS (the filters pass 1 kHz at
        // +0.7 dB, the −0.691 offset undoes it).
        let w = band_balance(&sine(997.0, 1.0, 4.0), RATE, &[window(0.5, 3.5)])
            .unwrap()
            .remove(0);
        assert!((w.loudness_lufs.unwrap() - -3.0).abs() < 0.2, "{:?}", w.loudness_lufs);
        // Bass counts for less: the same level at 60 Hz reads lower.
        let low = band_balance(&sine(60.0, 1.0, 4.0), RATE, &[window(0.5, 3.5)])
            .unwrap()
            .remove(0);
        assert!(low.loudness_lufs.unwrap() < w.loudness_lufs.unwrap() - 1.0);
        // Two identical channels read 3 LU louder than one.
        let mono = sine(997.0, 0.5, 4.0);
        let stereo: Vec<f32> = mono.iter().flat_map(|s| [*s, *s]).collect();
        let mut meter = BandMeter::new(RATE, 2, &[window(0.5, 3.5)]).unwrap();
        meter.push(&stereo);
        let s = meter.finish().remove(0);
        let m = band_balance(&mono, RATE, &[window(0.5, 3.5)]).unwrap().remove(0);
        assert!((s.loudness_lufs.unwrap() - m.loudness_lufs.unwrap() - 3.01).abs() < 0.05);
        assert!(
            (s.rms_dbfs.unwrap() - m.rms_dbfs.unwrap()).abs() < 0.01,
            "the bands read the mono mix"
        );
    }

    #[test]
    fn streaming_in_pieces_is_the_same_as_all_at_once() {
        let mut x = sine(200.0, 0.4, 2.0);
        x.extend(sine(2500.0, 0.3, 2.0));
        let windows = [window(0.2, 1.8), window(2.2, 3.8)];
        let whole = band_balance(&x, RATE, &windows).unwrap();
        let mut meter = BandMeter::new(RATE, 1, &windows).unwrap();
        for chunk in x.chunks(3001) {
            meter.push(chunk);
        }
        assert_eq!(meter.finish(), whole);
    }

    #[test]
    fn a_short_stream_is_measured_from_one_padded_frame_and_bad_windows_are_refused() {
        let w = band_balance(&sine(1000.0, 1.0, 0.3), RATE, &[window(0.0, 0.3)])
            .unwrap()
            .remove(0);
        assert_eq!(w.dominant, Some("mid"));
        assert!(BandMeter::new(RATE, 1, &[window(1.0, 1.1)]).is_err());
        assert!(BandMeter::new(RATE, 1, &[window(-1.0, 2.0)]).is_err());
        assert!(BandMeter::new(RATE, 1, &[window(f64::NAN, 2.0)]).is_err());
        assert!(BandMeter::new(4000, 1, &[window(0.0, 2.0)]).is_err());
        assert!(BandMeter::new(RATE, 3, &[window(0.0, 2.0)]).is_err());
    }

    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn a_file_streams_through_the_decoder_into_the_meter() {
        let dir = std::env::temp_dir().join(format!("kerf-bands-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        let mono = sine(100.0, 0.5, 3.0);
        super::super::pcm::write_wav(&path, &super::super::pcm::AudioBuffer::new(RATE, 1, mono)).unwrap();
        let mut meter = BandMeter::new(SAMPLE_RATE, 2, &[window(0.5, 2.5)]).unwrap();
        stream_file(&path, 0.0, None, &|| false, &mut |f| meter.push(f)).unwrap();
        let w = meter.finish().remove(0);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(w.dominant, Some("bass"));
        // 0.5 amplitude is −9.03 dBFS; ffmpeg's mono-to-stereo upmix puts it in each channel at
        // −3 dB (power preserved), and the bands read the mono sum of the two.
        assert!((w.rms_dbfs.unwrap() - -12.04).abs() < 0.1, "{:?}", w.rms_dbfs);
    }
}
