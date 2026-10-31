//! Sound effects, synthesized in pure Rust: nothing to download, nothing to license, the same
//! spec always the same audio.
//!
//! Every [`SfxKind`] is built from a handful of parts — seeded noise, band-limited oscillators
//! with pitch envelopes, a state-variable filter that can be swept every sample, short
//! envelopes — and a small stereo reverb for `tail`. A whoosh is pink noise through a band-pass
//! that rises to the peak and falls away, one filter per channel and a pan that travels with
//! it; a riser is noise and a detuned saw stack climbing together with the filter opening; an
//! impact is a sub sine falling in pitch (driven through `tanh` so a phone speaker still hears
//! it) under a mid thump, a noise burst that closes, a crack and a rumble. `intensity` scales
//! brightness, drive and density; `pitch` moves every frequency; `tail` is the reverb's level
//! and length; `reverse` plays the finished render backwards.
//!
//! The finished render is scaled so its peak is exactly [`PEAK_DBFS`] after a gentle `tanh`
//! (the sound's body comes up under its transient), then its kind's `level_db` is applied, so
//! a click does not sit as loud as a boom. Output is 48 kHz stereo 16-bit WAV with the same
//! seeded triangular dither music uses, in the **data** directory
//! (`<data>/kerf/sfx/sfx-<hash>.wav`, the only copy), named by a hash of the renderer version
//! and the spec with its length spelled out, so a spec is rendered once and is the same asset.
//! A render takes milliseconds: no `cpu::lease`, and never the project lock.

use std::f64::consts::{PI, TAU};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use super::compose::{finished, write_wav16, PEAK_DBFS};
use crate::error::{Error, Result};
use crate::model::{SfxAnchor, SfxKind, SfxSpec, MAX_SFX_PITCH};

/// The rate effects are rendered at.
pub const SAMPLE_RATE: u32 = 48_000;

/// Bump when what a spec sounds like changes, so a render made before is not reused for it.
pub const RENDERER_VERSION: u32 = 1;

const SR: f64 = SAMPLE_RATE as f64;

// ---- where things live ---------------------------------------------------------------------

/// Where effects are written: the *data* directory, because this file is the only copy of the audio.
pub fn output_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("kerf").join("sfx"))
}

/// The file `spec` renders to in `dir`: named by a hash of the renderer version and the spec with its
/// length spelled out, so two specs that sound alike are one file.
pub fn output_path_in(dir: &Path, spec: &SfxSpec) -> Result<PathBuf> {
    let mut hash = Sha256::new();
    for part in [format!("v{RENDERER_VERSION}"), serde_json::to_string(&spec.resolved())?] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    let digest = hash.finalize();
    let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    Ok(dir.join(format!("sfx-{name}.wav")))
}

// ---- the parts -----------------------------------------------------------------------------

type Buf = Vec<f64>;

/// A small seeded generator (xorshift64\*, started through splitmix64), so every random choice is
/// a function of the spec.
struct Rng(u64);

impl Rng {
    fn new(seed: u64, salt: u64) -> Self {
        let mut z = seed
            .wrapping_add(salt.wrapping_mul(0x9E37_79B9_7F4A_7C15))
            .wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Rng((z ^ (z >> 31)) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// In `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// In `[-1, 1)`.
    fn bipolar(&mut self) -> f64 {
        self.unit() * 2.0 - 1.0
    }

    fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    fn fork(&mut self, salt: u64) -> Rng {
        let seed = self.next();
        Rng::new(seed, salt)
    }
}

fn white(n: usize, rng: &mut Rng) -> Buf {
    (0..n).map(|_| rng.bipolar()).collect()
}

/// Pink noise: white through Paul Kellet's three-pole approximation of 1/f.
fn pink(n: usize, rng: &mut Rng) -> Buf {
    let (mut b0, mut b1, mut b2) = (0.0, 0.0, 0.0);
    (0..n)
        .map(|_| {
            let w = rng.bipolar();
            b0 = 0.997_65 * b0 + w * 0.099_046;
            b1 = 0.963 * b1 + w * 0.296_516_4;
            b2 = 0.57 * b2 + w * 1.052_691_3;
            (b0 + b1 + b2 + w * 0.1848) * 0.22
        })
        .collect()
}

/// Brown noise: white through a leaky integrator, brought back to a level near white's.
fn brown(n: usize, rng: &mut Rng) -> Buf {
    let mut b = 0.0;
    let mut out: Buf = (0..n)
        .map(|_| {
            b = 0.997 * b + 0.05 * rng.bipolar();
            b
        })
        .collect();
    scale_to_rms(&mut out, 0.3);
    out
}

fn rms(x: &[f64]) -> f64 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|s| s * s).sum::<f64>() / x.len() as f64).sqrt()
}

fn scale_to_rms(x: &mut [f64], target: f64) {
    let r = rms(x);
    if r > 1e-12 {
        let g = target / r;
        x.iter_mut().for_each(|s| *s *= g);
    }
}

fn lerp(a: f64, b: f64, x: f64) -> f64 {
    a + (b - a) * x
}

/// `a` to `b` along a curve straight in octaves (pitch and cutoff are heard that way).
fn lerp_exp(a: f64, b: f64, x: f64) -> f64 {
    a * (b / a).powf(x)
}

fn smooth(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn decay(t: f64, tau: f64) -> f64 {
    (-t / tau).exp()
}

/// An oscillator's output shape.
#[derive(Clone, Copy, PartialEq)]
enum Wave {
    Sine,
    Saw,
    Square,
}

fn poly_blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let x = t / dt;
        2.0 * x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}

fn wave_at(wave: Wave, ph: f64, dt: f64) -> f64 {
    match wave {
        Wave::Sine => (TAU * ph).sin(),
        Wave::Saw => 2.0 * ph - 1.0 - poly_blep(ph, dt),
        Wave::Square => {
            let v = if ph < 0.5 { 1.0 } else { -1.0 };
            v + poly_blep(ph, dt) - poly_blep((ph + 0.5).fract(), dt)
        }
    }
}

/// `n` samples of `wave` at the frequency `freq(t)` (t in seconds), the phase integrating it so a
/// moving pitch stays smooth. Band-limited with a poly-BLEP; never above 0.45 of the rate.
fn osc(n: usize, wave: Wave, mut freq: impl FnMut(f64) -> f64, phase: f64) -> Buf {
    let mut ph = phase.rem_euclid(1.0);
    (0..n)
        .map(|k| {
            let dt = (freq(k as f64 / SR) / SR).clamp(0.0, 0.45);
            let v = wave_at(wave, ph, dt);
            ph += dt;
            if ph >= 1.0 {
                ph -= 1.0;
            }
            v
        })
        .collect()
}

/// Multiply `x` by the envelope `env(t)`.
fn shape(x: &mut [f64], mut env: impl FnMut(f64) -> f64) {
    for (k, s) in x.iter_mut().enumerate() {
        *s *= env(k as f64 / SR);
    }
}

/// A Zavalishin trapezoidal state-variable filter: stable under a cutoff that moves every sample.
#[derive(Default, Clone, Copy)]
struct Svf {
    ic1: f64,
    ic2: f64,
}

impl Svf {
    /// `(low-pass, band-pass at unity peak gain, high-pass)` of the next sample.
    fn tick(&mut self, x: f64, fc: f64, q: f64) -> (f64, f64, f64) {
        let g = (PI * fc.clamp(8.0, SR * 0.45) / SR).tan();
        let k = 1.0 / q.max(0.05);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let v3 = x - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        (v2, v1 * k, x - k * v1 - v2)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Low,
    Band,
    High,
}

/// `x` through a filter whose cutoff is `fc(t)`.
fn filt(x: &[f64], mode: Mode, mut fc: impl FnMut(f64) -> f64, q: f64) -> Buf {
    let mut svf = Svf::default();
    x.iter()
        .enumerate()
        .map(|(k, &v)| {
            let (lo, band, hi) = svf.tick(v, fc(k as f64 / SR), q);
            match mode {
                Mode::Low => lo,
                Mode::Band => band,
                Mode::High => hi,
            }
        })
        .collect()
}

fn lowpass(x: &[f64], fc: f64) -> Buf {
    filt(x, Mode::Low, |_| fc, std::f64::consts::FRAC_1_SQRT_2)
}

fn highpass(x: &[f64], fc: f64) -> Buf {
    filt(x, Mode::High, |_| fc, std::f64::consts::FRAC_1_SQRT_2)
}

fn bandpass(x: &[f64], fc: f64, q: f64) -> Buf {
    filt(x, Mode::Band, |_| fc, q)
}

/// Left and right gains for a pan in `[-1, 1]`, constant power.
fn pan_gains(pan: f64) -> (f64, f64) {
    let a = (pan.clamp(-1.0, 1.0) + 1.0) * PI / 4.0;
    (a.cos(), a.sin())
}

struct Stereo {
    l: Buf,
    r: Buf,
}

impl Stereo {
    fn new(n: usize) -> Self {
        Stereo {
            l: vec![0.0; n],
            r: vec![0.0; n],
        }
    }

    fn add_mono(&mut self, x: &[f64], gain: f64, pan: f64) {
        self.add_at(0, x, gain, pan);
    }

    /// Mix `x` in from sample `start`, panned.
    fn add_at(&mut self, start: usize, x: &[f64], gain: f64, pan: f64) {
        let (gl, gr) = pan_gains(pan);
        for (k, s) in x.iter().enumerate() {
            let i = start + k;
            if i >= self.l.len() {
                break;
            }
            self.l[i] += s * gain * gl;
            self.r[i] += s * gain * gr;
        }
    }

