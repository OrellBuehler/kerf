//! Whether this FFmpeg's `gblur` runs as the compositor's blur does — measured once per process on the filter text the engine
//! writes ([`kerf_core::blur_filter`]), never assumed and never keyed on a version string. A blur is in `Compositor::caps` only on a
//! [`Blur::Match`].
//!
//! `gblur` is a recursion in `float`, and its x86 kernels fix the last bit of every step: with AVX2 or AVX-512 each step is a fused
//! multiply-add (`x86/vf_gblur.asm`), and the model ([`crate::effects::blur_plane`], the shader `blur.wgsl`) is that. A build
//! whose CPU has only SSE4 associates the sums another way (four samples at a time through powers of `nu`), and one with no kernel
//! at all does not fuse; they are a level from the model on the few samples where the value sits on a half, which is not
//! drawn. The probe runs the filter over noise in all three planes of a few pictures (an odd one, so the kernels' tails are looked
//! at, and a wide range of sigmas, so `nu` and the scales vary) and holds every sample of every plane to the model, with no
//! tolerance. A probe that could not tell is [`Blur::Unknown`]: asked again after a backoff, and nothing blurred is drawn
//! until it answers.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use crate::effects::{blur_plane, gaussian, Gaussian};
use crate::measure::{self, Budget, Probe, Timing, Verdict};

/// What the probe found: [`Verdict::Match`] (the filter is the model's, every sample of every case), [`Verdict::Differs`] (it
/// is not: the detail names the case) or [`Verdict::Unknown`].
pub type Blur = Verdict;

/// (`overall` is for up to [`MAX_CHILDREN`] children, [`WORKERS`] at a time: 8 rounds of about 5 s each, where the placement probe's
/// twenty-five run one after another in 45 s. A host that cannot start an FFmpeg in 5 s is not one the probe times out on forever by
/// its shape stage alone: every child shares the deadline, and a probe that ran out of it asks again after the backoff.)
const TIMING: Timing = Timing {
    backoff: Duration::from_secs(30),
    overall: Duration::from_secs(45),
    per_run: Duration::from_secs(20),
};

static STATE: Probe = Probe::new();

/// The vector width (8 or 16 floats) of the kernel the probe found, or 0 while it is not known which (not measured, or a machine on
/// which both widths' rules hold on every shape tried): what the plan judges a blur's planes against ([`GpuCaps::blur_lanes`]).
static LANES: AtomicU8 = AtomicU8::new(0);

/// The pictures: `(width, height, sigma, seed)`. The first three are **chosen to tell the fused recursion from the unfused one**: the
/// two differ by a level on a few samples in a hundred thousand, so a small picture of noise does not show it (an unfused build
/// matched six of them), and these are seeds found by running both models over a few hundred until one picture held five or more
/// samples where they disagree (6, 8 and 5, a test holds it). The last three are small and odd-sided, for the kernels' tails.
const CASES: [(usize, usize, f64, u64); 6] = [
    (160, 96, 250.0, 123),
    (160, 96, 1024.0, 34),
    (160, 96, 100.0, 211),
    (37, 23, 2.5, 1),
    (66, 38, 9.0, 2),
    (97, 61, 40.0, 3),
];

/// One picture for each of the recursion's four sweeps (rightwards and leftwards along the rows, downwards and upwards along the
/// columns), **chosen so that a build that is unfused in that sweep alone reads `Differs`**: the later sweeps re-round what the earlier
/// ones left, so a one-ulp difference in the first sweep survives to the 8-bit result on about one sample in a hundred million (the
/// last sweep's on one in a million), and a picture where it does has to be searched for. These are seeds found by running the model with
/// one sweep unfused over tens of thousands of pictures and keeping the one with the most samples that disagree with the fused model
/// (4, 34, 23 and 22; a test holds each at two or more). The probe would otherwise read `Match` on a build that fuses three sweeps
/// of four.
const SWEEP_CASES: [(usize, usize, f64, u64); 4] = [
    (160, 96, 250.0, 25817),
    (160, 96, 1024.0, 1885),
    (160, 96, 1024.0, 1574),
    (160, 96, 1024.0, 1667),
];

/// The generator of a case's noise (the three planes, one after the other).
fn noise(seed: u64) -> Lcg {
    Lcg(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2b99_17f3_0c5a_6d41)
}

/// What is known without running anything: the measured answer, else [`Blur::Unknown`]. What the compositor's caps are made from.
pub fn measured() -> Blur {
    measure::known(&STATE)
}

/// The kernel's vector width as the probe found it (`0`: not known which: both are judged). Only a measured `Match` sets it.
pub fn measured_lanes() -> u8 {
    LANES.load(Ordering::Relaxed)
}

/// The answer, measured on the first call (up to [`MAX_CHILDREN`] short `ffmpeg` children, four at a time, bounded by 20 s each and 45 s
/// in all, no lock held: a blocking thread's, ahead of the frame that needs it, [`crate::prepare`]). A caller that arrives while it runs waits for it and
/// takes its answer. `Unknown` is not remembered but for a backoff.
pub fn blur() -> Blur {
    measure::cached(&STATE, Instant::now(), TIMING, probe)
}

/// A deterministic generator.
struct Lcg(u64);

