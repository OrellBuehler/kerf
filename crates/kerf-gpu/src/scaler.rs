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
//! * **The names a delivery gives its scaler** (`bilinear`, `lanczos`, `neighbor`, `spline` besides `bicubic`). The first is proven by
//!   the chains above; the others by chains of their own in the delivery's text (`scale=W:H:flags=<name>`), compared through the model of
//!   the kernel the base chains found, **name by name**: Lanczos and the spline are `double` arithmetic (`libm`'s `sin`, a compiler's
//!   float contraction), so a build whose tables differ loses that name alone ([`Model::named`]). A picture cannot show a last-place
//!   difference in `sin` (it moves a 14-bit coefficient with a probability of about 2^-38), so Lanczos is also held to FFmpeg's `sin` itself,
//!   bit for bit, on a grid ([`sin_graph`]).
//!
//! The compositor draws the SIMD arithmetic where FFmpeg runs it and the C arithmetic where it does not, and the
//! frames that are *exact* because of it (a layer the scaler resizes under a grade, the blurred fill) depend on the
//! answer: it is measured, once per process, on the real filter ([`kernels`], like [`crate::grade::kernels`] for the
//! conversions to and from RGB). The horizontal pass has one arithmetic (SIMD and C agree: exact integer sums).

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use kerf_core::ScalerKinds;

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

/// What a build's `scale` is: its scaler ([`Kernel`]), the algorithm an unflagged `scale` runs in a complex filter
/// graph (the product's graphs): bicubic since FFmpeg 5.0, **bilinear before it** (4.4's `scale` defaults to
/// `flags=bilinear`, and only a simple `-vf` graph gets `-sws_flags`' bicubic from the command line), and the named
/// scalers a delivery can ask for whose tables were measured to be this build's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    pub kernel: Kernel,
    pub default: Scaler,
    /// The `flags=` names besides `bicubic` that this FFmpeg draws as the model does. `bilinear` is in it wherever the model
    /// matched (the probe's bilinear chains are its proof); `lanczos`, `spline` and `neighbor` only where **their** chains matched
    /// too: their tables are `double` arithmetic (`libm`'s `sin`, a compiler's float contraction) that is the machine's, so
    /// the one that differs is out of the set and its frames are FFmpeg's, while the rest stay drawn.
    pub named: ScalerKinds,
}

impl Model {
    /// What the compositor has always assumed: the C scaler, bicubic by default, no other name.
    pub const ASSUMED: Self = Self {
        kernel: Kernel::C,
        default: Scaler::Bicubic,
        named: ScalerKinds::NONE,
    };

    /// The algorithm of a scale: its own flag, else this build's default.
    pub fn scaler(self, flagged: Option<Scaler>) -> Scaler {
        flagged.unwrap_or(self.default)
    }

    /// Whether a scale that names `flag` is drawn as this build runs it: `bicubic` (and the unnamed default) wherever the model
    /// matched, the other names where their own chains did.
    pub fn draws(self, flag: Scaler) -> bool {
        match flag {
            Scaler::Bicubic => true,
            Scaler::Bilinear => self.named.contains(ScalerKinds::BILINEAR),
            Scaler::Lanczos => self.named.contains(ScalerKinds::LANCZOS),
            Scaler::Spline => self.named.contains(ScalerKinds::SPLINE),
            Scaler::Neighbor => self.named.contains(ScalerKinds::NEIGHBOR),
        }
    }
}

