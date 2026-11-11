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
//!
//! A **chroma-keyed** layer takes the same chain with `chromakey`'s alpha plane in front of the opacity (`format=yuva420p,chromakey=...,
//! colorchannelmixer=aa=<opacity>`): the key's levels are the mask's, as far as the round trip can tell. Its cases run the engine's own
//! `chromakey` text ([`kerf_core::chroma_key_filter`]) over a picture whose chroma is a ramp (the key is a distance in the chroma, so
//! the plane takes many levels), split the keyed picture in one graph (so both branches have the same slice threads, which the key's
//! alpha depends on), and hold the alpha the opacity's branch ends with to [`kerf_core::layer_geometry::ffmpeg_alpha_of`] of the alpha
//! the other branch (the key alone) ends with, level by level, and the colour planes to the model's. The key's alpha itself is
//! `chromakey`'s, held to the sample elsewhere (`effects`, `the_chroma_key_alpha_is_the_filters_plane_by_plane`). The verdict is still one: a
//! keyed case that differs is `Differs` for every translucent layer, plain and masked ones included.

use std::time::{Duration, Instant};

use kerf_core::YuvMatrix;

use crate::geometry::{ffmpeg_alpha, ffmpeg_alpha_of};
use crate::measure::{self, Budget, Probe, Timing, Verdict};
use crate::roundtrip::{self, OddWidth};

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

/// What the odd-size cases found: how this FFmpeg's way back makes the last chroma column of a picture of odd width
/// ([`OddWidth`]), or that no model the compositor has holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OddTranslucency {
    /// The sums of pixel pairs, the missing second pixel of the last pair 0, 0, 0 (6.1).
    Pairs,
    /// The chroma made for every pixel and scaled to the plane's width (9.0).
    Scaled,
    /// Neither: the detail names the case.
    Differs(String),
    /// Not measured yet, or the probe could not tell (asked again after a backoff).
    Unknown,
}

impl OddTranslucency {
    /// The model to draw an odd picture by, if there is one.
    pub fn model(&self) -> Option<OddWidth> {
        match self {
            Self::Pairs => Some(OddWidth::Pairs),
            Self::Scaled => Some(OddWidth::Scaled),
            Self::Differs(_) | Self::Unknown => None,
        }
    }
}

/// The odd cases held to the pairs model, and to the scaled one: each its own probe, so that each is kept for good or asked again after a
/// backoff by itself. The scaled one only runs when the pairs one found a difference.
static ODD_PAIRS: Probe = Probe::new();
static ODD_SCALED: Probe = Probe::new();

/// What is known of the odd sizes without running anything.
pub fn measured_odd() -> OddTranslucency {
    match measure::known(&ODD_PAIRS) {
        Verdict::Match => OddTranslucency::Pairs,
        Verdict::Unknown => OddTranslucency::Unknown,
        Verdict::Differs(pairs) => match measure::known(&ODD_SCALED) {
            Verdict::Match => OddTranslucency::Scaled,
            Verdict::Unknown => OddTranslucency::Unknown,
            Verdict::Differs(scaled) => OddTranslucency::Differs(format!("pairs: {pairs}; scaled: {scaled}")),
        },
    }
}

/// The odd sizes, measured on the first call (the cases of the round trip on three odd pictures, a few dozen short `ffmpeg` children, each
/// bounded by 20 s and 30 s in all; the scaled model's run only follows a pairs run that found a difference), under the same rules as
/// [`translucency`]: the export's own filter text, no lock held, a caller that arrives while it runs takes its answer.
pub fn odd_translucency() -> OddTranslucency {
    odd_over(&|model| measure::cached(model_state(model), Instant::now(), TIMING, |b| probe_odd(b, &[], model)))
}

/// [`odd_translucency`] for the runtime self-test, which draws an odd translucent frame only on a model (see [`translucency_for_selftest`]).
pub(crate) fn odd_translucency_for_selftest() -> OddTranslucency {
    odd_over(&|model| measure::checked(model_state(model), TIMING, |b| probe_odd(b, &[], model)))
}