impl Lcg {
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n)
            .map(|_| {
                self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (self.0 >> 33) as u8
            })
            .collect()
    }
}

fn probe(budget: &Budget) -> Blur {
    let (verdict, lanes) = probe_found(budget, &[], &|plane, w, h, g| blur_plane(plane, w, h, g));
    if verdict == Blur::Match {
        LANES.store(lanes, Ordering::Relaxed);
    }
    verdict
}

/// [`probe`] with `cpu` ahead of FFmpeg's other arguments (the tests run it with `-cpuflags 0`, a machine with none of the SIMD kernels).
#[cfg(test)]
fn probe_with(budget: &Budget, cpu: &[&str]) -> Blur {
    probe_found(budget, cpu, &|plane, w, h, g| blur_plane(plane, w, h, g)).0
}

/// [`probe_found`] against another model: the tests hold the pictures to telling it from FFmpeg's.
#[cfg(test)]
fn probe_against(budget: &Budget, cpu: &[&str], model: &Model<'_>) -> Blur {
    probe_found(budget, cpu, model).0
}

/// A model of the filter on one plane: `(plane, width, height, coefficients)` to the 8-bit result.
type Model<'a> = dyn Fn(&[u8], usize, usize, &Gaussian) -> Vec<u8> + 'a;

/// The three planes of a picture.
type Planes = [Vec<u8>; 3];

/// The noise of a case: its three planes.
fn planes_of((w, h, _, seed): (usize, usize, f64, u64)) -> Planes {
    let mut rng = noise(seed);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    [rng.bytes(w * h), rng.bytes(cw * ch), rng.bytes(cw * ch)]
}

/// One picture of noise through FFmpeg's `gblur` on `threads` slice threads (`-filter_threads` and `-filter_complex_threads`, written after
/// the budget's own caps so that they win): the three planes it comes back with, `None` when it would not run.
fn blurred(
    budget: &Budget,
    cpu: &[&str],
    (w, h, sigma, seed): (usize, usize, f64, u64),
    threads: usize,
) -> Option<(Planes, Planes)> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let planes = planes_of((w, h, sigma, seed));
    let raw: Vec<u8> = planes.iter().flatten().copied().collect();
    let size = format!("{w}x{h}");
    let filter = kerf_core::blur_filter(sigma);
    let n = threads.to_string();
    let args: Vec<&str> = cpu
        .iter()
        .copied()
        .chain([
            "-filter_threads",
            &n,
            "-filter_complex_threads",
            &n,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &size,
            "-i",
            "pipe:0",
            "-vf",
            &filter,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "pipe:1",
        ])
        .collect();
    let out = budget.ffmpeg(&args, raw)?;
    if out.len() != w * h + 2 * cw * ch {
        return None;
    }
    let (luma, rest) = out.split_at(w * h);
    let (u, v) = rest.split_at(cw * ch);
    Some((planes, [luma.to_vec(), u.to_vec(), v.to_vec()]))
}

/// The dimensions of a picture's three planes.
fn plane_dims((w, h): (usize, usize)) -> [(usize, usize); 3] {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    [(w, h), (cw, ch), (cw, ch)]
}

/// The pictures that decide the kernel's width and test the rule for the planes it is not the recursion on (`gblur::KernelHazard`),
/// each at the slice threads it runs on: `(width, height, threads)`, noise at sigma 3, for a machine whose graphs run on `lo` to `hi` slice
/// threads ([`kerf_core::cpu_graph_thread_range`]). The threads are `1`, `2`, `3` and the two ends of the range, each with a luma plane that
/// has a sample to spare over the threads and one that has not, and, at one thread, the shapes that tell AVX2's 8 lanes from AVX-512's 16.
/// **No picture has a plane one sample wide or tall** (`KernelHazard::OneSample`: the kernel's loops run once too many on it, and a plane
/// of width 1 corrupts FFmpeg's heap): the end cases `(40, 2t - 2, t)` and `(40, 2t, t)` come out 40x2 at two threads and at one, a chroma
/// plane of 20x1, so any case with a plane under two samples across is left out (and the luma of those that stay is four rows or more).
fn shape_cases((lo, hi): (usize, usize)) -> Vec<(usize, usize, usize)> {
    let mut cases = vec![
        // One thread: a slice of every row. (12x16: AVX-512 blocks 16 rows over a 16-wide row; chroma 6x8: AVX2 blocks 8 rows over an
        // 8-wide row. 8x8 the other way round. 16x32 and 24x40: both, and neither.)
        (12, 16, 1),
        (8, 8, 1),
        (16, 32, 1),
        (24, 40, 1),
        (40, 40, 1),
        // Two slices of 17 and 16 rows, and of 8.
        (12, 34, 2),
        (8, 16, 2),
        // Three threads: chroma of two rows has an empty slice, of three has none.
        (40, 4, 3),
        (40, 6, 3),
    ];
    for t in [lo, hi] {
        // Chroma of `t - 1` rows (an empty slice) and of `t` (none), and a plane with rows enough for several vectors a slice.
        cases.extend([(40, 2 * t - 2, t), (40, 2 * t, t), (40, 2 * t + 30, t)]);
    }
    cases.retain(|&(w, h, t)| t >= 1 && plane_dims((w, h)).iter().all(|&(pw, ph)| pw >= 2 && ph >= 2));
    cases.sort_unstable();
    cases.dedup();
    cases
}

