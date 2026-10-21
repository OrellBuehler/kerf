//! A clip's grade as FFmpeg draws it, as arithmetic — the scalar reference of `grade.wgsl` and the numbers it
//! reads.
//!
//! The grade (`lut3d`, `lutrgb`, `colorbalance`, in that order; `kerf_core::Grade`) is a chain of filters that
//! only take RGB, so FFmpeg converts the picture `yuv420p -> rgb24`, runs them on it, and converts it back, and
//! the compositor draws that conversion *and* the filters, to the level. Every stage was written from the
//! FFmpeg 6.1.1 and 9.0.2 sources and held to both builds' output on random pictures (every Y, U, V
//! combination, clipped RGB included; `tests/grade.rs`):
//!
//! * **In** is swscale's x86 kernel for `yuv420p -> rgb24` (`ff_yuv_420_rgb24_ssse3`, `yuv_2_rgb.asm`), **not** the
//!   C tables [`crate::roundtrip::Yuv2Rgb`] follows for the translucent round trip (`yuva420p -> argb` has no
//!   SIMD version): 16-bit lanes, `pmulhw` by 13-bit coefficients (`ff_yuv2rgb_c_init_tables`' `yCoeff` / `vrCoeff` /
//!   ..., `roundToInt16(c * 8192)`), `paddsw`, `packuswb`; chroma replicated 2x2. The same on 6.1.1 and 9.0.2,
//!   for BT.601, BT.709 and BT.2020. Odd heights leave this path (swscale falls back to its generic one) and odd
//!   widths read past the line, so the plan draws even pictures only.
//! * **Wheels** are `colorbalance`'s `float` arithmetic, op for op ([`Balance`]); **curves** are `lutrgb`'s table
//!   ([`curve_tables`]); a **LUT** is `lut3d`'s tetrahedral interpolation in `float` ([`lut3d_apply`]).
//! * **Out, untagged**: `rgb24 -> yuv420p` by the generic scaler: luma `rgb24ToY_c`, chroma `rgb24ToUV_half_c`
//!   (sums of horizontal pairs), then the vertical filter — swscale's x86 `yuv2yuvX` (`pmulhw` of each tap, a
//!   rounding constant of `(64 + 8 * (N - 1)) >> 4` for the filter size aligned to 2), **except the last chroma
//!   row**, which `swscale.c` hands to the C `yuv2planeX` (`dstY >= dstH - 2`: "we can't use MMX here without
//!   overwriting this array's tail"). The shader's scaler (`sws.rs`) is the C arithmetic; the two differ in one
//!   sample in fourteen by a level, which is what the translucent round trip's chroma is off by.
//! * **Out, tagged** (the picture names a matrix, or the graph labels it BT.709): the grade ends in an explicit
//!   `scale=out_color_matrix` to 4:4:4 (`rgb24ToY_c` and `rgb24ToUV_c`, then the unscaled vertical pass), and
//!   the 4:2:0 conversion of whatever follows is a 2:1 scale of both chroma planes (the horizontal filter,
//!   then the same vertical kernel and last-row rule).
//!
//! Float arithmetic is the one place a shader can differ from a CPU in the last bit: a backend may fuse a
//! product with the sum after it. Every product that is followed by a sum is therefore passed through
//! `pin` (an OR with a uniform that is zero), which a compiler cannot see through, and every `float` division
//! is a table made here (`v / 255.0f`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use kerf_core::{BakedLut, ClipLut, CurvePoint, Curves, Grade, GradeRoute, Wheel, Wheels, YuvMatrix};

use crate::roundtrip::{coefficients, Rgb2Yuv};
use crate::sws;

// ---- in: yuv420p -> rgb24 ------------------------------------------------------------------

/// `roundToInt16`: `(f + 0x8000) >> 16`, held to what 16 bits keep.
fn round_to_i16(f: i64) -> i32 {
    let r = (f + (1 << 15)) >> 16;
    if r < -0x7fff {
        -0x8000
    } else {
        r.min(0x7fff) as i32
    }
}

/// The constants of swscale's x86 `yuv420p -> rgb24` kernel for a matrix, limited range in, brightness,
/// contrast and saturation neutral (`ff_yuv2rgb_c_init_tables`): `yCoeff`, `vrCoeff`, `ubCoeff`, `vgCoeff`,
/// `ugCoeff` and `yOffset`, each a 16-bit lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X86Kernel {
    pub y: i32,
    pub vr: i32,
    pub ub: i32,
    pub vg: i32,
    pub ug: i32,
    pub y_offset: i32,
}

impl X86Kernel {
    pub fn new(matrix: YuvMatrix) -> Self {
        let [crv, cbu, cgu, cgv] = coefficients(matrix);
        let (cgu, cgv) = (-cgu, -cgv);
        let cy = 65_536 * 255 / 219;
        let oy: i64 = 16 << 16;
        // contrast = saturation = 1 << 16: `(c * contrast * saturation) >> 32` is `c`.
        let lane = |c: i64| round_to_i16(c * (1 << 13));
        Self {
            y: lane(cy),
            vr: lane(crv),
            ub: lane(cbu),
            vg: lane(cgv),
            ug: lane(cgu),
            y_offset: round_to_i16(oy * (1 << 3)),
        }
    }

    /// One pixel's `[R, G, B]` from its limited-range Y, U, V: `pmulhw` is the high half of a signed 16-bit
    /// product, `packuswb` clips to a byte. (Nothing saturates before it: the lanes stay within a few hundred.)
    pub fn rgb(&self, y: u8, u: u8, v: u8) -> [u8; 3] {
        let yy = ((i32::from(y) * 8 - self.y_offset) * self.y) >> 16;
        let (u, v) = (i32::from(u) * 8 - 0x400, i32::from(v) * 8 - 0x400);
        let r = yy + ((v * self.vr) >> 16);
        let g = yy + (((u * self.ug) >> 16) + ((v * self.vg) >> 16));
        let b = yy + ((u * self.ub) >> 16);
        [r, g, b].map(|c| c.clamp(0, 255) as u8)
    }

    /// The six numbers in the order `grade.wgsl` reads them.
    pub fn lanes(&self) -> [i32; 6] {
        [self.y, self.vr, self.ub, self.vg, self.ug, self.y_offset]
    }
}

// ---- wheels: colorbalance -----------------------------------------------------------------------

/// `colorbalance`'s nine numbers as the filter holds them, `float`s, channel by channel (red's shadows, midtones and
/// highlights, then green's, then blue's: the filter's `rs rm rh gs gm gh bs bm bh`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Balance {
    pub params: [f32; 9],
}