fn model_state(model: OddWidth) -> &'static Probe {
    match model {
        OddWidth::Pairs => &ODD_PAIRS,
        OddWidth::Scaled => &ODD_SCALED,
    }
}

fn odd_over(ask: &dyn Fn(OddWidth) -> Verdict) -> OddTranslucency {
    match ask(OddWidth::Pairs) {
        Verdict::Match => OddTranslucency::Pairs,
        Verdict::Unknown => OddTranslucency::Unknown,
        Verdict::Differs(pairs) => match ask(OddWidth::Scaled) {
            Verdict::Match => OddTranslucency::Scaled,
            Verdict::Unknown => OddTranslucency::Unknown,
            Verdict::Differs(scaled) => OddTranslucency::Differs(format!("pairs: {pairs}; scaled: {scaled}")),
        },
    }
}

/// The model of an even picture, and of an odd one by the pairs of pixels.
fn pairs_model(planes: [&[u8]; 3], size: (usize, usize), alpha: &[u8], forward: YuvMatrix, back: YuvMatrix) -> [Vec<u8>; 4] {
    model_with_alpha_odd(planes, size, alpha, forward, back, OddWidth::Pairs)
}

/// Every case on every odd size, held to the model that makes an odd width's last chroma column as `odd` says.
fn probe_odd(budget: &Budget, cpu: &[&str], odd: OddWidth) -> Translucency {
    probe_odd_model(budget, cpu, &|planes, size, alpha, forward, back| {
        model_with_alpha_odd(planes, size, alpha, forward, back, odd)
    })
}

/// [`probe_odd`] holding the cases to `model`: how the tests show those cases tell a model that is not FFmpeg's.
fn probe_odd_model(budget: &Budget, cpu: &[&str], model: &Model) -> Translucency {
    for size in ODD_SIZES {
        if let Err(found) = run_cases(budget, cpu, size, model, &masked_alpha) {
            return found;
        }
    }
    Translucency::Match
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
    model_with_alpha_odd(planes, (w, h), alpha, forward, back, OddWidth::Pairs)
}

/// [`model_with_alpha_plane`] for a picture of any size (`planes` at `ceil(w / 2)` by `ceil(h / 2)` for the chroma), the way back
/// making an odd width's last chroma column as `odd` says. An unwritten last column (an odd width and an even height) has the alpha 0.
pub(crate) fn model_with_alpha_odd(
    planes: [&[u8]; 3],
    (w, h): (usize, usize),
    alpha: &[u8],
    forward: YuvMatrix,
    back: YuvMatrix,
    odd: OddWidth,
) -> [Vec<u8>; 4] {
    let rgb = roundtrip::yuv_to_rgb(planes, (w, h), forward).expect("a picture the filter tables cover");
    let [y, u, v] = roundtrip::rgb_to_yuv(&rgb, (w, h), back, odd).expect("a picture the filter tables cover");
    let mut alpha = alpha.to_vec();
    if roundtrip::zero_last_column(w, h) {
        for row in 0..h {
            alpha[row * w + w - 1] = 0;
        }
    }
    [y, u, v, alpha]
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
    match run_cases(budget, cpu, PROBE, &pairs_model, masked) {
        Ok(()) => keyed_cases(budget, cpu, masked),
        Err(found) => found,
    }
}

/// The picture sizes of the odd cases: both sides odd, the width alone and the height alone (each takes another swscale route:
/// `roundtrip::yuv_to_rgb`, `roundtrip::rgb_to_yuv`).
const ODD_SIZES: [(usize, usize); 3] = [(47, 31), (47, 32), (48, 31)];

/// The layer `overlay` is handed, as a model makes it: the planes in, the picture's size, the alpha plane as it ends, the matrix out and the
/// one back; `[Y, U, V, A]`.
type Model<'a> = dyn Fn([&[u8]; 3], (usize, usize), &[u8], YuvMatrix, YuvMatrix) -> [Vec<u8>; 4] + 'a;