/// How many children run at once: a host that is slow to start a process spends a quarter of the time per child it would one after
/// another.
const WORKERS: usize = 4;

/// How many FFmpeg children the probe runs at most, whatever the range of threads: the pictures that decide fused from unfused (ten, at
/// one thread), four of them again on this machine's graph threads ([`on_threads`]), and the shape cases (nine, and three at each end of
/// the range: fifteen at most, [`shape_cases`]).
const MAX_CHILDREN: usize = 29;

/// The pictures run again on the slice threads of this machine's graphs: `(case, threads)`.
fn on_threads((lo, hi): (usize, usize)) -> [((usize, usize, f64, u64), usize); 4] {
    [(CASES[0], lo), (CASES[0], hi), (CASES[4], hi), (SWEEP_CASES[1], hi)]
}

/// How many children the probe runs for a machine whose graphs run on `range` slice threads.
#[cfg(test)]
fn children(range: (usize, usize)) -> usize {
    CASES.len() + SWEEP_CASES.len() + on_threads(range).len() + shape_cases(range).len()
}

/// What one child of the probe runs.
#[derive(Clone, Copy)]
enum Job {
    /// A picture of [`CASES`] or [`SWEEP_CASES`] at `threads`.
    Picture((usize, usize, f64, u64), usize),
    /// A shape of [`shape_cases`].
    Shape(usize, usize, usize),
}

impl Job {
    /// The picture and the slice threads it runs on.
    fn case(self) -> ((usize, usize, f64, u64), usize) {
        match self {
            Self::Picture(case, t) => (case, t),
            Self::Shape(w, h, t) => ((w, h, 3.0, (w * 7919 + h * 31 + t) as u64), t),
        }
    }
}

/// Every child of the probe for a machine whose graphs run on `range` slice threads, in the order [`judge`] reads them: the pictures that
/// decide fused from unfused, on one slice thread (the planes of none of them is a shape the kernel mishandles); the same on the slice
/// threads of this machine's graphs (planes the rule clears, rows enough for the threads); the shapes.
fn jobs_for(range: (usize, usize)) -> Vec<Job> {
    let mut jobs: Vec<Job> = CASES
        .into_iter()
        .chain(SWEEP_CASES)
        .map(|case| Job::Picture(case, 1))
        .collect();
    jobs.extend(on_threads(range).iter().map(|&(case, t)| Job::Picture(case, t)));
    jobs.extend(shape_cases(range).iter().map(|&(w, h, t)| Job::Shape(w, h, t)));
    jobs
}

/// Run every job, four at a time, on the budget's one deadline: each job's planes (the noise and FFmpeg's blur of it), `None` for one that
/// would not run. In the order of `jobs`.
fn run_jobs(budget: &Budget, cpu: &[&str], jobs: &[Job]) -> Vec<Option<(Planes, Planes)>> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<Option<(Planes, Planes)>>> = std::sync::Mutex::new(vec![None; jobs.len()]);
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(jobs.len()) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(job) = jobs.get(i) else { break };
                let (case, t) = job.case();
                let found = blurred(budget, cpu, case, t);
                results.lock().unwrap_or_else(std::sync::PoisonError::into_inner)[i] = found;
            });
        }
    });
    results.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The verdict, and the kernel's vector width when it is a `Match` (8 or 16; 0: not known which).
fn probe_found(budget: &Budget, cpu: &[&str], model: &Model<'_>) -> (Blur, u8) {
    let range = kerf_core::cpu_graph_thread_range();
    let jobs = jobs_for(range);
    debug_assert!(jobs.len() <= MAX_CHILDREN, "{} children", jobs.len());
    judge(range, model, run_jobs(budget, cpu, &jobs))
}