/// `av_clipf`.
fn clipf(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

/// `src / 255.0f` for every byte: the divisions of `color_balance8`, exactly (a GPU's division is not).
pub fn reciprocal_table() -> &'static [f32; 256] {
    static TABLE: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::array::from_fn(|i| i as f32 / 255.0))
}

impl Balance {
    /// The numbers of a clip's wheels as the filter's options parse them (`Wheel::rgb`, a `double`
    /// printed and read back, then a `float`), or `None` when every one is zero.
    pub fn of(wheels: &Wheels) -> Option<Self> {
        let mut params = [0f32; 9];
        for (range, wheel) in [wheels.shadows, wheels.midtones, wheels.highlights].iter().enumerate() {
            for (channel, v) in wheel.rgb().into_iter().enumerate() {
                params[channel * 3 + range] = v as f32;
            }
        }
        params.iter().any(|p| *p != 0.0).then_some(Self { params })
    }

    /// `get_component` for one channel's three numbers.
    fn component(v: f32, l: f32, s: f32, m: f32, h: f32) -> f32 {
        let (a, b, scale) = (4.0f32, 0.333f32, 0.7f32);
        let s = s * (clipf((b - l) * a + 0.5) * scale);
        let m = m * (clipf((l - b) * a + 0.5) * clipf((1.0 - l - b) * a + 0.5) * scale);
        let h = h * (clipf((l + b - 1.0) * a + 0.5) * scale);
        let mut v = v + s;
        v += m;
        v += h;
        clipf(v)
    }

    /// `color_balance8` on one `rgb24` pixel.
    pub fn apply(&self, rgb: [u8; 3]) -> [u8; 3] {
        let rf = reciprocal_table();
        let [r, g, b] = rgb.map(|c| rf[usize::from(c)]);
        let l = r.max(g).max(b) + r.min(g).min(b);
        let p = &self.params;
        [(r, 0), (g, 1), (b, 2)].map(|(v, c)| {
            let o = Self::component(v, l, p[c * 3], p[c * 3 + 1], p[c * 3 + 2]);
            (o * 255.0).round_ties_even().clamp(0.0, 255.0) as u8
        })
    }
}

// ---- curves: lutrgb ------------------------------------------------------------------------------

/// The `lutrgb` table of each channel for 8-bit input — `round(255 * curve(v / 255))`, the integer FFmpeg stores
/// (`kerf_core`'s `the_expression_is_the_model_for_every_value_at_every_depth` holds the expression to this) —
/// or `None` when no channel has a curve of its own or a master over it.
pub fn curve_tables(curves: &Curves) -> Option<[[u8; 256]; 3]> {
    let (channels, master) = curves.shapes();
    if channels.iter().all(Option::is_none) && master.is_none() {
        return None;
    }
    Some(std::array::from_fn(|c| {
        std::array::from_fn(|v| {
            if channels[c].is_none() && master.is_none() {
                return v as u8;
            }
            let x = v as f64 / 255.0;
            (255.0 * curves.apply(c, x)).round() as u8
        })
    }))
}

// ---- LUT: lut3d (tetrahedral) ---------------------------------------------------------------------

/// What `interp_8_tetrahedral` does to each byte before the interpolation, per axis: `src * (1.0f / 255)`
/// times `lut_max`, held to `0..=lut_max`, split into the cell it falls in and where in it. Made here in `float`
/// so that the shader only compares and subtracts.
#[derive(Debug, Clone, PartialEq)]
pub struct LutAxes {
    /// 256 entries: the lattice index below, the one above (held to the last), the fraction.
    pub axis: [(i32, i32, f32); 256],
}

impl LutAxes {
    pub fn new(size: usize) -> Self {
        let lut_max = (size - 1) as f32;
        let scale_f = 1.0f32 / 255.0;
        let axis = std::array::from_fn(|v| {
            let scaled = ((v as f32 * scale_f) * lut_max).clamp(0.0, lut_max);
            let prev = scaled as i32;
            let next = (prev + 1).min(size as i32 - 1);
            (prev, next, scaled - prev as f32)
        });
        Self { axis }
    }

    /// The table the shader reads: per axis (the same for all three), `prev`, `next`, `d` as `f32`s.
    pub fn texels(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(3 * 256);
        out.extend(self.axis.iter().map(|a| a.0 as f32));
        out.extend(self.axis.iter().map(|a| a.1 as f32));
        out.extend(self.axis.iter().map(|a| a.2));
        out
    }
}

/// `interp_8_tetrahedral` on one `rgb24` pixel, op for op: weights `(1 - d)`, differences of fractions, four
/// products summed left to right, scaled by `255.0f` and truncated.
pub fn lut3d_apply(lut: &BakedLut, axes: &LutAxes, rgb: [u8; 3]) -> [u8; 3] {
    let n = lut.size;
    let at = |r: i32, g: i32, b: i32| lut.data[r as usize + g as usize * n + b as usize * n * n];
    let [(pr, nr, dr), (pg, ng, dg), (pb, nb, db)] = [0, 1, 2].map(|i| axes.axis[usize::from(rgb[i])]);
    let c000 = at(pr, pg, pb);
    let c111 = at(nr, ng, nb);
    let ((ca, cb), w) = if dr > dg {
        if dg > db {
            ((at(nr, pg, pb), at(nr, ng, pb)), [1.0 - dr, dr - dg, dg - db, db])
        } else if dr > db {
            ((at(nr, pg, pb), at(nr, pg, nb)), [1.0 - dr, dr - db, db - dg, dg])
        } else {
            ((at(pr, pg, nb), at(nr, pg, nb)), [1.0 - db, db - dr, dr - dg, dg])
        }
    } else if db > dg {
        ((at(pr, pg, nb), at(pr, ng, nb)), [1.0 - db, db - dg, dg - dr, dr])
    } else if db > dr {
        ((at(pr, ng, pb), at(pr, ng, nb)), [1.0 - dg, dg - db, db - dr, dr])
    } else {
        ((at(pr, ng, pb), at(nr, ng, pb)), [1.0 - dg, dg - dr, dr - db, db])
    };
    std::array::from_fn(|k| {
        let sum = w[0] * c000[k] + w[1] * ca[k] + w[2] * cb[k] + w[3] * c111[k];
        ((sum * 255.0) as i32).clamp(0, 255) as u8
    })
}

// ---- the grade of a layer ---------------------------------------------------------------------------

/// What a layer's grade does, ready to draw: the parts that change the picture, in the chain's order
/// (LUT, curves, wheels), and the route of the conversions around them.
#[derive(Debug, Clone)]
pub struct GradeStages {
    pub route: GradeRoute,
    pub lut: Option<Arc<BakedLut>>,
    pub curves: Option<Box<[[u8; 256]; 3]>>,
    pub balance: Option<Balance>,
}

