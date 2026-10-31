//! Does this FFmpeg's `scale` filter run the arithmetic the compositor's scaler follows?
//!
//! swscale's scaler is not one arithmetic: what a build runs is a property of the CPU and the build, not of the graph.
//!
//! * **The tables.** A SIMD build (x86 with MMX, NEON) rounds each filter's size up to a multiple of 4 (horizontal) or
//!   2 (vertical) and *keeps the original taps in the padding*: swscale counts the near-zero taps on the right as
//!   droppable (`minFilterSize`) but copies them into the padded rows unless `SWS_BITEXACT`, so the row's weights are
//!   normalised with them and differ from the C build's by a unit here and there ([`crate::sws::table_aligned`]).
//! * **The vertical arithmetic.** The C one (`yuv2planeX_8_c`: 12-bit weights times the 15-bit intermediate, summed
//!   from `64 << 12` and shifted down by 19) and, on x86 with SSE2 or better, the SIMD one the default flags select
//!   (`yuv2yuvX.asm`, `use_mmx_vfilter`): `pmulhw` of each weight and sample (the high half of the 16-bit product, so
//!   every tap truncates), the sum started from `(64 + 8 * (N - 1)) >> 4` with `N` the tap count, then shifted down
//!   by 3 — **except on the last rows of the picture**, which `swscale.c` hands back to the C kernel
//!   (`dstY >= dstH - 2`, "we can't use MMX here without overwriting this array's tail"): the last two rows of
//!   luma, and so the last row of 4:2:0 chroma, which is produced with the even luma row. A shrink steeper than
//!   about 8:1 reads differently from the C arithmetic by a level on a good share of samples of busy pictures (and by
//!   up to five at 40:1), so a scaler that follows one is exact against a build that runs the other only by accident.
//!
//! The compositor draws the SIMD arithmetic where FFmpeg runs it and the C arithmetic where it does not, and the
//! frames that are *exact* because of it (a layer the scaler resizes under a grade, the blurred fill) depend on the
//! answer: it is measured, once per process, on the real filter ([`kernels`], like [`crate::grade::kernels`] for the
//! conversions to and from RGB). The horizontal pass has one arithmetic (SIMD and C agree: exact integer sums).

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::sws::{self, Scaler};

/// What a build's scaler is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernel {
    /// swscale's C scaler: unpadded tables, `yuv2planeX_8_c`. What a build without SIMD scalers runs.
    C,
    /// Tables padded for SIMD scalers whose vertical arithmetic is the C one (NEON on arm64, where it is bit-exact
    /// with C): the filters of a SIMD build, `yuv2planeX_8_c`'s sums. **Not measured on any machine here**; the probe
    /// says whether a build is this.
    Aligned,
    /// The x86 SIMD scaler: padded tables and `yuv2yuvX`, C on the last rows.
    X86,
}

impl Kernel {
    /// The filter-size alignment of the tables, horizontally and vertically.
    pub fn alignment(self) -> (usize, usize) {
        match self {
            Self::C => (1, 1),
            Self::Aligned | Self::X86 => (sws::SIMD_ALIGN_HORIZONTAL, sws::SIMD_ALIGN_VERTICAL),
        }
    }

    /// The vertical pass runs the x86 kernel's arithmetic (on every row but the last ones).
    pub fn simd_vertical(self) -> bool {
        self == Self::X86
    }
}

/// What a build's `scale` is: its scaler ([`Kernel`]) and the algorithm an unflagged `scale` runs in a complex filter
/// graph (the product's graphs): bicubic since FFmpeg 5.0, **bilinear before it** (4.4's `scale` defaults to
/// `flags=bilinear`, and only a simple `-vf` graph gets `-sws_flags`' bicubic from the command line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    pub kernel: Kernel,
    pub default: Scaler,
}

impl Model {
    /// What the compositor has always assumed: the C scaler, bicubic by default.
    pub const ASSUMED: Self = Self {
        kernel: Kernel::C,
        default: Scaler::Bicubic,
    };

    /// The algorithm of a scale: its own flag, else this build's default.
    pub fn scaler(self, flagged: Option<Scaler>) -> Scaler {
        flagged.unwrap_or(self.default)
    }
}