/// What the children's answers (`done`, in the order of [`jobs_for`]) say of `model`: a difference, a match and the vector width, or that
/// it cannot be told.
fn judge(range: (usize, usize), model: &Model<'_>, done: Vec<Option<(Planes, Planes)>>) -> (Blur, u8) {
    let on_threads = on_threads(range);
    let shapes = shape_cases(range);
    // (A child that would not run is a picture not looked at: a difference found in the others is still a difference, and only a probe
    // with nothing wrong in what it did see is `Unknown` for the child it did not. A shape that keeps failing on a machine whose kernel
    // is not the model's must not keep the probe asking again every backoff.)
    let mut done = done.into_iter();
    let mut missing = false;
    for case in CASES.into_iter().chain(SWEEP_CASES) {
        let (w, h, sigma, _) = case;
        let Some((planes, got)) = done.next().expect("a job per picture") else {
            missing = true;
            continue;
        };
        let Some(g) = gaussian(sigma) else {
            return (Blur::Unknown, 0);
        };
        let mut apart = 0;
        for (k, (pw, ph)) in plane_dims((w, h)).into_iter().enumerate() {
            let want = model(&planes[k], pw, ph, &g);
            apart += got[k].iter().zip(&want).filter(|(a, b)| a != b).count();
        }
        if apart != 0 {
            return (
                Blur::Differs(format!(
                    "{} over {w}x{h}: {apart} samples differ from the fused-multiply-add model",
                    kerf_core::blur_filter(sigma)
                )),
                0,
            );
        }
    }
    for &(case, t) in &on_threads {
        let (w, h, sigma, _) = case;
        let Some((planes, got)) = done.next().expect("a job per picture") else {
            missing = true;
            continue;
        };
        let Some(g) = gaussian(sigma) else {
            return (Blur::Unknown, 0);
        };
        for (k, (pw, ph)) in plane_dims((w, h)).into_iter().enumerate() {
            if kerf_core::gblur::plane_hazard((pw as u32, ph as u32), t as u32, 8).is_some()
                || kerf_core::gblur::plane_hazard((pw as u32, ph as u32), t as u32, 16).is_some()
            {
                continue;
            }
            if got[k] != model(&planes[k], pw, ph, &g) {
                return (
                    Blur::Differs(format!(
                        "{} over {w}x{h} on {t} slice threads: a plane the rule clears differs from the model",
                        kerf_core::blur_filter(sigma)
                    )),
                    0,
                );
            }
        }
    }
    // Which vector widths' rules were sound on every shape: a width whose rule calls a plane clear that is not the recursion is not this
    // machine's kernel.
    let Some(g) = gaussian(3.0) else {
        return (Blur::Unknown, 0);
    };
    let mut sound: Vec<u32> = kerf_core::gblur::KERNEL_LANES.to_vec();
    for &(w, h, t) in &shapes {
        let Some((planes, got)) = done.next().expect("a job per shape") else {
            missing = true;
            continue;
        };
        for (k, (pw, ph)) in plane_dims((w, h)).into_iter().enumerate() {
            let exact = got[k] == model(&planes[k], pw, ph, &g);
            sound.retain(|&l| exact || kerf_core::gblur::plane_hazard((pw as u32, ph as u32), t as u32, l).is_some());
        }
    }
    if sound.is_empty() {
        return (
            Blur::Differs(
                "gblur is not the recursion on planes that neither the 8-lane nor the 16-lane kernel's rule accounts for".into(),
            ),
            0,
        );
    }
    if missing {
        return (Blur::Unknown, 0);
    }
    // The width for the plan: one if only one's rule is sound, else not known which.
    (Blur::Match, if let [only] = sound[..] { only as u8 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::blur_plane_fused;

    /// The memory of the probe: a probe that could not tell is asked again after the backoff and not before, and a match or a
    /// difference is kept for good.
    #[test]
    fn an_unknown_is_asked_again_after_the_backoff_and_a_match_or_a_difference_is_kept() {
        let state = Probe::new();
        let t0 = Instant::now();
        assert_eq!(measure::known(&state), Blur::Unknown, "nothing measured yet");
        assert_eq!(measure::cached(&state, t0, TIMING, |_| Blur::Unknown), Blur::Unknown);
        assert_eq!(
            measure::cached(&state, t0 + Duration::from_secs(29), TIMING, |_| panic!(
                "probed inside the backoff"
            )),
            Blur::Unknown
        );
        let after = Instant::now() + TIMING.backoff + Duration::from_secs(1);
        let differs = Blur::Differs("gblur=sigma=2.5 over 37x23: 3 samples differ".into());
        assert_eq!(measure::cached(&state, after, TIMING, |_| differs.clone()), differs);
        assert_eq!(measure::known(&state), differs);
        assert_eq!(measure::cached(&state, after, TIMING, |_| panic!("measured twice")), differs);
    }

    /// The shader's fused multiply-add (`shaders/fma.wgsl`, integers) is `f32::mul_add` and its multiply is `*`, bit for bit, on
    /// random triples across a wide range of exponents, on the ties a rounding to nearest even has to break (a product of two
    /// 12-bit significands has exactly 24 bits, one of 13 and 12 has 25: half an ulp, which the addend then moves), on a zero
    /// addend, on an addend far above or far below the product, and on operands whose sum cancels to a power of two's edge; and on
    /// **subnormal and underflowing results** (a product within a few powers of two of 2^-126 and 2^-149, subnormal operands and
    /// addends, the ties and carries at the fixed quantum 2^-149, the decay of the recursion's own running value through the subnormals
    /// to zero) and on overflow to infinity. A float that goes through the shader as a float could be flushed by a backend: these
    /// are on the bit patterns.
    #[test]
    #[ignore = "needs a GPU adapter (lavapipe is enough)"]
    #[allow(clippy::print_stderr)]
    fn the_shaders_fused_multiply_add_is_f32_mul_add_on_every_case_tried() {
        use crate::gpu::{Gpu, GpuOptions};
        use wgpu::util::DeviceExt as _;
        let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
        let mut rng = Lcg(0x5eed_f00d_1234_abcd);
        let mut next = move || {
            rng.0 = rng.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng.0 >> 32) as u32
        };
        // A positive normal float with a random significand (or a few bits of one) and an exponent in 2^-e..2^e.
        let float = |bits: u32, e: i32, next: &mut dyn FnMut() -> u32| -> f32 {
            let mant = if bits >= 23 {
                next() & 0x7f_ffff
            } else {
                (next() & ((1 << bits) - 1)) << (23 - bits)
            };
            let exp = 127 + (next() % (2 * e as u32 + 1)) as i32 - e;
            f32::from_bits(((exp as u32) << 23) | mant)
        };
        let mut cases: Vec<[f32; 3]> = Vec::new();
        for _ in 0..200_000 {
            let (a, b) = (float(23, 30, &mut next), float(23, 30, &mut next));
            let c = match next() % 8 {
                0 => 0.0,
                // Near the product: the cancellation-free sum of two comparable terms.
                1..=3 => a * b * (0.25 + (next() % 1000) as f32 / 500.0),
                _ => float(23, 60, &mut next),
            };
            cases.push([a, b, c]);
        }
        // Ties: short significands, so the product is exact in 24 or 25 bits and the addend decides the rounding.
        for _ in 0..200_000 {
            let (a, b) = (float(12, 20, &mut next), float(13, 20, &mut next));
            let c = match next() % 4 {
                0 => 0.0,
                1 => float(3, 20, &mut next),
                2 => a * b,
                _ => float(23, 20, &mut next),
            };
            cases.push([a, b, c]);
        }
        for _ in 0..50_000 {
            // The recursion's own range: nu in (0, 1), a running value in 0..2^20, a pixel 0..255.
            let nu = f32::from_bits(0x3f80_0000 - 1 - (next() % 0x40_0000));
            let prev = (next() % (1 << 20)) as f32 / 64.0 + 1.0;
            let c = (next() % 256) as f32;
            cases.push([nu, prev, c]);
        }
        // Every exponent: operands from the whole range of finite non-negative floats, subnormals and zero among them, so results
        // overflow to infinity, underflow to zero and fall into the subnormals.
        let anything = |next: &mut dyn FnMut() -> u32| -> f32 {
            let exp = match next() % 8 {
                0 => 0,
                _ => next() % 255,
            };
            f32::from_bits((exp << 23) | (next() & 0x7f_ffff))
        };
        for _ in 0..200_000 {
            let (a, b) = (anything(&mut next), anything(&mut next));
            let c = if next() % 4 == 0 { 0.0 } else { anything(&mut next) };
            cases.push([a, b, c]);
        }
        // The edge of the subnormals: a product within a few powers of two of 2^-126 and 2^-149, with an addend that is zero, a
        // subnormal, or about the product's own size, so the rounding position is the fixed quantum 2^-149 rather than 24 bits.
        for _ in 0..300_000 {
            let a = if next() % 6 == 0 {
                anything(&mut next)
            } else {
                float(23, 60, &mut next)
            };
            let want_exp = -160 + (next() % 50) as i32;
            let ea = (a.to_bits() >> 23) as i32 - 127;
            let eb_field = (want_exp - ea + 127).clamp(0, 254) as u32;
            let b = f32::from_bits((eb_field << 23) | (next() & 0x7f_ffff));
            let c = match next() % 5 {
                0 => 0.0,
                1 => f32::from_bits(next() & 0x7f_ffff),
                2 => f32::from_bits(1 + next() % 8),
                3 => f32::from_bits(0x0080_0000 + (next() % 0x2000)),
                _ => f32::from_bits(((next() % 40) << 23) | (next() & 0x7f_ffff)),
            };
            cases.push([a, b, c]);
        }
        // Ties and carries at the quantum: powers of two and short significands whose product is exactly half a subnormal step,
        // one step, or the largest subnormal plus half a step (which rounds up into the smallest normal).
        for e1 in -80..=-60 {
            for e2 in -90..=-60 {
                for (m1, m2) in [
                    (0, 0),
                    (0x40_0000, 0),
                    (0x7f_ffff, 0x7f_ffff),
                    (0x7f_ffff, 0),
                    (1, 1),
                    (0x20_0000, 0x40_0000),
                ] {
                    let a = f32::from_bits((((e1 + 127) as u32) << 23) | m1);
                    let b = f32::from_bits((((e2 + 127) as u32) << 23) | m2);
                    for c in [
                        0.0,
                        f32::from_bits(1),
                        f32::from_bits(2),
                        f32::from_bits(0x007f_ffff),
                        f32::from_bits(0x0080_0000),
                    ] {
                        cases.push([a, b, c]);
                    }
                }
            }
        }
        // The top of the range: overflow to infinity, and the largest finite float.
        for (a, b, c) in [
            (f32::MAX, 1.0, 0.0),
            (f32::MAX, 1.0, f32::from_bits(0x7f7f_ffff)),
            (f32::MAX, 1.0000001, 0.0),
            (f32::MAX, 2.0, 0.0),
            (1.0e30, 1.0e30, 0.0),
            (3.0e38, 1.0, 3.0e38),
            (f32::MAX, 1.0, 1.0e31),
            (f32::MAX, 1.0, 1.0e30),
            (1.7e19, 2.0e19, 0.0),
        ] {
            cases.push([a, b, c]);
        }
        // The recursion's own decay: a bright dot on a field of zeros, `prev <- nu * prev + 0` until it is zero (through the
        // subnormals at sigma 0.3 to 4), and the same with a boundary scale and a post-scale at the end.
        for sigma in [0.3, 0.5, 1.0, 2.0, 4.0, 40.0] {
            let g = crate::effects::gaussian(sigma).unwrap();
            for start in [255.0f32, 235.0, 1.0] {
                let mut prev = start * g.boundary;
                for _ in 0..4000 {
                    cases.push([g.nu, prev, 0.0]);
                    cases.push([prev, g.post, 0.0]);
                    prev = g.nu.mul_add(prev, 0.0);
                    if prev == 0.0 {
                        break;
                    }
                }
            }
        }
        let mut data = Vec::with_capacity(cases.len() * 4);
        for [a, b, c] in &cases {
            data.extend([a.to_bits(), b.to_bits(), c.to_bits(), 0]);
        }
        let device = &gpu.device;
        let source = concat!(include_str!("shaders/fma.wgsl"), include_str!("shaders/fma_test.wgsl"));
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fma test"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fma test"),
            layout: None,
            module: &module,
            entry_point: Some("cs_test"),
            compilation_options: Default::default(),
            cache: None,
        });
        let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("triples"),
            contents: bytemuck::cast_slice(&data),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let size = (cases.len() * 8) as u64;
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("results"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fma test"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fma test") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("fma test"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups((cases.len() as u32).div_ceil(64), 1, 1);
        }
        enc.copy_buffer_to_buffer(&output, 0, &read, 0, size);
        gpu.queue.submit(Some(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        read.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(60)),
            })
            .expect("the GPU finishes");
        rx.recv().unwrap().unwrap();
        let mapped = read.slice(..).get_mapped_range().unwrap();
        let got: &[u32] = bytemuck::cast_slice(&mapped);
        let (mut bad_fma, mut bad_mul) = (Vec::new(), 0usize);
        for (i, [a, b, c]) in cases.iter().enumerate() {
            let (fused, plain) = (got[2 * i], got[2 * i + 1]);
            if fused != a.mul_add(*b, *c).to_bits() && bad_fma.len() < 5 {
                bad_fma.push((*a, *b, *c, f32::from_bits(fused), a.mul_add(*b, *c)));
            }
            if plain != (a * b).to_bits() {
                bad_mul += 1;
            }
        }
        let bad_total = cases
            .iter()
            .enumerate()
            .filter(|(i, [a, b, c])| got[2 * i] != a.mul_add(*b, *c).to_bits())
            .count();
        eprintln!(
            "{} triples: {bad_total} fused multiply-adds differ, {bad_mul} multiplies; first {bad_fma:?}",
            cases.len()
        );
        assert_eq!((bad_total, bad_mul), (0, 0), "first {bad_fma:?}");
    }

    /// The first three pictures tell the fused recursion from the unfused one by construction: the two models disagree on at least five
    /// samples of each, so a build that does not fuse cannot pass the probe by luck.
    #[test]
    fn the_probes_chosen_pictures_tell_the_fused_recursion_from_the_unfused_one() {
        for (w, h, sigma, seed) in &CASES[..3] {
            let (w, h) = (*w, *h);
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let mut rng = noise(*seed);
            let planes = [rng.bytes(w * h), rng.bytes(cw * ch), rng.bytes(cw * ch)];
            let g = gaussian(*sigma).unwrap();
            let apart: usize = planes
                .iter()
                .zip([(w, h), (cw, ch), (cw, ch)])
                .map(|(p, (pw, ph))| {
                    let (fused, plain) = (blur_plane(p, pw, ph, &g), blur_plane_fused(p, pw, ph, &g, [false; 4]));
                    fused.iter().zip(&plain).filter(|(a, b)| a != b).count()
                })
                .sum();
            assert!(
                apart >= 5,
                "{w}x{h} sigma {sigma} seed {seed}: the models disagree on {apart} samples"
            );
        }
    }

    /// How many samples of a picture's three planes differ between the fused model and the model with the sweeps in `fused` fused.
    fn disagreement((w, h, sigma, seed): (usize, usize, f64, u64), fused: [bool; 4]) -> usize {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let mut rng = noise(seed);
        let planes = [rng.bytes(w * h), rng.bytes(cw * ch), rng.bytes(cw * ch)];
        let g = gaussian(sigma).unwrap();
        planes
            .iter()
            .zip([(w, h), (cw, ch), (cw, ch)])
            .map(|(p, (pw, ph))| {
                let (want, other) = (blur_plane(p, pw, ph, &g), blur_plane_fused(p, pw, ph, &g, fused));
                want.iter().zip(&other).filter(|(a, b)| a != b).count()
            })
            .sum()
    }

    /// Each of the four sweeps has a picture that tells it fused from unfused: the model with that sweep alone unfused disagrees with
    /// the fused one on at least two samples of it (4, 34, 23 and 22 as chosen), so a build that fuses three sweeps of four and not the
    /// fourth cannot read `Match` by luck. (The blind spot this closes: with the first three pictures alone the horizontal forward
    /// sweep unfused went unseen.)
    #[test]
    fn every_sweep_unfused_alone_is_told_by_its_own_picture() {
        for (sweep, case) in SWEEP_CASES.into_iter().enumerate() {
            let mut fused = [true; 4];
            fused[sweep] = false;
            let own = disagreement(case, fused);
            assert!(own >= 2, "sweep {sweep} unfused alone: {case:?} disagrees on {own} samples");
        }
    }

    /// ... and over everything the probe draws, every single-sweep mutation of the model disagrees with the fused model somewhere, and so
    /// do the mutations of two, three and four sweeps: no combination of unfused sweeps is invisible to the probe's pictures.
    #[test]
    fn no_combination_of_unfused_sweeps_is_invisible_to_the_probes_pictures() {
        for mask in 1u8..16 {
            let fused: [bool; 4] = std::array::from_fn(|i| mask & (1 << i) == 0);
            let total: usize = CASES
                .into_iter()
                .chain(SWEEP_CASES)
                .map(|case| disagreement(case, fused))
                .sum();
            assert!(
                total >= 2,
                "sweeps unfused {mask:04b}: the probe's pictures disagree on {total} samples"
            );
        }
    }

    /// The probe, run against a model with one sweep unfused (a build that differs in that sweep alone, as the real FFmpeg here is not),
    /// says `Differs`: the pictures reach FFmpeg's output and the comparison is on every sample. (On a machine whose own `gblur` is not
    /// the fused recursion the probe says `Differs` before it gets there, which this reports and passes.)
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn a_model_unfused_in_one_sweep_alone_reads_differs() {
        if probe(&Budget::new(&TIMING)) != Blur::Match {
            eprintln!("this FFmpeg's gblur is not the fused recursion: the probe already says so");
            return;
        }
        for sweep in 0..4 {
            let mut fused = [true; 4];
            fused[sweep] = false;
            let found = probe_against(&Budget::new(&TIMING), &[], &|p, w, h, g| blur_plane_fused(p, w, h, g, fused));
            eprintln!("sweep {sweep} unfused: {found:?}");
            assert!(matches!(found, Blur::Differs(_)), "sweep {sweep}: {found:?}");
        }
    }

    /// The shapes tell the kernels apart and exercise both rules the probe can try without risking a crash, and **no shape has a plane one
    /// sample wide or tall, for any range of slice threads a graph can run on** (1 to 16 at each end, every pair: `KernelHazard::OneSample`
    /// crashes or corrupts FFmpeg, and the probe is run by a process that must not take it down): some plane is a hazard for the 16-lane
    /// kernel and not the 8-lane one, some the other way round, some are a hazard of too few rows for the threads and of a block of rows
    /// over a narrow plane, and some are clear of both (the exact ones the probe must find exact).
    #[test]
    fn the_shapes_tell_the_8_lane_kernel_from_the_16_lane_one_and_try_both_rules_at_every_range_of_threads() {
        use kerf_core::gblur::{plane_hazard, KernelHazard};
        for lo in 1..=16 {
            for hi in lo..=16 {
                let (mut only16, mut only8, mut slices, mut blocks, mut clear) = (0, 0, 0, 0, 0);
                for (w, h, t) in shape_cases((lo, hi)) {
                    for (pw, ph) in plane_dims((w, h)) {
                        assert!(
                            pw >= 2 && ph >= 2,
                            "range {lo}..={hi}: {w}x{h} at {t} threads has a plane {pw}x{ph}, one sample across"
                        );
                        let (a, b) = (
                            plane_hazard((pw as u32, ph as u32), t as u32, 8),
                            plane_hazard((pw as u32, ph as u32), t as u32, 16),
                        );
                        assert!(
                            a != Some(KernelHazard::OneSample) && b != Some(KernelHazard::OneSample),
                            "range {lo}..={hi}: a plane one sample wide or tall can crash FFmpeg ({w}x{h} at {t})"
                        );
                        only16 += usize::from(a.is_none() && b.is_some());
                        only8 += usize::from(a.is_some() && b.is_none());
                        slices += usize::from(a == Some(KernelHazard::EmptySlices) || b == Some(KernelHazard::EmptySlices));
                        blocks += usize::from(a == Some(KernelHazard::NarrowBlocks) || b == Some(KernelHazard::NarrowBlocks));
                        clear += usize::from(a.is_none() && b.is_none());
                    }
                }
                assert!(
                    only16 > 0 && only8 > 0 && slices > 0 && blocks > 0 && clear > 0,
                    "range {lo}..={hi}: {only16} {only8} {slices} {blocks} {clear}"
                );
            }
        }
        // ... and never more children than the probe's budget counts, on any range of threads a graph can run on.
        let most = (1..=64)
            .flat_map(|lo| (lo..=64).map(move |hi| (lo, hi)))
            .map(children)
            .max()
            .unwrap();
        assert!(most <= MAX_CHILDREN, "{most} children");
        assert_eq!(most, MAX_CHILDREN, "the bound is the real worst case");
    }

    /// What FFmpeg would answer for every child if its `gblur` were `model`, with the child at `missing` (if any) not running and the one at
    /// `wrong` (if any) off by a level on its first luma sample.
    fn answers(
        range: (usize, usize),
        model: &Model<'_>,
        missing: Option<usize>,
        wrong: Option<usize>,
    ) -> Vec<Option<(Planes, Planes)>> {
        jobs_for(range)
            .into_iter()
            .enumerate()
            .map(|(i, job)| {
                if missing == Some(i) {
                    return None;
                }
                let (case @ (w, h, sigma, _), _) = job.case();
                let planes = planes_of(case);
                let g = gaussian(sigma).expect("a sigma the probe takes");
                let mut got: Vec<Vec<u8>> = plane_dims((w, h))
                    .into_iter()
                    .enumerate()
                    .map(|(k, (pw, ph))| model(&planes[k], pw, ph, &g))
                    .collect();
                if wrong == Some(i) {
                    got[0][0] = got[0][0].wrapping_add(1);
                }
                let [y, u, v]: [Vec<u8>; 3] = got.try_into().expect("three planes");
                Some((planes, [y, u, v]))
            })
            .collect()
    }

    /// Every child answering as the model is a match; one that would not run is `Unknown` (nothing is said of it), and a **difference
    /// found in the others is still a difference** whichever child is missing, so a shape child that keeps failing on a machine whose
    /// kernel is not the model's does not make the probe ask again every backoff.
    #[test]
    fn a_child_that_would_not_run_is_unknown_unless_the_others_already_differ() {
        let model: &Model<'_> = &|plane, w, h, g| blur_plane(plane, w, h, g);
        let range = (3, 5);
        let jobs = jobs_for(range);
        assert!(jobs.len() <= MAX_CHILDREN);
        let (verdict, _) = judge(range, model, answers(range, model, None, None));
        assert_eq!(verdict, Blur::Match);
        let last = jobs.len() - 1;
        assert!(matches!(jobs[last], Job::Shape(..)), "the last child is a shape");
        for missing in [0, 10, last] {
            let (verdict, lanes) = judge(range, model, answers(range, model, Some(missing), None));
            assert_eq!((verdict, lanes), (Blur::Unknown, 0), "child {missing} missing");
            // A difference in a picture, whichever child is missing (a shape included).
            let wrong = if missing == 0 { 1 } else { 0 };
            let (verdict, _) = judge(range, model, answers(range, model, Some(missing), Some(wrong)));
            assert!(
                matches!(verdict, Blur::Differs(_)),
                "child {missing} missing, {wrong} wrong: {verdict:?}"
            );
        }
        // A shape that neither width's rule accounts for (40x40 at one thread: luma clear for both), with the last shape missing, is
        // `Differs` too.
        let clear = jobs
            .iter()
            .position(|j| matches!(j, Job::Shape(40, 40, 1)))
            .expect("the 40x40 shape");
        let (verdict, _) = judge(range, model, answers(range, model, Some(last), Some(clear)));
        assert!(matches!(verdict, Blur::Differs(_)), "{verdict:?}");
    }

    /// A host that is slow to start a process (about 5 s a child, which no host in use is near) still finishes the probe's worst case
    /// inside its deadline, with [`WORKERS`] at a time: it must not time out into `Unknown` on the shape stage alone.
    #[test]
    fn the_worst_case_of_children_fits_the_deadline_at_a_slow_start() {
        let rounds = MAX_CHILDREN.div_ceil(WORKERS) as u32;
        assert!(
            TIMING.overall / rounds >= Duration::from_secs(5),
            "{rounds} rounds in {:?}",
            TIMING.overall
        );
        assert!(TIMING.per_run <= TIMING.overall);
    }

    /// On this machine the probe finds the kernel's vector width, and the width it finds is the one whose rule holds (an AVX2 run on an
    /// AVX-512 machine finds 8, and a machine with neither kernel is not a `Match`).
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn the_probe_finds_the_kernels_vector_width() {
        let model: &Model<'_> = &|plane, w, h, g| blur_plane(plane, w, h, g);
        let (own, lanes) = probe_found(&Budget::new(&TIMING), &[], model);
        eprintln!("gblur: {own:?}, {lanes} lanes");
        if own != Blur::Match {
            return;
        }
        assert!(matches!(lanes, 8 | 16), "{lanes}");
        // AVX2 on a machine that has AVX-512 as well: 8 lanes.
        if lanes == 16 {
            let (avx2, narrow) = probe_found(&Budget::new(&TIMING), &["-cpuflags", "-avx512"], model);
            eprintln!("gblur with -cpuflags -avx512: {avx2:?}, {narrow} lanes");
            assert_eq!((avx2, narrow), (Blur::Match, 8));
        }
    }

    /// The probe has teeth: FFmpeg run without its AVX2 and AVX-512 kernels (`-cpuflags -avx2-avx512`: SSE4's horizontal pass, the C
    /// vertical one), or with no SIMD at all (`-cpuflags 0`: the C loop, which does not fuse), is not the model, and the probe says
    /// `Differs`, not `Match`.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn a_build_without_the_fused_kernels_is_differs() {
        for flags in [["-cpuflags", "0"], ["-cpuflags", "-avx2-avx512"]] {
            let found = probe_with(&Budget::new(&TIMING), &flags);
            eprintln!("gblur with {flags:?}: {found:?}");
            assert!(matches!(found, Blur::Differs(_)), "{flags:?}: {found:?}");
        }
    }

    /// The probe itself, on the FFmpeg in use (`#[ignore]`d: it needs one). It must be able to tell, and says the same twice.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn this_ffmpeg_blurs_as_the_model_does_or_the_probe_says_where_not() {
        let first = probe(&Budget::new(&TIMING));
        assert_ne!(first, Blur::Unknown, "the probe could not run");
        assert_eq!(first, probe(&Budget::new(&TIMING)));
        eprintln!("gblur: {first:?}");
    }
}