    fn reverse(&mut self) {
        self.l.reverse();
        self.r.reverse();
    }
}

// ---- reverb --------------------------------------------------------------------------------

struct Comb {
    buf: Vec<f64>,
    pos: usize,
    store: f64,
    feedback: f64,
}

impl Comb {
    fn tick(&mut self, x: f64, damp: f64) -> f64 {
        let out = self.buf[self.pos];
        self.store = out * (1.0 - damp) + self.store * damp;
        self.buf[self.pos] = x + self.store * self.feedback;
        self.pos = (self.pos + 1) % self.buf.len();
        out
    }
}

struct Allpass {
    buf: Vec<f64>,
    pos: usize,
}

impl Allpass {
    fn tick(&mut self, x: f64) -> f64 {
        let held = self.buf[self.pos];
        self.buf[self.pos] = x + held * 0.5;
        self.pos = (self.pos + 1) % self.buf.len();
        held - x
    }
}

const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;
/// Brings the wet signal of white noise to about the level of the dry.
const WET_NORM: f64 = 0.085;

/// The reverb of `dry` alone (not mixed): eight damped combs and four all-pass diffusers a side
/// (Schroeder / Freeverb), each comb's feedback set from its own length so the room rings for
/// `rt60` seconds, fed a mono sum high-passed at `hp`. The two sides' delays differ, which is the
/// stereo width.
fn reverb(dry: &Stereo, hp: f64, rt60: f64, damp: f64) -> Stereo {
    let n = dry.l.len();
    let scale = SR / 44_100.0;
    let mono: Buf = dry.l.iter().zip(&dry.r).map(|(l, r)| (l + r) * 0.5).collect();
    let input = if hp > 0.0 { highpass(&mono, hp) } else { mono };
    let mut out = Stereo::new(n);
    for side in 0..2 {
        let spread = STEREO_SPREAD * side;
        let mut combs: Vec<Comb> = COMBS
            .iter()
            .map(|&len| {
                let len = ((len + spread) as f64 * scale) as usize;
                Comb {
                    buf: vec![0.0; len],
                    pos: 0,
                    store: 0.0,
                    feedback: 10f64.powf(-3.0 * len as f64 / (SR * rt60.max(0.02))),
                }
            })
            .collect();
        let mut allpasses: Vec<Allpass> = ALLPASSES
            .iter()
            .map(|&len| Allpass {
                buf: vec![0.0; ((len + spread) as f64 * scale) as usize],
                pos: 0,
            })
            .collect();
        let target = if side == 0 { &mut out.l } else { &mut out.r };
        for (k, &x) in input.iter().enumerate() {
            let mut s: f64 = combs.iter_mut().map(|c| c.tick(x, damp)).sum();
            for a in &mut allpasses {
                s = a.tick(s);
            }
            target[k] = s * WET_NORM;
        }
    }
    out
}

/// How much room a kind gets: the wet level at full `tail`, the reverb time at no tail and at
/// full tail, how dark the room is (0 bright, 1 dull), and the sound below `hp` that stays out
/// of the reverb (a sub in a hall is mud).
struct Space {
    wet: f64,
    rt: (f64, f64),
    damp: f64,
    hp: f64,
    pre: f64,
}

fn space(kind: SfxKind) -> Space {
    use SfxKind::*;
    let s = |wet, rt, damp, hp, pre| Space { wet, rt, damp, hp, pre };
    match kind {
        Whoosh | Swoosh => s(0.45, (0.2, 1.1), 0.4, 300.0, 0.0),
        Swipe => s(0.4, (0.1, 0.5), 0.35, 400.0, 0.0),
        Impact => s(0.75, (0.3, 2.2), 0.35, 150.0, 0.012),
        Boom => s(0.65, (0.8, 4.0), 0.45, 90.0, 0.02),
        Thud => s(0.4, (0.12, 0.7), 0.4, 150.0, 0.0),
        Punch => s(0.4, (0.1, 0.6), 0.35, 250.0, 0.0),
        Riser | Downlifter => s(0.5, (0.3, 1.8), 0.4, 200.0, 0.0),
        ReverseSwell => s(0.9, (0.8, 3.0), 0.4, 200.0, 0.0),
        Pop | Click | Bubble | Typewriter | Shutter => s(0.5, (0.08, 0.5), 0.3, 400.0, 0.0),
        Ding | Notify => s(0.55, (0.2, 1.4), 0.25, 300.0, 0.0),
        Glitch => s(0.35, (0.05, 0.4), 0.3, 400.0, 0.0),
        TapeStop | RecordScratch => s(0.35, (0.1, 0.7), 0.4, 300.0, 0.0),
        Heartbeat => s(0.35, (0.2, 1.0), 0.45, 90.0, 0.0),
    }
}

// ---- the kinds -----------------------------------------------------------------------------

/// What a kind is built from: its length, the pitch and intensity asked for, a generator.
struct Ctx {
    kind: SfxKind,
    n: usize,
    d: f64,
    /// The pitch as a frequency ratio.
    p: f64,
    i: f64,
    rng: Rng,
}

impl Ctx {
    fn st(&self) -> Stereo {
        Stereo::new(self.n)
    }

    fn buf(&self) -> Buf {
        vec![0.0; self.n]
    }

    /// An independent noise pair, one per side.
    fn pair(&mut self, make: impl Fn(usize, &mut Rng) -> Buf) -> (Buf, Buf) {
        let mut a = self.rng.fork(1);
        let mut b = self.rng.fork(2);
        (make(self.n, &mut a), make(self.n, &mut b))
    }