impl GradeStages {
    /// The stages of `grade` as the graph builds them: a part at neutral is not in the chain, and a LUT FFmpeg
    /// leaves out (its file gone or unusable) is left out here. `None` when nothing is left, which FFmpeg
    /// does not take through RGB at all.
    pub fn of(grade: &Grade, route: GradeRoute) -> Option<Self> {
        let lut = grade.lut().and_then(|l| kerf_core::baked_lut(&l));
        let curves = grade.curves().and_then(|c| curve_tables(&c)).map(Box::new);
        let balance = grade.wheels().and_then(|w| Balance::of(&w));
        (lut.is_some() || curves.is_some() || balance.is_some()).then_some(Self {
            route,
            lut,
            curves,
            balance,
        })
    }

    /// The filters on one `rgb24` pixel, in the chain's order.
    pub fn apply(&self, rgb: [u8; 3]) -> [u8; 3] {
        let mut px = rgb;
        if let Some(lut) = &self.lut {
            px = lut3d_apply(lut, &LutAxes::new(lut.size), px);
        }
        if let Some(t) = &self.curves {
            px = [0, 1, 2].map(|c| t[c][usize::from(px[c])]);
        }
        if let Some(b) = &self.balance {
            px = b.apply(px);
        }
        px
    }
}

// ---- out: rgb24 -> yuv420p ---------------------------------------------------------------------------

/// The aligned filter size swscale's x86 vertical scaler works with: `filterAlign` 2, except an unscaled
/// axis (a single tap).
pub fn aligned_taps(filter: &sws::Filter) -> usize {
    if filter.size == 1 {
        1
    } else {
        (filter.size + 1) & !1
    }
}

/// The rounding constant `yuv2yuvX` starts its sum from: `(dither + 8 * (N - 1)) >> 4` with the flat 64.
pub fn vertical_start(filter: &sws::Filter) -> i32 {
    (64 + 8 * (aligned_taps(filter) as i32 - 1)) >> 4
}

/// One output row of the vertical scale of a 15-bit plane (`plane[row * width + x]`): the x86 kernel for every
/// row but the last, the C one (`(64 << 12) + sum >> 19`) for that.
fn vertical_row(plane: &[i32], width: usize, rows: usize, filter: &sws::Filter, i: usize, x: usize) -> u8 {
    let taps = (0..filter.size).map(|j| {
        let row = (filter.pos[i] as usize + j).min(rows - 1);
        (filter.coeff[i * filter.size + j], plane[row * width + x])
    });
    if i + 1 == filter.pos.len() {
        let acc: i64 = (64 << 12) + taps.map(|(c, s)| i64::from(c) * i64::from(s)).sum::<i64>();
        (acc >> 19).clamp(0, 255) as u8
    } else {
        let acc: i32 = vertical_start(filter) + taps.map(|(c, s)| (c * s) >> 16).sum::<i32>();
        (acc >> 3).clamp(0, 255) as u8
    }
}

/// A 15-bit plane (`width` x `rows`) scaled vertically to `out_rows` rows by swscale's 12-bit bicubic and the
/// x86 kernel.
fn vertical_x86(plane: &[i32], width: usize, rows: usize, out_rows: usize) -> Vec<u8> {
    let filter = sws::bicubic(rows as u32, out_rows as u32, sws::ONE_VERTICAL).expect("a 2:1 vertical filter");
    let mut out = vec![0u8; width * out_rows];
    for i in 0..out_rows {
        for x in 0..width {
            out[i * width + x] = vertical_row(plane, width, rows, &filter, i, x);
        }
    }
    out
}

/// `rgb24 -> yuv420p` (an untagged picture's way back): `rgb` is `w * h` pixels, three bytes each.
pub fn rgb_to_yuv420(rgb: &[u8], w: usize, h: usize, matrix: YuvMatrix) -> [Vec<u8>; 3] {
    let k = Rgb2Yuv::new(matrix);
    let px = |x: usize, y: usize| -> [i32; 3] { std::array::from_fn(|c| i32::from(rgb[(y * w + x) * 3 + c])) };
    let luma = (0..w * h)
        .map(|i| k.luma(i32::from(rgb[i * 3]), i32::from(rgb[i * 3 + 1]), i32::from(rgb[i * 3 + 2])))
        .map(|v| v as u8)
        .collect();
    let (cw, ch) = (w / 2, h / 2);
    let mut u15 = vec![0i32; cw * h];
    let mut v15 = vec![0i32; cw * h];
    for y in 0..h {
        for x in 0..cw {
            let (a, b) = (px(2 * x, y), px(2 * x + 1, y));
            let (u, v) = k.pair_to_chroma15([a[0] + b[0], a[1] + b[1], a[2] + b[2]]);
            u15[y * cw + x] = u;
            v15[y * cw + x] = v;
        }
    }
    [luma, vertical_x86(&u15, cw, h, ch), vertical_x86(&v15, cw, h, ch)]
}

/// `rgb24 -> yuv444p` by an explicit `scale` (a tagged picture's way back), then the 2:1 scale of both chroma
/// planes to 4:2:0: the horizontal filter into 15 bits, then the x86 vertical kernel.
pub fn rgb_to_yuv420_via_444(rgb: &[u8], w: usize, h: usize, matrix: YuvMatrix) -> [Vec<u8>; 3] {
    let k = Rgb2Yuv::new(matrix);
    let n = w * h;
    let mut luma = Vec::with_capacity(n);
    let mut u = Vec::with_capacity(n);
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let p = [0, 1, 2].map(|c| i32::from(rgb[i * 3 + c]));
        luma.push(k.luma(p[0], p[1], p[2]) as u8);
        let (u15, v15) = k.pixel_to_chroma15(p);
        u.push(((u15 + 64) >> 7).clamp(0, 255) as u8);
        v.push(((v15 + 64) >> 7).clamp(0, 255) as u8);
    }
    let down = |plane: &[u8]| -> Vec<u8> {
        let (cw, ch) = (w / 2, h / 2);
        let hf = sws::bicubic(w as u32, cw as u32, sws::ONE_HORIZONTAL).expect("a 2:1 horizontal filter");
        let mut mid = vec![0i32; cw * h];
        for y in 0..h {
            for x in 0..cw {
                let acc: i32 = (0..hf.size)
                    .map(|j| {
                        let col = (hf.pos[x] as usize + j).min(w - 1);
                        hf.coeff[x * hf.size + j] * i32::from(plane[y * w + col])
                    })
                    .sum();
                mid[y * cw + x] = (acc >> 7).min(32767);
            }
        }
        vertical_x86(&mid, cw, h, ch)
    };
    [luma, down(&u), down(&v)]
}

