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
//!
//! The same chain also takes a **masked** layer (`geq` writes the alpha plane ahead of the opacity): its cases hand the chain a
//! picture whose alpha plane holds every level in several orders, at opacities where `lrint` meets a tie (an exact half, quarters)
//! and where it does not, and hold the alpha plane it ends with to [`kerf_core::layer_geometry::ffmpeg_alpha_of`] level by level
//! (and the colour planes, which the alpha does not touch, to the model's). A build that maps the levels another way is `Differs`
//! for the masked layer and the unmasked one alike: the verdict is one.

use std::time::{Duration, Instant};

use kerf_core::YuvMatrix;

use crate::geometry::{ffmpeg_alpha, ffmpeg_alpha_of};
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
    /// The picture comes with an alpha plane that holds every level (a mask's), not the opaque one `format=yuva420p` adds.
    masked: bool,
}

const CASES: [Case; 9] = [
    Case {
        why: "an untagged layer at 60 %",
        opacity: 0.6,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: false,
    },
    Case {
        why: "an untagged layer at 99.9 %",
        opacity: 0.999,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: false,
    },
    Case {
        why: "an untagged layer at 4 %",
        opacity: 0.04,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: false,
    },
    Case {
        why: "a BT.709-tagged layer at 60 %",
        opacity: 0.6,
        tag: Some("bt709"),
        forward: YuvMatrix::Bt709,
        masked: false,
    },
    Case {
        why: "a masked untagged layer at 50 %, where every odd level is a tie",
        opacity: 0.5,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: true,
    },
    Case {
        why: "a masked untagged layer at 25 %",
        opacity: 0.25,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: true,
    },
    Case {
        why: "a masked untagged layer at 85 %",
        opacity: 0.85,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: true,
    },
    Case {
        why: "a masked untagged layer at 4 %",
        opacity: 0.04,
        tag: None,
        forward: YuvMatrix::Bt601,
        masked: true,
    },
    Case {
        why: "a masked BT.709-tagged layer at 99.9 %",
        opacity: 0.999,
        tag: Some("bt709"),
        forward: YuvMatrix::Bt709,
        masked: true,
    },
];

/// The layer `overlay` is handed, as the model makes it: out of YCbCr with the layer's matrix (swscale's C tables), the alpha
/// `colorchannelmixer` writes (a clip's `fade alpha=1` steps run on it, so the plane is not [`ffmpeg_alpha`] of the opacity alone,
/// [`kerf_core::layer_geometry::ffmpeg_alpha_ramps`]), and back with `back`'s (the generic scaler on the x86 vertical pass). `[Y, U, V, A]`.
/// For a picture of any (even) size: what the blend probe holds its translucent clip to.
pub(crate) fn model_with_alpha(
    planes: [&[u8]; 3],
    (w, h): (usize, usize),
    alpha: u8,
    forward: YuvMatrix,
    back: YuvMatrix,
) -> [Vec<u8>; 4] {
    model_with_alpha_plane(planes, (w, h), &vec![alpha; w * h], forward, back)
}

/// [`model_with_alpha`] with the alpha plane given as it ends: one level a sample.
pub(crate) fn model_with_alpha_plane(
    planes: [&[u8]; 3],
    (w, h): (usize, usize),
    alpha: &[u8],
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
    [y, u, v, alpha.to_vec()]
}

/// The alpha plane a masked case hands the chain: every level four times over in four different orders (strides coprime with 256, so each
/// pass is a permutation and the levels meet every column and row phase), then noise.
fn mask_levels(n: usize, rng: &mut Lcg) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    for stride in [1usize, 37, 101, 173] {
        out.extend((0..256).map(|i| ((i * stride + stride / 2) % 256) as u8));
    }
    out.extend(rng.bytes(n.saturating_sub(out.len())));
    out.truncate(n);
    out
}

/// The alpha plane the model ends a masked case with: `colorchannelmixer`'s mix and the conversion's scale, for each level.
fn masked_alpha(levels: &[u8], opacity: f64) -> Vec<u8> {
    levels.iter().map(|l| ffmpeg_alpha_of(*l, opacity, &[])).collect()
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
    probe_model(budget, cpu, &masked_alpha)
}