/// The scaler a delivery's `scaler` names, spelled as the engine's list has it (`SCALERS`): `None` for no name, for `bicubic`'s
/// siblings the engine does not write (it drops a name it does not list, and FFmpeg runs the build's default) and for a spelling
/// it does not know.
pub fn named(name: &str) -> Option<Scaler> {
    match name {
        "bicubic" => Some(Scaler::Bicubic),
        "bilinear" => Some(Scaler::Bilinear),
        "lanczos" => Some(Scaler::Lanczos),
        "spline" => Some(Scaler::Spline),
        "neighbor" => Some(Scaler::Neighbor),
        _ => None,
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

    /// The named scalers drawn on this machine ([`Model::named`]); none unless the model matched.
    pub fn named(self) -> ScalerKinds {
        match self {
            Self::Match(m) => m.named,
            Self::Differs | Self::Unknown => ScalerKinds::NONE,
        }
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

/// The scales that tell whether a **named** scaler's tables are this FFmpeg's, one set for each name the probe proves besides the
/// bicubic and bilinear ones above: two shrinks (one with odd sides) and an enlargement, in the text the engine writes for a
/// delivery (`scale=<w>:<h>:flags=<name>`). Lanczos and spline are `double` arithmetic and `libm`, so unlike the integer
/// tables a build's can differ from the compositor's, in a coefficient: a chain that comes back as another picture than the
/// model draws leaves **that name** out of [`Model::named`]. (Shrinks of 8:1 to 12:1 at most: a spline table does not exist
/// from 19.6:1, and a chain the model has no table for would read as a difference.)
const NAMED: [(ScalerKinds, [Chain; NAMED_PER]); 3] = [
    (
        ScalerKinds::LANCZOS,
        [
            chain("scale=22:20:flags=lanczos", 22, 20, Some(Scaler::Lanczos)),
            chain("scale=35:18:flags=lanczos", 35, 18, Some(Scaler::Lanczos)),
            chain("scale=300:200:flags=lanczos", 300, 200, Some(Scaler::Lanczos)),
        ],
    ),
    (
        ScalerKinds::SPLINE,
        [
            chain("scale=22:20:flags=spline", 22, 20, Some(Scaler::Spline)),
            chain("scale=35:18:flags=spline", 35, 18, Some(Scaler::Spline)),
            chain("scale=300:200:flags=spline", 300, 200, Some(Scaler::Spline)),
        ],
    ),
    (
        ScalerKinds::NEIGHBOR,
        [
            chain("scale=22:20:flags=neighbor", 22, 20, Some(Scaler::Neighbor)),
            chain("scale=35:18:flags=neighbor", 35, 18, Some(Scaler::Neighbor)),
            chain("scale=300:200:flags=neighbor", 300, 200, Some(Scaler::Neighbor)),
        ],
    ),
];

/// Does this FFmpeg's `sin` return the doubles ours does? **Lanczos's coefficients are `sin` of a few hundred arguments a table
/// (`initFilter`)**, and a last-place difference in `libm` moves a 14-bit coefficient with a probability of about 2^-38, which no
/// picture the chains above make can show; so the function itself is held to ours, bit for bit, on a grid. FFmpeg's expression
/// evaluator calls the same `sin` (`d->a.func0 = sin`, libavutil `eval.c`) in the same process as swscale does, so a `geq` that
/// evaluates `sin(n * 0.0023)` for 4096 values of `n` (arguments from 0 to 9.4: Lanczos's `πx` and `πx/3` for `x` to 3) and writes
/// its eight base-256 fractional digits as pixels (`floor(t * 256^(k+1)) mod 256`: every step is a power-of-two scaling, a `floor`
/// or a subtraction of integers, exact in `double`) hands back the exact result of every call. A picture that is not what
/// `f64::sin` gives leaves Lanczos out of [`Model::named`]; the rest of the names stand.
const SIN: (usize, usize) = (512, 64);
const SIN_STEP: &str = "0.0023";

fn sin_graph() -> String {
    let (w, h) = SIN;
    format!(
        "nullsrc=s={w}x{h}:r=1:d=1,format=yuv420p,\
         geq=lum='st(0,sin((floor(X/8)+64*Y)*{SIN_STEP}));mod(floor(ld(0)*pow(256,mod(X,8)+1)),256)':cb=128:cr=128"
    )
}

/// The luma plane of [`sin_graph`] as `f64::sin` computes it, in the evaluator's own order of operations.
fn sin_luma(sin: fn(f64) -> f64) -> Vec<u8> {
    let step: f64 = SIN_STEP.parse().unwrap_or(f64::NAN);
    let (w, h) = SIN;
    (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| {
            let t = sin(((x / 8) as f64 + 64.0 * y as f64) * step);
            let d = (t * 256_f64.powi((x % 8) as i32 + 1)).floor();
            (d - (d / 256.0).floor() * 256.0) as u8
        })
        .collect()
}

/// Chains the probe runs for each named scaler.
const NAMED_PER: usize = 3;

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

/// What a child of the probe measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Part {
    /// The picture every chain scales.
    Source,
    /// `CHAINS[i]`: the base scaler's.
    Base(usize),
    /// Chain `i` of `NAMED[n]`.
    Named(usize, usize),
    /// Lanczos's `sin` check ([`sin_graph`]).
    Sin,
}

/// One `ffmpeg` of a [`Round`]: its graph, whether it is a complex one and the length of the picture it must hand back.
struct Child {
    part: Part,
    graph: String,
    complex: bool,
    len: usize,
}

/// What one run of the probe asks for. The **base** (the unflagged scale, the bicubic and bilinear chains: ten children) and **each
/// named scaler** (three chains, and Lanczos's `sin`) are separate questions with a backoff of their own: a child that fails or hangs
/// leaves unanswered the one question it belongs to, never the base scaler's answer because a name's chain did not come back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Round {
    base: bool,
    names: [bool; NAMED.len()],
}

impl Round {
    fn is_empty(self) -> bool {
        !self.base && !self.names.contains(&true)
    }

    /// The children, in the order they are started: the source, the base chains, the named chains, the `sin` check. They all run at once
    /// ([`collect`]).
    fn children(self) -> Vec<Child> {
        let chained = |part, filter: &str, out| Child {
            part,
            graph: format!("{},{filter}", probe_source()),
            complex: true,
            len: frame_len(out),
        };
        let mut children = Vec::new();
        if self.base {
            children.push(Child {
                part: Part::Source,
                graph: probe_source(),
                complex: false,
                len: frame_len(PROBE),
            });
            children.extend(
                CHAINS
                    .iter()
                    .enumerate()
                    .map(|(i, c)| chained(Part::Base(i), c.filter, c.out)),
            );
        }
        for (n, (_, chains)) in NAMED.iter().enumerate().filter(|(n, _)| self.names[*n]) {
            children.extend(
                chains
                    .iter()
                    .enumerate()
                    .map(|(i, c)| chained(Part::Named(n, i), c.filter, c.out)),
            );
        }
        if self.names[LANCZOS] {
            children.push(Child {
                part: Part::Sin,
                graph: sin_graph(),
                complex: false,
                len: frame_len(SIN),
            });
        }
        children
    }
}

/// The index of Lanczos in [`NAMED`] (its question has the `sin` check besides the chains).
const LANCZOS: usize = 0;

/// Every child of a round, at once, each handed the thread budget's share of **all of them** (`share` is the number of `ffmpeg`s
/// that run side by side, which is what [`kerf_core::limit_frame_args`] divides the budget by). `run` is one child.
fn collect(children: &[Child], run: impl Fn(&Child, usize) -> Option<Vec<u8>> + Sync) -> Vec<Option<Vec<u8>>> {
    let share = children.len();
    std::thread::scope(|scope| {
        let runs: Vec<_> = children
            .iter()
            .map(|child| {
                let run = &run;
                scope.spawn(move || run(child, share))
            })
            .collect();
        runs.into_iter().map(|run| run.join().ok().flatten()).collect()
    })
}

/// One child as an `ffmpeg` process: bounded by [`PROBE_TIMEOUT`] (a hung one is killed and reads as a missing picture).
fn run_ffmpeg(child: &Child, share: usize) -> Option<Vec<u8>> {
    let mut args = probe_args(&child.graph, child.complex);
    // What a frame waits on is not niced, but the children share the budget's threads.
    kerf_core::limit_frame_args(&mut args, share);
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(&args);
    crate::source::run_child(cmd, child.len + 1, PROBE_TIMEOUT).ok()
}

/// The base scaler, as the probe found it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Base {
    /// It matched no model.
    Differs,
    /// It matched `model` (with no name but `bilinear`, which its own chains prove), on this probe picture (the one the named chains
    /// are compared through, in a later round that asks for one name alone).
    Match { model: Model, source: Vec<u8> },
}

/// What a named scaler's pictures say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Every picture came back as the model draws it.
    Proven,
    /// One did not (a different table): the name is out for good.
    Differs,
    /// A picture is missing or the wrong size (a child that failed or hung) and none differs: asked again after a backoff.
    Open,
}

/// What a round found out. `base` is `None` when the base was not asked or is not answered; a name's verdict is `None` when it was not
/// asked or there is no model yet to compare its pictures with (the base is open).
struct Outcome {
    base: Option<Base>,
    names: [Option<Verdict>; NAMED.len()],
    /// The model the names' pictures were compared through.
    against: Option<Model>,
}

/// Run a round (with `run` for each child) and classify what came back. `known` is the base answer already measured (the model and
/// the probe picture), what a round that asks for names alone compares with.
fn measure_with(round: Round, known: Option<(Model, &[u8])>, run: impl Fn(&Child, usize) -> Option<Vec<u8>> + Sync) -> Outcome {
    let children = round.children();
    let mut frames = collect(&children, run);
    let mut take = |part: Part| -> Option<Vec<u8>> {
        let at = children.iter().position(|c| c.part == part)?;
        frames[at].take()
    };
    let mut base = None;
    if round.base {
        let picture = take(Part::Source);
        let chains: Vec<_> = (0..CHAINS.len()).map(|i| take(Part::Base(i))).collect();
        base = picture.as_deref().and_then(|p| classify_base(p, &chains));
    }
    // The model to compare the names with: the base this round found, else the one measured before.
    let against: Option<(Model, Vec<u8>)> = match &base {
        Some(Base::Match { model, source }) => Some((*model, source.clone())),
        Some(Base::Differs) => None,
        None if round.base => None,
        None => known.map(|(m, s)| (m, s.to_vec())),
    };
    let mut names = [None; NAMED.len()];
    if let Some((model, source)) = &against {
        for (n, verdict) in names.iter_mut().enumerate().filter(|(n, _)| round.names[*n]) {
            let pictures: Vec<_> = (0..NAMED_PER).map(|i| take(Part::Named(n, i))).collect();
            let sin = if n == LANCZOS { take(Part::Sin) } else { None };
            *verdict = Some(classify_name(source, *model, n, &pictures, sin.as_deref()));
        }
    }
    Outcome {
        base,
        names,
        against: against.map(|(m, _)| m),
    }
}

