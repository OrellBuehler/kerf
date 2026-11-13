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

use std::sync::{Arc, Condvar, Mutex, PoisonError};
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

/// The base answer a round that asks for names alone compares their pictures with: the model and the probe picture the base was found with.
#[derive(Debug, Clone)]
struct Known {
    model: Model,
    source: Vec<u8>,
}

/// What a round found out about its names. A verdict is `None` when the name was not asked or there is no model yet to compare its pictures
/// with (the base is open).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Names {
    verdicts: [Option<Verdict>; NAMED.len()],
    /// The model the names' pictures were compared through.
    against: Option<Model>,
}

/// Run a round (`run` is one child) and classify what came back, in two phases: **the base is reported (`on_base`) as soon as its own
/// children are in**, while the names' children may still be running, and then the names are classified. All the children are started at
/// once, each handed the thread budget's share of **all of them** (`share` is the number of `ffmpeg`s that run side by side, which is
/// what [`kerf_core::limit_frame_args`] divides the budget by). `known` is the base already measured, what a round that asks for names
/// alone compares with.
fn measure_with(
    round: Round,
    known: Option<&Known>,
    run: impl Fn(&Child, usize) -> Option<Vec<u8>> + Sync,
    on_base: &mut dyn FnMut(Option<Base>),
) -> Names {
    let children = round.children();
    let share = children.len();
    let run = &run;
    std::thread::scope(|scope| {
        let mut handles: Vec<_> = children
            .iter()
            .map(|child| Some(scope.spawn(move || run(child, share))))
            .collect();
        let mut take = |part: Part| -> Option<Vec<u8>> {
            let at = children.iter().position(|c| c.part == part)?;
            handles[at].take()?.join().ok().flatten()
        };
        let mut base = None;
        if round.base {
            let picture = take(Part::Source);
            let chains: Vec<_> = (0..CHAINS.len()).map(|i| take(Part::Base(i))).collect();
            base = picture.as_deref().and_then(|p| classify_base(p, &chains));
            on_base(base.clone());
        }
        // The model to compare the names with: the base this round found, else the one measured before.
        let against: Option<Known> = match &base {
            Some(Base::Match { model, source }) => Some(Known {
                model: *model,
                source: source.clone(),
            }),
            Some(Base::Differs) => None,
            None if round.base => None,
            None => known.cloned(),
        };
        let mut verdicts = [None; NAMED.len()];
        if let Some(Known { model, source }) = &against {
            for (n, verdict) in verdicts.iter_mut().enumerate().filter(|(n, _)| round.names[*n]) {
                let pictures: Vec<_> = (0..NAMED_PER).map(|i| take(Part::Named(n, i))).collect();
                let sin = if n == LANCZOS { take(Part::Sin) } else { None };
                *verdict = Some(classify_name(source, *model, n, &pictures, sin.as_deref()));
            }
        }
        Names {
            verdicts,
            against: against.map(|k| k.model),
        }
    })
}

/// What the process measures with: a round of `ffmpeg` children.
type Measure = dyn Fn(Round, Option<Known>, &mut dyn FnMut(Option<Base>)) -> Names + Send + Sync;

fn measure_ffmpeg(round: Round, known: Option<Known>, on_base: &mut dyn FnMut(Option<Base>)) -> Names {
    measure_with(round, known.as_ref(), run_ffmpeg, on_base)
}

/// Where a detached retry runs (`spawn`, in [`Probe::settle`]): `false` when no thread could be started.
fn spawn_detached(task: Box<dyn FnOnce() + Send + 'static>) -> bool {
    std::thread::Builder::new().name("scaler-probe".into()).spawn(task).is_ok()
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

/// A question that is being measured: by when it must be over (a flight that outlives it no longer holds the question), and whether it is
/// a retry on a detached thread, which nobody waits for.
#[derive(Debug, Clone, Copy)]
struct Flight {
    until: Instant,
    background: bool,
}

impl Flight {
    fn alive(flight: &Option<Self>, clock: Instant) -> bool {
        flight.is_some_and(|f| f.until > clock)
    }
}

/// A named scaler's question: `settled` is `Some(true)` once proven and `Some(false)` once found different.
#[derive(Debug, Clone, Copy)]
struct Name {
    settled: Option<bool>,
    slot: Slot,
    flight: Option<Flight>,
}

struct State {
    base: Option<Base>,
    base_slot: Slot,
    base_flight: Option<Flight>,
    names: [Name; NAMED.len()],
    /// A self-test could not tell whether the scaler is the model, so it did not draw a resized frame: nothing is believed
    /// until a self-test has (a probe that succeeds later must not open resized layers the check never looked at).
    held_back: bool,
}

/// What a caller does now: the questions it measures itself (it waits for them), and the names it has retried on a detached thread (it
/// does not).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Plan {
    inline: Round,
    background: Round,
}

impl State {
    const fn new() -> Self {
        Self {
            base: None,
            base_slot: Slot::NEW,
            base_flight: None,
            names: [Name {
                settled: None,
                slot: Slot::NEW,
                flight: None,
            }; NAMED.len()],
            held_back: false,
        }
    }

    /// What there is to ask now of what the caller `wanted` (the names its delivery uses; the base always):
    ///
    /// * the base, unless it is answered, backing off or being measured by another caller (it is always measured by the caller that needs
    ///   it: the caller waits for it);
    /// * each wanted name never asked before, **when there is a model to compare it with** (the base answered with a match, or asked in
    ///   the same round): the caller waits for it too;
    /// * each wanted name that has failed before and is due: a retry, run on a detached thread that nobody waits for, so a name that hangs
    ///   again costs no caller 20 s.
    ///
    /// A question that is already being measured is not asked again (single flight). The questions asked are marked as flying, to end
    /// by `bound`.
    ///
    /// `again` is the plan a caller makes **after** it has waited for the measurements of others, for the names it could not ask before
    /// for want of a model (the base was another caller's, still flying): it never asks the base, and it leaves alone a name whose flight
    /// was left marked past its bound (a hung measurement is not taken over by a caller that has just waited out its bound).
    fn plan(&mut self, now: Instant, wanted: ScalerKinds, bound: Duration, again: bool) -> Plan {
        let clock = Instant::now();
        let base = !again && self.base.is_none() && self.base_slot.due(now) && !Flight::alive(&self.base_flight, clock);
        let matched = matches!(self.base, Some(Base::Match { .. }));
        let mut inline = Round {
            base,
            names: [false; NAMED.len()],
        };
        let mut background = Round {
            base: false,
            names: [false; NAMED.len()],
        };
        for (n, ((kind, _), name)) in NAMED.iter().zip(&self.names).enumerate() {
            if !wanted.contains(*kind) || name.settled.is_some() || Flight::alive(&name.flight, clock) || !name.slot.due(now) {
                continue;
            }
            if again && name.flight.is_some() {
                continue;
            }
            if name.slot.failures > 0 {
                background.names[n] = matched;
            } else {
                inline.names[n] = matched || base;
            }
        }
        let until = clock + bound;
        self.base_flight = if inline.base {
            Some(Flight {
                until,
                background: false,
            })
        } else {
            self.base_flight
        };
        for (n, name) in self.names.iter_mut().enumerate() {
            if inline.names[n] || background.names[n] {
                name.flight = Some(Flight {
                    until,
                    background: background.names[n],
                });
            }
        }
        Plan { inline, background }
    }