/// `yuv420p -> rgb24` by the x86 kernel: Y at `w * h`, U and V at `(w / 2) * (h / 2)`.
pub fn yuv420_to_rgb(planes: [&[u8]; 3], w: usize, h: usize, matrix: YuvMatrix) -> Vec<u8> {
    let k = X86Kernel::new(matrix);
    let cw = w / 2;
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let c = (y / 2) * cw + x / 2;
            out.extend(k.rgb(planes[0][y * w + x], planes[1][c], planes[2][c]));
        }
    }
    out
}

/// The scalar reference of the whole grade on a layer's planes: `yuv420p` in, `yuv420p` out, for an even
/// picture. What `Compositor` draws, plane for plane, where the layer is not resized.
pub fn reference(stages: &GradeStages, planes: [&[u8]; 3], w: usize, h: usize) -> [Vec<u8>; 3] {
    let mut rgb = yuv420_to_rgb(planes, w, h, stages.route.matrix);
    for px in rgb.as_chunks_mut::<3>().0 {
        *px = stages.apply(*px);
    }
    if stages.route.via_444 {
        rgb_to_yuv420_via_444(&rgb, w, h, stages.route.matrix)
    } else {
        rgb_to_yuv420(&rgb, w, h, stages.route.matrix)
    }
}

// ---- does this FFmpeg convert as the model does? ----------------------------------------------------

/// Whether this FFmpeg's conversions to and from RGB, and the filters between them, are the ones this module follows.
/// The conversions are SIMD kernels (`yuv_2_rgb.asm` and `yuv2yuvX.asm` on x86), and another CPU's FFmpeg runs other
/// kernels (NEON, plain C), which agree with these to a level or two and not to the level; the format FFmpeg
/// negotiates for the RGB filters is a property of the build and of which filters the chain holds. The grade is drawn
/// exactly or not at all, so it is measured, once per process, on the very chains the grade writes (like
/// [`kerf_core::geq_edge`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernels {
    /// Noise through every chain of the grade (each subset of the LUT, the curves and the wheels, untagged and
    /// BT.709-tagged) came back as the reference computes it.
    Match,
    /// It did not: the chain that differs.
    Differs(&'static str),
    /// Not known: not measured yet, FFmpeg would not run or timed out (asked again after a backoff), or a self-test
    /// could not tell (see `kernels_for_selftest`).
    Unknown,
}

/// The probe picture's size.
const PROBE: (usize, usize) = (48, 32);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// What the process knows: the measured answer once there is one (kept for good), else how many probes failed and when
/// the next may run.
struct KernelState {
    measured: Option<Kernels>,
    failures: u32,
    retry_at: Option<Instant>,
    /// A self-test could not tell whether the grade is drawn as this FFmpeg draws it, so it did not draw the grade's
    /// frame: nothing is believed until a self-test has (a probe that succeeds later must not open a grade the
    /// check never looked at).
    held_back: bool,
}

impl KernelState {
    const fn new() -> Self {
        Self {
            measured: None,
            failures: 0,
            retry_at: None,
            held_back: false,
        }
    }
}

static STATE: Mutex<KernelState> = Mutex::new(KernelState::new());

fn lock(state: &Mutex<KernelState>) -> std::sync::MutexGuard<'_, KernelState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether this FFmpeg converts as the model does ([`Kernels`]). The first call runs the probe: fifteen
/// short `ffmpeg` children at once (tens of milliseconds each, bounded by 20 s) with no lock held: a blocking
/// thread's, ahead of the frame that needs it ([`crate::prepare`]). [`Kernels::Unknown`] is not remembered: the probe
/// is run again after a backoff.
pub fn kernels() -> Kernels {
    cached(&STATE, Instant::now(), measure)
}

/// What is known without running anything: the measured answer, else [`Kernels::Unknown`] (not measured yet, backing
/// off after a probe that failed, or held back by a self-test). What a plan's caps are made from
/// ([`crate::Compositor::caps`]): a plan does not wait for a probe.
pub fn measured() -> Kernels {
    known(&STATE)
}

/// [`kernels`] for the runtime self-test, which draws the grade's frame only when this is [`Kernels::Match`]. A
/// `Unknown` here holds the grade back for the process ([`KernelState::held_back`]) until a self-test that can tell
/// has run, so that the frames the GPU draws are never the ones its check skipped; a `Differs` is permanent and needs
/// no holding.
pub(crate) fn kernels_for_selftest() -> Kernels {
    checked(&STATE, Instant::now(), measure)
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300))
}

fn known(state: &Mutex<KernelState>) -> Kernels {
    let s = lock(state);
    if s.held_back {
        return Kernels::Unknown;
    }
    s.measured.unwrap_or(Kernels::Unknown)
}

fn cached(state: &Mutex<KernelState>, now: Instant, measure: impl FnOnce() -> Option<Kernels>) -> Kernels {
    {
        let s = lock(state);
        if s.held_back {
            return Kernels::Unknown;
        }
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
            if let Kernels::Differs(why) = k {
                tracing::warn!(
                    why,
                    "this FFmpeg's conversions to and from RGB are not the ones the GPU's grade follows; graded layers are drawn by FFmpeg"
                );
            }
            s.measured = Some(k);
            k
        }
        // (A failure another caller recorded while this probe ran has put the retry in the future: this one is the
        // same failure, not another, and does not double the wait.)
        None if s.retry_at.is_some_and(|t| t > now) => Kernels::Unknown,
        None => {
            s.failures += 1;
            let wait = backoff(s.failures);
            s.retry_at = Some(Instant::now() + wait);
            tracing::warn!(
                failures = s.failures,
                retry_in_secs = wait.as_secs(),
                "could not measure FFmpeg's conversions to and from RGB; graded layers are not drawn on the GPU until it can"
            );
            Kernels::Unknown
        }
    }
}

fn checked(state: &Mutex<KernelState>, now: Instant, measure: impl FnOnce() -> Option<Kernels>) -> Kernels {
    lock(state).held_back = false;
    let k = cached(state, now, measure);
    if k == Kernels::Unknown {
        lock(state).held_back = true;
    }
    k
}

/// The probe picture: noise-like (a polynomial of the coordinates, mod 256, in the integers `geq` computes exactly)
/// in all three planes, so that every kernel branch is looked at. It is not `random()`: that restarts for every
/// slice thread, so the picture would depend on how many threads the child got.
fn probe_source() -> String {
    let (w, h) = PROBE;
    format!(
        "nullsrc=s={w}x{h}:r=1:d=1,format=yuv420p,\
         geq=lum='mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29,256)':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17,256)'"
    )
}