/// Whether this FFmpeg's `scale` is a [`Model`] of the compositor's, to the level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernels {
    /// Noise through the probe's scales came back as the model computes it.
    Match(Model),
    /// It matched no model (a CPU or a build whose scaler is another arithmetic).
    Differs,
    /// Not known: not measured yet, FFmpeg would not run or timed out (asked again after a backoff).
    Unknown,
}

impl Kernels {
    /// The model the compositor draws with: the measured one, else the one it has always assumed (a level from a
    /// SIMD build's scaler, and bicubic).
    pub fn model(self) -> Model {
        match self {
            Self::Match(m) => m,
            Self::Differs | Self::Unknown => Model::ASSUMED,
        }
    }

    /// The scaler is swscale's to the level on this machine.
    pub fn exact(self) -> bool {
        matches!(self, Self::Match(_))
    }
}

/// One plane of a scale, as the model computes it: swscale's table for `scaler` in both directions, the 15-bit
/// horizontal intermediate (clipped at the top only), and the vertical pass in `kernel`'s arithmetic with the last
/// `c_rows` rows of the picture (2 of luma, 1 of 4:2:0 chroma) in the C one.
pub fn scale_plane(
    src: &[u8],
    (sw, sh): (usize, usize),
    (dw, dh): (usize, usize),
    scaler: Scaler,
    kernel: Kernel,
    c_rows: usize,
) -> Option<Vec<u8>> {
    if src.len() != sw * sh {
        return None;
    }
    let (align_h, align_v) = kernel.alignment();
    let hf = sws::table_aligned(scaler, sw as u32, dw as u32, sws::ONE_HORIZONTAL, align_h)?;
    let vf = sws::table_aligned(scaler, sh as u32, dh as u32, sws::ONE_VERTICAL, align_v)?;
    let mut mid = vec![0i32; dw * sh];
    for y in 0..sh {
        for x in 0..dw {
            let acc: i32 = (0..hf.size)
                .map(|j| {
                    let col = (hf.pos[x] as usize + j).min(sw - 1);
                    hf.coeff[x * hf.size + j] * i32::from(src[y * sw + col])
                })
                .sum();
            mid[y * dw + x] = (acc >> 7).min(32767);
        }
    }
    let mut out = vec![0u8; dw * dh];
    for i in 0..dh {
        let simd = kernel.simd_vertical() && vf.size > 1 && i + c_rows < dh;
        let start = vertical_start(vf.size);
        for x in 0..dw {
            let taps = (0..vf.size).map(|j| {
                let row = (vf.pos[i] as usize + j).min(sh - 1);
                (vf.coeff[i * vf.size + j], mid[row * dw + x])
            });
            out[i * dw + x] = if simd {
                let acc: i32 = start + taps.map(|(c, s)| (c * s) >> 16).sum::<i32>();
                (acc >> 3).clamp(0, 255) as u8
            } else {
                let acc: i64 = (64 << 12) + taps.map(|(c, s)| i64::from(c) * i64::from(s)).sum::<i64>();
                (acc >> 19).clamp(0, 255) as u8
            };
        }
    }
    Some(out)
}

/// The sum `yuv2yuvX` starts from: `(64 + 8 * (N - 1)) >> 4`, `N` the tap count of the (aligned) vertical filter.
pub fn vertical_start(taps: usize) -> i32 {
    (64 + 8 * (taps as i32 - 1)) >> 4
}

// ---- the probe -----------------------------------------------------------------------------------

/// The probe picture's size: big enough for shrinks steep enough that swscale trims taps off a filter (it keeps them in
/// the padding of a SIMD build's, which is what tells the tables apart).
const PROBE: (usize, usize) = (256, 160);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// One scale of the probe: the filter's text, what it scales to and the algorithm its flag names (`None`: the build's
/// default).
struct Chain {
    filter: &'static str,
    out: (usize, usize),
    flag: Option<Scaler>,
}

const fn chain(filter: &'static str, w: usize, h: usize, flag: Option<Scaler>) -> Chain {
    Chain {
        filter,
        out: (w, h),
        flag,
    }
}