    /// The questions of `round` are no longer being measured.
    fn land(&mut self, round: Round) {
        if round.base {
            self.base_flight = None;
        }
        for (name, asked) in self.names.iter_mut().zip(round.names) {
            if asked {
                name.flight = None;
            }
        }
    }

    /// By when the answers a caller `wanted` stand to come from measurements other callers are making and it should wait for: the base,
    /// while it is not answered, and the names it wants that are not settled (a retry on a detached thread is not waited for).
    fn waiting_on(&self, wanted: ScalerKinds, clock: Instant) -> Option<Instant> {
        let base = self.base.is_none().then_some(self.base_flight).flatten();
        let names = NAMED
            .iter()
            .zip(&self.names)
            .filter(|((kind, _), name)| wanted.contains(*kind) && name.settled.is_none())
            .filter_map(|(_, name)| name.flight)
            .filter(|f| !f.background);
        base.into_iter()
            .chain(names)
            .filter(|f| f.until > clock)
            .map(|f| f.until)
            .max()
    }

    /// The base answer, as what a round that asks for names alone compares with.
    fn known(&self) -> Option<Known> {
        match &self.base {
            Some(Base::Match { model, source }) => Some(Known {
                model: *model,
                source: source.clone(),
            }),
            _ => None,
        }
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

    /// Store what the base's children found. A base already stored stands; one that did not come back backs off.
    fn absorb_base(&mut self, base: Option<Base>, now: Instant) {
        if self.base.is_some() {
            return;
        }
        match base {
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

    /// Store what the children of the names of `round` found; a name that did not come back backs off alone.
    fn absorb_names(&mut self, round: Round, found: Names, now: Instant) {
        // (The names' pictures were compared through the model of the base that stands.)
        let Some(Base::Match { model, .. }) = &self.base else {
            return;
        };
        if found.against != Some(*model) {
            return;
        }
        for (n, (name, verdict)) in self.names.iter_mut().zip(found.verdicts).enumerate() {
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

/// The scaler's memory for the process, with its single flight: a question is measured by one caller at a time, whoever asks. Callers that
/// need it wait for that measurement, no longer than its bound ([`Flight`]) and, if they cannot have it, are told what is known.
struct Probe {
    state: Mutex<State>,
    /// Signalled when a measurement stores its answer or ends.
    ended: Condvar,
    /// How long a measurement may take.
    bound: Duration,
}

/// A running measurement, ended (its questions landed, the waiting callers woken) when this is dropped, a panic included.
struct Flying<'a> {
    probe: &'a Probe,
    round: Round,
}

impl Drop for Flying<'_> {
    fn drop(&mut self) {
        lock(&self.probe.state).land(self.round);
        self.probe.ended.notify_all();
    }
}

/// A round of children, one measurement of a probe takes; the children of a round are bounded by [`PROBE_TIMEOUT`] each and run together,
/// so a round is over in a little more than that, and the flight's bound allows for the model's comparison of the pictures.
const FLIGHT_BOUND: Duration = Duration::from_secs(25);

static SCALER: Probe = Probe::new(FLIGHT_BOUND);

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Probe {
    const fn new(bound: Duration) -> Self {
        Self {
            state: Mutex::new(State::new()),
            ended: Condvar::new(),
            bound,
        }
    }

    /// What is known without running anything: the measured answer, else [`Kernels::Unknown`].
    fn known(&self) -> Kernels {
        let s = lock(&self.state);
        if s.held_back {
            return Kernels::Unknown;
        }
        s.view()
    }

    /// The base and the names `wanted`, measured if they are not, with `measure`; the retry of a name that failed before goes to `spawn`.
    /// `now` is the instant the backoffs are judged at.
    fn settle(
        &'static self,
        now: Instant,
        wanted: ScalerKinds,
        measure: &Arc<Measure>,
        spawn: &(dyn Fn(Box<dyn FnOnce() + Send + 'static>) -> bool + Sync),
    ) -> Kernels {
        // Two passes. The first asks for what is due, and then waits for the measurements of other callers that the answer stands on. A caller
        // that came while another was still measuring the base could not ask for its name (a name is compared with the model the base
        // gives): the second pass asks for it now that the base has landed, so that the caller is not told "not measured" for a name nobody
        // is measuring. What it waits for is bounded by the flights it joins, and it asks only for the names it wants.
        let mut told = Kernels::Unknown;
        for again in [false, true] {
            let (plan, known) = {
                let mut s = lock(&self.state);
                if s.held_back {
                    return Kernels::Unknown;
                }
                let plan = s.plan(now, wanted, self.bound, again);
                (plan, s.known())
            };
            if again && plan.inline.is_empty() && plan.background.is_empty() {
                break;
            }
            if !plan.background.is_empty() {
                let (probe, measure, known, round) = (self, Arc::clone(measure), known.clone(), plan.background);
                let started = spawn(Box::new(move || probe.run(round, known, &measure, now)));
                if !started {
                    // Nothing is measuring: the names stay due, and the next caller tries again.
                    lock(&self.state).land(round);
                    self.ended.notify_all();
                }
            }
            if !plan.inline.is_empty() {
                self.run(plan.inline, if plan.inline.base { None } else { known }, measure, now);
            }
            told = self.follow(wanted);
        }
        told
    }

    /// Measure `round` and store what it finds: the base the moment its own children are in (callers that need only the base are woken
    /// then, while the names' children may still be running), the names when theirs are.
    fn run(&self, round: Round, known: Option<Known>, measure: &Arc<Measure>, now: Instant) {
        let flying = Flying { probe: self, round };
        let found = measure(round, known, &mut |base| {
            lock(&self.state).absorb_base(base, now);
            self.ended.notify_all();
        });
        lock(&self.state).absorb_names(round, found, now);
        drop(flying);
    }

    /// Wait for the measurements of other callers that the answer `wanted` stands on, each no longer than its bound, and say what is known.
    fn follow(&self, wanted: ScalerKinds) -> Kernels {
        let mut s = lock(&self.state);
        loop {
            if s.held_back {
                return Kernels::Unknown;
            }
            let clock = Instant::now();
            let Some(until) = s.waiting_on(wanted, clock) else {
                return s.view();
            };
            s = self
                .ended
                .wait_timeout(s, until.saturating_duration_since(clock))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// [`Probe::settle`] for the self-test, which draws resized frames only for a base that is exact: a base that cannot be told holds the
    /// scaler back until a self-test that can has run.
    fn checked(
        &'static self,
        now: Instant,
        measure: &Arc<Measure>,
        spawn: &(dyn Fn(Box<dyn FnOnce() + Send + 'static>) -> bool + Sync),
    ) -> Kernels {
        lock(&self.state).held_back = false;
        let k = self.settle(now, ScalerKinds::NONE, measure, spawn);
        if k == Kernels::Unknown {
            lock(&self.state).held_back = true;
        }
        k
    }
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300))
}

fn real_measure() -> Arc<Measure> {
    Arc::new(measure_ffmpeg)
}

/// Whether this FFmpeg's `scale` is the model ([`Kernels`]), and which of the named scalers are proven: [`kernels_for`] with every name.
/// Twenty short `ffmpeg` children on the first call (the unflagged `scale`, nine chains of the bicubic and bilinear scalers, nine of the
/// named ones and the `sin` check; see there).
pub fn kernels() -> Kernels {
    kernels_for(ScalerKinds::ALL)
}

/// Whether this FFmpeg's `scale` is the model, and whether the named scalers `names` are proven (those of a delivery's scaler: none for
/// the default and for `bicubic`, one of [`ScalerKinds::of_name`] otherwise), measured if they are not: a blocking thread's, with no lock
/// held, ahead of the frame that needs it ([`crate::prepare`]). The first call runs the base's ten children, each bounded by 20 s and
/// handed an equal share of the thread budget, and those of each name asked for (its three chains, and Lanczos's `sin` check), all at
/// once. **Only what is asked for is measured, and waited for**: a project that names no scaler never waits for a name.
///
/// The base scaler and each name are separate questions, measured by one caller at a time (the others wait for that measurement, no
/// longer than its bound of 25 s, and are told what is known if it has not ended). [`Kernels::Unknown`] is the base's alone (ten children
/// that did not all come back) and is not remembered: the base is measured again, by whoever asks, after a backoff. A named scaler whose
/// children did not come back is left out of [`Model::named`], and **asked again alone** (its three or four children) after a backoff of
/// its own **on a detached thread**: the call that finds it due answers at once with what is known, and the answer joins the set for the
/// calls after, so that a name that hangs again costs nobody its 20 s. The base's answer stands throughout.
pub fn kernels_for(names: ScalerKinds) -> Kernels {
    SCALER.settle(Instant::now(), names, &real_measure(), &spawn_detached)
}

/// What is known without running anything: the measured answer, else [`Kernels::Unknown`]. What a plan's caps are
/// made from ([`crate::Compositor::caps`]): a plan does not wait for a probe.
pub fn measured() -> Kernels {
    SCALER.known()
}

/// [`kernels_for`] no name, for the runtime self-test, which draws resized frames only when this is [`Kernels::exact`]. An `Unknown` here
/// holds the scaler back for the process until a self-test that can tell has run, so that the frames the GPU draws are never
/// the ones its check left out; a `Differs` is permanent and needs no holding.
pub(crate) fn kernels_for_selftest() -> Kernels {
    SCALER.checked(Instant::now(), &real_measure(), &spawn_detached)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let mut off = frames;
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
            let outcome = outcome_with(round, None, |child: &Child, share| {
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
            // (Pictures of no model: nothing is told of the base, and the names have nothing to be compared with.)
            assert!(
                outcome.base.is_none() && outcome.names.iter().all(Option::is_none),
                "{round:?}"
            );
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

    /// What a round found, base and names together.
    struct Outcome {
        base: Option<Base>,
        names: [Option<Verdict>; NAMED.len()],
        against: Option<Model>,
    }

    fn outcome_with(round: Round, known: Option<&Known>, run: impl Fn(&Child, usize) -> Option<Vec<u8>> + Sync) -> Outcome {
        let (mut base, mut reported) = (None, 0);
        let found = measure_with(round, known, run, &mut |b| {
            base = b;
            reported += 1;
        });
        // (The base is reported once if it was asked, and only then.)
        assert_eq!(reported, usize::from(round.base));
        Outcome {
            base,
            names: found.verdicts,
            against: found.against,
        }
    }

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
                    let outcome = outcome_with(round, None, ffmpeg_of(model, |p| p == failing));
                    assert_eq!(outcome.base, Some(matched(model)), "{model:?} {kind:?} {failing:?}");
                    for (m, verdict) in outcome.names.iter().enumerate() {
                        let want = if m == n { Verdict::Open } else { Verdict::Proven };
                        assert_eq!(*verdict, Some(want), "{model:?} {kind:?} {failing:?} name {m}");
                    }
                }
            }
            // A base child that fails leaves the base open and the names without a model to be compared with: no verdict, and no failure.
            for failing in [Part::Source, Part::Base(0), Part::Base(CHAINS.len() - 1)] {
                let outcome = outcome_with(round, None, ffmpeg_of(model, |p| p == failing));
                assert!(outcome.base.is_none(), "{model:?} {failing:?}");
                assert!(outcome.names.iter().all(Option::is_none), "{model:?} {failing:?}");
            }
            // A round for one name alone is compared with the model and the picture the base was found with.
            let source = probe_source_planes();
            let alone = Round {
                base: false,
                names: [false, true, false],
            };
            let known = Known { model, source };
            let outcome = outcome_with(alone, Some(&known), ffmpeg_of(model, |_| false));
            assert!(outcome.base.is_none());
            assert_eq!(outcome.names, [None, Some(Verdict::Proven), None]);
            assert_eq!(outcome.against, Some(model));
            // ... and with nothing to compare with, it has no verdict.
            let outcome = outcome_with(alone, None, ffmpeg_of(model, |_| false));
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
                let outcome = outcome_with(
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
            let outcome = outcome_with(
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

    // ---- the process's answer: single flight, retries off the caller's path ---------------------------------------------

    /// A probe of its own, kept for the test (the detached threads need a `'static` one).
    fn probe(bound: Duration) -> &'static Probe {
        Box::leak(Box::new(Probe::new(bound)))
    }

    /// A measurement of an FFmpeg that is `model`, except for the children `fails` names.
    fn fake(model: Model, fails: impl Fn(Part) -> bool + Send + Sync + 'static) -> Arc<Measure> {
        let run = ffmpeg_of(model, fails);
        Arc::new(
            move |round: Round, known: Option<Known>, on_base: &mut dyn FnMut(Option<Base>)| {
                measure_with(round, known.as_ref(), &run, on_base)
            },
        )
    }

    /// `inner`, with the rounds it was asked for written down.
    fn recording(inner: Arc<Measure>, rounds: &Arc<Mutex<Vec<Round>>>) -> Arc<Measure> {
        let rounds = Arc::clone(rounds);
        Arc::new(
            move |round: Round, known: Option<Known>, on_base: &mut dyn FnMut(Option<Base>)| {
                lock_any(&rounds).push(round);
                inner(round, known, on_base)
            },
        )
    }

    /// A door that stays shut until it is opened.
    struct Gate(Mutex<bool>, Condvar);

    impl Gate {
        fn new() -> Arc<Self> {
            Arc::new(Self(Mutex::new(false), Condvar::new()))
        }

        fn open(&self) {
            *lock_any(&self.0) = true;
            self.1.notify_all();
        }

        /// Waits for the door, 30 s at most (a test that never opens it fails rather than hangs).
        fn wait(&self) {
            let until = Instant::now() + Duration::from_secs(30);
            let mut open = lock_any(&self.0);
            while !*open {
                let left = until.saturating_duration_since(Instant::now());
                assert!(!left.is_zero(), "the gate was never opened");
                open = self.1.wait_timeout(open, left).unwrap_or_else(PoisonError::into_inner).0;
            }
        }
    }

    /// `inner`, except that a round `when` is true of is held at the gate after the base has been reported (the names' children still
    /// running, as far as the probe can tell), and `reached` is told it got there.
    fn held(
        inner: Arc<Measure>,
        when: impl Fn(Round) -> bool + Send + Sync + 'static,
        gate: &Arc<Gate>,
        reached: std::sync::mpsc::Sender<Round>,
    ) -> Arc<Measure> {
        let gate = Arc::clone(gate);
        let reached = Mutex::new(reached);
        Arc::new(
            move |round: Round, known: Option<Known>, on_base: &mut dyn FnMut(Option<Base>)| {
                let found = inner(round, known, on_base);
                if when(round) {
                    let _ = lock_any(&reached).send(round);
                    gate.wait();
                }
                found
            },
        )
    }

    /// The detached threads a test runs by hand.
    type Tasks = Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>;

    fn run_tasks(tasks: &Tasks) -> usize {
        let todo: Vec<_> = std::mem::take(&mut *lock_any(tasks));
        let n = todo.len();
        for t in todo {
            t();
        }
        n
    }

    const NO_NAMES: ScalerKinds = ScalerKinds::NONE;

    fn match_of(model: Model, named: ScalerKinds) -> Kernels {
        Kernels::Match(Model {
            named: ScalerKinds::BILINEAR.union(named),
            ..model
        })
    }

    #[test]
    fn only_what_is_asked_for_is_measured_and_waited_for() {
        let model = MODELS[0];
        let p = probe(Duration::from_secs(60));
        let rounds = Arc::new(Mutex::new(Vec::new()));
        let m = recording(fake(model, |_| false), &rounds);
        let spawn = |_: Box<dyn FnOnce() + Send>| -> bool { panic!("a detached thread for a first measurement") };
        let t0 = Instant::now();
        // A project that names no scaler: the base alone (ten children), no name.
        let got = p.settle(t0, NO_NAMES, &m, &spawn);
        assert_eq!(got, match_of(model, ScalerKinds::NONE));
        assert!(got.exact());
        let base_only = Round {
            base: true,
            names: [false; NAMED.len()],
        };
        assert_eq!(base_only.children().len(), 10);
        assert_eq!(lock_any(&rounds).as_slice(), [base_only]);
        // Another that names Lanczos: Lanczos alone, through the model that stands (three chains and the `sin`).
        let got = p.settle(t0, ScalerKinds::LANCZOS, &m, &spawn);
        assert_eq!(got, match_of(model, ScalerKinds::LANCZOS));
        let lanczos = Round {
            base: false,
            names: [true, false, false],
        };
        assert_eq!(lanczos.children().len(), 4);
        assert_eq!(lock_any(&rounds).as_slice(), [base_only, lanczos]);
        // `bilinear` is the base's, and a name of the engine's list asked for twice is measured once.
        assert_eq!(p.settle(t0, ScalerKinds::BILINEAR, &m, &spawn), got);
        assert_eq!(p.settle(t0, ScalerKinds::LANCZOS, &m, &spawn), got);
        assert_eq!(lock_any(&rounds).len(), 2);
        // Every name at once is what `kernels()` asks: the two that are left, together.
        let all = p.settle(t0, ScalerKinds::ALL, &m, &spawn);
        assert_eq!(all, match_of(model, ScalerKinds::ALL));
        assert_eq!(
            lock_any(&rounds)[2],
            Round {
                base: false,
                names: [false, true, true]
            }
        );
    }

    #[test]
    fn each_question_backs_off_by_itself_and_only_the_failed_one_is_asked_again_on_a_detached_thread() {
        let model = MODELS[0];
        let p = probe(Duration::from_secs(60));
        let rounds = Arc::new(Mutex::new(Vec::new()));
        let tasks: Tasks = Mutex::new(Vec::new());
        let spawn = |t: Box<dyn FnOnce() + Send>| {
            lock_any(&tasks).push(t);
            true
        };
        let t0 = Instant::now();
        // The first call: the spline's children fail; the base and the other names are answered.
        let failing = recording(fake(model, |p| matches!(p, Part::Named(1, _))), &rounds);
        let got = p.settle(t0, ScalerKinds::ALL, &failing, &spawn);
        let want = ScalerKinds::LANCZOS.union(ScalerKinds::NEIGHBOR);
        assert_eq!(got, match_of(model, want));
        assert!(got.exact());
        assert_eq!(
            lock_any(&rounds).as_slice(),
            [Round {
                base: true,
                names: ALL_NAMES
            }]
        );
        assert_eq!(lock_any(&p.state).base_slot.failures, 0, "the base did not fail");
        let failures = lock_any(&p.state).names.map(|n| n.slot.failures);
        assert_eq!(failures, [0, 1, 0]);
        // Backing off: nothing is run, and the answer stands as it was.
        let never: Arc<Measure> = Arc::new(|_, _, _| panic!("measured during the backoff"));
        assert_eq!(p.settle(t0, ScalerKinds::ALL, &never, &spawn), got);
        assert_eq!(run_tasks(&tasks), 0);
        assert_eq!(p.known(), got);
        // After the backoff: the caller is told what is known **at once**, and the spline alone is asked on a detached thread, compared
        // with the base's model and picture.
        let later = lock_any(&p.state).names[1].slot.retry_at.unwrap() + Duration::from_millis(1);
        let fine = recording(fake(model, |_| false), &rounds);
        assert_eq!(
            p.settle(later, ScalerKinds::ALL, &fine, &spawn),
            got,
            "told what is known, at once"
        );
        assert_eq!(lock_any(&rounds).len(), 1, "nothing ran on the caller's thread");
        // ... one retry, whoever asks meanwhile (single flight).
        assert_eq!(p.settle(later, ScalerKinds::ALL, &never, &spawn), got);
        assert_eq!(lock_any(&tasks).len(), 1);
        assert_eq!(run_tasks(&tasks), 1);
        assert_eq!(
            lock_any(&rounds)[1],
            Round {
                base: false,
                names: [false, true, false]
            }
        );
        let all = p.settle(later, ScalerKinds::ALL, &never, &spawn);
        assert_eq!(all, match_of(model, ScalerKinds::ALL));
        assert_eq!(run_tasks(&tasks), 0, "everything is answered: nothing is run again");
    }

    #[test]
    fn a_retry_of_a_name_that_hangs_delays_no_caller_and_no_caller_that_did_not_ask_for_it() {
        let model = MODELS[0];
        let p = probe(Duration::from_secs(60));
        let t0 = Instant::now();
        let ordinary: Tasks = Mutex::new(Vec::new());
        let spawn = |t: Box<dyn FnOnce() + Send>| {
            lock_any(&ordinary).push(t);
            true
        };
        // The spline failed once (its children failed).
        let m = fake(model, |p| matches!(p, Part::Named(1, _)));
        let known = p.settle(t0, ScalerKinds::ALL, &m, &spawn);
        assert_eq!(known, match_of(model, ScalerKinds::LANCZOS.union(ScalerKinds::NEIGHBOR)));
        // Its backoff has passed, and its children hang this time: the retry is held at a gate.
        let later = lock_any(&p.state).names[1].slot.retry_at.unwrap() + Duration::from_millis(1);
        let gate = Gate::new();
        let (tx, reached) = std::sync::mpsc::channel();
        let hanging = held(fake(model, |_| false), |r| r.names[1], &gate, tx);
        let never: Arc<Measure> = Arc::new(|_, _, _| panic!("measured on a caller's thread"));
        let tasks: Arc<Tasks> = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();
        // A bicubic project asks for no name: told at once, and the retry is not even started for it.
        let spawn_held = {
            let tasks = Arc::clone(&tasks);
            move |t: Box<dyn FnOnce() + Send>| {
                lock_any(&tasks).push(t);
                true
            }
        };
        assert_eq!(p.settle(later, NO_NAMES, &hanging, &spawn_held), known);
        assert_eq!(lock_any(&tasks).len(), 0, "a caller that did not ask for the name retried it");
        // A caller that asks for it is told what is known, at once, and the retry goes to a detached thread.
        assert_eq!(p.settle(later, ScalerKinds::SPLINE, &hanging, &spawn_held), known);
        assert_eq!(lock_any(&tasks).len(), 1);
        let retry = std::thread::spawn({
            let task = lock_any(&tasks).pop().unwrap();
            move || task()
        });
        let round = reached.recv_timeout(Duration::from_secs(30)).expect("the retry ran");
        assert_eq!(
            round,
            Round {
                base: false,
                names: [false, true, false]
            }
        );
        // ... and while it hangs, nobody waits for it, whatever they ask for, and nobody starts another.
        for wanted in [NO_NAMES, ScalerKinds::SPLINE, ScalerKinds::ALL, ScalerKinds::LANCZOS] {
            assert_eq!(p.settle(later, wanted, &never, &spawn_held), known, "{wanted:?}");
        }
        assert_eq!(lock_any(&tasks).len(), 0, "a second retry of a name that is being retried");
        assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
        // The retry ends when the hang does, and the name joins the set.
        gate.open();
        retry.join().expect("the retry");
        assert_eq!(
            p.settle(later, ScalerKinds::ALL, &never, &spawn_held),
            match_of(model, ScalerKinds::ALL)
        );
    }

    #[test]
    fn a_caller_that_asks_for_the_base_alone_is_not_held_by_a_name_that_hangs_on_first_use() {
        let model = MODELS[2];
        let p = probe(Duration::from_secs(60));
        let t0 = Instant::now();
        let spawn = |_: Box<dyn FnOnce() + Send>| -> bool { panic!("a detached thread for a first measurement") };
        // The first call asks for every name; Lanczos's children hang (the round is held at the gate once the base is in).
        let gate = Gate::new();
        let (tx, reached) = std::sync::mpsc::channel();
        let hanging = held(fake(model, |_| false), |r| r.base, &gate, tx);
        let (tx_a, rx_a) = std::sync::mpsc::channel();
        let (a, b) = {
            let hanging = Arc::clone(&hanging);
            let never: Arc<Measure> = Arc::new(|_, _, _| panic!("measured by a follower"));
            let first = std::thread::spawn(move || {
                let got = p.settle(t0, ScalerKinds::ALL, &hanging, &spawn);
                let _ = tx_a.send(got);
            });
            reached
                .recv_timeout(Duration::from_secs(30))
                .expect("the round reached its gate");
            // The base is in. A caller that asks for it alone is answered now, and does not start a measurement of its own.
            let started = Instant::now();
            let base_alone = p.settle(t0, NO_NAMES, &never, &spawn);
            assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
            assert!(base_alone.exact(), "{base_alone:?}");
            // A caller that asks for Lanczos waits for the round that is measuring it, and is told what the round found.
            let waiting = {
                let never = Arc::clone(&never);
                std::thread::spawn(move || p.settle(t0, ScalerKinds::LANCZOS, &never, &spawn))
            };
            assert!(
                rx_a.recv_timeout(Duration::from_millis(500)).is_err(),
                "the first caller was released before the gate was opened"
            );
            std::thread::sleep(Duration::from_millis(300));
            assert!(!waiting.is_finished(), "a caller that asked for Lanczos did not wait for it");
            gate.open();
            let got = rx_a.recv_timeout(Duration::from_secs(30)).expect("the first caller");
            first.join().expect("the first caller");
            (got, waiting.join().expect("the waiting caller"))
        };
        assert_eq!(a, match_of(model, ScalerKinds::ALL));
        assert_eq!(b, a);
    }

    #[test]
    fn a_caller_that_came_while_the_base_was_flying_has_its_name_measured_once_the_base_has_landed() {
        let model = MODELS[0];
        let p = probe(Duration::from_secs(60));
        let t0 = Instant::now();
        let rounds = Arc::new(Mutex::new(Vec::new()));
        let (base_gate, lanczos_gate) = (Gate::new(), Gate::new());
        let (tx, reached) = std::sync::mpsc::channel::<Round>();
        let tx = Mutex::new(tx);
        // The base's children are held (before the base is reported); so are Lanczos's, once they are asked for.
        let held_up: Arc<Measure> = {
            let inner = recording(fake(model, |_| false), &rounds);
            let (base_gate, lanczos_gate) = (Arc::clone(&base_gate), Arc::clone(&lanczos_gate));
            Arc::new(
                move |round: Round, known: Option<Known>, on_base: &mut dyn FnMut(Option<Base>)| {
                    let _ = lock_any(&tx).send(round);
                    if round.base {
                        base_gate.wait();
                    }
                    let found = inner(round, known, on_base);
                    if round.names[LANCZOS] {
                        lanczos_gate.wait();
                    }
                    found
                },
            )
        };
        let never_spawned = |_: Box<dyn FnOnce() + Send>| -> bool { panic!("a detached thread for a first measurement") };
        // A caller that asks for no name measures the base ...
        let first = std::thread::spawn({
            let m = Arc::clone(&held_up);
            move || p.settle(t0, NO_NAMES, &m, &never_spawned)
        });
        let base_only = Round {
            base: true,
            names: [false; NAMED.len()],
        };
        assert_eq!(
            reached.recv_timeout(Duration::from_secs(30)).expect("the base is measured"),
            base_only
        );
        // ... and one that asks for Lanczos arrives while it is flying: it has nothing to start, and waits for the base.
        let second = std::thread::spawn({
            let m = Arc::clone(&held_up);
            move || p.settle(t0, ScalerKinds::LANCZOS, &m, &never_spawned)
        });
        std::thread::sleep(Duration::from_millis(300));
        assert!(!second.is_finished(), "a caller did not wait for the base");
        base_gate.open();
        // The base has landed: the caller that asked for Lanczos measures it (its own question, not the first caller's) ...
        let lanczos = Round {
            base: false,
            names: [true, false, false],
        };
        assert_eq!(
            reached.recv_timeout(Duration::from_secs(30)).expect("Lanczos is measured"),
            lanczos
        );
        // ... and the caller that asked for no name is not held by it.
        let until = Instant::now() + Duration::from_secs(10);
        while !first.is_finished() {
            assert!(Instant::now() < until, "a caller that did not ask for Lanczos waited for it");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(first.join().expect("the first caller").exact());
        assert!(!second.is_finished());
        lanczos_gate.open();
        assert_eq!(
            second.join().expect("the second caller"),
            match_of(model, ScalerKinds::LANCZOS),
            "told that Lanczos is not measured, though nobody was measuring it"
        );
        assert_eq!(lock_any(&rounds).as_slice(), [base_only, lanczos]);
    }

    #[test]
    fn a_failure_that_two_flights_of_one_question_both_report_is_counted_once() {
        // A flight that outlived its bound is still running when a second one is started for the same question; both come back with the
        // children failed. The backoff is the first failure's: the second report is the same failure, not another (it would double the wait).
        let model = MODELS[0];
        let p = probe(Duration::from_millis(150));
        let spawn = |_: Box<dyn FnOnce() + Send>| -> bool { panic!("a detached thread for a first measurement") };
        assert!(p.settle(Instant::now(), NO_NAMES, &fake(model, |_| false), &spawn).exact());
        let gate = Gate::new();
        let (tx, reached) = std::sync::mpsc::channel();
        let slow = held(fake(model, |p| matches!(p, Part::Named(1, _))), |r| r.names[1], &gate, tx);
        let first = std::thread::spawn({
            let slow = Arc::clone(&slow);
            move || p.settle(Instant::now(), ScalerKinds::SPLINE, &slow, &spawn)
        });
        reached
            .recv_timeout(Duration::from_secs(30))
            .expect("the first flight is measuring");
        std::thread::sleep(Duration::from_millis(400));
        // Past its bound the first flight holds nothing: the next caller measures the question itself, and the spline's children fail again.
        let failing = fake(model, |p| matches!(p, Part::Named(1, _)));
        let told = p.settle(Instant::now(), ScalerKinds::SPLINE, &failing, &spawn);
        assert!(told.exact() && !told.named().contains(ScalerKinds::SPLINE));
        assert_eq!(lock_any(&p.state).names[1].slot.failures, 1);
        let retry_at = lock_any(&p.state).names[1].slot.retry_at;
        // The first flight now comes back, with the same failure.
        gate.open();
        first.join().expect("the first caller");
        assert_eq!(lock_any(&p.state).names[1].slot.failures, 1, "one failure counted twice");
        assert_eq!(lock_any(&p.state).names[1].slot.retry_at, retry_at, "the backoff moved");
    }

    #[test]
    fn callers_that_arrive_together_share_one_measurement() {
        let model = MODELS[0];
        let p = probe(Duration::from_secs(60));
        let t0 = Instant::now();
        let calls = Arc::new(Mutex::new(0usize));
        let slow = {
            let (inner, calls) = (fake(model, |_| false), Arc::clone(&calls));
            Arc::new(
                move |round: Round, known: Option<Known>, on_base: &mut dyn FnMut(Option<Base>)| {
                    *lock_any(&calls) += 1;
                    std::thread::sleep(Duration::from_millis(400));
                    inner(round, known, on_base)
                },
            )
        };
        let spawn = |_: Box<dyn FnOnce() + Send>| -> bool { panic!("a detached thread for a first measurement") };
        let told: Vec<Kernels> = std::thread::scope(|scope| {
            // The first caller asks for every name; the others come once its measurement has started.
            let first = {
                let m: Arc<Measure> = slow.clone();
                scope.spawn(move || p.settle(t0, ScalerKinds::ALL, &m, &spawn))
            };
            while *lock_any(&calls) == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
            let late: Vec<_> = [NO_NAMES, ScalerKinds::SPLINE, ScalerKinds::ALL, ScalerKinds::LANCZOS]
                .into_iter()
                .map(|wanted| {
                    let m: Arc<Measure> = slow.clone();
                    scope.spawn(move || p.settle(t0, wanted, &m, &spawn))
                })
                .collect();
            std::iter::once(first)
                .chain(late)
                .map(|r| r.join().expect("a caller"))
                .collect()
        });
        // One round (the callers that came late were followers, and the ones that wanted less were released when theirs was in).
        assert_eq!(*lock_any(&calls), 1, "{told:?}");
        for (i, got) in told.iter().enumerate() {
            assert!(got.exact(), "{i}: {got:?}");
        }
        assert_eq!(told[0], match_of(model, ScalerKinds::ALL));
        assert_eq!(told[3], told[0]);
        assert_eq!(told[0], told[4]);
        assert_eq!(p.known(), told[0]);
    }

    #[test]
    fn a_follower_waits_no_longer_than_the_measurement_is_bounded_to_and_a_flight_that_outlived_it_holds_nothing() {
        let model = MODELS[0];
        let p = probe(Duration::from_millis(400));
        let t0 = Instant::now();
        let calls = Arc::new(Mutex::new(0usize));
        let gate = Gate::new();
        let hung: Arc<Measure> = {
            let (inner, gate, calls) = (fake(model, |_| false), Arc::clone(&gate), Arc::clone(&calls));
            Arc::new(
                move |round: Round, known: Option<Known>, on_base: &mut dyn FnMut(Option<Base>)| {
                    *lock_any(&calls) += 1;
                    gate.wait();
                    inner(round, known, on_base)
                },
            )
        };
        let spawn = |_: Box<dyn FnOnce() + Send>| -> bool { panic!("a detached thread for a first measurement") };
        let (tx, rx) = std::sync::mpsc::channel();
        let first = std::thread::spawn({
            let hung = Arc::clone(&hung);
            move || {
                let _ = tx.send(p.settle(t0, ScalerKinds::ALL, &hung, &spawn));
            }
        });
        while *lock_any(&calls) == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
        // A follower is told what is known (nothing yet) once the flight's bound has passed, not when the hang ends.
        let started = Instant::now();
        assert_eq!(p.settle(t0, ScalerKinds::ALL, &hung, &spawn), Kernels::Unknown);
        let waited = started.elapsed();
        assert!(
            (Duration::from_millis(300)..Duration::from_secs(10)).contains(&waited),
            "{waited:?}"
        );
        assert_eq!(*lock_any(&calls), 1, "a follower measured");
        // A flight past its bound holds nothing: the next caller measures the question itself.
        let second = std::thread::spawn({
            let hung = Arc::clone(&hung);
            move || p.settle(t0, ScalerKinds::ALL, &hung, &spawn)
        });
        let until = Instant::now() + Duration::from_secs(10);
        while *lock_any(&calls) < 2 {
            assert!(Instant::now() < until, "the stale flight still held the question");
            std::thread::sleep(Duration::from_millis(5));
        }
        gate.open();
        let got = rx.recv_timeout(Duration::from_secs(30)).expect("the first caller");
        first.join().expect("the first caller");
        assert_eq!(got, match_of(model, ScalerKinds::ALL));
        assert_eq!(second.join().expect("the second caller"), got);
    }

    #[test]
    fn a_measurement_that_panics_lands_and_wakes_the_callers_that_waited_for_it() {
        let model = MODELS[0];
        let p = probe(Duration::from_secs(60));
        let t0 = Instant::now();
        let gate = Gate::new();
        let (tx, reached) = std::sync::mpsc::channel::<()>();
        let tx = Mutex::new(tx);
        let boom: Arc<Measure> = {
            let gate = Arc::clone(&gate);
            Arc::new(move |_: Round, _: Option<Known>, _: &mut dyn FnMut(Option<Base>)| -> Names {
                let _ = lock_any(&tx).send(());
                gate.wait();
                panic!("a probe that panicked")
            })
        };
        let spawn = |_: Box<dyn FnOnce() + Send>| -> bool { false };
        let first = std::thread::spawn({
            let boom = Arc::clone(&boom);
            move || p.settle(t0, NO_NAMES, &boom, &spawn)
        });
        reached
            .recv_timeout(Duration::from_secs(30))
            .expect("the measurement started");
        let follower = std::thread::spawn({
            let fine = fake(model, |_| false);
            move || {
                let spawn = |_: Box<dyn FnOnce() + Send>| -> bool { false };
                p.settle(t0, NO_NAMES, &fine, &spawn)
            }
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(!follower.is_finished(), "the follower did not wait");
        gate.open();
        assert!(first.join().is_err(), "the measurement did not panic");
        // The follower woke when the flight landed. It was told what was known (the base was not), without waiting out the bound.
        assert_eq!(follower.join().expect("the follower"), Kernels::Unknown);
        // The question is free again: the next caller measures it (the flight did not stay marked).
        let spawn = |_: Box<dyn FnOnce() + Send>| -> bool { false };
        let got = p.settle(t0, NO_NAMES, &fake(model, |_| false), &spawn);
        assert_eq!(got, match_of(model, ScalerKinds::NONE));
    }

    #[test]
    fn a_retry_that_cannot_get_a_thread_leaves_the_name_due_and_nothing_flying() {
        let model = MODELS[0];
        let p = probe(Duration::from_secs(60));
        let t0 = Instant::now();
        let tasks: Tasks = Mutex::new(Vec::new());
        let collect = |t: Box<dyn FnOnce() + Send>| {
            lock_any(&tasks).push(t);
            true
        };
        let failing = fake(model, |p| matches!(p, Part::Named(1, _)));
        let known = p.settle(t0, ScalerKinds::ALL, &failing, &collect);
        let later = lock_any(&p.state).names[1].slot.retry_at.unwrap() + Duration::from_millis(1);
        let refuse = |_: Box<dyn FnOnce() + Send>| false;
        let fine = fake(model, |_| false);
        assert_eq!(p.settle(later, ScalerKinds::ALL, &fine, &refuse), known);
        assert!(lock_any(&p.state).names[1].flight.is_none(), "a flight was left marked");
        // The next caller, with a thread to give, retries it.
        assert_eq!(p.settle(later, ScalerKinds::ALL, &fine, &collect), known);
        assert_eq!(run_tasks(&tasks), 1);
        assert_eq!(p.known(), match_of(model, ScalerKinds::ALL));
    }

    #[test]
    fn a_name_that_keeps_failing_backs_off_longer_and_does_not_hold_the_base_back() {
        let model = MODELS[0];
        let p = probe(Duration::from_secs(60));
        let t0 = Instant::now();
        let tasks: Tasks = Mutex::new(Vec::new());
        let spawn = |t: Box<dyn FnOnce() + Send>| {
            lock_any(&tasks).push(t);
            true
        };
        let hang_neighbor = fake(model, |p| matches!(p, Part::Named(2, _)));
        let first = p.settle(t0, ScalerKinds::ALL, &hang_neighbor, &spawn);
        assert!(first.exact() && !first.named().contains(ScalerKinds::NEIGHBOR) && first.named().contains(ScalerKinds::SPLINE));
        let mut waits = Vec::new();
        for _ in 0..3 {
            // Asked again just after the backoff: the retry fails again, and the name waits twice as long.
            let at = lock_any(&p.state).names[2].slot.retry_at.unwrap() + Duration::from_millis(1);
            let again = p.settle(at, ScalerKinds::ALL, &hang_neighbor, &spawn);
            assert!(again.exact() && !again.named().contains(ScalerKinds::NEIGHBOR));
            assert_eq!(run_tasks(&tasks), 1);
            let retry = lock_any(&p.state).names[2].slot.retry_at.unwrap();
            waits.push(retry.saturating_duration_since(Instant::now()).as_secs_f64());
        }
        for (wait, want) in waits.iter().zip([10.0, 20.0, 40.0]) {
            assert!((want - 2.0..=want).contains(wait), "{waits:?}");
        }
        assert_eq!(lock_any(&p.state).names[2].slot.failures, 4);
        // The base was asked once and never again; the other names are settled.
        assert_eq!(lock_any(&p.state).base_slot.failures, 0);
        assert_eq!(lock_any(&p.state).names[1].slot.failures, 0);
    }

    #[test]
    fn a_base_that_fails_leaves_the_names_unasked_until_it_is_measured() {
        let model = MODELS[2];
        let p = probe(Duration::from_secs(60));
        let t0 = Instant::now();
        let rounds = Arc::new(Mutex::new(Vec::new()));
        let spawn = |_: Box<dyn FnOnce() + Send>| -> bool { panic!("a detached thread for a first measurement") };
        let failing = recording(fake(model, |p| p == Part::Base(3)), &rounds);
        assert_eq!(p.settle(t0, ScalerKinds::ALL, &failing, &spawn), Kernels::Unknown);
        assert_eq!(p.known(), Kernels::Unknown);
        // No name has failed: it had nothing to be compared with.
        assert!(lock_any(&p.state)
            .names
            .iter()
            .all(|n| n.slot.failures == 0 && n.settled.is_none() && n.flight.is_none()));
        assert_eq!(lock_any(&p.state).base_slot.failures, 1);
        assert!(lock_any(&p.state).base_flight.is_none());
        let never: Arc<Measure> = Arc::new(|_, _, _| panic!("measured during the backoff"));
        assert_eq!(p.settle(t0, ScalerKinds::ALL, &never, &spawn), Kernels::Unknown);
        // After the backoff the base and the names are asked together, by the caller, and they all come back.
        let later = lock_any(&p.state).base_slot.retry_at.unwrap() + Duration::from_millis(1);
        let fine = recording(fake(model, |_| false), &rounds);
        let got = p.settle(later, ScalerKinds::ALL, &fine, &spawn);
        assert_eq!(got, match_of(model, ScalerKinds::ALL));
        let full = Round {
            base: true,
            names: ALL_NAMES,
        };
        assert_eq!(lock_any(&rounds).as_slice(), [full, full]);
    }

    #[test]
    fn a_kernel_that_differs_asks_nothing_about_the_names_and_a_name_that_differs_is_out_for_good() {
        let model = MODELS[4];
        let t0 = Instant::now();
        let spawn = |_: Box<dyn FnOnce() + Send>| -> bool { panic!("a detached thread for a first measurement") };
        // The base's chains match no model: the answer is `Differs`, whatever the names' pictures are, and the names are never asked.
        let p = probe(Duration::from_secs(60));
        let differs: Arc<Measure> = {
            let inner = fake(model, |_| false);
            Arc::new(
                move |round: Round, known: Option<Known>, on_base: &mut dyn FnMut(Option<Base>)| {
                    let found = inner(round, known, &mut |_| ());
                    on_base(Some(Base::Differs));
                    found
                },
            )
        };
        assert_eq!(p.settle(t0, ScalerKinds::ALL, &differs, &spawn), Kernels::Differs);
        let never: Arc<Measure> = Arc::new(|_, _, _| panic!("asked about a build that differs"));
        assert_eq!(p.settle(t0, ScalerKinds::ALL, &never, &spawn), Kernels::Differs);
        let far = t0 + Duration::from_secs(3600);
        assert_eq!(
            lock_any(&p.state).plan(far, ScalerKinds::ALL, Duration::from_secs(1), false),
            Plan {
                inline: Round {
                    base: false,
                    names: [false; NAMED.len()]
                },
                background: Round {
                    base: false,
                    names: [false; NAMED.len()]
                }
            }
        );
        // A name whose tables differ is settled as such, and is not asked again.
        let p = probe(Duration::from_secs(60));
        let lanczos_differs: Arc<Measure> = {
            let inner = fake(model, |_| false);
            Arc::new(
                move |round: Round, known: Option<Known>, on_base: &mut dyn FnMut(Option<Base>)| {
                    let mut found = inner(round, known, on_base);
                    found.verdicts[0] = Some(Verdict::Differs);
                    found
                },
            )
        };
        let got = p.settle(t0, ScalerKinds::ALL, &lanczos_differs, &spawn);
        assert_eq!(got.named(), ScalerKinds::ALL.without(ScalerKinds::LANCZOS));
        assert_eq!(p.settle(far, ScalerKinds::ALL, &never, &spawn), got);
        assert_eq!(lock_any(&p.state).names[0].slot.failures, 0);
    }

    #[test]
    fn a_name_found_against_another_model_than_the_one_that_stands_is_not_believed() {
        // (A base stored by a measurement that outlived its flight, with names found through another model: dropped.)
        let ours = MODELS[0];
        let other = MODELS[4];
        let mut s = State::new();
        s.absorb_base(Some(matched(other)), Instant::now());
        let round = Round {
            base: false,
            names: ALL_NAMES,
        };
        s.absorb_names(
            round,
            Names {
                verdicts: [Some(Verdict::Proven); NAMED.len()],
                against: Some(ours),
            },
            Instant::now(),
        );
        assert_eq!(s.view(), Kernels::Match(other));
        assert!(s.names.iter().all(|n| n.settled.is_none()));
        // The same names through the model that stands are believed.
        s.absorb_names(
            round,
            Names {
                verdicts: [Some(Verdict::Proven); NAMED.len()],
                against: Some(other),
            },
            Instant::now(),
        );
        assert_eq!(s.view().named(), ScalerKinds::ALL);
        // A base that is stored stands: a later measurement of it is dropped.
        s.absorb_base(Some(matched(ours)), Instant::now());
        assert_eq!(s.view().named(), ScalerKinds::ALL);
        assert!(matches!(s.base, Some(Base::Match { model, .. }) if model.kernel == other.kernel));
    }

    #[test]
    fn an_unknown_answer_is_retried_after_a_backoff_and_a_measured_one_is_kept() {
        let model = MODELS[0];
        let p = probe(Duration::from_secs(60));
        let t0 = Instant::now();
        let spawn = |_: Box<dyn FnOnce() + Send>| -> bool { panic!("a detached thread for a first measurement") };
        let failing = fake(model, |p| p == Part::Source);
        assert_eq!(p.settle(t0, ScalerKinds::ALL, &failing, &spawn), Kernels::Unknown);
        // Backing off: the probe is not run.
        let never: Arc<Measure> = Arc::new(|_, _, _| panic!("measured during the backoff"));
        assert_eq!(p.settle(t0, ScalerKinds::ALL, &never, &spawn), Kernels::Unknown);
        let later = lock_any(&p.state).base_slot.retry_at.unwrap() + Duration::from_millis(1);
        let m = match_of(model, ScalerKinds::ALL);
        assert_eq!(p.settle(later, ScalerKinds::ALL, &fake(model, |_| false), &spawn), m);
        assert_eq!(
            p.settle(later, ScalerKinds::ALL, &never, &spawn),
            m,
            "a measured answer is kept for good"
        );
        assert_eq!(lock_any(&p.state).base_slot.failures, 1);
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
        let model = MODELS[0];
        let t0 = Instant::now();
        let spawn = |_: Box<dyn FnOnce() + Send>| -> bool { panic!("a detached thread for a first measurement") };
        let fine = fake(model, |_| false);
        let broken = fake(model, |p| p == Part::Base(1));
        let p = probe(Duration::from_secs(60));
        assert_eq!(p.checked(t0, &broken, &spawn), Kernels::Unknown);
        assert!(lock_any(&p.state).held_back);
        // A probe that would succeed now is not believed (and not run): the check left the resized frames out.
        let later = t0 + Duration::from_secs(60);
        let never: Arc<Measure> = Arc::new(|_, _, _| panic!("probed while held back"));
        assert_eq!(p.settle(later, ScalerKinds::ALL, &never, &spawn), Kernels::Unknown);
        assert_eq!(p.known(), Kernels::Unknown);
        // The next self-test that can tell lifts the hold, and the answer stands. (The self-test asks for the base alone: it draws the
        // base's frames, and no name's.)
        let m = p.checked(later, &fine, &spawn);
        assert_eq!(m, match_of(model, ScalerKinds::NONE));
        assert!(!lock_any(&p.state).held_back);
        assert_eq!(p.known(), m);
        // A name that did not come back does not hold the base back.
        let p = probe(Duration::from_secs(60));
        let spline_fails = fake(model, |p| matches!(p, Part::Named(1, _)));
        let m = p.settle(t0, ScalerKinds::ALL, &spline_fails, &spawn);
        assert!(m.exact());
        assert!(p.checked(t0, &never, &spawn).exact() && !lock_any(&p.state).held_back);
        // A difference is permanent and holds nothing back.
        let p = probe(Duration::from_secs(60));
        let differs: Arc<Measure> = {
            let inner = fake(model, |_| false);
            Arc::new(
                move |round: Round, known: Option<Known>, on_base: &mut dyn FnMut(Option<Base>)| {
                    let found = inner(round, known, &mut |_| ());
                    on_base(Some(Base::Differs));
                    found
                },
            )
        };
        assert_eq!(p.checked(t0, &differs, &spawn), Kernels::Differs);
        assert!(!lock_any(&p.state).held_back);
    }
}