/// The parts of a probe grade, as a set: the LUT, the curves, the wheels.
pub(crate) const PART_LUT: u8 = 1;
pub(crate) const PART_CURVES: u8 = 2;
pub(crate) const PART_WHEELS: u8 = 4;
pub(crate) const PARTS_ALL: u8 = 7;

/// The `.cube` of the probe grade's LUT: a 5^3 table with a tint and cross-channel terms (every tetrahedron of a cell is
/// walked).
pub(crate) fn probe_cube() -> String {
    let size = 5usize;
    let n = (size - 1) as f32;
    let mut data = Vec::with_capacity(size * size * size);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let (r, g, b) = (r as f32 / n, g as f32 / n, b as f32 / n);
                data.push([
                    0.92 * r + 0.08 * g * b + 0.03,
                    0.05 * r * r + 0.9 * g + 0.04 * b,
                    (0.1 + 0.8 * b + 0.15 * r * g).min(1.0),
                ]);
            }
        }
    }
    kerf_core::color_lut::Lut3d {
        title: String::new(),
        size,
        data,
    }
    .to_cube(1.0)
}

/// A grade of the parts in `parts` ([`PART_LUT`] and the others), strong enough to move every channel: the LUT is
/// `cube`, a file holding [`probe_cube`].
pub(crate) fn probe_grade(parts: u8, cube: &str) -> Grade {
    let point = |x, y| CurvePoint { x, y };
    Grade {
        lut: (parts & PART_LUT != 0).then(|| ClipLut {
            path: cube.to_string(),
            intensity: 0.8,
        }),
        curves: (parts & PART_CURVES != 0).then(|| Curves {
            master: vec![point(0.0, 0.03), point(0.3, 0.22), point(0.75, 0.84), point(1.0, 0.98)],
            ..Curves::default()
        }),
        wheels: (parts & PART_WHEELS != 0).then(|| Wheels {
            shadows: Wheel {
                x: 0.4,
                y: -0.2,
                lightness: 0.1,
            },
            highlights: Wheel {
                x: 0.1,
                y: 0.7,
                lightness: 0.05,
            },
            ..Wheels::default()
        }),
    }
}

/// What a probe chain is, by its parts (1 to 7: [`PART_LUT`] as bit 0) and then untagged or BT.709-tagged.
const WHY: [[&str; 2]; 7] = [
    ["a LUT on an untagged picture", "a LUT on a BT.709-tagged picture"],
    ["curves on an untagged picture", "curves on a BT.709-tagged picture"],
    [
        "a LUT and curves on an untagged picture",
        "a LUT and curves on a BT.709-tagged picture",
    ],
    ["wheels on an untagged picture", "wheels on a BT.709-tagged picture"],
    [
        "a LUT and wheels on an untagged picture",
        "a LUT and wheels on a BT.709-tagged picture",
    ],
    [
        "curves and wheels on an untagged picture",
        "curves and wheels on a BT.709-tagged picture",
    ],
    [
        "a LUT, curves and wheels on an untagged picture",
        "a LUT, curves and wheels on a BT.709-tagged picture",
    ],
];

/// How many graphs the probe runs: the picture, then every non-empty set of the grade's parts through each route.
const PROBE_CHAINS: usize = 1 + 2 * 7;

/// One chain of the probe: a grade's filters as the graph writes them, over the probe picture.
struct Chain {
    why: &'static str,
    graph: String,
    stages: GradeStages,
}

/// The probe's chains. **One graph each**, not one graph stacking them: FFmpeg 9 negotiates the colour matrix across a
/// whole graph, so a tagged branch beside an untagged one converts both with its matrix, and the answer is no longer
/// that of either route. **Every non-empty set of parts**, not an identity `lutrgb`: the RGB format FFmpeg
/// negotiates is the intersection of what the chain's filters take, so a build that picks another one (planar
/// `gbrp`, say) for `colorbalance` beside `lut3d` than for either alone would convert elsewhere than the model does.
/// The filters are `kerf_core::grade_filter_chain`'s text, the expected picture the compositor's own reference.
/// `None` when the chain is not the one it should be (a LUT's copy that could not be written leaves the filter out).
fn chains(cube: &str) -> Option<Vec<Chain>> {
    let source = probe_source();
    let routes = [
        (
            None,
            GradeRoute {
                matrix: YuvMatrix::Bt601,
                via_444: false,
            },
        ),
        (
            Some("bt709"),
            GradeRoute {
                matrix: YuvMatrix::Bt709,
                via_444: true,
            },
        ),
    ];
    let mut out = Vec::with_capacity(PROBE_CHAINS - 1);
    for parts in 1..=PARTS_ALL {
        let grade = probe_grade(parts, cube);
        for (tagged, (tag, route)) in routes.iter().enumerate() {
            let filters = kerf_core::grade_filter_chain(&grade, *tag);
            if filters.len() != parts.count_ones() as usize + if tag.is_some() { 3 } else { 0 } {
                return None;
            }
            let mut parts_of_graph = vec![source.clone()];
            if let Some(tag) = tag {
                parts_of_graph.push(format!("setparams=colorspace={tag}"));
            }
            parts_of_graph.extend(filters);
            parts_of_graph.push("format=yuv420p".to_string());
            out.push(Chain {
                why: WHY[usize::from(parts) - 1][tagged],
                graph: parts_of_graph.join(","),
                stages: GradeStages::of(&grade, *route)?,
            });
        }
    }
    Some(out)
}

fn probe_frame_len() -> usize {
    PROBE.0 * PROBE.1 * 3 / 2
}

/// A scratch directory holding the probe's `.cube`, removed when dropped.
struct ProbeDir(PathBuf);

