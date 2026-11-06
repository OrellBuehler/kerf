//! Whether this FFmpeg converts a finished `yuv420p` composite to RGB as the compositor's display pass does — measured once per process,
//! on FFmpeg's own conversion (what the still's PNG and the RGB pipe are read through), never assumed and never keyed on a version string.
//!
//! The preview shows RGB, and the compositor converts its composite once at the end (`composite.wgsl` `fs_convert`) in the integers of
//! swscale's x86 `yuv420p -> rgb24` kernel (`ff_yuv_420_rgb24_ssse3`: `pmulhw` on 13-bit coefficients in 16-bit lanes,
//! [`crate::grade::X86Kernel`]). That is the conversion an x86 FFmpeg with SSSE3 runs; another CPU's FFmpeg runs another kernel (NEON,
//! or the C one with its table lookups) that agrees to a level or two and not to the level (with `-cpuflags 0` the preview RGBA read up to
//! two levels off in about 45% of samples). On such a machine the preview **must not show GPU-converted RGB as FFmpeg's**, so the
//! compositor refuses to hand RGB out ([`Display`] not [`Display::Match`]: `GpuError::Unsupported`, the preview is FFmpeg's) and
//! keeps handing out YUV planes, which no conversion touches.
//!
//! What FFmpeg does with the composite in the engine's own still (`StillOutput::File` PNG, `RgbPipe`) is **no conversion text at all**: the
//! graph ends in `yuv420p` (or the 4:4:4 the scopes read) and the output's `-pix_fmt rgb24` makes FFmpeg insert its scaler, whose
//! unscaled `yuv420p -> rgb24` is the kernel in question; the matrix is the composite's tag (BT.601 untagged). So the probe runs exactly
//! that, as a bare `-pix_fmt rgb24` (the untagged composite), and, for the matrix a tag names, through `scale=in_color_matrix=<m>:in_range=tv`:
//! the same scaler, told its input. That text is not one the engine writes (the blend chain's `blend.rs` writes
//! `scale=flags=neighbor:in_color_matrix=<m>:in_range=tv`, for its own RGB round trip; the bare form is this probe's and the parity
//! harness's, which names a plan's matrix to a still with it). `tests/parity.rs`
//! (`the_display_probes_text_is_the_conversion_a_stills_png_runs`) holds both to the real still's PNG. The probe runs it over noise in all three planes at several sizes (a width of one chroma sample, a vector's width and a
//! tail past it) and holds every sample to the model with no tolerance. A probe that could not tell is [`Display::Unknown`]: asked again
//! after a backoff, and no RGB is handed out until it answers.

use std::time::{Duration, Instant};

use kerf_core::YuvMatrix;

use crate::grade::yuv420_to_rgb;
use crate::measure::{self, Budget, Probe, Timing, Verdict};

/// What the probe found: [`Verdict::Match`] (the conversion is the model's, every sample of every case), [`Verdict::Differs`]
/// (it is not: the detail names the case) or [`Verdict::Unknown`].
pub type Display = Verdict;

const TIMING: Timing = Timing {
    backoff: Duration::from_secs(30),
    overall: Duration::from_secs(30),
    per_run: Duration::from_secs(20),
};

static STATE: Probe = Probe::new();

/// The pictures: even sides, from one chroma sample to a vector's width and a tail past it, with several rows so the kernel's
/// row pairs are looked at.
const SIZES: [(usize, usize); 6] = [(2, 2), (6, 4), (18, 10), (66, 34), (130, 18), (202, 100)];

/// What is known without running anything: the measured answer, else [`Display::Unknown`]. What a render reads.
pub fn measured() -> Display {
    measure::known(&STATE)
}

/// Whether the compositor hands out RGB on this machine: the probe has measured a [`Display::Match`] (nothing is handed out, and no
/// span of a cut is the GPU's, before it has). What the render bar reads.
pub fn offered() -> bool {
    measured() == Display::Match
}

/// The answer, measured on the first call (about twenty short `ffmpeg` children, bounded by 20 s each and 30 s in all, no lock
/// held: a blocking thread's, ahead of the frame that needs it, [`crate::prepare`]). A caller that arrives while it runs waits for it
/// and takes its answer. `Unknown` is not remembered but for a backoff.
pub fn display() -> Display {
    measure::cached(&STATE, Instant::now(), TIMING, probe)
}

/// [`display`] for the runtime self-test, which has RGB frames to draw only on a `Match` (an `Unknown` that survives a second try
/// after the backoff holds the preview back for the process, [`crate::measure::checked`]).
pub(crate) fn display_for_selftest() -> Display {
    measure::checked(&STATE, TIMING, probe)
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

/// `scale`'s name for a matrix.
fn name(matrix: YuvMatrix) -> &'static str {
    match matrix {
        YuvMatrix::Bt601 => "bt601",
        YuvMatrix::Bt709 => "bt709",
        YuvMatrix::Bt2020 => "bt2020nc",
    }
}

/// The filter ahead of the conversion the probe runs: `scale` told the composite's matrix (`tagged`, how a plan's matrix is named to
/// FFmpeg), or none (`None`: the bare `-pix_fmt rgb24` an untagged still's PNG is made with).
pub fn conversion_filter(matrix: YuvMatrix, tagged: bool) -> Option<String> {
    tagged.then(|| format!("scale=in_color_matrix={}:in_range=tv", name(matrix)))
}