fn measure(round: Round, known: Option<(Model, &[u8])>) -> Outcome {
    measure_with(round, known, run_ffmpeg)
}

/// Every model a build can be: the three scalers, each with either default algorithm.
const MODELS: [Model; 6] = {
    const fn m(kernel: Kernel, default: Scaler) -> Model {
        Model {
            kernel,
            default,
            named: ScalerKinds::BILINEAR,
        }
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

/// Whether one named scaler's chains (`pictures`, in order) and, for Lanczos, the `sin` check (`sin`: its picture, if it came back) are
/// the model's (`sin` is ignored for any other name), for the model the base chains found. A picture that differs is conclusive wherever the rest stands; one that is missing
/// or the wrong size leaves the name [`Verdict::Open`]. Lanczos's tables are `double` arithmetic (`libm`'s `sin`, a compiler's float
/// contraction), so a build can differ in a name alone: that name is out ([`Model::named`]), and the rest stand.
fn classify_name(source: &[u8], model: Model, n: usize, pictures: &[Option<Vec<u8>>], sin: Option<&[u8]>) -> Verdict {
    let (kind, chains) = &NAMED[n];
    let mut open = pictures.len() != chains.len();
    let mut tables_differ = false;
    for (chain, picture) in chains.iter().zip(pictures) {
        match picture.as_deref().filter(|f| f.len() == frame_len(chain.out)) {
            None => open = true,
            Some(picture) => tables_differ |= modelled(source, chain, model).as_deref() != Some(picture),
        }
    }
    let mut sin_differs = false;
    if *kind == ScalerKinds::LANCZOS {
        match sin.filter(|f| f.len() == frame_len(SIN)) {
            None => open = true,
            Some(picture) => sin_differs = picture[..SIN.0 * SIN.1] != sin_luma(f64::sin)[..],
        }
    }
    if tables_differ {
        tracing::warn!(
            scaler = ?kind,
            "this FFmpeg's named scaler is not the table the GPU computes (a different libm or float contraction); \
             a delivery that names it is drawn by FFmpeg"
        );
        Verdict::Differs
    } else if sin_differs {
        tracing::warn!(
            "this FFmpeg's `sin` is not the one the GPU computes Lanczos with; a delivery that names lanczos is drawn by FFmpeg"
        );
        Verdict::Differs
    } else if open {
        Verdict::Open
    } else {
        Verdict::Proven
    }
}

/// The base scaler from the probe's pictures: `None` when one is missing or the wrong size and the models left cannot be narrowed to
/// one (a difference from every model is conclusive wherever the rest stands).
fn classify_base(source: &[u8], frames: &[Option<Vec<u8>>]) -> Option<Base> {
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
        ([], _) => Some(Base::Differs),
        // (A chain that did not come back leaves the answer open, unless only one model is left to be.)
        ([m], false) => Some(Base::Match {
            model: *m,
            source: source.to_vec(),
        }),
        _ => None,
    }
}

// ---- the process's answer ------------------------------------------------------------------------

/// When a question was last given up on: the failures so far and when to ask again.
#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    failures: u32,
    retry_at: Option<Instant>,
}

impl Slot {
    const NEW: Self = Self {
        failures: 0,
        retry_at: None,
    };

    /// Not backing off.
    fn due(&self, now: Instant) -> bool {
        self.retry_at.is_none_or(|t| now >= t)
    }

    /// Give up on the question for a while. `None` when another caller already did (the backoff stands).
    fn failed(&mut self, now: Instant) -> Option<Duration> {
        if self.retry_at.is_some_and(|t| t > now) {
            return None;
        }
        self.failures += 1;
        let wait = backoff(self.failures);
        self.retry_at = Some(Instant::now() + wait);
        Some(wait)
    }
}

/// A named scaler's question: `settled` is `Some(true)` once proven and `Some(false)` once found different.
#[derive(Debug, Clone, Copy)]
struct Name {
    settled: Option<bool>,
    slot: Slot,
}

struct State {
    base: Option<Base>,
    base_slot: Slot,
    names: [Name; NAMED.len()],
    /// A self-test could not tell whether the scaler is the model, so it did not draw a resized frame: nothing is believed
    /// until a self-test has (a probe that succeeds later must not open resized layers the check never looked at).
    held_back: bool,
}

impl State {
    const fn new() -> Self {
        Self {
            base: None,
            base_slot: Slot::NEW,
            names: [Name {
                settled: None,
                slot: Slot::NEW,
            }; NAMED.len()],
            held_back: false,
        }
    }

    /// What there is to ask now: the base unless it is answered or backing off, and the names that are neither settled nor backing off,
    /// **when there is a model to compare them with** (the base answered with a match, or asked in the same round).
    fn round(&self, now: Instant) -> Round {
        let base = self.base.is_none() && self.base_slot.due(now);
        let modelled = base || matches!(self.base, Some(Base::Match { .. }));
        let mut names = [false; NAMED.len()];
        for (ask, name) in names.iter_mut().zip(&self.names) {
            *ask = modelled && name.settled.is_none() && name.slot.due(now);
        }
        Round { base, names }
    }

    /// What is known: the base's answer, with the names proven so far.
    fn view(&self) -> Kernels {
        match &self.base {
            None => Kernels::Unknown,
            Some(Base::Differs) => Kernels::Differs,
            Some(Base::Match { model, .. }) => {
                let mut named = model.named;
                for ((kind, _), name) in NAMED.iter().zip(&self.names) {
                    if name.settled == Some(true) {
                        named = named.union(*kind);
                    }
                }
                Kernels::Match(Model { named, ..*model })
            }
        }
    }

