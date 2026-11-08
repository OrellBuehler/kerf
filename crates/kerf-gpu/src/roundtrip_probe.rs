//! Whether this FFmpeg takes a translucent layer through RGB and back as [`crate::roundtrip`] does — measured once per process on
//! the filter text the engine writes ([`kerf_core::opacity_filter`]), never assumed and never keyed on a version string.
//! `GpuCaps::translucent_approximate` is cleared only on a [`Translucency::Match`]; otherwise every layer that takes the round trip is
//! FFmpeg's.
//!
//! `colorchannelmixer` only takes RGB, so for a clip below full opacity the graph converts the layer `yuva420p -> argb`, mixes, and
//! converts `argb -> yuva420p` before `overlay` sees it. Both conversions are swscale's, and they run on this machine's kernels:
//! the way in is the C `yuva2argb` on x86 (`ARGB` has no x86 SIMD version) and NEON on arm64; the way back is the generic scaler
//! whose vertical pass is `yuv2yuvX` in SIMD on x86 for every chroma row but the last (the same pass as a grade's way back:
//! [`crate::grade`]) and another kernel elsewhere. The model follows the x86 pair to the sample; another CPU's FFmpeg is
//! a level of chroma from it, and this probe says so before a frame is drawn.
//!
//! The probe runs the chain over noise in all three planes (every table entry, every rounding), at three opacities (`lrint`
//! of an exact half, a value near full, one near empty), untagged and BT.709-tagged, and holds all four planes of the layer
//! `overlay` is handed to the model with no tolerance. A probe that could not tell is [`Translucency::Unknown`]: asked again
//! after a backoff, and nothing translucent is drawn until it answers.

use std::time::{Duration, Instant};

use kerf_core::YuvMatrix;

use crate::geometry::ffmpeg_alpha;
use crate::grade::rgb_to_yuv420;
use crate::measure::{self, Budget, Probe, Timing, Verdict};
use crate::roundtrip::Yuv2Rgb;

/// What the probe found: [`Verdict::Match`] (the chain is the model's, every plane of every case), [`Verdict::Differs`] (it is
/// not: the detail names the case) or [`Verdict::Unknown`].
pub type Translucency = Verdict;

/// The probe picture: even sides (an odd one is never drawn), several rows so the vertical filter has an interior and a last row.
const PROBE: (usize, usize) = (48, 32);
const TIMING: Timing = Timing {
    backoff: Duration::from_secs(30),
    overall: Duration::from_secs(30),
    per_run: Duration::from_secs(20),
};

static STATE: Probe = Probe::new();

/// What is known without running anything: the measured answer, else [`Translucency::Unknown`]. What the compositor's caps are made
/// from.
pub fn measured() -> Translucency {
    measure::known(&STATE)
}

/// The answer, measured on the first call (a handful of short `ffmpeg` children, bounded by 20 s each and 30 s in all, no lock
/// held: a blocking thread's, ahead of the frame that needs it, [`crate::prepare`]). A caller that arrives while it runs waits for
/// it and takes its answer. `Unknown` is not remembered but for a backoff.
pub fn translucency() -> Translucency {
    measure::cached(&STATE, Instant::now(), TIMING, probe)
}

/// [`translucency`] for the runtime self-test, which draws a translucent frame only on a `Match` (an `Unknown` that survives a
/// second try after the backoff holds the translucent path back for the process, [`crate::measure::checked`]).
pub(crate) fn translucency_for_selftest() -> Translucency {
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

/// One case of the probe: the opacity and how the picture is tagged.
struct Case {
    why: &'static str,
    opacity: f64,
    /// `setparams=colorspace=...` ahead of the chain (`None`: untagged).
    tag: Option<&'static str>,
    /// The matrix the layer goes out of YCbCr with.
    forward: YuvMatrix,
}

const CASES: [Case; 4] = [
    Case {
        why: "an untagged layer at 60 %",
        opacity: 0.6,
        tag: None,
        forward: YuvMatrix::Bt601,
    },
    Case {
        why: "an untagged layer at 99.9 %",
        opacity: 0.999,
        tag: None,
        forward: YuvMatrix::Bt601,
    },
    Case {
        why: "an untagged layer at 4 %",
        opacity: 0.04,
        tag: None,
        forward: YuvMatrix::Bt601,
    },
    Case {
        why: "a BT.709-tagged layer at 60 %",
        opacity: 0.6,
        tag: Some("bt709"),
        forward: YuvMatrix::Bt709,
    },
];

/// The layer `overlay` is handed, as the model makes it: out of YCbCr with the layer's matrix (swscale's C tables), the alpha
/// `colorchannelmixer` writes, and back with `back`'s (the generic scaler on the x86 vertical pass). `[Y, U, V, A]`.
fn model(planes: [&[u8]; 3], opacity: f64, forward: YuvMatrix, back: YuvMatrix) -> [Vec<u8>; 4] {
    model_of(planes, PROBE, opacity, forward, back)
}

/// [`model`] for a picture of another (even) size: what the blend probe holds its translucent clip to.
pub(crate) fn model_of(
    planes: [&[u8]; 3],
    (w, h): (usize, usize),
    opacity: f64,
    forward: YuvMatrix,
    back: YuvMatrix,
) -> [Vec<u8>; 4] {
    let t = Yuv2Rgb::new(forward);
    let cw = w / 2;
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let c = (y / 2) * cw + x / 2;
            let [r, g, b] = t.rgb(
                i32::from(planes[0][y * w + x]),
                i32::from(planes[1][c]),
                i32::from(planes[2][c]),
            );
            rgb.extend([r as u8, g as u8, b as u8]);
        }
    }
    let [y, u, v] = rgb_to_yuv420(&rgb, w, h, back);
    [y, u, v, vec![ffmpeg_alpha(opacity); w * h]]
}