fn filter(matrix: YuvMatrix, explicit: bool) -> Option<String> {
    conversion_filter(matrix, explicit)
}

/// The cases: every matrix through the explicit scale, and BT.601 through none.
const CASES: [(YuvMatrix, bool); 4] = [
    (YuvMatrix::Bt601, true),
    (YuvMatrix::Bt709, true),
    (YuvMatrix::Bt2020, true),
    (YuvMatrix::Bt601, false),
];

fn probe(budget: &Budget) -> Display {
    probe_with(budget, &[])
}

/// [`probe`] with `cpu` ahead of FFmpeg's other arguments (the tests run it with `-cpuflags 0`, a machine with none of the SIMD kernels).
fn probe_with(budget: &Budget, cpu: &[&str]) -> Display {
    let mut rng = Lcg(0x3c6e_f372_fe94_f82b);
    for (w, h) in SIZES {
        let (cw, ch) = (w / 2, h / 2);
        let planes = [rng.bytes(w * h), rng.bytes(cw * ch), rng.bytes(cw * ch)];
        let raw: Vec<u8> = planes.iter().flatten().copied().collect();
        let size = format!("{w}x{h}");
        for (matrix, explicit) in CASES {
            let filter = filter(matrix, explicit);
            let mut args: Vec<&str> = cpu.to_vec();
            args.extend(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", &size, "-i", "pipe:0"]);
            if let Some(filter) = &filter {
                args.extend(["-vf", filter]);
            }
            args.extend(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
            let Some(out) = budget.ffmpeg(&args, raw.clone()) else {
                return Display::Unknown;
            };
            if out.len() != w * h * 3 {
                return Display::Unknown;
            }
            let want = yuv420_to_rgb([&planes[0], &planes[1], &planes[2]], w, h, matrix);
            let apart = out.iter().zip(&want).filter(|(a, b)| a != b).count();
            if apart != 0 {
                return Display::Differs(format!(
                    "yuv420p -> rgb24 ({} {}) over {w}x{h}: {apart} samples differ from the x86 kernel's",
                    name(matrix),
                    if explicit { "through scale" } else { "untagged" }
                ));
            }
        }
    }
    Display::Match
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The memory of the probe: a probe that could not tell is asked again after the backoff and not before, and a match or a
    /// difference is kept for good.
    #[test]
    fn an_unknown_is_asked_again_after_the_backoff_and_a_match_or_a_difference_is_kept() {
        let state = Probe::new();
        let t0 = Instant::now();
        assert_eq!(measure::known(&state), Display::Unknown, "nothing measured yet");
        assert_eq!(measure::cached(&state, t0, TIMING, |_| Display::Unknown), Display::Unknown);
        assert_eq!(
            measure::cached(&state, t0 + Duration::from_secs(29), TIMING, |_| panic!(
                "probed inside the backoff"
            )),
            Display::Unknown
        );
        let after = Instant::now() + TIMING.backoff + Duration::from_secs(1);
        let differs = Display::Differs("yuv420p -> rgb24 (bt601 through scale) over 18x10: 7 samples".into());
        assert_eq!(measure::cached(&state, after, TIMING, |_| differs.clone()), differs);
        assert_eq!(measure::known(&state), differs);
        assert_eq!(measure::cached(&state, after, TIMING, |_| panic!("measured twice")), differs);
    }

    /// The probe's text is the bare `scale` with the matrix named (the parity harness's), not the blend chain's `scale=flags=neighbor:...`
    /// (`blend.rs`), and none at all for an untagged composite; `the_display_probes_text_is_the_conversion_a_stills_png_runs` holds it
    /// to a still's PNG.
    #[test]
    fn the_scale_is_the_bare_one_the_parity_harness_names_a_matrix_with() {
        assert_eq!(
            filter(YuvMatrix::Bt709, true).as_deref(),
            Some("scale=in_color_matrix=bt709:in_range=tv")
        );
        assert_eq!(
            filter(YuvMatrix::Bt2020, true).as_deref(),
            Some("scale=in_color_matrix=bt2020nc:in_range=tv")
        );
        assert_eq!(filter(YuvMatrix::Bt601, false), None);
    }

    /// The probe has teeth: FFmpeg run with none of its SIMD kernels (`-cpuflags 0`: the C conversion's tables) is not the x86 kernel,
    /// and the probe says `Differs`, not `Match`.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn a_build_with_no_simd_kernels_is_differs() {
        let found = probe_with(&Budget::new(&TIMING), &["-cpuflags", "0"]);
        eprintln!("yuv420p -> rgb24 with -cpuflags 0: {found:?}");
        assert!(matches!(found, Display::Differs(_)), "{found:?}");
    }

    /// The probe itself, on the FFmpeg in use (`#[ignore]`d: it needs one). It must be able to tell, and says the same twice.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn this_ffmpeg_converts_to_rgb_as_the_display_pass_does_or_the_probe_says_where_not() {
        let first = probe(&Budget::new(&TIMING));
        assert_ne!(first, Display::Unknown, "the probe could not run");
        assert_eq!(first, probe(&Budget::new(&TIMING)));
        eprintln!("the display conversion: {first:?}");
    }
}