impl ProbeDir {
    fn new() -> std::io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kerf-gpu-grade-probe-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for ProbeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn measure() -> Option<Kernels> {
    let dir = ProbeDir::new().ok()?;
    let cube = dir.0.join("probe.cube");
    std::fs::write(&cube, probe_cube()).ok()?;
    let chains = chains(&cube.to_string_lossy())?;
    let mut graphs = vec![probe_source()];
    graphs.extend(chains.iter().map(|c| c.graph.clone()));
    let share = graphs.len();
    let frames: Vec<Option<Vec<u8>>> = std::thread::scope(|scope| {
        let runs: Vec<_> = graphs
            .iter()
            .map(|graph| {
                scope.spawn(move || {
                    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i"]
                        .into_iter()
                        .map(String::from)
                        .collect();
                    args.push(graph.clone());
                    args.extend(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"].map(String::from));
                    // The probe is what a frame waits on, so it is not niced, but it is not allowed every core
                    // times its dozen: the budget's threads are shared among the children.
                    kerf_core::limit_frame_args(&mut args, share);
                    let mut cmd = kerf_core::ffmpeg_command();
                    cmd.args(&args);
                    crate::source::run_child(cmd, probe_frame_len() + 1, PROBE_TIMEOUT).ok()
                })
            })
            .collect();
        runs.into_iter().map(|run| run.join().ok().flatten()).collect()
    });
    let (source, rest) = frames.split_first()?;
    classify(source.as_deref()?, &chains, rest)
}

/// The answer from the probe pictures: `None` when the source or one of the chains' is missing or not the size it
/// should be and none of the others differs (a difference is conclusive wherever the rest stands).
fn classify(source: &[u8], chains: &[Chain], frames: &[Option<Vec<u8>>]) -> Option<Kernels> {
    let (w, h) = PROBE;
    let (cw, ch) = (w / 2, h / 2);
    if source.len() != probe_frame_len() || frames.len() != chains.len() {
        return None;
    }
    let planes = |f: &[u8]| -> [Vec<u8>; 3] {
        let (y, rest) = f.split_at(w * h);
        let (u, v) = rest.split_at(cw * ch);
        [y.to_vec(), u.to_vec(), v.to_vec()]
    };
    let [y, u, v] = planes(source);
    let source = [&y[..], &u[..], &v[..]];
    let mut unknown = false;
    for (chain, frame) in chains.iter().zip(frames) {
        match frame.as_deref().filter(|f| f.len() == probe_frame_len()) {
            None => unknown = true,
            Some(frame) if planes(frame) != reference(&chain.stages, source, w, h) => {
                return Some(Kernels::Differs(chain.why));
            }
            Some(_) => {}
        }
    }
    (!unknown).then_some(Kernels::Match)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerf_core::{CurvePoint, Wheel};

    #[test]
    fn the_kernel_constants_are_what_swscale_computes() {
        // Read off `ff_yuv2rgb_c_init_tables` for BT.601 and checked against the output of both builds.
        assert_eq!(
            X86Kernel::new(YuvMatrix::Bt601),
            X86Kernel {
                y: 9539,
                vr: 13075,
                ub: 16525,
                vg: -6660,
                ug: -3209,
                y_offset: 128
            }
        );
        assert_eq!(X86Kernel::new(YuvMatrix::Bt709).lanes()[0], 9539);
    }

    #[test]
    fn black_and_the_limited_range_white_are_what_the_kernel_makes_of_them() {
        let k = X86Kernel::new(YuvMatrix::Bt601);
        assert_eq!(k.rgb(16, 128, 128), [0, 0, 0]);
        assert_eq!(k.rgb(235, 128, 128), [255, 255, 255]);
        assert_eq!(k.rgb(0, 0, 0), [0, 135, 0]);
    }

    #[test]
    fn a_balance_of_nothing_is_none_and_a_wheel_gives_its_floats() {
        assert_eq!(Balance::of(&Wheels::default()), None);
        let w = Wheels {
            shadows: Wheel {
                x: 1.0,
                y: 0.0,
                lightness: 0.0,
            },
            ..Wheels::default()
        };
        let b = Balance::of(&w).expect("a wheel");
        assert_eq!(b.params, [1.0, 0.0, 0.0, -0.5, 0.0, 0.0, -0.5, 0.0, 0.0]);
    }

    #[test]
    fn colorbalance_of_a_grey_with_a_midtone_lift_is_the_floats_of_the_filter() {
        // A dark grey sits where the midtones weigh (l = 2 * 60 / 255), so a lift of them moves it; black and
        // white sit outside.
        let b = Balance {
            params: [0.0, 0.2, 0.0, 0.0, 0.2, 0.0, 0.0, 0.2, 0.0],
        };
        let dark = b.apply([60, 60, 60]);
        assert!(dark[0] > 60 && dark[0] == dark[1] && dark[1] == dark[2], "{dark:?}");
        assert_eq!(b.apply([0, 0, 0]), [0, 0, 0]);
        assert_eq!(b.apply([255, 255, 255]), [255, 255, 255]);
    }

    #[test]
    fn the_curve_tables_hold_a_channel_without_a_curve_and_apply_the_master_to_all() {
        let pts = |p: &[(f64, f64)]| p.iter().map(|&(x, y)| CurvePoint { x, y }).collect::<Vec<_>>();
        let c = Curves {
            red: pts(&[(0.0, 1.0), (1.0, 0.0)]),
            ..Curves::default()
        };
        let t = curve_tables(&c).expect("a red curve");
        assert_eq!((t[0][0], t[0][255], t[1][77], t[2][200]), (255, 0, 77, 200));
        let m = Curves {
            master: pts(&[(0.0, 1.0), (1.0, 0.0)]),
            ..Curves::default()
        };
        let t = curve_tables(&m).expect("a master curve");
        assert_eq!((t[0][0], t[1][0], t[2][255]), (255, 255, 0));
        assert_eq!(curve_tables(&Curves::default()), None);
    }

    #[test]
    fn an_identity_lut_leaves_a_pixel_where_it_is_within_the_rounding_of_a_truncation() {
        let size = 17;
        let n = (size - 1) as f32;
        let mut data = Vec::new();
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push([r as f32 / n, g as f32 / n, b as f32 / n]);
                }
            }
        }
        let lut = BakedLut { size, data };
        let axes = LutAxes::new(size);
        for px in [[0u8, 0, 0], [255, 255, 255], [200, 30, 99], [1, 254, 127]] {
            let out = lut3d_apply(&lut, &axes, px);
            for c in 0..3 {
                // `av_clip_uint8` truncates: an identity table comes back at most one level low.
                assert!(
                    i32::from(out[c]) <= i32::from(px[c]) && i32::from(px[c]) - i32::from(out[c]) <= 1,
                    "{px:?} -> {out:?}"
                );
            }
        }
    }

    #[test]
    fn the_last_chroma_row_is_the_c_kernel_and_the_rest_the_x86_one() {
        let (w, h) = (16usize, 16usize);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 37 % 251) as u8).collect();
        let [y, u, v] = rgb_to_yuv420(&rgb, w, h, YuvMatrix::Bt601);
        assert_eq!((y.len(), u.len(), v.len()), (w * h, w * h / 4, w * h / 4));
        let [_, u444, _] = rgb_to_yuv420_via_444(&rgb, w, h, YuvMatrix::Bt709);
        assert_eq!(u444.len(), w * h / 4);
    }

    /// The probe picture, as `probe_source`'s expressions make it (the integers are exact in `geq`'s doubles).
    fn probe_picture() -> [Vec<u8>; 3] {
        let (w, h) = PROBE;
        let plane = |w: usize, h: usize, k: [usize; 5]| -> Vec<u8> {
            (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    ((x * x * k[0] + y * y * k[1] + x * y * k[2] + x * k[3] + y * k[4]) % 256) as u8
                })
                .collect()
        };
        [
            plane(w, h, [37, 91, 53, 11, 29]),
            plane(w / 2, h / 2, [13, 71, 29, 7, 3]),
            plane(w / 2, h / 2, [53, 19, 97, 5, 17]),
        ]
    }

    fn concat(p: &[Vec<u8>; 3]) -> Vec<u8> {
        p.iter().flatten().copied().collect()
    }

    /// Run `f` over the probe's chains, with the LUT file they read in a directory that is gone afterwards.
    fn with_chains<T>(f: impl FnOnce(&[Chain]) -> T) -> T {
        let dir = ProbeDir::new().expect("a scratch directory");
        let cube = dir.0.join("probe.cube");
        std::fs::write(&cube, probe_cube()).unwrap();
        let chains = chains(&cube.to_string_lossy()).expect("the probe's chains");
        f(&chains)
    }

    /// What a build that converts as the model does makes of the probe picture, chain by chain.
    fn expected(chains: &[Chain]) -> Vec<Option<Vec<u8>>> {
        let (w, h) = PROBE;
        let src = probe_picture();
        let source = [&src[0][..], &src[1][..], &src[2][..]];
        chains
            .iter()
            .map(|c| Some(concat(&reference(&c.stages, source, w, h))))
            .collect()
    }

    #[test]
    fn the_probe_runs_every_set_of_parts_through_both_routes_as_the_graph_writes_them() {
        with_chains(|chains| {
            assert_eq!(chains.len(), PROBE_CHAINS - 1);
            let mut whys: Vec<_> = chains.iter().map(|c| c.why).collect();
            whys.sort_unstable();
            whys.dedup();
            assert_eq!(whys.len(), 14, "every chain is named for what it is");
            for (i, chain) in chains.iter().enumerate() {
                let (parts, tagged) = (i / 2 + 1, i % 2 == 1);
                let g = &chain.graph;
                let at = |filter: &str| g.find(filter);
                assert_eq!(at("lut3d=").is_some(), parts & 1 != 0, "{g}");
                assert_eq!(at("lutrgb=").is_some(), parts & 2 != 0, "{g}");
                assert_eq!(at("colorbalance=").is_some(), parts & 4 != 0, "{g}");
                // In the graph's order.
                let order: Vec<_> = ["lut3d=", "lutrgb=", "colorbalance="].iter().filter_map(|f| at(f)).collect();
                assert!(order.windows(2).all(|w| w[0] < w[1]), "{g}");
                assert_eq!(g.contains("setparams=colorspace=bt709"), tagged, "{g}");
                assert_eq!(g.contains("scale=out_color_matrix=bt709"), tagged, "{g}");
                assert_eq!(chain.stages.route.via_444, tagged);
                assert_eq!(
                    chain.stages.route.matrix,
                    if tagged { YuvMatrix::Bt709 } else { YuvMatrix::Bt601 }
                );
                assert!(g.starts_with("nullsrc=") && g.ends_with(",format=yuv420p"), "{g}");
                assert_eq!(chain.stages.lut.is_some(), parts & 1 != 0);
                assert_eq!(chain.stages.curves.is_some(), parts & 2 != 0);
                assert_eq!(chain.stages.balance.is_some(), parts & 4 != 0);
            }
            // A probe whose chains all came out alike would prove nothing about any of them.
            let mut answers: Vec<_> = expected(chains).into_iter().flatten().collect();
            answers.sort();
            answers.dedup();
            assert_eq!(answers.len(), 14);
        });
    }

    #[test]
    fn a_probe_that_is_the_reference_is_a_match_and_one_sample_off_in_any_chain_is_named() {
        with_chains(|chains| {
            let (w, h) = PROBE;
            let source = concat(&probe_picture());
            let good = expected(chains);
            assert_eq!(classify(&source, chains, &good), Some(Kernels::Match));
            // Not the sizes it should be: no answer.
            assert_eq!(classify(&source[1..], chains, &good), None);
            assert_eq!(classify(&source, chains, &good[1..]), None);
            let mut missing = good.clone();
            missing[3] = None;
            assert_eq!(classify(&source, chains, &missing), None);
            let mut short = good.clone();
            short[0] = Some(vec![0; 5]);
            assert_eq!(classify(&source, chains, &short), None);
            // One sample a level off, in a plane of each chain in turn: that chain is the one named.
            for (i, chain) in chains.iter().enumerate() {
                for at in [0, w * h + 5, w * h * 5 / 4 + 2] {
                    let mut off = good.clone();
                    off[i].as_mut().unwrap()[at] ^= 1;
                    assert_eq!(classify(&source, chains, &off), Some(Kernels::Differs(chain.why)), "{i} {at}");
                }
            }
            // A difference is conclusive wherever the rest stands.
            let mut both = good;
            both[2] = None;
            both[5].as_mut().unwrap()[0] ^= 1;
            assert_eq!(classify(&source, chains, &both), Some(Kernels::Differs(chains[5].why)));
        });
    }

    fn fresh() -> Mutex<KernelState> {
        Mutex::new(KernelState::new())
    }

    #[test]
    fn a_measured_answer_is_kept_and_unknown_is_never_remembered() {
        let state = fresh();
        let t0 = Instant::now();
        assert_eq!(known(&state), Kernels::Unknown, "nothing measured yet");
        assert_eq!(cached(&state, t0, || None), Kernels::Unknown);
        assert_eq!(known(&state), Kernels::Unknown);
        // Inside the backoff the probe is not run again.
        assert_eq!(
            cached(&state, t0, || panic!("probed again inside the backoff")),
            Kernels::Unknown
        );
        // After it, it is (the failure was not kept as an answer), and what it finds is kept for good.
        let later = t0 + Duration::from_secs(60);
        assert_eq!(
            cached(&state, later, || Some(Kernels::Differs("a chain"))),
            Kernels::Differs("a chain")
        );
        assert_eq!(known(&state), Kernels::Differs("a chain"));
        assert_eq!(
            cached(&state, later, || panic!("a measured answer is kept")),
            Kernels::Differs("a chain")
        );
        assert_eq!(lock(&state).failures, 1);
        // A match is kept the same way.
        let state = fresh();
        assert_eq!(cached(&state, t0, || Some(Kernels::Match)), Kernels::Match);
        assert_eq!(known(&state), Kernels::Match);
        assert_eq!(cached(&state, t0, || panic!("a measured answer is kept")), Kernels::Match);
    }

    #[test]
    fn the_wait_doubles_with_each_failure_up_to_five_minutes() {
        assert_eq!(backoff(1).as_secs(), 5);
        assert_eq!(backoff(2).as_secs(), 10);
        assert_eq!(backoff(3).as_secs(), 20);
        assert_eq!(backoff(40).as_secs(), 300);
        let state = fresh();
        let mut now = Instant::now();
        for failures in 1..=3u32 {
            let before = Instant::now();
            assert_eq!(cached(&state, now, || None), Kernels::Unknown);
            let s = lock(&state);
            assert_eq!(s.failures, failures);
            let retry = s.retry_at.expect("a retry is set");
            let wait = retry.duration_since(before);
            assert!(
                wait >= backoff(failures) && wait < backoff(failures) + Duration::from_secs(1),
                "{wait:?}"
            );
            now = retry + Duration::from_millis(1);
        }
    }

    #[test]
    fn the_probe_runs_with_no_lock_held_and_the_first_answer_stored_wins() {
        let state = fresh();
        let now = Instant::now();
        // While the probe runs another caller is not queued behind it: it can take the lock, and its own answer
        // (here the whole nested call) is stored first.
        let got = cached(&state, now, || {
            assert!(state.try_lock().is_ok(), "the state is locked while the probe runs");
            assert_eq!(cached(&state, now, || Some(Kernels::Match)), Kernels::Match);
            Some(Kernels::Differs("late"))
        });
        assert_eq!(got, Kernels::Match, "the first writer wins");
        assert_eq!(cached(&state, now, || panic!("measured twice")), Kernels::Match);
        // A failure that lost the race to an answer changes nothing.
        let state = fresh();
        let got = cached(&state, now, || {
            cached(&state, now, || Some(Kernels::Match));
            None
        });
        assert_eq!(got, Kernels::Match);
        assert_eq!(lock(&state).failures, 0);
        // Two failures at once are one: the second does not double the wait...
        let state = fresh();
        let got = cached(&state, now, || {
            assert_eq!(cached(&state, now, || None), Kernels::Unknown);
            None
        });
        assert_eq!(got, Kernels::Unknown);
        assert_eq!(lock(&state).failures, 1);
        // ...while one after the wait has passed is another, and does.
        assert_eq!(cached(&state, now + Duration::from_secs(60), || None), Kernels::Unknown);
        assert_eq!(lock(&state).failures, 2);
    }

    #[test]
    fn a_self_test_that_cannot_tell_holds_the_grade_back_until_one_can() {
        let state = fresh();
        let t0 = Instant::now();
        assert_eq!(checked(&state, t0, || None), Kernels::Unknown);
        assert!(lock(&state).held_back);
        // A probe that would succeed now is not believed (and not run): the check skipped the grade's frame.
        let later = t0 + Duration::from_secs(60);
        assert_eq!(cached(&state, later, || panic!("probed while held back")), Kernels::Unknown);
        assert_eq!(known(&state), Kernels::Unknown);
        // The next self-test that can tell lifts the hold, and the answer stands.
        assert_eq!(checked(&state, later, || Some(Kernels::Match)), Kernels::Match);
        assert!(!lock(&state).held_back);
        assert_eq!(known(&state), Kernels::Match);
        assert_eq!(cached(&state, later, || panic!("measured twice")), Kernels::Match);
        // One that finds a difference holds nothing back: the difference is permanent.
        let state = fresh();
        assert_eq!(
            checked(&state, t0, || Some(Kernels::Differs("a chain"))),
            Kernels::Differs("a chain")
        );
        assert!(!lock(&state).held_back);
        // A measured answer is what a self-test finds, with no probe.
        let state = fresh();
        cached(&state, t0, || Some(Kernels::Match));
        assert_eq!(checked(&state, t0, || panic!("measured twice")), Kernels::Match);
        // One still inside the backoff cannot tell, and holds the grade back again.
        let state = fresh();
        assert_eq!(cached(&state, t0, || None), Kernels::Unknown);
        assert_eq!(checked(&state, t0, || panic!("probed inside the backoff")), Kernels::Unknown);
        assert!(lock(&state).held_back);
    }

    /// A probe that cannot tell the kernels it is for from the plain C ones proves nothing: this picture makes
    /// the x86 vertical scaler's chroma differ from the C arithmetic's on a good share of samples.
    #[test]
    fn the_probe_picture_tells_the_x86_vertical_kernel_from_the_c_one() {
        let (w, h) = PROBE;
        let src = probe_picture();
        let source = [&src[0][..], &src[1][..], &src[2][..]];
        let rgb = yuv420_to_rgb(source, w, h, YuvMatrix::Bt601);
        let x86 = rgb_to_yuv420(&rgb, w, h, YuvMatrix::Bt601);
        let k = Rgb2Yuv::new(YuvMatrix::Bt601);
        let (cw, ch) = (w / 2, h / 2);
        let mut u15 = vec![0i32; cw * h];
        for y in 0..h {
            for x in 0..cw {
                let px = |dx: usize| -> [i32; 3] { std::array::from_fn(|c| i32::from(rgb[(y * w + 2 * x + dx) * 3 + c])) };
                let (a, b) = (px(0), px(1));
                u15[y * cw + x] = k.pair_to_chroma15([a[0] + b[0], a[1] + b[1], a[2] + b[2]]).0;
            }
        }
        let filter = sws::bicubic(h as u32, ch as u32, sws::ONE_VERTICAL).expect("a 2:1 vertical filter");
        let mut differing = 0;
        for i in 0..ch {
            for x in 0..cw {
                let acc: i64 = (64 << 12)
                    + (0..filter.size)
                        .map(|j| {
                            let row = (filter.pos[i] as usize + j).min(h - 1);
                            i64::from(filter.coeff[i * filter.size + j]) * i64::from(u15[row * cw + x])
                        })
                        .sum::<i64>();
                if (acc >> 19).clamp(0, 255) as u8 != x86[1][i * cw + x] {
                    differing += 1;
                }
            }
        }
        assert!(differing >= 10, "{differing} of {} chroma samples differ", cw * ch);
    }

    #[test]
    fn the_start_of_the_vertical_sum_follows_the_aligned_filter_size() {
        let f = sws::bicubic(48, 24, sws::ONE_VERTICAL).unwrap();
        assert_eq!(f.size, 8);
        assert_eq!(vertical_start(&f), 7);
        let one = sws::bicubic(48, 48, sws::ONE_VERTICAL).unwrap();
        assert_eq!(aligned_taps(&one), 1);
    }
}