    fn sample(&self, t: f64) -> usize {
        (t.max(0.0) * SR).round() as usize
    }
}

fn peak_fraction(kind: SfxKind) -> f64 {
    match kind.preset().anchor {
        SfxAnchor::Peak { fraction } => fraction,
        _ => 0.5,
    }
}

fn hit_s(kind: SfxKind) -> f64 {
    match kind.preset().anchor {
        SfxAnchor::Hit { seconds } => seconds,
        _ => 0.0,
    }
}

/// How a pass of air through the picture is shaped.
struct Pass {
    /// The band-pass centre at the start, at the peak and at the end, Hz.
    f: [f64; 3],
    q: f64,
    /// The exponents of the amplitude rise and fall around the peak.
    rise: f64,
    fall: f64,
    /// The weights of the low body, the airy hiss and the narrow whistle riding the sweep.
    low: f64,
    air: f64,
    whistle: f64,
    /// Where it travels, left to right, in pan units.
    pan: (f64, f64),
}

/// Whoosh, swoosh, swipe: noise through a band-pass that climbs to the peak and falls away, a
/// filter per side a little apart, a pan that travels with it.
fn pass(c: &mut Ctx, pr: &Pass) -> Stereo {
    let (d, p, i) = (c.d, c.p, c.i);
    let tp = peak_fraction(c.kind) * d;
    let env = move |t: f64| {
        if t < tp {
            smooth(t / tp).powf(pr.rise)
        } else {
            smooth((d - t) / (d - tp)).powf(pr.fall)
        }
    };
    let fc = move |t: f64| {
        p * if t < tp {
            lerp_exp(pr.f[0], pr.f[1], smooth(t / tp))
        } else {
            lerp_exp(pr.f[1], pr.f[2], smooth((t - tp) / (d - tp)))
        }
    };
    let (nl, nr) = c.pair(pink);
    let mut sides: Vec<Buf> = Vec::new();
    for (ch, noise) in [nl, nr].iter().enumerate() {
        let skew = if ch == 0 { 1.0 } else { 1.045 };
        let mut band = filt(noise, Mode::Band, |t| fc(t) * skew, pr.q);
        let mut whistle = filt(noise, Mode::Band, |t| fc(t) * skew * 1.5, 14.0);
        shape(&mut band, |t| env(t) * pr.q.sqrt());
        shape(&mut whistle, |t| env(t).powf(2.0) * 14f64.sqrt() * pr.whistle);
        band.iter_mut().zip(&whistle).for_each(|(b, w)| *b += w);
        sides.push(band);
    }
    let (wl, wr) = c.pair(white);
    let low_noise = c.pair(brown);
    let air_hz = 4_500.0 * p * (0.6 + 0.4 * i);
    for (ch, (w, lo)) in [(wl, low_noise.0), (wr, low_noise.1)].into_iter().enumerate() {
        let mut air = highpass(&w, air_hz);
        shape(&mut air, |t| env(t).powf(1.5) * pr.air * (0.35 + 0.65 * i));
        let mut low = filt(&lo, Mode::Low, |t| p * lerp(180.0, 420.0, smooth(t / d)), 0.8);
        shape(&mut low, |t| env(t).powf(1.2) * pr.low);
        for (side, (a, l)) in sides[ch].iter_mut().zip(air.iter().zip(&low)) {
            *side += a + l;
        }
    }
    let mut st = c.st();
    let width = 0.6;
    let outs = st.l.iter_mut().zip(st.r.iter_mut());
    for (k, ((l, r), (x, y))) in outs.zip(sides[0].iter().zip(&sides[1])).enumerate() {
        let t = k as f64 / SR;
        let pos = lerp(pr.pan.0, pr.pan.1, smooth((t - 0.04 * d) / (0.92 * d)));
        let (gl, gr) = pan_gains(pos);
        let (mid, side) = ((x + y) * 0.5, (x - y) * 0.5);
        *l = mid * gl * std::f64::consts::SQRT_2 + side * width;
        *r = mid * gr * std::f64::consts::SQRT_2 - side * width;
    }
    st
}

fn whoosh(c: &mut Ctx) -> Stereo {
    let i = c.i;
    pass(
        c,
        &Pass {
            f: [220.0, 1_500.0 + 3_000.0 * i, 350.0],
            q: 1.1 + 1.6 * i,
            rise: 1.6,
            fall: 1.8,
            low: 0.5,
            air: 0.28,
            whistle: 0.06,
            pan: (-0.85, 0.85),
        },
    )
}

fn swoosh(c: &mut Ctx) -> Stereo {
    let i = c.i;
    pass(
        c,
        &Pass {
            f: [700.0, 3_500.0 + 3_500.0 * i, 1_800.0],
            q: 2.2 + 2.5 * i,
            rise: 1.3,
            fall: 2.4,
            low: 0.1,
            air: 0.3,
            whistle: 0.2,
            pan: (-0.7, 0.7),
        },
    )
}

fn swipe(c: &mut Ctx) -> Stereo {
    let i = c.i;
    pass(
        c,
        &Pass {
            f: [1_500.0, 5_500.0 + 2_500.0 * i, 3_000.0],
            q: 1.6,
            rise: 1.2,
            fall: 2.0,
            low: 0.0,
            air: 0.4,
            whistle: 0.08,
            pan: (-0.9, 0.9),
        },
    )
}

/// A sine whose pitch falls from `from` to `to` Hz with time constant `tau_p`, attacked in `attack`
/// seconds and decaying with `tau`, driven through `tanh` so it carries harmonics a small speaker
/// can reproduce.
fn drop_sine(n: usize, from: f64, to: f64, tau_p: f64, attack: f64, tau: f64, drive: f64) -> Buf {
    let mut s = osc(n, Wave::Sine, |t| to + (from - to) * decay(t, tau_p), 0.0);
    shape(&mut s, |t| smooth(t / attack) * decay(t, tau));
    if drive > 1e-6 {
        let norm = drive.tanh();
        s.iter_mut().for_each(|x| *x = (drive * *x).tanh() / norm);
    }
    s
}

/// Noise through a low-pass that closes from `from` to `to` Hz, shaped by `attack` and `tau`.
fn noise_burst(noise: &[f64], from: f64, to: f64, tau_f: f64, attack: f64, tau: f64) -> Buf {
    let mut b = filt(noise, Mode::Low, |t| to + (from - to) * decay(t, tau_f), 0.8);
    shape(&mut b, |t| smooth(t / attack) * decay(t, tau));
    b
}

fn impact(c: &mut Ctx) -> Stereo {
    let (n, d, p, i) = (c.n, c.d, c.p, c.i);
    let mut st = c.st();
    let tau = 0.1 + 0.22 * d;
    st.add_mono(&drop_sine(n, 110.0 * p, 36.0 * p, 0.085, 0.003, tau, 0.9 + 1.4 * i), 1.0, 0.0);
    st.add_mono(&drop_sine(n, 220.0 * p, 72.0 * p, 0.085, 0.003, tau * 0.45, 0.0), 0.3, 0.0);
    st.add_mono(&drop_sine(n, 170.0 * p, 62.0 * p, 0.028, 0.0015, 0.11, 0.0), 0.6, 0.0);
    let (wl, wr) = c.pair(white);
    for (ch, w) in [wl, wr].iter().enumerate() {
        let side = if ch == 0 { -1.0 } else { 1.0 };
        let burst = noise_burst(
            w,
            7_000.0 * (0.35 + 0.65 * i) * p.min(2.0),
            500.0,
            0.07,
            0.0008,
            0.09 + 0.05 * i,
        );
        let mut crack = highpass(w, 1_500.0 * p);
        shape(&mut crack, |t| smooth(t / 0.0006) * decay(t, 0.012));
        st.add_mono(&burst, 0.5 * (0.5 + 0.5 * i), side);
        st.add_mono(&crack, 0.15 + 0.35 * i, side);
    }
    let mut rng = c.rng.fork(3);
    let mut rumble = lowpass(&brown(n, &mut rng), 140.0);
    shape(&mut rumble, |t| smooth(t / 0.025) * decay(t, 0.3 * d));
    st.add_mono(&rumble, 0.5, 0.0);
    st
}

fn boom(c: &mut Ctx) -> Stereo {
    let (n, d, p, i) = (c.n, c.d, c.p, c.i);
    let mut st = c.st();
    let tau = 0.2 + 0.14 * d;
    st.add_mono(&drop_sine(n, 70.0 * p, 24.0 * p, 0.35, 0.006, tau, 0.8 + 1.2 * i), 1.0, 0.0);
    st.add_mono(&drop_sine(n, 140.0 * p, 48.0 * p, 0.35, 0.004, tau * 0.5, 0.0), 0.35, 0.0);
    let mut rng = c.rng.fork(3);
    let mut rumble = lowpass(&brown(n, &mut rng), 80.0 * p);
    shape(&mut rumble, |t| smooth(t / 0.04) * decay(t, tau * 1.1));
    st.add_mono(&rumble, 0.9, 0.0);
    let (wl, wr) = c.pair(white);
    for (ch, w) in [wl, wr].iter().enumerate() {
        let side = if ch == 0 { -1.0 } else { 1.0 };
        let blast = noise_burst(w, 3_000.0 * (0.4 + 0.6 * i) * p.min(2.0), 300.0, 0.12, 0.002, 0.2 + 0.1 * d);
        st.add_mono(&blast, 0.45 * (0.4 + 0.6 * i), side);
    }
    // Debris: small grains of noise thrown across the first half second.
    let grains = 4 + (10.0 * i) as usize;
    let mut debris = c.rng.fork(4);
    let noise = white(n, &mut debris);
    for _ in 0..grains {
        let at = c.sample(debris.between(0.02, 0.55));
        let len = c.sample(0.05).min(n.saturating_sub(at));
        let mut grain = bandpass(&noise[at..at + len], debris.between(500.0, 3_500.0) * p.min(2.0), 2.0);
        shape(&mut grain, |t| decay(t, 0.008));
        st.add_at(at, &grain, 0.5 * debris.between(0.3, 1.0), debris.bipolar() * 0.8);
    }
    st
}

fn thud(c: &mut Ctx) -> Stereo {
    let (n, d, p, i) = (c.n, c.d, c.p, c.i);
    let mut st = c.st();
    st.add_mono(
        &drop_sine(n, 106.0 * p, 46.0 * p, 0.035, 0.002, 0.06 + 0.1 * d, 0.8 + 1.0 * i),
        1.0,
        0.0,
    );
    let (wl, wr) = c.pair(white);
    for (ch, w) in [wl, wr].iter().enumerate() {
        let side = if ch == 0 { -1.0 } else { 1.0 };
        st.add_mono(&noise_burst(w, 700.0 * p, 200.0, 0.02, 0.001, 0.02), 0.5, side);
        let mut click = highpass(w, 1_500.0);
        shape(&mut click, |t| smooth(t / 0.0005) * decay(t, 0.003));
        st.add_mono(&click, 0.15 * i, side);
    }
    st
}

fn punch(c: &mut Ctx) -> Stereo {
    let (n, p, i) = (c.n, c.p, c.i);
    let mut st = c.st();
    st.add_mono(
        &drop_sine(n, 280.0 * p, 90.0 * p, 0.015, 0.001, 0.07, 0.6 + 0.9 * i),
        0.9,
        0.0,
    );
    st.add_mono(&drop_sine(n, 100.0 * p, 52.0 * p, 0.03, 0.002, 0.14, 0.6), 0.6, 0.0);
    let (wl, wr) = c.pair(white);
    for (ch, w) in [wl, wr].iter().enumerate() {
        let side = if ch == 0 { -1.0 } else { 1.0 };
        let mut crack = bandpass(w, 1_900.0 * p * (0.7 + 0.6 * i), 0.9);
        shape(&mut crack, |t| smooth(t / 0.0006) * decay(t, 0.035));
        let mut snap = highpass(w, 4_000.0);
        shape(&mut snap, |t| smooth(t / 0.0004) * decay(t, 0.004));
        st.add_mono(&crack, 0.8 * (0.5 + 0.5 * i), side);
        st.add_mono(&snap, 0.4 * i, side);
    }
    st
}

/// Riser and downlifter: noise and a detuned saw stack that climb (or fall) together while the
/// filters open (or close); the riser ends on its peak, the downlifter starts on a thump.
fn build(c: &mut Ctx, rising: bool) -> Stereo {
    let (n, d, p, i) = (c.n, c.d, c.p, c.i);
    let mut st = c.st();
    let x = move |t: f64| (t / d).clamp(0.0, 1.0);
    let u = move |t: f64| if rising { x(t) } else { 1.0 - x(t) };
    let env = move |t: f64, power: f64| {
        if rising {
            0.03 + 0.97 * x(t).powf(power)
        } else {
            smooth(t / 0.008) * (1.0 - x(t)).powf(power * 0.8)
        }
    };

    let (nl, nr) = c.pair(pink);
    for (ch, noise) in [nl, nr].iter().enumerate() {
        let skew = if ch == 0 { 1.0 } else { 1.05 };
        let mut band = filt(
            noise,
            Mode::Band,
            |t| lerp_exp(180.0 * p, 9_000.0 * p, u(t).powf(1.4)) * skew,
            1.4,
        );
        shape(&mut band, |t| env(t, 2.0) * 1.3);
        st.add_mono(&band, 0.9, if ch == 0 { -0.6 } else { 0.6 });
    }
    let (wl, wr) = c.pair(white);
    for (ch, w) in [wl, wr].iter().enumerate() {
        let mut air = highpass(w, 5_000.0 * p);
        shape(&mut air, |t| env(t, 2.6) * (0.12 + 0.3 * i));
        st.add_mono(&air, 0.6, if ch == 0 { -0.8 } else { 0.8 });
    }

    let octaves = 2.5 + 1.5 * i;
    let spread = 0.4 + 0.6 * i;
    let voices: [(f64, f64); 4] = [(-11.0, -0.7), (-4.0, -0.25), (4.0, 0.25), (11.0, 0.7)];
    for (v, (cents, pan)) in voices.iter().enumerate() {
        let detune = 2f64.powf(cents * spread / 1200.0);
        let phase = c.rng.fork(10 + v as u64).unit();
        let saw = osc(
            n,
            Wave::Saw,
            |t| 82.4 * p * 2f64.powf(octaves * u(t).powf(1.35)) * detune,
            phase,
        );
        let mut saw = filt(
            &saw,
            Mode::Low,
            |t| lerp_exp(350.0, 14_000.0, u(t).powf(1.2)) * (0.6 + 0.4 * i),
            0.9,
        );
        shape(&mut saw, |t| env(t, 2.3));
        st.add_mono(&saw, 0.2, *pan);
    }
    let mut sub = osc(n, Wave::Sine, |t| 55.0 * p * 2f64.powf(u(t)), 0.0);
    shape(&mut sub, |t| {
        if rising {
            x(t).powi(3) * 0.6
        } else {
            smooth(t / 0.008) * decay(t, 0.45 * d) * 0.7
        }
    });
    st.add_mono(&sub, 0.6, 0.0);
    if !rising {
        st.add_mono(&drop_sine(n, 100.0 * p, 45.0 * p, 0.08, 0.004, 0.25, 1.5), 0.8, 0.0);
        let w = white(n, &mut c.rng.fork(20));
        st.add_mono(&noise_burst(&w, 6_000.0 * p.min(2.0), 600.0, 0.05, 0.001, 0.07), 0.4, 0.0);
    }
    st
}

/// A crash and a pad, the forward half of a reverse swell: shimmering inharmonic partials over
/// hissing noise, a low thump, a chord that rings. Played backwards it swells into the hit.
fn crash(c: &mut Ctx) -> Stereo {
    let (n, d, p, i) = (c.n, c.d, c.p, c.i);
    let mut st = c.st();
    let tau = 0.18 * d + 0.1;
    let (wl, wr) = c.pair(white);
    for (ch, w) in [wl, wr].iter().enumerate() {
        let mut hiss = highpass(w, 2_200.0 * p * (0.7 + 0.3 * i));
        shape(&mut hiss, |t| smooth(t / 0.004) * decay(t, tau));
        st.add_mono(&hiss, 0.7, if ch == 0 { -0.5 } else { 0.5 });
    }
    for (j, ratio) in [2.0, 3.0, 4.16, 5.43, 6.79, 8.21].iter().enumerate() {
        let mut rng = c.rng.fork(30 + j as u64);
        let f = 1_250.0 * p * ratio * rng.between(0.99, 1.01);
        let mut s = osc(n, Wave::Sine, |_| f, rng.unit());
        let life = tau * rng.between(0.5, 1.0);
        shape(&mut s, |t| smooth(t / 0.003) * decay(t, life));
        st.add_mono(&s, 0.12 * (0.5 + 0.5 * i), rng.bipolar() * 0.7);
    }
    for (j, f) in [130.8, 196.0, 261.6, 329.6].iter().enumerate() {
        let saw = osc(n, Wave::Saw, |_| f * p, 0.25 * j as f64);
        let mut pad = lowpass(&saw, 1_400.0);
        shape(&mut pad, |t| smooth(t / 0.01) * decay(t, 0.4 * d));
        st.add_mono(&pad, 0.14, (j as f64 - 1.5) * 0.4);
    }
    st.add_mono(&drop_sine(n, 90.0 * p, 55.0 * p, 0.05, 0.003, 0.2, 1.0), 0.5, 0.0);
    st
}

fn pop(c: &mut Ctx) -> Stereo {
    let (n, d, p, i) = (c.n, c.d, c.p, c.i);
    let mut st = c.st();
    let f0 = 520.0 * p;
    let tau = 0.2 * d;
    let glide = |t: f64| f0 * (1.0 + 1.7 * decay(t, 0.012));
    let mut body = osc(n, Wave::Sine, glide, 0.0);
    shape(&mut body, |t| smooth(t / 0.0012) * decay(t, tau));
    let mut octave = osc(n, Wave::Sine, |t| glide(t) * 2.0, 0.0);
    shape(&mut octave, |t| smooth(t / 0.0012) * decay(t, tau * 0.5));
    st.add_mono(&body, 1.0, 0.0);
    st.add_mono(&octave, 0.3 * i, 0.0);
    let w = white(n, &mut c.rng.fork(1));
    let mut tick = highpass(&w, 3_000.0);
    shape(&mut tick, |t| smooth(t / 0.0004) * decay(t, 0.0015));
    st.add_mono(&tick, 0.3, 0.0);
    st
}

fn click(c: &mut Ctx) -> Stereo {
    let (n, d, p, i) = (c.n, c.d, c.p, c.i);
    let mut st = c.st();
    let (wl, wr) = c.pair(white);
    for (ch, w) in [wl, wr].iter().enumerate() {
        let side = if ch == 0 { -0.3 } else { 0.3 };
        let fc = (2_400.0 + 2_600.0 * i) * p;
        for (at, level) in [(0.0, 1.0), (0.012, 0.25)] {
            if at + 0.01 > d {
                continue;
            }
            let start = c.sample(at);
            let mut tick = bandpass(&w[start..], fc, 3.0);
            shape(&mut tick, |t| smooth(t / 0.0004) * decay(t, 0.0022));
            st.add_at(start, &tick, 3.0 * level, side);
        }
    }
    let mut tock = osc(n, Wave::Sine, |t| 700.0 * p * (1.0 + 0.5 * decay(t, 0.004)), 0.0);
    shape(&mut tock, |t| smooth(t / 0.0006) * decay(t, 0.006));
    st.add_mono(&tock, 0.4 * (0.4 + 0.6 * i), 0.0);
    st
}

/// A bell strike: inharmonic partials with their own lives over a short noise tick, a twin of
/// every partial a hair off so it shimmers. Mixed into `st` from `start` seconds.
fn bell(c: &mut Ctx, st: &mut Stereo, start: f64, f0: f64, tau: f64, gain: f64, tag: u64) {
    const PARTIALS: [(f64, f64, f64); 6] = [
        (1.0, 1.0, 1.0),
        (2.0, 0.45, 0.7),
        (2.76, 0.35, 0.35),
        (4.07, 0.18, 0.25),
        (5.4, 0.12, 0.15),
        (8.93, 0.06, 0.08),
    ];
    let at = c.sample(start);
    let len = c.n.saturating_sub(at);
    for (j, (ratio, amp, life)) in PARTIALS.iter().enumerate() {
        let f = f0 * ratio;
        if f > SR * 0.4 {
            continue;
        }
        for (twin, drift) in [(0u64, 0.0), (1, 0.35 + 0.1 * j as f64)] {
            let mut rng = c.rng.fork(tag * 16 + j as u64 * 2 + twin);
            let mut s = osc(len, Wave::Sine, |_| f + drift, rng.unit());
            let life = tau * life;
            shape(&mut s, |t| smooth(t / 0.0008) * decay(t, life));
            let level = amp * if twin == 0 { 1.0 } else { 0.5 } * if j == 0 { 1.0 } else { 0.5 + 0.5 * c.i };
            st.add_at(at, &s, gain * level * 0.5, ((j as f64 + twin as f64) % 3.0 - 1.0) * 0.4);
        }
    }
    let w = white(len, &mut c.rng.fork(tag * 16 + 15));
    let mut strike = bandpass(&w, 5_000.0, 1.0);
    shape(&mut strike, |t| smooth(t / 0.0004) * decay(t, 0.002));
    st.add_at(at, &strike, gain * 0.4, 0.0);
}

fn ding(c: &mut Ctx) -> Stereo {
    let (d, p) = (c.d, c.p);
    let mut st = c.st();
    bell(c, &mut st, 0.0, 1_174.66 * p, 0.28 * d, 1.0, 1);
    st
}

fn notify(c: &mut Ctx) -> Stereo {
    let (d, p) = (c.d, c.p);
    let mut st = c.st();
    bell(c, &mut st, 0.0, 1_046.5 * p, 0.14 * d, 0.8, 1);
    bell(c, &mut st, 0.14, 1_568.0 * p, 0.24 * d, 1.0, 2);
    st
}

fn bubble(c: &mut Ctx) -> Stereo {
    let (n, d, p, i) = (c.n, c.d, c.p, c.i);
    let mut st = c.st();
    let count = 1 + (((d - 0.15).max(0.0) / 0.16) * (0.4 + 0.6 * i)).round() as usize;
    let mut rng = c.rng.fork(5);
    let mut at = 0.0;
    for k in 0..count {
        let f0 = rng.between(420.0, 1_100.0) * p;
        let size = rng.between(0.8, 1.2);
        let len = c.sample(0.2).min(n.saturating_sub(c.sample(at)));
        let mut blip = osc(len, Wave::Sine, |t| f0 * (1.0 + 1.4 * (1.0 - decay(t, 0.05 * size))), 0.0);
        shape(&mut blip, |t| smooth(t / 0.002) * decay(t, 0.03 * size));
        st.add_at(
            c.sample(at),
            &blip,
            if k == 0 { 1.0 } else { rng.between(0.5, 0.9) },
            rng.bipolar() * 0.5,
        );
        at += rng.between(0.8, 1.2) * ((d - 0.1) / count as f64).clamp(0.05, 0.2);
    }
    st
}

fn typewriter(c: &mut Ctx) -> Stereo {
    let (n, d, p, i) = (c.n, c.d, c.p, c.i);
    let mut st = c.st();
    let rate = 8.0 + 8.0 * i;
    let mut rng = c.rng.fork(6);
    let noise = white(n, &mut rng);
    let mut t = 0.0;
    let mut key = 0;
    while t < d - 0.03 {
        let space = key % 5 == 4;
        let vel = if key == 0 { 1.0 } else { rng.between(0.55, 1.0) };
        let start = c.sample(t);
        let len = c.sample(0.07).min(n.saturating_sub(start));
        let mut tick = bandpass(&noise[start..start + len], rng.between(2_500.0, 3_600.0) * p, 2.5);
        shape(&mut tick, |t| smooth(t / 0.0004) * decay(t, 0.003));
        let f = if space { 110.0 } else { rng.between(140.0, 190.0) } * p;
        let mut thock = osc(len, Wave::Sine, |t| f * (1.0 + 0.4 * decay(t, 0.006)), 0.0);
        shape(&mut thock, |t| {
            smooth(t / 0.0006) * decay(t, if space { 0.02 } else { 0.014 })
        });
        let pan = rng.bipolar() * 0.3;
        st.add_at(start, &tick, 2.4 * vel, pan);
        st.add_at(start, &thock, (if space { 0.9 } else { 0.55 }) * vel, pan);
        t += rng.between(0.65, 1.5) / rate * if space { 1.3 } else { 1.0 };
        key += 1;
    }
    st
}

fn glitch(c: &mut Ctx) -> Stereo {
    let (n, d, p, i) = (c.n, c.d, c.p, c.i);
    let mut rng = c.rng.fork(7);
    let mut src = c.buf();
    let mut pans = vec![0.0; n];
    let mut at = 0;
    let mut last: (usize, usize) = (0, 0);
    let mut slice = 0;
    while at < n {
        let len = ((rng.between(0.012, 0.055) * SR) as usize).min(n - at).max(1);
        let pan = rng.bipolar() * 0.7;
        let mut kind = rng.unit();
        if slice == 0 {
            kind = 0.0;
        }
        if kind < 0.35 {
            let f = [220.0, 330.0, 440.0, 660.0, 880.0, 1_320.0, 1_760.0, 2_640.0, 3_520.0][(rng.next() % 9) as usize] * p;
            let wave = if rng.unit() < 0.5 { Wave::Square } else { Wave::Saw };
            let tone = osc(len, wave, |t| f * (1.0 + 0.3 * (t / 0.05)), 0.0);
            src[at..at + len].copy_from_slice(&tone);
            last = (at, len);
        } else if kind < 0.6 {
            let w = white(len, &mut rng);
            let burst = lowpass(&w, rng.between(800.0, 9_000.0));
            src[at..at + len].copy_from_slice(&burst);
            last = (at, len);
        } else if kind < 0.75 {
        } else if last.1 > 0 {
            for k in 0..len {
                src[at + k] = src[last.0 + k % last.1];
            }
        }
        pans[at..at + len].fill(pan);
        at += len;
        slice += 1;
    }
    let hold = 1 + (i * rng.between(6.0, 16.0)) as usize;
    let levels = 2f64.powf(7.0 - 3.0 * i);
    let mut held = 0.0;
    for (k, s) in src.iter_mut().enumerate() {
        if k % hold == 0 {
            held = (*s * levels).round() / levels;
        }
        *s = held;
    }
    shape(&mut src, |t| smooth(t / 0.0005) * (1.0 - smooth((t - 0.7 * d) / (0.3 * d))));
    let mut st = c.st();
    for k in 0..n {
        let (gl, gr) = pan_gains(pans[k]);
        st.l[k] = src[k] * gl;
        st.r[k] = src[k] * gr;
    }
    st
}

fn tape_stop(c: &mut Ctx) -> Stereo {
    let (d, p, i) = (c.d, c.p, c.i);
    let stop = 0.9 * d;
    let speed = |t: f64| {
        if t >= stop {
            0.0
        } else {
            let x = t / stop;
            (1.0 - x).powf(1.6) * (1.0 + 0.012 * (TAU * 5.5 * t).sin() * x)
        }
    };
    let notes = [110.0, 138.6, 164.8, 220.0, 277.2];
    let mut bed = c.buf();
    let mut phases: Vec<f64> = (0..notes.len()).map(|j| c.rng.fork(40 + j as u64).unit()).collect();
    for (k, s) in bed.iter_mut().enumerate() {
        let v = speed(k as f64 / SR);
        for (j, f) in notes.iter().enumerate() {
            let detune = 1.0 + 0.003 * (j as f64 - 2.0);
            let dt = (f * p * detune * v / SR).clamp(0.0, 0.45);
            *s += wave_at(Wave::Saw, phases[j], dt) * 0.2;
            phases[j] = (phases[j] + dt).fract();
        }
    }
    let bright = 0.5 + 0.5 * i;
    let mut bed = filt(&bed, Mode::Low, |t| (300.0 + 5_200.0 * speed(t).powf(1.2)) * bright, 1.0);
    let (nl, nr) = c.pair(pink);
    let mut st = c.st();
    for (ch, noise) in [nl, nr].iter().enumerate() {
        let mut hiss = filt(noise, Mode::Low, |t| 200.0 + 4_000.0 * speed(t), 0.8);
        shape(&mut hiss, |t| 0.3 * speed(t).sqrt());
        st.add_mono(&hiss, 1.0, if ch == 0 { -0.7 } else { 0.7 });
    }
    shape(&mut bed, |t| speed(t).powf(0.35));
    st.add_mono(&bed, 1.0, 0.0);
    st
}

fn record_scratch(c: &mut Ctx) -> Stereo {
    let (n, d, p, i) = (c.n, c.d, c.p, c.i);
    let mut st = c.st();
    let loop_len = c.sample(0.35).max(2);
    let mut rng = c.rng.fork(8);
    let mut src = vec![0.0; loop_len];
    for f in [196.0, 294.0, 392.0, 587.0] {
        let saw = osc(loop_len, Wave::Saw, |_| f * p * 0.5, rng.unit());
        src.iter_mut().zip(&saw).for_each(|(s, v)| *s += v * 0.25);
    }
    let src = lowpass(&src, 3_200.0);
    let grit = white(loop_len, &mut rng);
    let src: Buf = src.iter().zip(&grit).map(|(s, g)| s + g * 0.15).collect();
    let keys: [(f64, f64); 9] = [
        (0.0, 1.4),
        (0.08, -2.4),
        (0.18, 2.6),
        (0.3, -2.0),
        (0.42, 1.6),
        (0.56, -1.0),
        (0.7, 0.35),
        (0.85, 0.0),
        (1.0, 0.0),
    ];
    let jitter: Vec<f64> = keys.iter().map(|_| rng.between(0.9, 1.1)).collect();
    let velocity = |t: f64| {
        let x = (t / d).clamp(0.0, 1.0);
        let k = keys.iter().rposition(|(kt, _)| *kt <= x).unwrap_or(0).min(keys.len() - 2);
        let local = smooth((x - keys[k].0) / (keys[k + 1].0 - keys[k].0));
        lerp(keys[k].1 * jitter[k], keys[k + 1].1 * jitter[k + 1], local) * (0.8 + 0.4 * i)
    };
    let mut pos = 0.0;
    let mut out = c.buf();
    let hiss_src = white(n, &mut rng);
    for (k, o) in out.iter_mut().enumerate() {
        let t = k as f64 / SR;
        let v = velocity(t);
        pos += v;
        let at = pos.rem_euclid(loop_len as f64);
        let (a, frac) = (at as usize % loop_len, at.fract());
        let b = (a + 1) % loop_len;
        let speed = v.abs().min(1.0);
        *o = (src[a] * (1.0 - frac) + src[b] * frac) * speed.powf(0.6) + hiss_src[k] * 0.12 * speed;
    }
    let mut out = filt(&out, Mode::Low, |t| 800.0 + 3_500.0 * velocity(t).abs().min(1.0), 1.2);
    shape(&mut out, |t| smooth(t / 0.001) * (1.0 - smooth((t - 0.8 * d) / (0.2 * d))));
    st.add_mono(&out, 1.0, 0.0);
    st
}

fn shutter(c: &mut Ctx) -> Stereo {
    let (n, d, p, _) = (c.n, c.d, c.p, c.i);
    let mut st = c.st();
    let mut rng = c.rng.fork(9);
    let noise = white(n, &mut rng);
    let gap = (0.4 * d).min(0.07);
    for (at, level, low) in [(0.0, 1.0, 420.0), (gap, 0.75, 300.0)] {
        let start = c.sample(at);
        let len = c.sample(0.06).min(n.saturating_sub(start));
        let mut tick = bandpass(&noise[start..start + len], 3_800.0 * p, 2.0);
        shape(&mut tick, |t| smooth(t / 0.0004) * decay(t, 0.0025));
        let mut thunk = osc(len, Wave::Sine, |t| low * p * (1.0 + 0.5 * decay(t, 0.004)), 0.0);
        shape(&mut thunk, |t| smooth(t / 0.0005) * decay(t, 0.009));
        let mut body = lowpass(&noise[start..start + len], 900.0);
        shape(&mut body, |t| decay(t, 0.012));
        st.add_at(start, &tick, 2.6 * level, -0.15);
        st.add_at(start, &thunk, 0.9 * level, 0.0);
        st.add_at(start, &body, 0.7 * level, 0.15);
    }
    let from = c.sample(0.012);
    let to = c.sample(gap).max(from);
    let mut rattle = highpass(&noise[from..to], 1_200.0);
    shape(&mut rattle, |t| 0.5 * smooth(t / 0.01));
    st.add_at(from, &rattle, 0.15, 0.0);
    st
}

fn heartbeat(c: &mut Ctx) -> Stereo {
    let (n, d, p, i) = (c.n, c.d, c.p, c.i);
    let mut st = c.st();
    let mut rng = c.rng.fork(10);
    let noise = white(n, &mut rng);
    let period = 0.86;
    let beats = 1 + ((d - 0.8).max(0.0) / period) as usize;
    let drive = 0.8 + 1.2 * i;
    for b in 0..beats {
        let base = b as f64 * period + hit_s(c.kind);
        for (at, level, from, to, tau) in [(0.0, 1.0, 82.0, 56.0, 0.07), (0.26, 0.7, 92.0, 62.0, 0.055)] {
            let start = c.sample(base + at - if at == 0.0 { hit_s(c.kind) } else { 0.0 });
            let len = c.sample(0.4).min(n.saturating_sub(start));
            if len == 0 {
                continue;
            }
            let thump = drop_sine(len, from * p, to * p, 0.03, 0.005, tau, drive);
            st.add_at(start, &thump, level, 0.0);
            let mut body = lowpass(&noise[start..start + len], 160.0);
            shape(&mut body, |t| smooth(t / 0.004) * decay(t, 0.035));
            st.add_at(start, &body, 0.5 * level, 0.0);
        }
    }
    st
}

// ---- rendering -----------------------------------------------------------------------------

fn sanitize(spec: &SfxSpec) -> SfxSpec {
    let clean = |x: f64, lo: f64, hi: f64, default: f64| if x.is_finite() { x.clamp(lo, hi) } else { default };
    SfxSpec {
        pitch: clean(spec.pitch, -MAX_SFX_PITCH, MAX_SFX_PITCH, 0.0),
        intensity: clean(spec.intensity, 0.0, 1.0, 0.7),
        tail: clean(spec.tail, 0.0, 1.0, 0.3),
        seed: spec.seed.min(crate::model::MAX_SEED),
        ..spec.resolved()
    }
}

/// The part of the render's end that fades out, seconds.
fn end_fade_s(kind: SfxKind, d: f64) -> f64 {
    match kind {
        SfxKind::Riser | SfxKind::Downlifter => 0.006,
        _ => (0.08 * d).clamp(0.01, 0.12),
    }
}

fn fade_edges(st: &mut Stereo, fade_in: f64, fade_out: f64) {
    let n = st.l.len();
    let fi = ((fade_in * SR) as usize).min(n / 4);
    let fo = ((fade_out * SR) as usize).min(n / 3);
    for ch in [&mut st.l, &mut st.r] {
        for (k, s) in ch.iter_mut().take(fi).enumerate() {
            *s *= 0.5 - 0.5 * (PI * k as f64 / fi as f64).cos();
        }
        for (k, s) in ch[n - fo..].iter_mut().enumerate() {
            *s *= 0.5 + 0.5 * (PI * k as f64 / fo as f64).cos();
        }
    }
}

/// The soft knee of the final saturation.
const SATURATION: f64 = 1.0;

/// Make the render ready to be a file: scrub non-finite samples, scale to unity peak, saturate
/// gently (the body comes up under the transient), scale to exactly [`PEAK_DBFS`] + `level_db`.
fn master(st: &mut Stereo, level_db: f64) {
    let mut peak = 0.0_f64;
    for s in st.l.iter_mut().chain(st.r.iter_mut()) {
        if !s.is_finite() {
            *s = 0.0;
        }
        peak = peak.max(s.abs());
    }
    if peak < 1e-12 {
        return;
    }
    let norm = SATURATION.tanh();
    let mut shaped = 0.0_f64;
    for s in st.l.iter_mut().chain(st.r.iter_mut()) {
        *s = (SATURATION * *s / peak).tanh() / norm;
        shaped = shaped.max(s.abs());
    }
    let target = 10f64.powf((PEAK_DBFS + level_db.min(0.0)) / 20.0);
    let scale = target / shaped.max(1e-12);
    for s in st.l.iter_mut().chain(st.r.iter_mut()) {
        *s *= scale;
    }
}

/// Render `spec`: 48 kHz stereo, [`SfxSpec::resolved_duration_s`] long, deterministic for the spec and
/// [`RENDERER_VERSION`]. Pure; out-of-range numbers are clamped (validate first to refuse them).
pub fn render(spec: &SfxSpec) -> Vec<[f32; 2]> {
    let spec = sanitize(spec);
    let kind = spec.kind;
    let d = spec.resolved_duration_s();
    let n = (d * SR).round() as usize;
    let index = SfxKind::ALL.iter().position(|k| *k == kind).unwrap_or(0) as u64;
    let mut c = Ctx {
        kind,
        n,
        d,
        p: 2f64.powf(spec.pitch / 12.0),
        i: spec.intensity,
        rng: Rng::new(spec.seed, index + 1),
    };
    let mut st = match kind {
        SfxKind::Whoosh => whoosh(&mut c),
        SfxKind::Swoosh => swoosh(&mut c),
        SfxKind::Swipe => swipe(&mut c),
        SfxKind::Impact => impact(&mut c),
        SfxKind::Boom => boom(&mut c),
        SfxKind::Thud => thud(&mut c),
        SfxKind::Punch => punch(&mut c),
        SfxKind::Riser => build(&mut c, true),
        SfxKind::Downlifter => build(&mut c, false),
        SfxKind::ReverseSwell => crash(&mut c),
        SfxKind::Pop => pop(&mut c),
        SfxKind::Click => click(&mut c),
        SfxKind::Ding => ding(&mut c),
        SfxKind::Notify => notify(&mut c),
        SfxKind::Bubble => bubble(&mut c),
        SfxKind::Typewriter => typewriter(&mut c),
        SfxKind::Glitch => glitch(&mut c),
        SfxKind::TapeStop => tape_stop(&mut c),
        SfxKind::RecordScratch => record_scratch(&mut c),
        SfxKind::Shutter => shutter(&mut c),
        SfxKind::Heartbeat => heartbeat(&mut c),
    };
    st.l = lowpass(&st.l, 14_000.0);
    st.r = lowpass(&st.r, 14_000.0);
    let room = space(kind);
    let tail = if kind == SfxKind::ReverseSwell {
        0.4 + 0.6 * spec.tail
    } else {
        spec.tail
    };
    if tail > 0.0 {
        let wet = reverb(&st, room.hp, lerp(room.rt.0, room.rt.1, tail), room.damp);
        let shift = c.sample(room.pre);
        let gain = room.wet * tail;
        for k in shift..n {
            st.l[k] += wet.l[k - shift] * gain;
            st.r[k] += wet.r[k - shift] * gain;
        }
    }
    fade_edges(&mut st, 0.0005, end_fade_s(kind, d));
    if kind == SfxKind::ReverseSwell {
        st.reverse();
    }
    if spec.reverse {
        st.reverse();
    }
    master(&mut st, kind.preset().level_db);
    st.l.iter().zip(&st.r).map(|(l, r)| [*l as f32, *r as f32]).collect()
}

// ---- writing -------------------------------------------------------------------------------

static PARTS: AtomicU64 = AtomicU64::new(0);

/// Render `spec` to a WAV in `dir` (see [`output_path_in`]) unless it is there already, and say
/// where. Checks the spec first and `cancel` before the work and the write; writes through
/// `.part` and a rename, so a cancel or a crash leaves no half file.
pub fn synthesize_in(dir: &Path, spec: &SfxSpec, cancel: &dyn Fn() -> bool) -> Result<PathBuf> {
    spec.validate()?;
    let path = output_path_in(dir, spec)?;
    if finished(&path) {
        return Ok(path);
    }
    if cancel() {
        return Err(Error::Cancelled);
    }
    let frames = render(spec);
    if cancel() {
        return Err(Error::Cancelled);
    }
    let (left, right): (Vec<f32>, Vec<f32>) = frames.iter().map(|f| (f[0], f[1])).unzip();
    std::fs::create_dir_all(dir).map_err(|e| Error::Engine(format!("could not create the sound effects folder: {e}")))?;
    let tmp = path.with_extension(format!(
        "{}.{}.part",
        std::process::id(),
        PARTS.fetch_add(1, Ordering::Relaxed)
    ));
    let written = std::fs::File::create(&tmp)
        .and_then(|f| {
            let mut w = BufWriter::new(f);
            write_wav16(&mut w, &left, &right, SAMPLE_RATE)?;
            w.flush()
        })
        .and_then(|_| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Engine(format!("could not write the sound effect: {e}")));
    }
    Ok(path)
}