/// The graph of a case: the engine's own opacity filter between the conversions the graph puts on either side of it.
fn graph(case: &Case) -> String {
    let mut parts = Vec::new();
    if let Some(tag) = case.tag {
        parts.push(format!("setparams=colorspace={tag}"));
    }
    parts.push("format=yuva420p".to_string());
    parts.push(kerf_core::opacity_filter(case.opacity));
    parts.push("format=yuva420p".to_string());
    parts.join(",")
}

fn probe(budget: &Budget) -> Translucency {
    probe_with(budget, &[])
}

/// [`probe`] with `cpu` ahead of FFmpeg's other arguments (the tests run it with `-cpuflags 0`, a machine with none of the SIMD kernels).
fn probe_with(budget: &Budget, cpu: &[&str]) -> Translucency {
    let (w, h) = PROBE;
    let cw = w / 2;
    let mut rng = Lcg(0x7d3a_91c4_55e2_08b1);
    let (y, u, v) = (rng.bytes(w * h), rng.bytes(cw * (h / 2)), rng.bytes(cw * (h / 2)));
    let raw: Vec<u8> = [&y, &u, &v].into_iter().flatten().copied().collect();
    let size = format!("{w}x{h}");
    for case in &CASES {
        let graph = graph(case);
        let args: Vec<&str> = cpu
            .iter()
            .copied()
            .chain([
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "-s",
                &size,
                "-i",
                "pipe:0",
                "-vf",
                &graph,
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuva420p",
                "pipe:1",
            ])
            .collect();
        let Some(out) = budget.ffmpeg(&args, raw.clone()) else {
            return Translucency::Unknown;
        };
        let (luma, chroma) = (w * h, cw * (h / 2));
        if out.len() != 2 * luma + 2 * chroma {
            return Translucency::Unknown;
        }
        let planes = [
            &out[..luma],
            &out[luma..luma + chroma],
            &out[luma + chroma..luma + 2 * chroma],
            &out[luma + 2 * chroma..],
        ];
        // The matrix the way back uses is the composite's, which a lone chain does not have: BT.601 on an FFmpeg whose
        // composite is untagged, the layer's own where the graph negotiates it. A tagged case is the model's when the
        // planes are either's; an untagged one has one.
        let backs: &[YuvMatrix] = if case.forward == YuvMatrix::Bt601 {
            &[YuvMatrix::Bt601]
        } else {
            &[YuvMatrix::Bt601, case.forward]
        };
        let mut worst = None;
        for back in backs {
            let want = model([&y, &u, &v], case.opacity, case.forward, *back);
            let apart: Vec<usize> = planes
                .iter()
                .zip(&want)
                .map(|(got, want)| got.iter().zip(want).filter(|(a, b)| a != b).count())
                .collect();
            if apart.iter().all(|n| *n == 0) {
                worst = None;
                break;
            }
            worst = Some(apart);
        }
        if let Some(apart) = worst {
            return Translucency::Differs(format!(
                "{}: the layer FFmpeg hands `overlay` differs from the round trip's in {} luma, {} U, {} V and {} alpha samples",
                case.why, apart[0], apart[1], apart[2], apart[3]
            ));
        }
    }
    Translucency::Match
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
        assert_eq!(measure::known(&state), Translucency::Unknown, "nothing measured yet");
        assert_eq!(
            measure::cached(&state, t0, TIMING, |_| Translucency::Unknown),
            Translucency::Unknown
        );
        assert_eq!(
            measure::cached(&state, t0 + Duration::from_secs(29), TIMING, |_| panic!(
                "probed inside the backoff"
            )),
            Translucency::Unknown
        );
        let after = Instant::now() + TIMING.backoff + Duration::from_secs(1);
        let differs = Translucency::Differs("an untagged layer at 60 %: 3 samples".into());
        assert_eq!(measure::cached(&state, after, TIMING, |_| differs.clone()), differs);
        assert_eq!(measure::known(&state), differs);
        assert_eq!(measure::cached(&state, after, TIMING, |_| panic!("measured twice")), differs);
    }

    /// The graph is the engine's own text between the two conversions.
    #[test]
    fn the_graph_is_the_engines_opacity_filter() {
        assert_eq!(graph(&CASES[0]), "format=yuva420p,colorchannelmixer=aa=0.6,format=yuva420p");
        assert_eq!(
            graph(&CASES[3]),
            "setparams=colorspace=bt709,format=yuva420p,colorchannelmixer=aa=0.6,format=yuva420p"
        );
    }

    /// The probe has teeth: FFmpeg run with none of its SIMD kernels (`-cpuflags 0`: the C vertical scaler, as an arm64 or a bare build runs) is
    /// not the model, and the probe says `Differs`, not `Match`.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn a_build_with_no_simd_kernels_is_differs() {
        let found = probe_with(&Budget::new(&TIMING), &["-cpuflags", "0"]);
        eprintln!("the translucent round trip with -cpuflags 0: {found:?}");
        assert!(matches!(found, Translucency::Differs(_)), "{found:?}");
    }

    /// The probe itself, on the FFmpeg in use (`#[ignore]`d: it needs one). It must be able to tell, and says the same twice.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn this_ffmpeg_takes_a_translucent_layer_through_rgb_as_the_model_does_or_the_probe_says_where_not() {
        let first = probe(&Budget::new(&TIMING));
        assert_ne!(first, Translucency::Unknown, "the probe could not run");
        assert_eq!(first, probe(&Budget::new(&TIMING)));
        eprintln!("the translucent round trip: {first:?}");
    }
}
