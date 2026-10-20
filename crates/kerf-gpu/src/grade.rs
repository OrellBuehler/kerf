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

use std::sync::Arc;

use kerf_core::{BakedLut, Curves, Grade, GradeRoute, Wheels, YuvMatrix};

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

    #[test]
    fn the_start_of_the_vertical_sum_follows_the_aligned_filter_size() {
        let f = sws::bicubic(48, 24, sws::ONE_VERTICAL).unwrap();
        assert_eq!(f.size, 8);
        assert_eq!(vertical_start(&f), 7);
        let one = sws::bicubic(48, 48, sws::ONE_VERTICAL).unwrap();
        assert_eq!(aligned_taps(&one), 1);
    }
}