/// Render `spec` to a WAV in the data directory (`<data>/kerf/sfx/`) unless it is there already.
pub fn synthesize(spec: &SfxSpec, cancel: &dyn Fn() -> bool) -> Result<PathBuf> {
    let dir = output_dir().ok_or_else(|| Error::Engine("no data directory for sound effects".into()))?;
    synthesize_in(&dir, spec, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MAX_SEED;

    fn spec(kind: SfxKind) -> SfxSpec {
        SfxSpec::new(kind)
    }

    fn mono(frames: &[[f32; 2]]) -> Vec<f64> {
        frames.iter().map(|f| (f[0] as f64 + f[1] as f64) * 0.5).collect()
    }

    fn peak_of(frames: &[[f32; 2]]) -> f64 {
        frames.iter().flatten().fold(0.0_f64, |m, s| m.max(s.abs() as f64))
    }

    /// The RMS of `win`-second windows, one per window.
    fn envelope(frames: &[[f32; 2]], win: f64) -> Vec<f64> {
        let x = mono(frames);
        let w = (win * SR) as usize;
        x.chunks(w).map(rms).collect()
    }

    fn loudest_window_s(frames: &[[f32; 2]], win: f64) -> f64 {
        let env = envelope(frames, win);
        let k = env.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        (k as f64 + 0.5) * win
    }

    /// The energy of `x` at `hz` (Goertzel), as a fraction of its total.
    fn at_hz(x: &[f64], hz: f64) -> f64 {
        let w = TAU * hz / SR;
        let (mut re, mut im) = (0.0, 0.0);
        for (k, s) in x.iter().enumerate() {
            re += s * (w * k as f64).cos();
            im += s * (w * k as f64).sin();
        }
        (re * re + im * im) / (x.len() as f64 * x.iter().map(|s| s * s).sum::<f64>().max(1e-12))
    }

    /// How bright a sound is: the share of its energy in the first difference (high frequencies
    /// weigh up to four times).
    fn brightness(frames: &[[f32; 2]]) -> f64 {
        let x = mono(frames);
        let diff: f64 = x.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
        diff / x.iter().map(|s| s * s).sum::<f64>().max(1e-12)
    }

    fn frames_of(d: f64) -> usize {
        (d * SR).round() as usize
    }

    #[test]
    fn every_kind_renders_its_length_audibly_finite_and_at_its_level() {
        for kind in SfxKind::ALL {
            let s = spec(kind);
            let frames = render(&s);
            let d = s.resolved_duration_s();
            assert!(
                frames.len().abs_diff(frames_of(d)) <= 1,
                "{kind:?}: {} frames for {d} s",
                frames.len()
            );
            assert!(frames.iter().flatten().all(|x| x.is_finite()), "{kind:?}: finite");
            let peak = peak_of(&frames);
            let want = 10f64.powf((PEAK_DBFS + kind.preset().level_db) / 20.0);
            assert!((peak - want).abs() < want * 0.01, "{kind:?}: peak {peak:.4} vs {want:.4}");
            assert!(peak <= 10f64.powf(PEAK_DBFS / 20.0) + 1e-6, "{kind:?}: at most -1 dBFS");
            let x = mono(&frames);
            assert!(rms(&x) > 0.004, "{kind:?}: not silent ({})", rms(&x));
            let mean = x.iter().sum::<f64>() / x.len() as f64;
            assert!(mean.abs() < 0.03 * peak, "{kind:?}: no DC offset ({mean})");
            // Edges are click-free in both directions.
            for reverse in [false, true] {
                let r = render(&SfxSpec { reverse, ..s.clone() });
                let (first, last) = (
                    r[0][0].abs().max(r[0][1].abs()),
                    r[r.len() - 1][0].abs().max(r[r.len() - 1][1].abs()),
                );
                assert!(
                    first < 0.08 * peak as f32 && last < 0.08 * peak as f32,
                    "{kind:?} reverse {reverse}: {first} {last}"
                );
            }
        }
    }

    #[test]
    fn a_spec_is_the_same_audio_and_another_seed_is_another_sound() {
        for kind in SfxKind::ALL {
            let s = spec(kind);
            let a = render(&s);
            assert_eq!(a, render(&s), "{kind:?}: deterministic");
            let other = SfxSpec { seed: 1, ..s.clone() };
            assert_ne!(a, render(&other), "{kind:?}: a seed changes the sound");
            let spelled = SfxSpec {
                duration_s: Some(s.resolved_duration_s()),
                ..s.clone()
            };
            assert_eq!(
                a,
                render(&spelled),
                "{kind:?}: the default length spelled out is the same render"
            );
        }
        // The largest seed works and is not the same as its neighbour.
        let hi = SfxSpec {
            seed: MAX_SEED,
            ..spec(SfxKind::Whoosh)
        };
        assert_ne!(
            render(&hi),
            render(&SfxSpec {
                seed: MAX_SEED - 1,
                ..hi.clone()
            })
        );
    }

    #[test]
    fn reverse_is_the_render_played_backwards() {
        for kind in SfxKind::ALL {
            let fwd = render(&spec(kind));
            let mut back = render(&SfxSpec {
                reverse: true,
                ..spec(kind)
            });
            back.reverse();
            assert_eq!(fwd, back, "{kind:?}");
        }
    }

    #[test]
    fn extreme_numbers_stay_finite_and_inside_the_level() {
        for kind in SfxKind::ALL {
            let preset = kind.preset();
            let want = 10f64.powf((PEAK_DBFS + preset.level_db) / 20.0);
            for (pitch, intensity, tail, duration) in [
                (-12.0, 0.0, 0.0, preset.min_duration_s),
                (12.0, 1.0, 1.0, preset.min_duration_s),
                (12.0, 1.0, 1.0, preset.default_duration_s),
                (-12.0, 1.0, 1.0, preset.default_duration_s),
                (0.0, 0.0, 1.0, preset.default_duration_s),
                (12.0, 0.0, 0.0, preset.default_duration_s),
            ] {
                let s = SfxSpec {
                    pitch,
                    intensity,
                    tail,
                    duration_s: Some(duration),
                    seed: MAX_SEED,
                    ..spec(kind)
                };
                let frames = render(&s);
                assert_eq!(frames.len(), frames_of(duration), "{kind:?} {s:?}");
                assert!(frames.iter().flatten().all(|x| x.is_finite()), "{kind:?} {s:?}");
                let peak = peak_of(&frames);
                assert!((peak - want).abs() < want * 0.01, "{kind:?} {s:?}: peak {peak}");
            }
        }
        // Numbers that validation would refuse are clamped, not trusted.
        let wild = SfxSpec {
            pitch: f64::NAN,
            intensity: 40.0,
            tail: f64::INFINITY,
            duration_s: Some(f64::NAN),
            seed: u64::MAX,
            ..spec(SfxKind::Impact)
        };
        let frames = render(&wild);
        assert_eq!(frames.len(), frames_of(1.6));
        assert!(frames.iter().flatten().all(|x| x.is_finite()));
    }

    #[test]
    fn the_longest_renders_stay_inside_their_bounds() {
        for kind in [SfxKind::Riser, SfxKind::Boom, SfxKind::Typewriter, SfxKind::Glitch] {
            let s = SfxSpec {
                duration_s: Some(kind.preset().max_duration_s),
                ..spec(kind)
            };
            let frames = render(&s);
            assert_eq!(frames.len(), frames_of(kind.preset().max_duration_s), "{kind:?}");
            assert!(frames.iter().flatten().all(|x| x.is_finite() && x.abs() <= 1.0), "{kind:?}");
        }
    }

    #[test]
    fn the_hit_lands_where_the_catalogue_says() {
        for kind in SfxKind::ALL {
            let s = spec(kind);
            let d = s.resolved_duration_s();
            let frames = render(&s);
            let anchor = crate::model::sfx_anchor_s(&s, d);
            match kind.preset().anchor {
                SfxAnchor::Peak { .. } => {
                    let at = loudest_window_s(&frames, 0.02);
                    assert!(
                        (at - anchor).abs() < 0.15 * d,
                        "{kind:?}: loudest at {at:.3}, anchor {anchor:.3}"
                    );
                }
                SfxAnchor::End => {
                    let at = loudest_window_s(&frames, 0.02);
                    assert!(at > d - 0.12 * d, "{kind:?}: loudest at {at:.3} of {d}");
                    let r = render(&SfxSpec {
                        reverse: true,
                        ..s.clone()
                    });
                    assert!(loudest_window_s(&r, 0.02) < 0.12 * d, "{kind:?} reversed starts on the hit");
                }
                SfxAnchor::Hit { seconds } => {
                    // The sound is under way within a few ms of the hit point ...
                    let x = mono(&frames);
                    let peak = peak_of(&frames);
                    let onset = x.iter().position(|v| v.abs() > 0.1 * peak).unwrap() as f64 / SR;
                    assert!(onset < seconds + 0.03, "{kind:?}: onset {onset:.4} s, hit {seconds}");
                    // ... and most of these are loudest at it.
                    if !matches!(
                        kind,
                        SfxKind::Bubble
                            | SfxKind::Typewriter
                            | SfxKind::Glitch
                            | SfxKind::Notify
                            | SfxKind::Heartbeat
                            | SfxKind::TapeStop
                            | SfxKind::RecordScratch
                            | SfxKind::Downlifter
                    ) {
                        let at = x.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0 as f64 / SR;
                        assert!(at < seconds + 0.05, "{kind:?}: loudest sample at {at:.4}");
                    }
                }
            }
        }
        let downlift = render(&spec(SfxKind::Downlifter));
        assert!(
            loudest_window_s(&downlift, 0.02) < 0.25,
            "a downlifter starts on its hit and falls away"
        );
    }

    #[test]
    fn a_riser_builds_all_the_way_and_a_downlifter_falls_all_the_way() {
        let riser = envelope(&render(&spec(SfxKind::Riser)), 0.25);
        assert!(
            riser.windows(2).filter(|w| w[1] >= w[0]).count() >= riser.len() - 3,
            "{riser:?}"
        );
        assert!(riser[riser.len() - 2] > 6.0 * riser[0], "{riser:?}");
        let down = envelope(&render(&spec(SfxKind::Downlifter)), 0.25);
        assert!(down[0] > 6.0 * down[down.len() - 2], "{down:?}");
        // A tape stop slows to nothing.
        let stop = envelope(&render(&spec(SfxKind::TapeStop)), 0.1);
        assert!(stop[0] > 10.0 * stop[stop.len() - 1], "{stop:?}");
    }

    #[test]
    fn intensity_brightens_and_pitch_moves_the_tone() {
        for kind in [
            SfxKind::Whoosh,
            SfxKind::Swoosh,
            SfxKind::Riser,
            SfxKind::Impact,
            SfxKind::Click,
        ] {
            let soft = brightness(&render(&SfxSpec {
                intensity: 0.0,
                ..spec(kind)
            }));
            let hard = brightness(&render(&SfxSpec {
                intensity: 1.0,
                ..spec(kind)
            }));
            assert!(hard > soft * 1.15, "{kind:?}: {soft:.4} -> {hard:.4}");
        }
        // A bell an octave up has its fundamental an octave up.
        let ding = |pitch| {
            mono(&render(&SfxSpec {
                pitch,
                duration_s: Some(0.4),
                tail: 0.0,
                ..spec(SfxKind::Ding)
            }))
        };
        let (low, high) = (ding(0.0), ding(12.0));
        assert!(
            at_hz(&low, 1_174.66) > 3.0 * at_hz(&low, 2_349.3),
            "the unshifted bell rings at D6"
        );
        assert!(
            at_hz(&high, 2_349.3) > 20.0 * at_hz(&high, 1_174.66),
            "+12 semitones rings at D7"
        );
        let pitched = |pitch| {
            mono(&render(&SfxSpec {
                pitch,
                tail: 0.0,
                ..spec(SfxKind::Boom)
            }))
        };
        assert!(
            brightness(&render(&SfxSpec {
                pitch: 12.0,
                tail: 0.0,
                ..spec(SfxKind::Boom)
            })) > brightness(&render(&SfxSpec {
                pitch: -12.0,
                tail: 0.0,
                ..spec(SfxKind::Boom)
            }))
        );
        assert!(rms(&pitched(0.0)) > 0.0);
    }

    #[test]
    fn the_tail_is_a_room_that_keeps_ringing() {
        // What rings in the back half of the render, above the sub that carries a hit on its own,
        // compared with the whole.
        for kind in [SfxKind::Impact, SfxKind::Boom, SfxKind::Thud, SfxKind::Punch, SfxKind::Pop] {
            let ring = |tail| {
                let frames = render(&SfxSpec { tail, ..spec(kind) });
                let x = highpass(&mono(&frames), 300.0);
                let n = x.len();
                rms(&x[n * 3 / 5..n * 9 / 10]) / rms(&x)
            };
            assert!(ring(1.0) > 1.5 * ring(0.0), "{kind:?}: {} vs {}", ring(0.0), ring(1.0));
        }
    }

    #[test]
    fn a_pass_travels_from_left_to_right() {
        for kind in [SfxKind::Whoosh, SfxKind::Swoosh, SfxKind::Swipe] {
            let frames = render(&SfxSpec { tail: 0.0, ..spec(kind) });
            let n = frames.len();
            let side = |range: std::ops::Range<usize>, ch: usize| {
                (frames[range.clone()].iter().map(|f| (f[ch] as f64).powi(2)).sum::<f64>() / range.len() as f64).sqrt()
            };
            let early = side(n / 10..n * 3 / 10, 0) / side(n / 10..n * 3 / 10, 1);
            let late = side(n * 7 / 10..n * 9 / 10, 0) / side(n * 7 / 10..n * 9 / 10, 1);
            assert!(early > 1.4 && late < 0.7, "{kind:?}: left/right {early:.2} then {late:.2}");
        }
    }

    /// How many times `x` changes sign over its `secs` seconds (about twice the pitch for a tone).
    fn crossings(x: &[f64]) -> f64 {
        x.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count() as f64 / (x.len() as f64 / SR)
    }

    #[test]
    fn the_sweeps_climb_and_fall_the_way_their_names_say() {
        let thirds = |frames: &[[f32; 2]]| {
            let n = frames.len();
            [
                brightness(&frames[..n / 3]),
                brightness(&frames[n / 3..n * 2 / 3]),
                brightness(&frames[n * 2 / 3..]),
            ]
        };
        // A pass is brightest as it goes by, in the middle.
        for kind in [SfxKind::Whoosh, SfxKind::Swoosh, SfxKind::Swipe] {
            let [a, b, c] = thirds(&render(&SfxSpec { tail: 0.0, ..spec(kind) }));
            // (A swipe is bright from the start; it only closes at the end.)
            let rise = if kind == SfxKind::Swipe { 1.0 } else { 1.3 };
            assert!(b > a * rise && b > c * 1.3, "{kind:?}: {a:.4} {b:.4} {c:.4}");
        }
        // A riser opens, a downlifter closes.
        let [a, _, c] = thirds(&render(&spec(SfxKind::Riser)));
        assert!(c > a * 2.0, "riser {a:.4} -> {c:.4}");
        let [a, _, c] = thirds(&render(&spec(SfxKind::Downlifter)));
        assert!(a > c * 2.0, "downlifter {a:.4} -> {c:.4}");
        // A tape slows down: the pitch of what is left falls.
        let stop = mono(&render(&SfxSpec {
            tail: 0.0,
            intensity: 0.0,
            ..spec(SfxKind::TapeStop)
        }));
        let n = (0.9 * 1.2 * SR) as usize;
        assert!(
            crossings(&stop[..n / 4]) > 1.6 * crossings(&stop[n / 2..n * 3 / 4]),
            "tape stop"
        );
        // A pop and a bubble: down, and up.
        let pop = mono(&render(&SfxSpec {
            tail: 0.0,
            ..spec(SfxKind::Pop)
        }));
        let at = |x: &[f64], a: f64, b: f64| crossings(&x[(a * SR) as usize..(b * SR) as usize]);
        assert!(at(&pop, 0.003, 0.012) > 1.3 * at(&pop, 0.05, 0.09), "a pop falls in pitch");
        let bubble = mono(&render(&SfxSpec {
            tail: 0.0,
            ..spec(SfxKind::Bubble)
        }));
        assert!(
            at(&bubble, 0.05, 0.1) > 1.2 * at(&bubble, 0.004, 0.02),
            "a bubble rises in pitch"
        );
    }

    #[test]
    fn a_heartbeat_is_lub_dub_and_a_long_one_beats_again() {
        // The envelope's peaks, 20 ms windows apart by at least 120 ms.
        let peaks = |frames: &[[f32; 2]]| {
            let env = envelope(frames, 0.02);
            let top = env.iter().copied().fold(0.0, f64::max);
            let mut found: Vec<f64> = Vec::new();
            let at = |k: isize| env.get(k as usize).copied().unwrap_or(0.0);
            for k in 0..env.len() as isize {
                let t = k as f64 * 0.02;
                if at(k) > 0.35 * top && at(k) >= at(k - 1) && at(k) > at(k + 1) && found.last().is_none_or(|l| t - l > 0.12) {
                    found.push(t);
                }
            }
            found
        };
        let one = peaks(&render(&SfxSpec {
            tail: 0.0,
            ..spec(SfxKind::Heartbeat)
        }));
        assert_eq!(one.len(), 2, "{one:?}");
        assert!((one[1] - one[0] - 0.26).abs() < 0.05, "{one:?}");
        let long = peaks(&render(&SfxSpec {
            tail: 0.0,
            duration_s: Some(2.4),
            ..spec(SfxKind::Heartbeat)
        }));
        assert_eq!(long.len(), 4, "{long:?}");
        assert!((long[2] - long[0] - 0.86).abs() < 0.06, "{long:?}");
    }

    #[test]
    fn typewriter_keys_come_at_a_typists_pace() {
        let x = mono(&render(&SfxSpec {
            tail: 0.0,
            ..spec(SfxKind::Typewriter)
        }));
        // Each key is a click: count the 10 ms windows that jump well above the one before.
        let w = (0.005 * SR) as usize;
        let env: Vec<f64> = x.chunks(w).map(rms).collect();
        let top = env.iter().copied().fold(0.0, f64::max);
        let keys = (1..env.len())
            .filter(|&k| env[k] > 0.25 * top && env[k - 1] < 0.5 * env[k])
            .count();
        assert!((7..=17).contains(&keys), "{keys} keys in a second");
        let slow = mono(&render(&SfxSpec {
            tail: 0.0,
            intensity: 0.0,
            ..spec(SfxKind::Typewriter)
        }));
        let fast = mono(&render(&SfxSpec {
            tail: 0.0,
            intensity: 1.0,
            ..spec(SfxKind::Typewriter)
        }));
        let count = |x: &[f64]| {
            let env: Vec<f64> = x.chunks(w).map(rms).collect();
            let top = env.iter().copied().fold(0.0, f64::max);
            (1..env.len())
                .filter(|&k| env[k] > 0.25 * top && env[k - 1] < 0.5 * env[k])
                .count()
        };
        assert!(count(&fast) > count(&slow) + 3, "{} vs {}", count(&slow), count(&fast));
    }

    #[test]
    fn the_reverb_returns_about_what_it_is_given() {
        let mut rng = Rng::new(1, 1);
        let n = 2 * SAMPLE_RATE as usize;
        let mut dry = Stereo::new(n);
        dry.l = white(n, &mut rng);
        dry.r = white(n, &mut rng);
        for rt in [0.2, 1.0, 3.0] {
            let wet = reverb(&dry, 0.0, rt, 0.3);
            let ratio = rms(&wet.l[n / 2..]) / rms(&dry.l);
            assert!((0.6..1.8).contains(&ratio), "rt60 {rt}: wet/dry {ratio:.2}");
        }
        // Silence in, silence out; the room rings out after a click.
        let mut click = Stereo::new(n);
        click.l[0] = 1.0;
        click.r[0] = 1.0;
        let wet = reverb(&click, 0.0, 0.5, 0.3);
        assert!(rms(&wet.l[SAMPLE_RATE as usize / 10..SAMPLE_RATE as usize / 2]) > rms(&wet.l[n - 10_000..]) * 20.0);
    }

    #[test]
    fn the_filter_is_stable_under_a_cutoff_that_moves_every_sample() {
        let mut rng = Rng::new(3, 3);
        let x = white(SAMPLE_RATE as usize, &mut rng);
        for q in [0.5, 4.0, 40.0] {
            let y = filt(&x, Mode::Band, |t| 40.0 * (20_000.0f64 / 40.0).powf((t * 7.0).fract()), q);
            assert!(y.iter().all(|v| v.is_finite() && v.abs() < 200.0), "q {q}");
        }
        let y = lowpass(&x, 1_000.0);
        assert!(rms(&y) < rms(&x) * 0.7);
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-sfx-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_wav_is_written_once_through_a_part_and_found_again() {
        let dir = scratch();
        let s = SfxSpec {
            duration_s: Some(0.5),
            ..spec(SfxKind::Thud)
        };
        let path = synthesize_in(&dir, &s, &|| false).unwrap();
        assert_eq!(path, output_path_in(&dir, &s).unwrap());
        let bytes = std::fs::read(&path).unwrap();
        let frames = frames_of(0.5);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 2, "stereo");
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), SAMPLE_RATE);
        assert_eq!(u16::from_le_bytes([bytes[34], bytes[35]]), 16, "16-bit");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize, frames * 4);
        assert_eq!(bytes.len(), 44 + frames * 4);
        // The samples are the render, to a least significant bit and a dither.
        let rendered = render(&s);
        let sample = |i: usize| i16::from_le_bytes([bytes[44 + 4 * i], bytes[45 + 4 * i]]) as f32 / 32767.0;
        for i in (0..frames).step_by(997) {
            assert!((sample(i) - rendered[i][0]).abs() < 3.0 / 32767.0, "frame {i}");
        }
        // The same spec is a cache hit (the file is not touched), spelled any way.
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let again = synthesize_in(&dir, &s.resolved(), &|| false).unwrap();
        assert_eq!(again, path);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no part file is left");
        // Another seed is another file.
        let other = synthesize_in(&dir, &SfxSpec { seed: 5, ..s }, &|| false).unwrap();
        assert_ne!(other, path);
        // The same spec in another folder is the same bytes.
        let dir2 = scratch();
        let twin = synthesize_in(&dir2, &s, &|| false).unwrap();
        assert_eq!(std::fs::read(twin).unwrap(), bytes);
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&dir2).ok();
    }

    #[test]
    fn the_name_is_the_resolved_spec_and_the_renderer_version() {
        let dir = Path::new("/x");
        let name = |s: &SfxSpec| output_path_in(dir, s).unwrap();
        let base = spec(SfxKind::Whoosh);
        assert_eq!(
            name(&base),
            name(&SfxSpec {
                duration_s: Some(1.0),
                ..base.clone()
            })
        );
        assert_eq!(
            name(&base),
            name(&SfxSpec {
                duration_s: Some(1.0004),
                ..base.clone()
            }),
            "to the millisecond"
        );
        assert_ne!(
            name(&base),
            name(&SfxSpec {
                duration_s: Some(1.5),
                ..base.clone()
            })
        );
        assert_ne!(
            name(&base),
            name(&SfxSpec {
                pitch: 1.0,
                ..base.clone()
            })
        );
        assert_ne!(
            name(&base),
            name(&SfxSpec {
                reverse: true,
                ..base.clone()
            })
        );
        assert_ne!(name(&base), name(&spec(SfxKind::Swoosh)));
        let file = name(&base);
        let file = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            file.starts_with("sfx-") && file.ends_with(".wav") && file.len() == "sfx-".len() + 16 + ".wav".len(),
            "{file}"
        );
    }

    #[test]
    fn a_bad_spec_or_a_cancel_writes_nothing() {
        let dir = scratch();
        let bad = SfxSpec {
            pitch: 99.0,
            ..spec(SfxKind::Pop)
        };
        let e = synthesize_in(&dir, &bad, &|| false).unwrap_err().to_string();
        assert!(e.contains("pitch"), "{e}");
        let e = synthesize_in(&dir, &spec(SfxKind::Pop), &|| true).unwrap_err();
        assert!(matches!(e, Error::Cancelled), "{e}");
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "nothing written");
        // A cancel after the render, before the write.
        let polls = std::cell::Cell::new(0);
        let e = synthesize_in(&dir, &spec(SfxKind::Pop), &|| {
            polls.set(polls.get() + 1);
            polls.get() > 1
        })
        .unwrap_err();
        assert!(matches!(e, Error::Cancelled), "{e}");
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `KERF_SFX_DUMP=<dir> cargo test -p kerf-core --no-default-features dump_to_listen -- --nocapture`
    /// writes every kind (and a few variations) there, to listen to or to look at in a spectrogram.
    #[test]
    fn dump_to_listen() {
        let Some(dir) = std::env::var_os("KERF_SFX_DUMP") else {
            return;
        };
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        for kind in SfxKind::ALL {
            for (name, s) in [
                (kind.name().to_string(), spec(kind)),
                (
                    format!("{}_soft", kind.name()),
                    SfxSpec {
                        intensity: 0.1,
                        tail: 0.0,
                        ..spec(kind)
                    },
                ),
                (
                    format!("{}_hard", kind.name()),
                    SfxSpec {
                        intensity: 1.0,
                        tail: 0.8,
                        seed: 3,
                        ..spec(kind)
                    },
                ),
                (
                    format!("{}_rev", kind.name()),
                    SfxSpec {
                        reverse: true,
                        ..spec(kind)
                    },
                ),
            ] {
                let frames = render(&s);
                let (l, r): (Vec<f32>, Vec<f32>) = frames.iter().map(|f| (f[0], f[1])).unzip();
                let mut file = std::fs::File::create(dir.join(format!("{name}.wav"))).unwrap();
                write_wav16(&mut file, &l, &r, SAMPLE_RATE).unwrap();
            }
        }
    }
}