/// Every case of [`CASES`] on a picture of `size`, held to `model`.
fn run_cases(
    budget: &Budget,
    cpu: &[&str],
    (w, h): (usize, usize),
    model: &Model,
    masked: &dyn Fn(&[u8], f64) -> Vec<u8>,
) -> Result<(), Translucency> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut rng = Lcg(0x7d3a_91c4_55e2_08b1);
    let (y, u, v) = (rng.bytes(w * h), rng.bytes(cw * ch), rng.bytes(cw * ch));
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
            return Err(Translucency::Unknown);
        };
        let (luma, chroma) = (w * h, cw * ch);
        if out.len() != 2 * luma + 2 * chroma {
            return Err(Translucency::Unknown);
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
            let want = model([&y, &u, &v], (w, h), &alpha, case.forward, *back);
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
            return Err(Translucency::Differs(format!(
                "{} on {size}: the layer FFmpeg hands `overlay` differs from the round trip's in {} luma, {} U, {} V and {} alpha samples",
                case.why, apart[0], apart[1], apart[2], apart[3]
            )));
        }
    }
    Ok(())
}

/// One case of the keyed probe: a key, an opacity and how the picture is tagged.
struct KeyedCase {
    why: &'static str,
    color: &'static str,
    similarity: f64,
    blend: f64,
    opacity: f64,
    tag: Option<&'static str>,
    forward: YuvMatrix,
}

const KEYED_CASES: [KeyedCase; 4] = [
    KeyedCase {
        why: "a keyed untagged layer at 50 %, where every odd level is a tie",
        color: "0x808080",
        similarity: 0.1,
        blend: 0.5,
        opacity: 0.5,
        tag: None,
        forward: YuvMatrix::Bt601,
    },
    KeyedCase {
        why: "a keyed untagged layer at 85 %",
        color: "0x1a8cd8",
        similarity: 0.05,
        blend: 0.4,
        opacity: 0.85,
        tag: None,
        forward: YuvMatrix::Bt601,
    },
    KeyedCase {
        why: "a keyed BT.709-tagged layer at 4 %",
        color: "0x808080",
        similarity: 0.02,
        blend: 0.8,
        opacity: 0.04,
        tag: Some("bt709"),
        forward: YuvMatrix::Bt709,
    },
    KeyedCase {
        why: "a keyed untagged layer with a hard edge at 60 %",
        color: "green",
        similarity: 0.3,
        blend: 0.0,
        opacity: 0.6,
        tag: None,
        forward: YuvMatrix::Bt601,
    },
];

/// The picture of a keyed case: noise in the luma, a ramp of chroma along each axis with a little noise (the key's distance is a 3x3 average,
/// so noise alone would give one level everywhere), 4:2:0.
fn keyed_picture(rng: &mut Lcg) -> [Vec<u8>; 3] {
    let (w, h) = PROBE;
    let (cw, ch) = (w / 2, h / 2);
    let noise = rng.bytes(2 * cw * ch);
    let ramp = |n: usize, i: usize, x: i32| ((i * 255 / (n - 1)) as i32 + x % 7 - 3).clamp(0, 255) as u8;
    let (mut u, mut v) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
    for cy in 0..ch {
        for cx in 0..cw {
            u.push(ramp(cw, cx, i32::from(noise[cy * cw + cx])));
            v.push(ramp(ch, cy, i32::from(noise[cw * ch + cy * cw + cx])));
        }
    }
    [rng.bytes(w * h), u, v]
}

/// The graph of a keyed case: the engine's own `chromakey` and opacity filters, the keyed picture in one branch (`[a]`, the key alone) and the
/// same picture through the opacity's round trip in the other (`[b]`), side by side.
fn keyed_graph(case: &KeyedCase) -> String {
    let tag = case.tag.map(|t| format!("setparams=colorspace={t},")).unwrap_or_default();
    format!(
        "[0:v]{tag}format=yuva420p,{key},split[k][c];[k]format=yuva420p[a];[c]{opacity},format=yuva420p[b];[a][b]hstack=inputs=2,format=yuva420p",
        key = kerf_core::chroma_key_filter(case.color, case.similarity, case.blend),
        opacity = kerf_core::opacity_filter(case.opacity),
    )
}