/// [`probe_with`] holding the masked cases to `masked`'s alpha plane (`levels`, `opacity`) instead of the model's: how the tests show those
/// cases can tell a map of the levels that is another from the one FFmpeg makes.
fn probe_model(budget: &Budget, cpu: &[&str], masked: &dyn Fn(&[u8], f64) -> Vec<u8>) -> Translucency {
    let (w, h) = PROBE;
    let cw = w / 2;
    let mut rng = Lcg(0x7d3a_91c4_55e2_08b1);
    let (y, u, v) = (rng.bytes(w * h), rng.bytes(cw * (h / 2)), rng.bytes(cw * (h / 2)));
    let levels = mask_levels(w * h, &mut rng);
    let size = format!("{w}x{h}");
    for case in &CASES {
        let graph = graph(case);
        let (pix_fmt, raw): (&str, Vec<u8>) = if case.masked {
            ("yuva420p", [&y, &u, &v, &levels].into_iter().flatten().copied().collect())
        } else {
            ("yuv420p", [&y, &u, &v].into_iter().flatten().copied().collect())
        };
        let args: Vec<&str> = cpu
            .iter()
            .copied()
            .chain([
                "-f",
                "rawvideo",
                "-pix_fmt",
                pix_fmt,
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
        let Some(out) = budget.ffmpeg(&args, raw) else {
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
        let alpha = if case.masked {
            masked(&levels, case.opacity)
        } else {
            vec![ffmpeg_alpha(case.opacity); w * h]
        };
        let mut worst = None;
        for back in backs {
            let want = model_with_alpha_plane([&y, &u, &v], PROBE, &alpha, case.forward, *back);
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

    /// The masked cases have teeth: a model that maps the levels another way (truncating `level * opacity` where `colorchannelmixer` rounds
    /// it, or rounding half up where `lrint` goes to even, or skipping the conversion's scale) is `Differs`, naming a masked case, on
    /// this FFmpeg, whose unmasked cases the same run finds the model's.
    #[test]
    #[ignore = "needs ffmpeg"]
    fn a_map_of_the_levels_that_is_not_ffmpegs_is_differs_in_a_masked_case() {
        let truncated = |levels: &[u8], op: f64| {
            levels
                .iter()
                .map(|l| ((((f64::from(*l) * op) as u32) * 256 + 127) / 255).min(255) as u8)
                .collect::<Vec<u8>>()
        };
        let half_up = |levels: &[u8], op: f64| {
            levels
                .iter()
                .map(|l| ((((f64::from(*l) * op).round() as u32) * 256 + 127) / 255).min(255) as u8)
                .collect::<Vec<u8>>()
        };
        let unscaled = |levels: &[u8], op: f64| {
            levels
                .iter()
                .map(|l| (f64::from(*l) * op).round_ties_even() as u8)
                .collect::<Vec<u8>>()
        };
        for (name, model) in [
            ("truncated", &truncated as &dyn Fn(&[u8], f64) -> Vec<u8>),
            ("half up", &half_up),
            ("unscaled", &unscaled),
        ] {
            let found = probe_model(&Budget::new(&TIMING), &[], model);
            assert!(
                matches!(&found, Translucency::Differs(why) if why.contains("a masked")),
                "{name}: {found:?}"
            );
        }
        assert_eq!(probe_model(&Budget::new(&TIMING), &[], &masked_alpha), Translucency::Match);
    }

    /// The alpha a mask ends with, for **every level** (a 16x16 picture whose alpha plane is 0 to 255) at every opacity where `lrint` meets a
    /// tie or a rounding of the conversion's scale could: the 255 grid points' ties and the doubles either side of them, the ties of
    /// `level * opacity` for the levels that make most (`(2j + 1) / (2 n)`), the extremes. Thirty-two opacities share a run (each is its own
    /// `colorchannelmixer ... format=yuva420p` branch, the pictures stacked), and every level of every one is the model's, `ffmpeg_alpha_of`.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn every_level_of_a_masks_alpha_ends_as_the_model_says_at_every_tie() {
        let next = |x: f64| f64::from_bits(x.to_bits() + 1);
        let prev = |x: f64| f64::from_bits(x.to_bits() - 1);
        let mut ops: Vec<f64> = vec![0.0, 1e-9, 0.0001, 0.003, 0.04, 0.1, 0.5, 0.85, 0.999, prev(1.0), 1.0 - 1e-9];
        for k in 0..255u32 {
            let tie = (f64::from(k) + 0.5) / 255.0;
            ops.extend([tie, next(tie), prev(tie)]);
        }
        for n in [2u32, 3, 4, 5, 6, 8, 10, 12, 16, 20, 51, 85, 100, 127, 128, 170, 200, 254, 255] {
            for j in 0..n {
                let tie = (f64::from(2 * j + 1)) / f64::from(2 * n);
                if tie < 1.0 {
                    ops.extend([tie, next(tie)]);
                }
            }
        }
        ops.dedup();
        let budget = Budget::new(&TIMING);
        let (side, mut checked) = (16usize, 0usize);
        let y = vec![90u8; side * side];
        let (u, v) = (vec![110u8; side * side / 4], vec![150u8; side * side / 4]);
        let levels: Vec<u8> = (0..=255u8).collect();
        let raw: Vec<u8> = [&y, &u, &v, &levels].into_iter().flatten().copied().collect();
        for chunk in ops.chunks(32) {
            let n = chunk.len();
            let branches: String = (0..n).map(|i| format!("[a{i}]")).collect();
            let mut graph = format!("[0:v]split={n}{branches};");
            for (i, op) in chunk.iter().enumerate() {
                graph += &format!("[a{i}]{},format=yuva420p[b{i}];", kerf_core::opacity_filter(*op));
            }
            graph += &(0..n).map(|i| format!("[b{i}]")).collect::<String>();
            graph += &format!("hstack=inputs={n},format=yuva420p");
            let args = [
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuva420p",
                "-s",
                "16x16",
                "-i",
                "pipe:0",
                "-filter_complex",
                &graph,
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuva420p",
                "pipe:1",
            ];
            let out = budget.ffmpeg(&args, raw.clone()).expect("ffmpeg ran");
            let wide = side * n;
            assert_eq!(out.len(), wide * side * 2 + wide * side / 2, "{n} pictures stacked");
            let alpha = &out[wide * side + wide * side / 2..];
            for (i, op) in chunk.iter().enumerate() {
                for row in 0..side {
                    for col in 0..side {
                        let level = levels[row * side + col];
                        let got = alpha[row * wide + i * side + col];
                        let want = ffmpeg_alpha_of(level, *op, &[]);
                        assert_eq!(got, want, "level {level} at opacity {op:e}");
                        checked += 1;
                    }
                }
            }
        }
        eprintln!(
            "{} opacities x 256 levels = {checked} alpha samples equal the model's",
            ops.len()
        );
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