    /// Store what a round found. The first answer stored wins (callers that arrive meanwhile measure too); a question that did not
    /// come back backs off alone.
    fn absorb(&mut self, round: Round, outcome: Outcome, now: Instant) {
        if round.base && self.base.is_none() {
            match outcome.base {
                Some(base) => {
                    if base == Base::Differs {
                        tracing::warn!(
                            "this FFmpeg's scaler is neither the C arithmetic nor the x86 SIMD one the GPU follows; \
                             what depends on the scaler being exact is drawn by FFmpeg"
                        );
                    }
                    self.base = Some(base);
                }
                None => {
                    if let Some(wait) = self.base_slot.failed(now) {
                        tracing::warn!(
                            failures = self.base_slot.failures,
                            retry_in_secs = wait.as_secs(),
                            "could not measure FFmpeg's scaler; what depends on it being exact is not drawn on the GPU until it can"
                        );
                    }
                }
            }
        }
        // (The names' pictures were compared through the model of the base that stands.)
        let Some(Base::Match { model, .. }) = &self.base else {
            return;
        };
        if outcome.against != Some(*model) {
            return;
        }
        for (n, (name, verdict)) in self.names.iter_mut().zip(outcome.names).enumerate() {
            if !round.names[n] || name.settled.is_some() {
                continue;
            }
            match verdict {
                Some(Verdict::Proven) => name.settled = Some(true),
                Some(Verdict::Differs) => name.settled = Some(false),
                Some(Verdict::Open) => {
                    if let Some(wait) = name.slot.failed(now) {
                        tracing::warn!(
                            scaler = ?NAMED[n].0,
                            failures = name.slot.failures,
                            retry_in_secs = wait.as_secs(),
                            "could not measure one of FFmpeg's named scalers; a delivery that names it is drawn by FFmpeg until it can"
                        );
                    }
                }
                None => {}
            }
        }
    }
}

static STATE: Mutex<State> = Mutex::new(State::new());

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether this FFmpeg's `scale` is the model ([`Kernels`]), and which of the named scalers are proven. The first call runs the probe:
/// twenty short `ffmpeg` children at once (the unflagged `scale`, nine chains of the bicubic and bilinear scalers, nine of the named ones
/// and the `sin` check; each bounded by 20 s and handed an equal share of the thread budget) with no lock held, a blocking thread's,
/// ahead of the frame that needs it ([`crate::prepare`]).
///
/// The base scaler and each name are separate questions. [`Kernels::Unknown`] is the base's alone (ten children that did not all
/// come back) and is not remembered: the probe is run again after a backoff. A named scaler whose children did not come back is
/// left out of [`Model::named`] and **asked again alone** (its three or four children) after a backoff of its own, while the base's
/// answer stands.
pub fn kernels() -> Kernels {
    cached(&STATE, Instant::now(), measure)
}

/// What is known without running anything: the measured answer, else [`Kernels::Unknown`]. What a plan's caps are
/// made from ([`crate::Compositor::caps`]): a plan does not wait for a probe.
pub fn measured() -> Kernels {
    known(&STATE)
}

/// [`kernels`] for the runtime self-test, which draws resized frames only when this is [`Kernels::exact`]. An `Unknown` here
/// holds the scaler back for the process until a self-test that can tell has run, so that the frames the GPU draws are never
/// the ones its check left out; a `Differs` is permanent and needs no holding.
pub(crate) fn kernels_for_selftest() -> Kernels {
    checked(&STATE, Instant::now(), measure)
}

fn known(state: &Mutex<State>) -> Kernels {
    let s = lock(state);
    if s.held_back {
        return Kernels::Unknown;
    }
    s.view()
}

fn checked(state: &Mutex<State>, now: Instant, measure: impl FnOnce(Round, Option<(Model, &[u8])>) -> Outcome) -> Kernels {
    lock(state).held_back = false;
    let k = cached(state, now, measure);
    if k == Kernels::Unknown {
        lock(state).held_back = true;
    }
    k
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300))
}