/// Shrinks of both scalers whose tables the C build and a SIMD build pad differently, one with odd sides (a chroma plane
/// of 9 rows under a luma plane of 17: the rows swscale hands to the C kernel are not the same count in both), and an
/// enlargement of each. The unflagged ones tell which algorithm an unflagged `scale` runs here.
const CHAINS: [Chain; 9] = [
    chain("scale=22:20", 22, 20, None),
    chain("scale=35:18", 35, 18, None),
    chain("scale=300:200", 300, 200, None),
    chain("scale=22:20:flags=bicubic", 22, 20, Some(Scaler::Bicubic)),
    chain("scale=35:18:flags=bicubic", 35, 18, Some(Scaler::Bicubic)),
    chain("scale=15:17:flags=bilinear", 15, 17, Some(Scaler::Bilinear)),
    chain("scale=17:25:flags=bilinear", 17, 25, Some(Scaler::Bilinear)),
    chain("scale=512:320:flags=bilinear", 512, 320, Some(Scaler::Bilinear)),
    chain("scale=300:200:flags=bilinear", 300, 200, Some(Scaler::Bilinear)),
];

fn frame_len((w, h): (usize, usize)) -> usize {
    w * h + 2 * w.div_ceil(2) * h.div_ceil(2)
}

/// The probe picture: a polynomial of the coordinates, mod 256, in all three planes (not `random()`, which restarts
/// for every slice thread), so every branch of both kernels is looked at.
fn probe_source() -> String {
    let (w, h) = PROBE;
    format!(
        "nullsrc=s={w}x{h}:r=1:d=1,format=yuv420p,\
         geq=lum='mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29,256)':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17,256)'"
    )
}

fn planes(frame: &[u8], (w, h): (usize, usize)) -> [&[u8]; 3] {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (y, rest) = frame.split_at(w * h);
    let (u, v) = rest.split_at(cw * ch);
    [y, u, v]
}

/// The planes the model makes of the probe picture for `chain`.
fn modelled(source: &[u8], chain: &Chain, model: Model) -> Option<Vec<u8>> {
    let [y, u, v] = planes(source, PROBE);
    let (cw, ch) = (PROBE.0.div_ceil(2), PROBE.1.div_ceil(2));
    let (ow, oh) = chain.out;
    let (ocw, och) = (ow.div_ceil(2), oh.div_ceil(2));
    let scaler = model.scaler(chain.flag);
    let mut out = scale_plane(y, PROBE, chain.out, scaler, model.kernel, 2)?;
    out.extend(scale_plane(u, (cw, ch), (ocw, och), scaler, model.kernel, 1)?);
    out.extend(scale_plane(v, (cw, ch), (ocw, och), scaler, model.kernel, 1)?);
    Some(out)
}

/// `-filter_complex`: the product's graphs are complex ones, and an unflagged `scale` gets its algorithm from the
/// graph: a simple `-vf` one is handed the command line's bicubic, a complex one the filter's own default.
fn probe_args(graph: &str, complex: bool) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin"]
        .into_iter()
        .map(String::from)
        .collect();
    if complex {
        args.extend([
            "-filter_complex".to_string(),
            format!("{graph}[o]"),
            "-map".to_string(),
            "[o]".to_string(),
        ]);
    } else {
        args.extend(["-f", "lavfi", "-i"].map(String::from));
        args.push(graph.to_string());
    }
    args.extend(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"].map(String::from));
    args
}

fn measure() -> Option<Kernels> {
    // (graph, whether it is a complex one, the picture's length)
    let mut graphs = vec![(probe_source(), false, frame_len(PROBE))];
    graphs.extend(
        CHAINS
            .iter()
            .map(|c| (format!("{},{}", probe_source(), c.filter), true, frame_len(c.out))),
    );
    let share = graphs.len();
    let frames: Vec<Option<Vec<u8>>> = std::thread::scope(|scope| {
        let runs: Vec<_> = graphs
            .iter()
            .map(|(graph, complex, len)| {
                scope.spawn(move || {
                    let mut args = probe_args(graph, *complex);
                    // What a frame waits on is not niced, but the children share the budget's threads.
                    kerf_core::limit_frame_args(&mut args, share);
                    let mut cmd = kerf_core::ffmpeg_command();
                    cmd.args(&args);
                    crate::source::run_child(cmd, len + 1, PROBE_TIMEOUT).ok()
                })
            })
            .collect();
        runs.into_iter().map(|run| run.join().ok().flatten()).collect()
    });
    let (source, rest) = frames.split_first()?;
    classify(source.as_deref()?, rest)
}