/// The keyed cases, with the levels mapped as `map` does (the model's [`masked_alpha`], or another that the tests show these cases tell from it).
fn keyed_cases(budget: &Budget, cpu: &[&str], map: &dyn Fn(&[u8], f64) -> Vec<u8>) -> Translucency {
    let (w, h) = PROBE;
    let mut rng = Lcg(0x2c91_5b07_e3a4_6d18);
    let [y, u, v] = keyed_picture(&mut rng);
    let size = format!("{w}x{h}");
    let raw: Vec<u8> = [&y, &u, &v].into_iter().flatten().copied().collect();
    let (cw, ch) = (w / 2, h / 2);
    for case in &KEYED_CASES {
        let graph = keyed_graph(case);
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
                "-filter_complex",
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
        // Two pictures side by side: the planes are 2w wide (luma, alpha) and w wide (chroma).
        let (luma, chroma) = (2 * w * h, cw * 2 * ch);
        if out.len() != 2 * luma + 2 * chroma {
            return Translucency::Unknown;
        }
        let (oy, ou, ov, oa) = (
            &out[..luma],
            &out[luma..luma + chroma],
            &out[luma + chroma..luma + 2 * chroma],
            &out[luma + 2 * chroma..],
        );
        let half = |plane: &[u8], pw: usize, rows: usize, right: bool| -> Vec<u8> {
            (0..rows)
                .flat_map(|r| plane[r * 2 * pw + usize::from(right) * pw..][..pw].iter().copied())
                .collect::<Vec<u8>>()
        };
        let key = half(oa, w, h, false);
        let planes = [
            half(oy, w, h, true),
            half(ou, cw, ch, true),
            half(ov, cw, ch, true),
            half(oa, w, h, true),
        ];
        // The key branch hands the colour planes on as it was given them (`chromakey` writes the alpha alone): the model starts from them.
        let key_colour = [half(oy, w, h, false), half(ou, cw, ch, false), half(ov, cw, ch, false)];
        if key_colour[0] != y || key_colour[1] != u || key_colour[2] != v {
            return Translucency::Differs(format!("{}: `chromakey` changed the colour planes", case.why));
        }
        let alpha = map(&key, case.opacity);
        let backs: &[YuvMatrix] = if case.forward == YuvMatrix::Bt601 {
            &[YuvMatrix::Bt601]
        } else {
            &[YuvMatrix::Bt601, case.forward]
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

    /// The keyed graph is the engine's own `chromakey` and opacity text between the conversions.
    #[test]
    fn the_keyed_graph_is_the_engines_chromakey_and_opacity_filters() {
        assert_eq!(
            keyed_graph(&KEYED_CASES[0]),
            "[0:v]format=yuva420p,chromakey=0x808080:0.1:0.5,split[k][c];[k]format=yuva420p[a];[c]colorchannelmixer=aa=0.5,format=yuva420p[b];[a][b]hstack=inputs=2,format=yuva420p"
        );
        assert!(keyed_graph(&KEYED_CASES[2]).starts_with("[0:v]setparams=colorspace=bt709,format=yuva420p,chromakey="));
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

    /// The keyed cases have teeth of their own (the masked ones are not in this run): a map of the key's levels that is not FFmpeg's is `Differs`,
    /// naming a keyed case, and the key's alpha plane holds many levels (a case whose key is one level everywhere would tell nothing about the
    /// map): every case's plane is made of at least 20 distinct levels but the hard edge's (0 and 255).
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn a_map_of_the_levels_that_is_not_ffmpegs_is_differs_in_a_keyed_case() {
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
            let found = keyed_cases(&Budget::new(&TIMING), &[], model);
            assert!(
                matches!(&found, Translucency::Differs(why) if why.contains("a keyed")),
                "{name}: {found:?}"
            );
        }
        let distinct = std::cell::RefCell::new(Vec::new());
        let counting = |levels: &[u8], op: f64| {
            let mut seen = [false; 256];
            levels.iter().for_each(|l| seen[usize::from(*l)] = true);
            distinct.borrow_mut().push(seen.iter().filter(|s| **s).count());
            masked_alpha(levels, op)
        };
        assert_eq!(keyed_cases(&Budget::new(&TIMING), &[], &counting), Translucency::Match);
        let distinct = distinct.into_inner();
        eprintln!("distinct levels of the key's alpha per keyed case: {distinct:?}");
        assert_eq!(distinct.len(), KEYED_CASES.len());
        for (n, case) in distinct.iter().zip(&KEYED_CASES) {
            assert!(*n >= if case.blend == 0.0 { 2 } else { 20 }, "{}: {n} levels", case.why);
        }
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

    /// The odd sizes on the FFmpeg in use: one model holds every case of every size (and the other does not, on a build whose odd
    /// widths differ), the answer is the same twice, and it is the model the pinned builds are known to follow (6.1: pairs, 9.0: scaled)
    /// or the probe says where it is neither.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn this_ffmpeg_makes_an_odd_widths_last_chroma_column_as_one_of_the_two_models_does() {
        let budget = Budget::new(&TIMING);
        let pairs = probe_odd(&budget, &[], OddWidth::Pairs);
        let scaled = probe_odd(&budget, &[], OddWidth::Scaled);
        eprintln!("odd sizes, pairs: {pairs:?}");
        eprintln!("odd sizes, scaled: {scaled:?}");
        assert!(
            [&pairs, &scaled].iter().all(|v| **v != Translucency::Unknown),
            "the probe could not run"
        );
        assert_eq!(
            [&pairs, &scaled].iter().filter(|v| ***v == Translucency::Match).count(),
            1,
            "exactly one model holds"
        );
        assert_eq!(pairs, probe_odd(&Budget::new(&TIMING), &[], OddWidth::Pairs));
        eprintln!(
            "the odd sizes: {:?}",
            odd_over(&|m| if m == OddWidth::Pairs { pairs.clone() } else { scaled.clone() })
        );
    }

    /// The odd cases have teeth: a machine with none of the SIMD kernels is neither model (the even cases already say so).
    #[test]
    #[ignore = "needs ffmpeg"]
    fn a_build_with_no_simd_kernels_is_neither_odd_model() {
        for model in [OddWidth::Pairs, OddWidth::Scaled] {
            let found = probe_odd(&Budget::new(&TIMING), &["-cpuflags", "0"], model);
            assert!(matches!(found, Translucency::Differs(_)), "{model:?}: {found:?}");
        }
    }

    /// The odd cases have teeth of their own: a model that keeps the unwritten last column opaque, one that leaves an odd width's last chroma
    /// column to the other build's route, and one a level off in the luma of the odd picture's last row are each `Differs`, naming a case,
    /// on this FFmpeg, where the model itself is `Match` for one of the two routes.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn a_model_of_the_odd_sizes_that_is_not_ffmpegs_is_differs() {
        let budget = Budget::new(&TIMING);
        let held = [OddWidth::Pairs, OddWidth::Scaled]
            .into_iter()
            .find(|m| probe_odd(&budget, &[], *m) == Translucency::Match)
            .expect("one of the two routes is this FFmpeg's");
        let opaque_column = |planes: [&[u8]; 3], size: (usize, usize), alpha: &[u8], f: YuvMatrix, b: YuvMatrix| {
            let mut out = model_with_alpha_odd(planes, size, alpha, f, b, held);
            out[3] = alpha.to_vec();
            out
        };
        let bright_last_row = |planes: [&[u8]; 3], size: (usize, usize), alpha: &[u8], f: YuvMatrix, b: YuvMatrix| {
            let mut out = model_with_alpha_odd(planes, size, alpha, f, b, held);
            let (w, h) = size;
            if h % 2 == 1 {
                for x in 0..w {
                    out[0][(h - 1) * w + x] = out[0][(h - 1) * w + x].wrapping_add(1);
                }
            }
            out
        };
        let other = if held == OddWidth::Pairs {
            OddWidth::Scaled
        } else {
            OddWidth::Pairs
        };
        let wrong_route = |planes: [&[u8]; 3], size: (usize, usize), alpha: &[u8], f: YuvMatrix, b: YuvMatrix| {
            model_with_alpha_odd(planes, size, alpha, f, b, other)
        };
        for (name, model) in [
            ("an opaque last column", &opaque_column as &Model),
            ("a luma level off in the last row", &bright_last_row),
            ("the other build's last chroma column", &wrong_route),
        ] {
            let found = probe_odd_model(&budget, &[], model);
            eprintln!("{name}: {found:?}");
            assert!(
                matches!(&found, Translucency::Differs(why) if why.contains(" on ")),
                "{name}: {found:?}"
            );
        }
        assert_eq!(probe_odd(&budget, &[], held), Translucency::Match);
    }

    /// The model on an even picture is the grade's way back and the unscaled converter's tables, whichever route it is asked by.
    #[test]
    fn the_model_of_an_even_picture_is_the_even_models() {
        let (w, h) = PROBE;
        let mut rng = Lcg(0x1234_5678_9abc_def0);
        let (y, u, v) = (rng.bytes(w * h), rng.bytes(w * h / 4), rng.bytes(w * h / 4));
        let alpha = vec![154u8; w * h];
        for odd in [OddWidth::Pairs, OddWidth::Scaled] {
            let got = model_with_alpha_odd([&y, &u, &v], PROBE, &alpha, YuvMatrix::Bt709, YuvMatrix::Bt601, odd);
            let t = roundtrip::Yuv2Rgb::new(YuvMatrix::Bt709);
            let mut want_rgb = Vec::new();
            for row in 0..h {
                for col in 0..w {
                    let c = (row / 2) * (w / 2) + col / 2;
                    let p = t.rgb(i32::from(y[row * w + col]), i32::from(u[c]), i32::from(v[c]));
                    want_rgb.extend(p.map(|v| v as u8));
                }
            }
            let [wy, wu, wv] = crate::grade::rgb_to_yuv420(&want_rgb, w, h, YuvMatrix::Bt601);
            assert_eq!([&got[0], &got[1], &got[2]], [&wy, &wu, &wv], "{odd:?}");
            assert_eq!(got[3], alpha);
        }
    }

    /// An odd width with an even height has its last column transparent and the rest of the alpha plane as it came; any other size's is
    /// untouched.
    #[test]
    fn an_unwritten_last_column_is_transparent() {
        for ((w, h), zeroed) in [
            ((47usize, 32usize), true),
            ((47, 31), false),
            ((48, 31), false),
            ((48, 32), false),
        ] {
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let (y, u, v) = (vec![90u8; w * h], vec![110u8; cw * ch], vec![150u8; cw * ch]);
            let alpha = vec![154u8; w * h];
            let got = model_with_alpha_odd(
                [&y, &u, &v],
                (w, h),
                &alpha,
                YuvMatrix::Bt601,
                YuvMatrix::Bt601,
                OddWidth::Pairs,
            );
            let last: Vec<u8> = (0..h).map(|r| got[3][r * w + w - 1]).collect();
            let rest = (0..h).flat_map(|r| got[3][r * w..r * w + w - 1].to_vec()).all(|a| a == 154);
            assert!(rest, "{w}x{h}");
            assert_eq!(last.iter().all(|a| *a == 0), zeroed, "{w}x{h}");
            assert_eq!(last.iter().all(|a| *a == 154), !zeroed, "{w}x{h}");
        }
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