fn cached(state: &Mutex<State>, now: Instant, measure: impl FnOnce(Round, Option<(Model, &[u8])>) -> Outcome) -> Kernels {
    let (round, known) = {
        let s = lock(state);
        if s.held_back {
            return Kernels::Unknown;
        }
        let round = s.round(now);
        if round.is_empty() {
            return s.view();
        }
        let known = match &s.base {
            Some(Base::Match { model, source }) => Some((*model, source.clone())),
            _ => None,
        };
        (round, known)
    };
    // Measured with no lock held: a probe is child processes, and nothing else waits behind them. Callers that arrive
    // meanwhile measure too, boundedly; the first answer stored wins.
    let outcome = measure(round, known.as_ref().map(|(m, s)| (*m, s.as_slice())));
    let mut s = lock(state);
    s.absorb(round, outcome, now);
    s.view()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Mutex<State> {
        Mutex::new(State::new())
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

    /// The picture of [`sin_graph`] for a `sin` that is `sin`: its luma digits, and the 128 of both chroma planes.
    fn sin_picture(sin: fn(f64) -> f64) -> Vec<u8> {
        let mut v = sin_luma(sin);
        v.resize(frame_len(SIN), 128);
        v
    }

    /// What an FFmpeg that is exactly `model` returns for the base chains.
    fn base_pictures(source: &[u8], model: Model) -> Vec<Option<Vec<u8>>> {
        CHAINS.iter().map(|c| modelled(source, c, model)).collect()
    }

    /// ... and for the chains of `NAMED[n]`.
    fn name_pictures(source: &[u8], model: Model, n: usize) -> Vec<Option<Vec<u8>>> {
        NAMED[n].1.iter().map(|c| modelled(source, c, model)).collect()
    }

    fn matched(model: Model) -> Base {
        Base::Match {
            model,
            source: probe_source_planes(),
        }
    }

    /// The child `part` of a round as an FFmpeg that is exactly `model` (and has this process's `sin`) answers it. The pictures of each model
    /// are made once (a debug build takes seconds over them).
    fn exact_child(source: &[u8], model: Model, part: Part) -> Option<Vec<u8>> {
        static CACHE: [std::sync::OnceLock<std::collections::HashMap<Part, Option<Vec<u8>>>>; MODELS.len()] =
            [const { std::sync::OnceLock::new() }; MODELS.len()];
        let at = MODELS.iter().position(|m| *m == model)?;
        CACHE[at]
            .get_or_init(|| {
                let mut all = std::collections::HashMap::new();
                all.insert(Part::Source, Some(source.to_vec()));
                for (i, chain) in CHAINS.iter().enumerate() {
                    all.insert(Part::Base(i), modelled(source, chain, model));
                }
                for (n, (_, chains)) in NAMED.iter().enumerate() {
                    for (i, chain) in chains.iter().enumerate() {
                        all.insert(Part::Named(n, i), modelled(source, chain, model));
                    }
                }
                all.insert(Part::Sin, Some(sin_picture(f64::sin)));
                all
            })
            .get(&part)
            .cloned()
            .flatten()
    }

    /// The first instant at which a question that backs off is asked again.
    fn after(slot: &Slot) -> Instant {
        slot.retry_at.expect("it backs off") + Duration::from_millis(1)
    }

    const NAME_KINDS: [ScalerKinds; 3] = [ScalerKinds::LANCZOS, ScalerKinds::SPLINE, ScalerKinds::NEIGHBOR];

    #[test]
    fn the_names_are_indexed_as_the_probe_asks_them() {
        assert_eq!(NAMED.map(|(kind, _)| kind), NAME_KINDS);
        assert_eq!(NAMED[LANCZOS].0, ScalerKinds::LANCZOS);
        for (_, chains) in &NAMED {
            assert_eq!(chains.len(), NAMED_PER);
        }
    }

    #[test]
    fn a_base_probe_that_matches_a_kernel_says_so_and_one_that_matches_none_differs() {
        let source = probe_source_planes();
        for model in MODELS {
            let frames = base_pictures(&source, model);
            assert_eq!(frames.len(), CHAINS.len());
            assert!(
                frames.iter().all(Option::is_some),
                "a chain has no table in the model: {model:?}"
            );
            assert_eq!(classify_base(&source, &frames), Some(matched(model)), "{model:?}");
            // One frame missing and the rest agreeing is not an answer.
            let mut short = frames.clone();
            short[2] = None;
            assert_eq!(classify_base(&source, &short), None);
            // ... and a missing frame does not hide a difference.
            let mut off_short = short.clone();
            if let Some(f) = off_short[0].as_mut() {
                f[5] = f[5].wrapping_add(1);
            }
            assert_eq!(classify_base(&source, &off_short), Some(Base::Differs));
            // One sample off in one chain matches neither.
            let mut off = frames.clone();
            if let Some(f) = off[1].as_mut() {
                f[5] = f[5].wrapping_add(1);
            }
            assert_eq!(classify_base(&source, &off), Some(Base::Differs));
            // ... whatever else is missing.
            off[3] = None;
            assert_eq!(classify_base(&source, &off), Some(Base::Differs));
        }
        // Too few or too many pictures is not an answer either, nor a source of the wrong size.
        let frames = base_pictures(&source, MODELS[0]);
        assert_eq!(classify_base(&source, &frames[..CHAINS.len() - 1]), None);
        assert_eq!(classify_base(&source[1..], &frames), None);
    }

    #[test]
    fn a_name_whose_tables_differ_is_out_and_one_whose_picture_is_missing_is_open() {
        let source = probe_source_planes();
        // (The kernel is the base's business: one SIMD and one C model are enough, and a debug build is slow over the tables.)
        for model in [MODELS[0], MODELS[5]] {
            for (n, (kind, chains)) in NAMED.iter().enumerate() {
                let frames = name_pictures(&source, model, n);
                let sin = (n == LANCZOS).then(|| sin_picture(f64::sin));
                let verdict = |frames: &[Option<Vec<u8>>], sin: Option<&[u8]>| classify_name(&source, model, n, frames, sin);
                assert_eq!(verdict(&frames, sin.as_deref()), Verdict::Proven, "{model:?} {kind:?}");
                for k in 0..chains.len() {
                    // One sample of one chain comes back as another picture: the table differs (a `sin` that rounds the other way
                    // moves a coefficient, and so some samples).
                    let mut off = frames.clone();
                    if let Some(f) = off[k].as_mut() {
                        f[7] = f[7].wrapping_add(1);
                    }
                    assert_eq!(
                        verdict(&off, sin.as_deref()),
                        Verdict::Differs,
                        "{model:?} {kind:?} chain {k}"
                    );
                    // ... whatever else is missing.
                    let mut off_and_short = off.clone();
                    off_and_short[(k + 1) % chains.len()] = None;
                    assert_eq!(verdict(&off_and_short, sin.as_deref()), Verdict::Differs);
                    // A chain that did not come back, or came back short, is no answer.
                    let mut missing = frames.clone();
                    missing[k] = None;
                    assert_eq!(
                        verdict(&missing, sin.as_deref()),
                        Verdict::Open,
                        "{model:?} {kind:?} chain {k}"
                    );
                    let mut cut = frames.clone();
                    if let Some(f) = cut[k].as_mut() {
                        f.pop();
                    }
                    assert_eq!(verdict(&cut, sin.as_deref()), Verdict::Open);
                }
                assert_eq!(verdict(&frames[..chains.len() - 1], sin.as_deref()), Verdict::Open);
                // The check of `sin` is Lanczos's alone.
                if n != LANCZOS {
                    assert_eq!(verdict(&frames, Some(&sin_picture(f64::sin)[1..])), Verdict::Proven);
                }
            }
        }
    }

    #[test]
    fn a_sin_that_is_not_ours_leaves_lanczos_out_and_nothing_else() {
        let source = probe_source_planes();
        // A `sin` of float precision, or one that is off in the last place of a few arguments: the picture is another one.
        let single = |x: f64| f64::from((x as f32).sin());
        let one_ulp_off = |x: f64| {
            let t = x.sin();
            if (x * 1e4).round() % 7.0 == 0.0 {
                f64::from_bits(t.to_bits() + 1)
            } else {
                t
            }
        };
        for (what, sin) in [("float precision", single as fn(f64) -> f64), ("a last place", one_ulp_off)] {
            assert_ne!(sin_picture(sin), sin_picture(f64::sin), "{what}: the probe cannot see it");
            for model in [MODELS[0], MODELS[5]] {
                let lanczos = name_pictures(&source, model, LANCZOS);
                let ours = sin_picture(f64::sin);
                let theirs = sin_picture(sin);
                assert_eq!(classify_name(&source, model, LANCZOS, &lanczos, Some(&ours)), Verdict::Proven);
                assert_eq!(
                    classify_name(&source, model, LANCZOS, &lanczos, Some(&theirs)),
                    Verdict::Differs,
                    "{what} {model:?}"
                );
                // The `sin` that did not come back leaves Lanczos open, not out.
                assert_eq!(classify_name(&source, model, LANCZOS, &lanczos, None), Verdict::Open);
                let short = &ours[1..];
                assert_eq!(classify_name(&source, model, LANCZOS, &lanczos, Some(short)), Verdict::Open);
                // The other names do not look at it.
                for n in 1..NAMED.len() {
                    let pictures = name_pictures(&source, model, n);
                    assert_eq!(classify_name(&source, model, n, &pictures, Some(&theirs)), Verdict::Proven);
                }
            }
        }
    }

    #[test]
    fn the_sin_pictures_digits_rebuild_the_double() {
        // Eight base-256 digits of `floor(t * 2^64)` are all of a double of this size, so two `sin`s that differ anywhere on the grid
        // come back as different pictures, and one that agrees comes back as the same.
        let luma = sin_luma(f64::sin);
        let (w, _) = SIN;
        let step: f64 = SIN_STEP.parse().unwrap();
        let mut positive = 0;
        for n in 0..(SIN.0 / 8) * SIN.1 {
            let (x, y) = ((n % (w / 8)) * 8, n / (w / 8));
            let t = (n as f64 * step).sin();
            let digits = &luma[y * w + x..y * w + x + 8];
            if t > 0.0 {
                positive += 1;
                let rebuilt: f64 = digits
                    .iter()
                    .enumerate()
                    .map(|(k, d)| f64::from(*d) / 256_f64.powi(k as i32 + 1))
                    .sum();
                assert_eq!(rebuilt, t, "argument {n}");
            }
        }
        assert!(positive > 1000, "{positive}");
        // The grid reaches Lanczos's arguments: pi * x for x up to 3.
        assert!(((SIN.0 / 8) * SIN.1 - 1) as f64 * step > 3.0 * std::f64::consts::PI - 0.02);
    }

    #[test]
    fn the_probe_tells_every_named_scaler_from_every_other_on_each_of_its_chains() {
        let source = probe_source_planes();
        let model = |scaler| Model {
            default: scaler,
            ..MODELS[0]
        };
        let all = [
            Scaler::Bicubic,
            Scaler::Bilinear,
            Scaler::Lanczos,
            Scaler::Spline,
            Scaler::Neighbor,
        ];
        for (_, chains) in &NAMED {
            for chain in chains {
                let own = chain.flag.unwrap();
                let mine = modelled(&source, chain, model(own)).unwrap();
                for other in all.iter().filter(|o| **o != own) {
                    let theirs = modelled(
                        &source,
                        &Chain {
                            flag: Some(*other),
                            ..*chain
                        },
                        model(own),
                    )
                    .unwrap();
                    assert_ne!(mine, theirs, "{}: {own:?} cannot be told from {other:?}", chain.filter);
                }
            }
        }
    }

    #[test]
    fn a_name_is_drawn_where_the_model_proved_it() {
        let m = Model {
            named: ScalerKinds::LANCZOS.union(ScalerKinds::BILINEAR),
            ..MODELS[0]
        };
        assert!(m.draws(Scaler::Bicubic) && m.draws(Scaler::Bilinear) && m.draws(Scaler::Lanczos));
        assert!(!m.draws(Scaler::Spline) && !m.draws(Scaler::Neighbor));
        // Nothing is proven for a scaler that was not measured, or one that differed.
        for k in [Kernels::Differs, Kernels::Unknown] {
            assert_eq!(k.named(), ScalerKinds::NONE);
            assert!(!k.model().draws(Scaler::Lanczos) && !k.model().draws(Scaler::Bilinear));
        }
        assert_eq!(Kernels::Match(m).named(), m.named);
        for (name, scaler) in [
            ("bicubic", Scaler::Bicubic),
            ("bilinear", Scaler::Bilinear),
            ("lanczos", Scaler::Lanczos),
            ("spline", Scaler::Spline),
            ("neighbor", Scaler::Neighbor),
        ] {
            assert_eq!(named(name), Some(scaler));
            // The plan's kinds and the compositor's scalers are the same set of names.
            assert_eq!(ScalerKinds::of_name(name).is_some(), scaler != Scaler::Bicubic, "{name}");
        }
        for not_a_name in ["Bicubic", "LANCZOS", "area", "bilinear:x=1", "", "gauss"] {
            assert_eq!(named(not_a_name), None, "{not_a_name}");
            assert_eq!(ScalerKinds::of_name(not_a_name), None, "{not_a_name}");
        }
    }

    #[test]
    fn a_round_asks_for_the_children_of_what_it_is_asked_and_runs_them_all_at_once() {
        let full = Round {
            base: true,
            names: [true; NAMED.len()],
        };
        let children = full.children();
        // The probe's twenty children: the source, nine base chains, nine named chains and the `sin` check.
        assert_eq!(children.len(), 20);
        assert_eq!(children.iter().filter(|c| c.part == Part::Sin).count(), 1);
        assert_eq!(
            children.iter().filter(|c| matches!(c.part, Part::Base(_))).count(),
            CHAINS.len()
        );
        assert_eq!(children.iter().filter(|c| matches!(c.part, Part::Named(..))).count(), 9);
        // One name alone: its chains, and `sin` if it is Lanczos; the base is not asked again.
        for (n, want) in [(0, 4), (1, 3), (2, 3)] {
            let mut names = [false; NAMED.len()];
            names[n] = true;
            let children = Round { base: false, names }.children();
            assert_eq!(children.len(), want, "name {n}");
            assert!(children.iter().all(|c| match c.part {
                Part::Named(m, _) => m == n,
                Part::Sin => n == LANCZOS,
                _ => false,
            }));
        }
        // The thread budget is shared among the children that run side by side: every child is handed the count of the round, and they
        // are all running together (none waits for another to finish).
        for round in [
            full,
            Round {
                base: true,
                names: [false; 3],
            },
            Round {
                base: false,
                names: [false, true, false],
            },
        ] {
            let children = round.children();
            let running = std::sync::atomic::AtomicUsize::new(0);
            let together = std::sync::atomic::AtomicUsize::new(0);
            let shares = Mutex::new(Vec::new());
            let frames = collect(&children, |child, share| {
                running.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let until = Instant::now() + Duration::from_secs(10);
                while running.load(std::sync::atomic::Ordering::SeqCst) < share && Instant::now() < until {
                    std::thread::yield_now();
                }
                together.fetch_max(
                    running.load(std::sync::atomic::Ordering::SeqCst),
                    std::sync::atomic::Ordering::SeqCst,
                );
                lock_any(&shares).push(share);
                Some(vec![child.len as u8])
            });
            assert_eq!(frames.len(), children.len());
            assert_eq!(
                together.load(std::sync::atomic::Ordering::SeqCst),
                children.len(),
                "{round:?}"
            );
            assert!(lock_any(&shares).iter().all(|s| *s == children.len()), "{round:?}");
        }
    }

    fn lock_any<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An FFmpeg that answers like `model`, except for the children `fails` names, which fail (`None`).
    fn ffmpeg_of(model: Model, fails: impl Fn(Part) -> bool + Sync) -> impl Fn(&Child, usize) -> Option<Vec<u8>> + Sync {
        let source = probe_source_planes();
        move |child, _| {
            if fails(child.part) {
                None
            } else {
                exact_child(&source, model, child.part)
            }
        }
    }

    const ALL_NAMES: [bool; NAMED.len()] = [true; NAMED.len()];

    #[test]
    fn a_name_that_fails_to_measure_is_left_out_alone_and_the_base_stands() {
        for model in MODELS {
            let round = Round {
                base: true,
                names: ALL_NAMES,
            };
            // Whichever child of a name fails, it is that name that is open; the base answers as its own ten chains measured.
            for (n, (kind, chains)) in NAMED.iter().enumerate() {
                let mut parts: Vec<Part> = (0..chains.len()).map(|i| Part::Named(n, i)).collect();
                if n == LANCZOS {
                    parts.push(Part::Sin);
                }
                for failing in parts {
                    let outcome = measure_with(round, None, ffmpeg_of(model, |p| p == failing));
                    assert_eq!(outcome.base, Some(matched(model)), "{model:?} {kind:?} {failing:?}");
                    for (m, verdict) in outcome.names.iter().enumerate() {
                        let want = if m == n { Verdict::Open } else { Verdict::Proven };
                        assert_eq!(*verdict, Some(want), "{model:?} {kind:?} {failing:?} name {m}");
                    }
                }
            }
            // A base child that fails leaves the base open and the names without a model to be compared with: no verdict, and no failure.
            for failing in [Part::Source, Part::Base(0), Part::Base(CHAINS.len() - 1)] {
                let outcome = measure_with(round, None, ffmpeg_of(model, |p| p == failing));
                assert!(outcome.base.is_none(), "{model:?} {failing:?}");
                assert!(outcome.names.iter().all(Option::is_none), "{model:?} {failing:?}");
            }
            // A round for one name alone is compared with the model and the picture the base was found with.
            let source = probe_source_planes();
            let alone = Round {
                base: false,
                names: [false, true, false],
            };
            let outcome = measure_with(alone, Some((model, &source)), ffmpeg_of(model, |_| false));
            assert!(outcome.base.is_none());
            assert_eq!(outcome.names, [None, Some(Verdict::Proven), None]);
            assert_eq!(outcome.against, Some(model));
            // ... and with nothing to compare with, it has no verdict.
            let outcome = measure_with(alone, None, ffmpeg_of(model, |_| false));
            assert_eq!(outcome.names, [None; 3]);
        }
    }

    #[test]
    fn a_hung_child_is_killed_at_its_deadline_and_leaves_only_its_own_question_open() {
        // A child that never answers is read as a missing picture once its deadline passes (the probe's, here a short one): the base
        // stands and only the name that owns the child is open. The rest are the model's pictures.
        #[cfg(unix)]
        {
            let model = MODELS[0];
            let source = probe_source_planes();
            for (hung, expect) in [
                (Part::Sin, [Some(Verdict::Open), Some(Verdict::Proven), Some(Verdict::Proven)]),
                (
                    Part::Named(2, 1),
                    [Some(Verdict::Proven), Some(Verdict::Proven), Some(Verdict::Open)],
                ),
                (
                    Part::Named(1, 0),
                    [Some(Verdict::Proven), Some(Verdict::Open), Some(Verdict::Proven)],
                ),
            ] {
                let started = Instant::now();
                let outcome = measure_with(
                    Round {
                        base: true,
                        names: ALL_NAMES,
                    },
                    None,
                    |child: &Child, _| {
                        if child.part == hung {
                            let mut sleeper = std::process::Command::new("sh");
                            sleeper.args(["-c", "exec sleep 30"]);
                            crate::source::run_child(sleeper, child.len + 1, Duration::from_millis(300)).ok()
                        } else {
                            exact_child(&source, model, child.part)
                        }
                    },
                );
                assert!(
                    started.elapsed() < Duration::from_secs(10),
                    "{hung:?}: the deadline did not hold"
                );
                assert_eq!(outcome.base, Some(matched(model)), "{hung:?}");
                assert_eq!(outcome.names, expect, "{hung:?}");
            }
            // A hung base child leaves the base open, and with it every name: no model yet to compare with.
            let outcome = measure_with(
                Round {
                    base: true,
                    names: ALL_NAMES,
                },
                None,
                |child: &Child, _| {
                    if child.part == Part::Base(5) {
                        let mut sleeper = std::process::Command::new("sh");
                        sleeper.args(["-c", "exec sleep 30"]);
                        crate::source::run_child(sleeper, child.len + 1, Duration::from_millis(300)).ok()
                    } else {
                        exact_child(&source, model, child.part)
                    }
                },
            );
            assert!(outcome.base.is_none() && outcome.names.iter().all(Option::is_none));
        }
    }

    /// The outcome of an FFmpeg that is `model` except for the children `fails` names.
    fn outcome_of(round: Round, known: Option<(Model, &[u8])>, model: Model, fails: impl Fn(Part) -> bool + Sync) -> Outcome {
        measure_with(round, known, ffmpeg_of(model, fails))
    }

    fn named_of(k: Kernels) -> ScalerKinds {
        k.named()
    }

    #[test]
    fn each_question_backs_off_by_itself_and_only_the_failed_one_is_asked_again() {
        let model = MODELS[0];
        let s = state();
        let t0 = Instant::now();
        let rounds = Mutex::new(Vec::new());
        // The first call: the spline's children fail; the base and the other names are answered.
        let got = cached(&s, t0, |round, known| {
            lock_any(&rounds).push(round);
            outcome_of(round, known, model, |p| matches!(p, Part::Named(1, _)))
        });
        let want = ScalerKinds::BILINEAR.union(ScalerKinds::LANCZOS).union(ScalerKinds::NEIGHBOR);
        assert_eq!(got, Kernels::Match(Model { named: want, ..model }));
        assert!(got.exact());
        assert_eq!(
            lock_any(&rounds).as_slice(),
            [Round {
                base: true,
                names: ALL_NAMES
            }]
        );
        assert_eq!(lock(&s).base_slot.failures, 0, "the base did not fail");
        let failures = lock(&s).names.map(|n| n.slot.failures);
        assert_eq!(failures, [0, 1, 0]);
        // Backing off: nothing is run, and the answer stands as it was.
        assert_eq!(
            cached(&s, t0, |_, _| panic!("measured during the backoff")),
            got,
            "the base is kept"
        );
        assert_eq!(known(&s), got);
        // After the backoff: the spline alone, compared with the base's model and its picture. It comes back, and joins the set.
        let later = after(&lock(&s).names[1].slot);
        let all = cached(&s, later, |round, known| {
            lock_any(&rounds).push(round);
            assert!(known.is_some(), "the base's model and picture are handed over");
            outcome_of(round, known, model, |_| false)
        });
        assert_eq!(
            lock_any(&rounds)[1],
            Round {
                base: false,
                names: [false, true, false]
            }
        );
        assert_eq!(named_of(all), ScalerKinds::ALL);
        assert_eq!(
            all,
            Kernels::Match(Model {
                named: ScalerKinds::ALL,
                ..model
            })
        );
        // Everything is answered: nothing is run again.
        assert_eq!(cached(&s, later, |_, _| panic!("measured twice")), all);
    }

    #[test]
    fn a_name_that_keeps_failing_backs_off_longer_and_does_not_hold_the_base_back() {
        let model = MODELS[0];
        let s = state();
        let t0 = Instant::now();
        let hang_neighbor =
            |round: Round, known: Option<(Model, &[u8])>| outcome_of(round, known, model, |p| matches!(p, Part::Named(2, _)));
        let first = cached(&s, t0, hang_neighbor);
        assert!(first.exact() && !first.named().contains(ScalerKinds::NEIGHBOR) && first.named().contains(ScalerKinds::SPLINE));
        let mut waits = Vec::new();
        for _ in 0..3 {
            // Asked again just after the backoff: it fails again, and waits twice as long.
            let at = lock(&s).names[2].slot.retry_at.unwrap() + Duration::from_millis(1);
            let again = cached(&s, at, hang_neighbor);
            assert!(again.exact() && !again.named().contains(ScalerKinds::NEIGHBOR));
            let retry = lock(&s).names[2].slot.retry_at.unwrap();
            waits.push(retry.saturating_duration_since(Instant::now()).as_secs_f64());
        }
        for (wait, want) in waits.iter().zip([10.0, 20.0, 40.0]) {
            assert!((want - 2.0..=want).contains(wait), "{waits:?}");
        }
        assert_eq!(lock(&s).names[2].slot.failures, 4);
        // The base was asked once and never again; the other names are settled.
        assert_eq!(lock(&s).base_slot.failures, 0);
        assert_eq!(lock(&s).names[1].slot.failures, 0);
    }

    #[test]
    fn a_base_that_fails_leaves_the_names_unasked_until_it_is_measured() {
        let model = MODELS[2];
        let s = state();
        let t0 = Instant::now();
        let rounds = Mutex::new(Vec::new());
        let got = cached(&s, t0, |round, known| {
            lock_any(&rounds).push(round);
            outcome_of(round, known, model, |p| p == Part::Base(3))
        });
        assert_eq!(got, Kernels::Unknown);
        assert_eq!(known(&s), Kernels::Unknown);
        // No name has failed: it had nothing to be compared with.
        assert!(lock(&s).names.iter().all(|n| n.slot.failures == 0 && n.settled.is_none()));
        assert_eq!(lock(&s).base_slot.failures, 1);
        assert_eq!(cached(&s, t0, |_, _| panic!("measured during the backoff")), Kernels::Unknown);
        // After the backoff the base and the names are asked together, and they all come back.
        let later = after(&lock(&s).base_slot);
        let got = cached(&s, later, |round, known| {
            lock_any(&rounds).push(round);
            outcome_of(round, known, model, |_| false)
        });
        assert_eq!(
            got,
            Kernels::Match(Model {
                named: ScalerKinds::ALL,
                ..model
            })
        );
        let full = Round {
            base: true,
            names: ALL_NAMES,
        };
        assert_eq!(lock_any(&rounds).as_slice(), [full, full]);
    }

    #[test]
    fn a_kernel_that_differs_asks_nothing_about_the_names_and_a_name_that_differs_is_out_for_good() {
        let model = MODELS[4];
        let s = state();
        let t0 = Instant::now();
        // The base's chains match no model: the answer is `Differs`, whatever the names' pictures are, and the names are never asked.
        let got = cached(&s, t0, |round, known| {
            let mut o = outcome_of(round, known, model, |_| false);
            o.base = Some(Base::Differs);
            o
        });
        assert_eq!(got, Kernels::Differs);
        assert_eq!(
            cached(&s, t0, |_, _| panic!("asked about a build that differs")),
            Kernels::Differs
        );
        assert!(lock(&s).round(t0 + Duration::from_secs(3600)).is_empty());
        // A name whose tables differ is settled as such, and is not asked again.
        let s = state();
        let got = cached(&s, t0, |round, known| {
            let mut o = outcome_of(round, known, model, |_| false);
            o.names[0] = Some(Verdict::Differs);
            o
        });
        assert_eq!(got.named(), ScalerKinds::ALL.without(ScalerKinds::LANCZOS));
        assert_eq!(cached(&s, t0 + Duration::from_secs(3600), |_, _| panic!("asked again")), got);
        assert_eq!(lock(&s).names[0].slot.failures, 0);
    }

    #[test]
    fn a_name_found_against_another_model_than_the_one_that_stands_is_not_believed() {
        // (Two callers that measured at once: the first base stored wins, and the names found through another model are dropped.)
        let ours = MODELS[0];
        let other = MODELS[4];
        let s = state();
        let t0 = Instant::now();
        let got = cached(&s, t0, |round, known| {
            let mut o = outcome_of(round, known, ours, |_| false);
            // A concurrent caller stores its answer while this one measures; its model is the one that stands.
            lock(&s).base = Some(matched(other));
            o.base = Some(matched(ours));
            o
        });
        assert_eq!(got, Kernels::Match(other));
        assert!(lock(&s).names.iter().all(|n| n.settled.is_none()));
    }

    #[test]
    fn an_unknown_answer_is_retried_after_a_backoff_and_a_measured_one_is_kept() {
        let s = state();
        let t0 = Instant::now();
        let model = MODELS[0];
        let failing = |round: Round, known: Option<(Model, &[u8])>| outcome_of(round, known, model, |p| p == Part::Source);
        assert_eq!(cached(&s, t0, failing), Kernels::Unknown);
        // Backing off: the probe is not run.
        assert_eq!(cached(&s, t0, |_, _| panic!("measured during the backoff")), Kernels::Unknown);
        let later = after(&lock(&s).base_slot);
        let m = Kernels::Match(Model {
            named: ScalerKinds::ALL,
            ..model
        });
        assert_eq!(cached(&s, later, |r, k| outcome_of(r, k, model, |_| false)), m);
        assert_eq!(
            cached(&s, later, |_, _| panic!("measured twice")),
            m,
            "a measured answer is kept for good"
        );
        assert_eq!(lock(&s).base_slot.failures, 1);
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
    #[test]
    fn a_self_test_that_cannot_tell_holds_the_scaler_back_until_one_can() {
        let t0 = Instant::now();
        let model = MODELS[0];
        let fine = |round: Round, known: Option<(Model, &[u8])>| outcome_of(round, known, model, |_| false);
        let broken = |round: Round, known: Option<(Model, &[u8])>| outcome_of(round, known, model, |p| p == Part::Base(1));
        let st = state();
        assert_eq!(checked(&st, t0, broken), Kernels::Unknown);
        assert!(lock(&st).held_back);
        // A probe that would succeed now is not believed (and not run): the check left the resized frames out.
        let later = t0 + Duration::from_secs(60);
        assert_eq!(cached(&st, later, |_, _| panic!("probed while held back")), Kernels::Unknown);
        assert_eq!(known(&st), Kernels::Unknown);
        // The next self-test that can tell lifts the hold, and the answer stands.
        let m = checked(&st, later, fine);
        assert_eq!(
            m,
            Kernels::Match(Model {
                named: ScalerKinds::ALL,
                ..model
            })
        );
        assert!(!lock(&st).held_back);
        assert_eq!(known(&st), m);
        // A name that did not come back does not hold the base back: the self-test draws the base's frames.
        let st = state();
        let spline_fails =
            |round: Round, known: Option<(Model, &[u8])>| outcome_of(round, known, model, |p| matches!(p, Part::Named(1, _)));
        let m = checked(&st, t0, spline_fails);
        assert!(m.exact() && !lock(&st).held_back);
        // A difference is permanent and holds nothing back.
        let st = state();
        let differs = |round: Round, known: Option<(Model, &[u8])>| {
            let mut o = outcome_of(round, known, model, |_| false);
            o.base = Some(Base::Differs);
            o
        };
        assert_eq!(checked(&st, t0, differs), Kernels::Differs);
        assert!(!lock(&st).held_back);
    }
}