/// Every model a build can be: the three scalers, each with either default algorithm.
const MODELS: [Model; 6] = {
    const fn m(kernel: Kernel, default: Scaler) -> Model {
        Model { kernel, default }
    }
    [
        m(Kernel::X86, Scaler::Bicubic),
        m(Kernel::X86, Scaler::Bilinear),
        m(Kernel::Aligned, Scaler::Bicubic),
        m(Kernel::Aligned, Scaler::Bilinear),
        m(Kernel::C, Scaler::Bicubic),
        m(Kernel::C, Scaler::Bilinear),
    ]
};

/// The answer from the probe's pictures: `None` when one is missing or the wrong size and the models left cannot be
/// narrowed to one (a difference from every model is conclusive wherever the rest stands).
fn classify(source: &[u8], frames: &[Option<Vec<u8>>]) -> Option<Kernels> {
    if source.len() != frame_len(PROBE) || frames.len() != CHAINS.len() {
        return None;
    }
    let mut unknown = false;
    // The models this machine's FFmpeg can still be: every chain that came back narrows them.
    let mut alive = [true; MODELS.len()];
    for (chain, frame) in CHAINS.iter().zip(frames) {
        match frame.as_deref().filter(|f| f.len() == frame_len(chain.out)) {
            None => unknown = true,
            Some(frame) => {
                for (model, alive) in MODELS.iter().zip(alive.iter_mut()) {
                    if *alive && modelled(source, chain, *model).as_deref() != Some(frame) {
                        *alive = false;
                    }
                }
            }
        }
    }
    let left: Vec<Model> = MODELS.iter().zip(alive).filter(|(_, a)| *a).map(|(m, _)| *m).collect();
    match (left.as_slice(), unknown) {
        ([], _) => Some(Kernels::Differs),
        // (A chain that did not come back leaves the answer open, unless only one model is left to be.)
        ([m], false) => Some(Kernels::Match(*m)),
        _ => None,
    }
}

// ---- the process's answer ------------------------------------------------------------------------

struct State {
    measured: Option<Kernels>,
    failures: u32,
    retry_at: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State {
    measured: None,
    failures: 0,
    retry_at: None,
});

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether this FFmpeg's `scale` is the model ([`Kernels`]). The first call runs the probe: seven short `ffmpeg`
/// children at once (bounded by 20 s) with no lock held, a blocking thread's, ahead of the frame that needs it
/// ([`crate::prepare`]). [`Kernels::Unknown`] is not remembered: the probe is run again after a backoff.
pub fn kernels() -> Kernels {
    cached(&STATE, Instant::now(), measure)
}

/// What is known without running anything: the measured answer, else [`Kernels::Unknown`]. What a plan's caps are
/// made from ([`crate::Compositor::caps`]): a plan does not wait for a probe.
pub fn measured() -> Kernels {
    lock(&STATE).measured.unwrap_or(Kernels::Unknown)
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300))
}

fn cached(state: &Mutex<State>, now: Instant, measure: impl FnOnce() -> Option<Kernels>) -> Kernels {
    {
        let s = lock(state);
        if let Some(k) = s.measured {
            return k;
        }
        if s.retry_at.is_some_and(|t| now < t) {
            return Kernels::Unknown;
        }
    }
    // Measured with no lock held: a probe is child processes, and nothing else waits behind them. Callers that arrive
    // meanwhile measure too, boundedly; the first answer stored wins.
    let measured = measure();
    let mut s = lock(state);
    if let Some(k) = s.measured {
        return k;
    }
    match measured {
        Some(k) => {
            if k == Kernels::Differs {
                tracing::warn!(
                    "this FFmpeg's scaler is neither the C arithmetic nor the x86 SIMD one the GPU follows; \
                     what depends on the scaler being exact is drawn by FFmpeg"
                );
            }
            s.measured = Some(k);
            k
        }
        None if s.retry_at.is_some_and(|t| t > now) => Kernels::Unknown,
        None => {
            s.failures += 1;
            let wait = backoff(s.failures);
            s.retry_at = Some(Instant::now() + wait);
            tracing::warn!(
                failures = s.failures,
                retry_in_secs = wait.as_secs(),
                "could not measure FFmpeg's scaler; what depends on it being exact is not drawn on the GPU until it can"
            );
            Kernels::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Mutex<State> {
        Mutex::new(State {
            measured: None,
            failures: 0,
            retry_at: None,
        })
    }

    /// A picture of integer noise, deterministic.
    fn noise(w: usize, h: usize) -> Vec<u8> {
        (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as u64, (i / w) as u64);
                ((x * x * 37 + y * y * 91 + x * y * 53 + x * 11 + y * 29) % 256) as u8
            })
            .collect()
    }

    #[test]
    fn the_start_constant_is_the_dither_and_the_aligned_taps() {
        // (64 + 8 * (N - 1)) >> 4 with N the tap count of the aligned filter.
        assert_eq!(vertical_start(1), 4);
        assert_eq!(vertical_start(2), 4);
        assert_eq!(vertical_start(4), 5);
        assert_eq!(vertical_start(6), 6);
        assert_eq!(vertical_start(162), 84);
    }

    #[test]
    fn the_kernels_differ_on_noise_and_agree_on_a_flat_picture() {
        let src = noise(48, 32);
        let c = scale_plane(&src, (48, 32), (36, 24), Scaler::Bicubic, Kernel::C, 2).unwrap();
        let x = scale_plane(&src, (48, 32), (36, 24), Scaler::Bicubic, Kernel::X86, 2).unwrap();
        assert_ne!(c, x, "the probe would tell nothing");
        // (Both are 8-bit planes of the right size.)
        assert_eq!((c.len(), x.len()), (36 * 24, 36 * 24));
        let flat = vec![77u8; 48 * 32];
        for kernel in [Kernel::C, Kernel::Aligned, Kernel::X86] {
            for scaler in [Scaler::Bicubic, Scaler::Bilinear] {
                let out = scale_plane(&flat, (48, 32), (36, 24), scaler, kernel, 2).unwrap();
                assert!(out.iter().all(|v| *v == 77), "{kernel:?} {scaler:?} moved a flat picture");
            }
        }
    }

    fn probe_source_planes() -> Vec<u8> {
        let (w, h) = PROBE;
        let mut v = noise(w, h);
        v.extend(noise(w / 2, h / 2));
        v.extend(noise(w / 2, h / 2));
        v
    }

    #[test]
    fn the_probe_tells_every_model_apart() {
        let source = probe_source_planes();
        let pictures: Vec<Vec<Vec<u8>>> = CHAINS
            .iter()
            .map(|c| MODELS.iter().map(|m| modelled(&source, c, *m).unwrap()).collect())
            .collect();
        // Each pair of models disagrees on some chain's picture: a build that is any one of them is told from the others.
        for (i, a) in MODELS.iter().enumerate() {
            for (j, b) in MODELS.iter().enumerate().skip(i + 1) {
                assert!(
                    pictures.iter().any(|p| p[i] != p[j]),
                    "{a:?} and {b:?} cannot be told apart by the probe"
                );
            }
        }
    }

    #[test]
    fn the_last_rows_are_the_c_kernels_and_the_rest_the_simd_one() {
        let src = noise(48, 32);
        let c = scale_plane(&src, (48, 32), (36, 24), Scaler::Bicubic, Kernel::Aligned, 2).unwrap();
        let x = scale_plane(&src, (48, 32), (36, 24), Scaler::Bicubic, Kernel::X86, 2).unwrap();
        // Rows 22 and 23 are the C kernel's whichever kernel is asked for; an earlier one differs somewhere.
        assert_eq!(c[22 * 36..], x[22 * 36..]);
        assert_ne!(c[..22 * 36], x[..22 * 36]);
        // Asked for one C row only, row 22 is SIMD.
        let one = scale_plane(&src, (48, 32), (36, 24), Scaler::Bicubic, Kernel::X86, 1).unwrap();
        assert_eq!(c[23 * 36..], one[23 * 36..]);
        assert_ne!(c[22 * 36..23 * 36], one[22 * 36..23 * 36]);
    }

    #[test]
    fn an_unscaled_axis_is_the_same_in_both_kernels() {
        // One tap: `yuv2plane1`, `(s + 64) >> 7`, which the C and SIMD kernels share.
        let src = noise(48, 32);
        let c = scale_plane(&src, (48, 32), (36, 32), Scaler::Bicubic, Kernel::Aligned, 2).unwrap();
        let x = scale_plane(&src, (48, 32), (36, 32), Scaler::Bicubic, Kernel::X86, 2).unwrap();
        assert_eq!(c, x);
    }

    #[test]
    fn a_simd_build_keeps_the_trimmed_taps_in_the_padding_and_a_c_build_does_not() {
        // 1280 -> 120 trims the tails of the bicubic window (a count of 42 taps out of 44): the SIMD build's rows are
        // rounded up and hold the originals, so some normalise to other weights than the C build's.
        let c = sws::table_aligned(Scaler::Bicubic, 1280, 120, sws::ONE_HORIZONTAL, 1).unwrap();
        let s = sws::table_aligned(Scaler::Bicubic, 1280, 120, sws::ONE_HORIZONTAL, 4).unwrap();
        assert_eq!((c.size, s.size), (42, 44));
        let differing = (0..120)
            .filter(|i| s.coeff[i * 44..i * 44 + 42] != c.coeff[i * 42..(i + 1) * 42])
            .count();
        assert!(differing > 0, "no row differs");
        // Both sum to one.
        for i in 0..120 {
            assert_eq!(s.coeff[i * 44..(i + 1) * 44].iter().sum::<i32>(), sws::ONE_HORIZONTAL);
        }
    }

    #[test]
    fn a_probe_that_matches_a_kernel_says_so_and_one_that_matches_none_differs() {
        let source = probe_source_planes();
        for model in MODELS {
            let frames: Vec<Option<Vec<u8>>> = CHAINS.iter().map(|c| modelled(&source, c, model)).collect();
            assert_eq!(classify(&source, &frames), Some(Kernels::Match(model)), "{model:?}");
            // One frame missing and the rest agreeing is not an answer.
            let mut short = frames.clone();
            short[2] = None;
            assert_eq!(classify(&source, &short), None);
            // ... and a missing frame does not hide a difference.
            let mut off_short = short.clone();
            if let Some(f) = off_short[0].as_mut() {
                f[5] = f[5].wrapping_add(1);
            }
            assert_eq!(classify(&source, &off_short), Some(Kernels::Differs));
            // One sample off in one chain matches neither.
            let mut off = frames.clone();
            if let Some(f) = off[1].as_mut() {
                f[5] = f[5].wrapping_add(1);
            }
            assert_eq!(classify(&source, &off), Some(Kernels::Differs));
            // ... whatever else is missing.
            off[3] = None;
            assert_eq!(classify(&source, &off), Some(Kernels::Differs));
        }
    }

    #[test]
    fn an_unknown_answer_is_retried_after_a_backoff_and_a_measured_one_is_kept() {
        let s = state();
        let t0 = Instant::now();
        assert_eq!(cached(&s, t0, || None), Kernels::Unknown);
        // Backing off: the probe is not run.
        assert_eq!(cached(&s, t0, || panic!("measured during the backoff")), Kernels::Unknown);
        let later = t0 + Duration::from_secs(6);
        let m = Kernels::Match(MODELS[0]);
        assert_eq!(cached(&s, later, || Some(m)), m);
        assert_eq!(
            cached(&s, later, || panic!("measured twice")),
            m,
            "a measured answer is kept for good"
        );
        assert_eq!(lock(&s).failures, 1);
    }

    #[test]
    fn only_a_match_is_exact_and_everything_else_draws_the_c_kernel() {
        for m in MODELS {
            assert!(Kernels::Match(m).exact());
            assert_eq!(Kernels::Match(m).model(), m);
        }
        assert!(!Kernels::Differs.exact() && !Kernels::Unknown.exact());
        assert_eq!(Kernels::Differs.model(), Model::ASSUMED);
        assert_eq!(Kernels::Unknown.model(), Model::ASSUMED);
    }
}
