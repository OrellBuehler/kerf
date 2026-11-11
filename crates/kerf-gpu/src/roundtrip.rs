//! The RGB round trip FFmpeg puts a translucent layer through, as integer
//! arithmetic — the scalar reference of `roundtrip.wgsl`.
//!
//! `colorchannelmixer` (which writes a clip's opacity into its alpha plane) only
//! takes RGB, so for any clip below full opacity the graph converts the layer
//! `yuva420p -> argb`, mixes, and converts `argb -> yuva420p` before `overlay`
//! sees it. Both conversions are lossy in ways that show: colour outside the RGB
//! gamut is clipped (every saturated test pattern has some), luma loses a level or
//! two to truncation, and 4:2:0 chroma is rebuilt from the RGB.
//!
//! Ported from libswscale 6.1 and checked against both FFmpeg 6.1 and 9.0 (an
//! exhaustive run on random pictures, and the `opacity` parity cases) **to the sample, every plane**, where
//! [`crate::roundtrip_probe`] says this machine's swscale kernels are the ones modelled (x86):
//!
//! * **yuv -> argb** is swscale's *C* converter (`yuva2argb_c` — `ARGB` is not
//!   `RGB32` on a little-endian machine, so there is no SIMD version of it): three
//!   integer tables (`ff_yuv2rgb_c_init_tables`), one per colour, indexed by the
//!   luma plus a chroma-dependent offset. It uses the matrix the *frame* carries —
//!   BT.709 for a BT.709-tagged stream, BT.601 for an untagged one. Reproduced
//!   exactly (zero mismatches on random pictures, all three matrices).
//! * **argb -> yuv** goes through the generic scaler. Luma is
//!   `(ry*R + gy*G + by*B + rounding) >> 9`, then rounded to 8 bits: exact. The
//!   RGB frame in between carries no colourspace tag of its own, so the way back
//!   takes the matrix of the composite it is headed for — BT.601 on an FFmpeg whose
//!   composite is untagged, the stack's matrix on one that negotiates it
//!   ([`kerf_core::CompositeColorPolicy`]) — and never the layer's own: a BT.709
//!   layer goes out of YCbCr as BT.709 and comes back as whatever the canvas is.
//!   Chroma is the sum of each pair of
//!   horizontal pixels (swscale halves the chroma of an RGB source on input), then a
//!   vertical stretched-bicubic filter over the rows (`chrSrcVSubSample` is 0 for
//!   RGB, so it is a real 2:1 scale) **in the x86 `yuv2yuvX` kernel** for every row but the last, which swscale hands to
//!   the C one (`grade::vertical_start`, the grade's way back: the C arithmetic alone was a level of chroma off on
//!   about one sample in fourteen).
//! * **alpha** is `lrint(255 * opacity)` scaled by 256/255 on the way back to YUV:
//!   see [`kerf_core::layer_geometry::ffmpeg_alpha`].
//!
//! **Odd sizes** take other routes through swscale and are reproduced too ([`yuv_to_rgb`], [`rgb_to_yuv`], [`OddWidth`]): an odd width
//! leaves the unscaled converter's last pixel unwritten (a zeroed, transparent one), an odd height takes the scaler's generic path
//! (with the full-chroma functions for an odd width as well), and the way back reads the pixel past an odd row (zero) on FFmpeg 6.1 and
//! scales the row on 9.0. [`crate::roundtrip_probe::odd_translucency`] measures which, and `GpuCaps::translucent_odd` says whether the plan
//! may hand such a layer to the compositor.

use kerf_core::YuvMatrix;

/// `ff_yuv2rgb_coeffs`: `{crv, cbu, cgu, cgv}` per matrix.
pub(crate) fn coefficients(m: YuvMatrix) -> [i64; 4] {
    match m {
        YuvMatrix::Bt601 => [104_597, 132_201, 25_675, 53_279],
        YuvMatrix::Bt709 => [117_489, 138_438, 13_975, 34_925],
        YuvMatrix::Bt2020 => [110_013, 140_363, 12_277, 42_626],
    }
}

/// `YUVRGB_TABLE_HEADROOM` / `YUVRGB_TABLE_LUMA_HEADROOM`.
const HEADROOM: i64 = 512;
/// Where the luma starts in the table (limited range source).
const Y_OFFSET: i64 = 326 + HEADROOM;

/// swscale's yuv -> rgb tables for one matrix, limited range in, 8-bit RGB out,
/// brightness / contrast / saturation neutral.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Yuv2Rgb {
    /// The four chroma coefficients, divided by the luma gain as swscale does
    /// (`crv`, `cbu`, `cgu`, `cgv`; the last two negative).
    pub crv: i32,
    pub cbu: i32,
    pub cgu: i32,
    pub cgv: i32,
    /// The luma gain, `65536 * 255 / 219`.
    pub cy: i32,
    /// The luma offset, `16 << 16`.
    pub oy: i32,
}

impl Yuv2Rgb {
    pub fn new(matrix: YuvMatrix) -> Self {
        let [crv, cbu, cgu, cgv] = coefficients(matrix);
        let (cgu, cgv) = (-cgu, -cgv);
        let cy = 65_536 * 255 / 219;
        // `(c * contrast * saturation) >> 32` with both at 1 << 16 is `c`; the
        // coefficients are then scaled by the luma gain, rounded.
        let scaled = |c: i64| ((c * 65_536) + 0x8000) / cy;
        Self {
            crv: scaled(crv) as i32,
            cbu: scaled(cbu) as i32,
            cgu: scaled(cgu) as i32,
            cgv: scaled(cgv) as i32,
            cy: cy as i32,
            oy: 16 << 16,
        }
    }

    /// `y_table[i]`: the luma term for table index `i`, clipped to a byte.
    fn table(&self, index: i64) -> i32 {
        let (cy, oy) = (i64::from(self.cy), i64::from(self.oy));
        let yb = -(384 << 16) - HEADROOM * cy - oy + index * cy;
        ((yb + 0x8000) >> 16).clamp(0, 255) as i32
    }

    /// One pixel's `[R, G, B]` as `yuv2rgb_X_c_template` makes it: `y` is the filtered luma, which may overshoot a byte (the
    /// table has headroom for it), `u` and `v` are clipped by their tables.
    pub fn rgb_unclipped(&self, y: i32, u: i32, v: i32) -> [i32; 3] {
        self.rgb(y, u.clamp(0, 255), v.clamp(0, 255))
    }

    /// One pixel's `[R, G, B]` from its limited-range Y, U, V (each 0..=255).
    pub fn rgb(&self, y: i32, u: i32, v: i32) -> [i32; 3] {
        let (y, u, v) = (i64::from(y), i64::from(u), i64::from(v));
        let (crv, cbu, cgu, cgv) = (
            i64::from(self.crv),
            i64::from(self.cbu),
            i64::from(self.cgu),
            i64::from(self.cgv),
        );
        [
            self.table(Y_OFFSET + y - (crv >> 9) + ((v * crv) >> 16)),
            self.table(Y_OFFSET + y - (cgu >> 9) + ((u * cgu) >> 16) - (cgv >> 9) + ((v * cgv) >> 16)),
            self.table(Y_OFFSET + y - (cbu >> 9) + ((u * cbu) >> 16)),
        ]
    }
}

/// swscale's integer RGB -> YCbCr coefficients for a matrix, limited range out, 15
/// fractional bits (`RGB2YUV_SHIFT`): `fill_rgb2yuv_table`. For BT.601 swscale
/// substitutes an explicit table (the analytic one rounds differently in a few
/// places).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb2Yuv {
    pub ry: i32,
    pub gy: i32,
    pub by: i32,
    pub ru: i32,
    pub gu: i32,
    pub bu: i32,
    pub rv: i32,
    pub gv: i32,
    pub bv: i32,
}

/// `ROUNDED_DIV`.
fn rounded_div(a: i64, b: i64) -> i64 {
    (if a >= 0 { a + (b >> 1) } else { a - (b >> 1) }) / b
}

impl Rgb2Yuv {
    pub fn new(matrix: YuvMatrix) -> Self {
        if matrix == YuvMatrix::Bt601 {
            return Self {
                ry: 8414,
                gy: 16519,
                by: 3208,
                ru: -4865,
                gu: -9528,
                bu: 14392,
                rv: 14392,
                gv: -12061,
                bv: -2332,
            };
        }
        const ONE: i64 = 65_536;
        const S: u32 = 15;
        let [vr, ub, ug, vg] = {
            let [crv, cbu, cgu, cgv] = coefficients(matrix);
            [crv, cbu, -cgu, -cgv]
        };
        let cy = ONE * 255 / 219;
        let w = rounded_div(ONE * ONE * ug, ub);
        let v = rounded_div(ONE * ONE * vg, vr);
        let z = ONE * ONE - w - v;
        let (c_y, c_u, c_v) = (rounded_div(cy * z, ONE), rounded_div(ub * z, ONE), rounded_div(vr * z, ONE));
        let one = 1i64 << S;
        Self {
            ry: -rounded_div(one * v, c_y) as i32,
            gy: rounded_div(one * ONE * ONE, c_y) as i32,
            by: -rounded_div(one * w, c_y) as i32,
            ru: rounded_div(one * v, c_u) as i32,
            gu: -rounded_div(one * ONE * ONE, c_u) as i32,
            bu: rounded_div(one * (z + w), c_u) as i32,
            rv: rounded_div(one * (v + z), c_v) as i32,
            gv: -rounded_div(one * ONE * ONE, c_v) as i32,
            bv: rounded_div(one * w, c_v) as i32,
        }
    }

    /// The 8-bit luma of an RGB pixel.
    pub fn luma(&self, r: i32, g: i32, b: i32) -> i32 {
        let y14 = (self.ry * r + self.gy * g + self.by * b + (32 << 14) + (1 << 8)) >> 9;
        ((y14 + 32) >> 6).clamp(0, 255)
    }

    /// The 15-bit `(U, V)` of one RGB pixel (swscale's `rgb24ToUV_c`: the chroma of 4:4:4 output, one sample
    /// a pixel), before the vertical pass.
    pub fn pixel_to_chroma15(&self, rgb: [i32; 3]) -> (i32, i32) {
        let (u, v) = self.pixel_to_chroma14(rgb);
        (u * 2, v * 2)
    }

    /// `rgb24ToUV_c`'s own 14-bit `(U, V)`: what the horizontal scaler is handed when it brings a row of per-pixel chroma to the
    /// chroma plane's width (it reads RGB input as 16 bits and shifts the sum by 13, which makes it 15).
    pub fn pixel_to_chroma14(&self, rgb: [i32; 3]) -> (i32, i32) {
        let [r, g, b] = rgb;
        let u = (self.ru * r + self.gu * g + self.bu * b + (256 << 14) + (1 << 8)) >> 9;
        let v = (self.rv * r + self.gv * g + self.bv * b + (256 << 14) + (1 << 8)) >> 9;
        (u, v)
    }

    /// The 15-bit `(U, V)` of a horizontal pair of RGB pixels given as the *sum* of
    /// the two (swscale's `bgr24ToUV_half`), before the vertical filter.
    pub fn pair_to_chroma15(&self, sum: [i32; 3]) -> (i32, i32) {
        let [r, g, b] = sum;
        let u = (self.ru * r + self.gu * g + self.bu * b + (256 << 15) + (1 << 9)) >> 10;
        let v = (self.rv * r + self.gv * g + self.bv * b + (256 << 15) + (1 << 9)) >> 10;
        (u * 2, v * 2)
    }
}

/// How the way back (`argb -> yuva420p`) makes the chroma of a picture of **odd width**, which depends on the swscale it runs on.
///
/// * [`OddWidth::Pairs`] (FFmpeg 6.1): the chroma is the sum of horizontal pixel pairs (`bgr24ToUV_half`), and the last pair of a
///   row is the last pixel and the one after it, **in the row's padding**, which the graph's frame pool zeroed when it allocated
///   the buffer and nothing writes: a pixel of 0, 0, 0.
/// * [`OddWidth::Scaled`] (FFmpeg 9.0): `sws_init_context` no longer halves the chroma of an RGB source of odd width
///   (`!(srcW & 1)`), so it is made for every pixel (`rgb24ToUV_c`) and the horizontal bicubic scaler brings the row to the chroma
///   plane's width, its edge taps clamped to the last pixel: nothing is read past the line.
///
/// Even widths are the pairs on both. The compositor follows the one `roundtrip_probe` measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OddWidth {
    Pairs,
    Scaled,
}

/// `roundToInt16` of swscale's `utils.c`.
fn round_to_i16(f: i64) -> i64 {
    ((f + (1 << 15)) >> 16).clamp(-0x7FFF, 0x7FFF)
}

/// The 13-bit constants of `yuv2rgb_write_full` (`c->yuv2rgb_*_coeff`, `ff_yuv2rgb_c_init_tables`), which the scaler's
/// **full-chroma** output functions use instead of the tables: `yuv -> argb` takes them for a picture of odd width that is not
/// converted by the unscaled converter (an odd height, `SWS_FULL_CHR_H_INT` being forced on an RGB output of odd width).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullChroma {
    pub y_coeff: i32,
    pub y_offset: i32,
    pub v2r: i32,
    pub v2g: i32,
    pub u2g: i32,
    pub u2b: i32,
}

impl FullChroma {
    pub fn new(matrix: YuvMatrix) -> Self {
        let [crv, cbu, cgu, cgv] = coefficients(matrix);
        let (cgu, cgv) = (-cgu, -cgv);
        let cy = 65_536 * 255 / 219;
        let oy = 16i64 << 16;
        Self {
            y_coeff: round_to_i16(cy * (1 << 13)) as i32,
            y_offset: round_to_i16(oy * (1 << 9)) as i32,
            v2r: round_to_i16(crv * (1 << 13)) as i32,
            v2g: round_to_i16(cgv * (1 << 13)) as i32,
            u2g: round_to_i16(cgu * (1 << 13)) as i32,
            u2b: round_to_i16(cbu * (1 << 13)) as i32,
        }
    }

    /// One pixel from the filtered luma `y` (8 bits shifted by 9) and chroma `u`, `v` (centred on 0, 9 more fractional bits), as
    /// `yuv2rgb_write_full` computes it in 32-bit integers, wrapping, then clipped to 30 bits.
    pub fn rgb(&self, y: i32, u: i32, v: i32) -> [u8; 3] {
        let y = y.wrapping_sub(self.y_offset).wrapping_mul(self.y_coeff).wrapping_add(1 << 21);
        let r = y.wrapping_add(v.wrapping_mul(self.v2r));
        let g = y
            .wrapping_add(v.wrapping_mul(self.v2g))
            .wrapping_add(u.wrapping_mul(self.u2g));
        let b = y.wrapping_add(u.wrapping_mul(self.u2b));
        let fin = |c: i32| (c.clamp(0, (1 << 30) - 1) >> 22) as u8;
        [fin(r), fin(g), fin(b)]
    }
}

/// `yuva420p -> argb` of a picture of any size, as swscale does it: `y` is `w * h`, `u` and `v` the chroma planes
/// (`ceil(w / 2)` by `ceil(h / 2)`). Three conversions, by the picture's sides:
///
/// * **even height**: the unscaled converter ([`Yuv2Rgb`]'s tables, the chroma of a pixel that of its 2x2 block). Its tail loop
///   stops at `w & !1`, so **an odd width leaves the last pixel as the buffer was**: the pool zeroed it, so it is 0, 0, 0 and
///   the alpha is 0 (it comes out as `[0, 0, 0]` here, and the plane's alpha is [`zero_last_column`]'s).
/// * **odd height, even width**: not the unscaled converter (`!(dstH & 1)`) but the generic scaler, whose chroma is brought from
///   `ceil(h / 2)` rows to `h` by the bicubic vertical filter (12-bit weights, `>> 19`) and then looked up in the same tables.
/// * **odd height and width**: the same with the full-chroma functions: the chroma is also scaled to the width by the horizontal
///   filter (14-bit weights, `>> 7`, 15 bits), and the colour is the 13-bit arithmetic of [`FullChroma`].
///
/// `None` for a picture the filter tables do not cover.
pub fn yuv_to_rgb(planes: [&[u8]; 3], (w, h): (usize, usize), matrix: YuvMatrix) -> Option<Vec<u8>> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let [y, u, v] = planes;
    let t = Yuv2Rgb::new(matrix);
    let mut rgb = Vec::with_capacity(w * h * 3);
    if h % 2 == 0 {
        for r in 0..h {
            for x in 0..w {
                if x + 1 == w && w % 2 == 1 {
                    rgb.extend([0u8; 3]);
                    continue;
                }
                let c = (r / 2) * cw + x / 2;
                let [pr, pg, pb] = t.rgb(i32::from(y[r * w + x]), i32::from(u[c]), i32::from(v[c]));
                rgb.extend([pr as u8, pg as u8, pb as u8]);
            }
        }
        return Some(rgb);
    }
    let vertical = crate::sws::bicubic(ch as u32, h as u32, crate::sws::ONE_VERTICAL)?;
    if w % 2 == 0 {
        // Chroma at the chroma plane's width, 15 bits, then the vertical filter into the table's index (which the tables clip).
        let filtered = |plane: &[u8]| -> Vec<i32> {
            let mut out = vec![0i32; cw * h];
            for r in 0..h {
                for x in 0..cw {
                    let acc: i32 = (0..vertical.size)
                        .map(|j| {
                            let row = (vertical.pos[r] as usize + j).min(ch - 1);
                            vertical.coeff[r * vertical.size + j] * (i32::from(plane[row * cw + x]) << 7)
                        })
                        .sum();
                    out[r * cw + x] = ((1 << 18) + acc) >> 19;
                }
            }
            out
        };
        let (fu, fv) = (filtered(u), filtered(v));
        for r in 0..h {
            for x in 0..w {
                let c = r * cw + x / 2;
                let [pr, pg, pb] = t.rgb(i32::from(y[r * w + x]), fu[c].clamp(0, 255), fv[c].clamp(0, 255));
                rgb.extend([pr as u8, pg as u8, pb as u8]);
            }
        }
        return Some(rgb);
    }
    let horizontal = crate::sws::bicubic(cw as u32, w as u32, crate::sws::ONE_HORIZONTAL)?;
    let full = FullChroma::new(matrix);
    let filtered = |plane: &[u8]| -> Vec<i32> {
        // To the picture's width on each of the chroma rows, 15 bits clipped at the top (`hScale8To15_c`)...
        let mut wide = vec![0i32; w * ch];
        for r in 0..ch {
            for x in 0..w {
                let acc: i32 = (0..horizontal.size)
                    .map(|j| {
                        let col = (horizontal.pos[x] as usize + j).min(cw - 1);
                        horizontal.coeff[x * horizontal.size + j] * i32::from(plane[r * cw + col])
                    })
                    .sum();
                wide[r * w + x] = (acc >> 7).min(32767);
            }
        }
        // ... then to the picture's height, centred on 0 (`yuv2rgb_full_X_c_template`).
        let mut out = vec![0i32; w * h];
        for r in 0..h {
            for x in 0..w {
                let acc: i32 = (0..vertical.size)
                    .map(|j| {
                        let row = (vertical.pos[r] as usize + j).min(ch - 1);
                        vertical.coeff[r * vertical.size + j] * wide[row * w + x]
                    })
                    .sum();
                out[r * w + x] = ((1 << 9) - (128 << 19) + acc) >> 10;
            }
        }
        out
    };
    let (fu, fv) = (filtered(u), filtered(v));
    for i in 0..w * h {
        rgb.extend(full.rgb(i32::from(y[i]) << 9, fu[i], fv[i]));
    }
    Some(rgb)
}

/// Whether the conversion before `colorchannelmixer` leaves the picture's last column unwritten, so that its alpha is 0 and the
/// whole column is transparent: an odd width the unscaled converter's tail loop does not reach (an odd height takes the scaler,
/// which writes every pixel).
pub fn zero_last_column(w: usize, h: usize) -> bool {
    w % 2 == 1 && h % 2 == 0
}

/// `argb -> yuva420p` of an `rgb` picture (`w * h * 3` bytes) of any size with `back`'s matrix: the luma of every pixel, the chroma of
/// pixel pairs or of the scaled row ([`OddWidth`]), and the vertical bicubic filter in the x86 kernel for every chroma row but the
/// last (`ceil(h / 2)` rows). Returns `[Y, U, V]`; `None` for a picture the filter tables do not cover.
pub fn rgb_to_yuv(rgb: &[u8], (w, h): (usize, usize), back: YuvMatrix, odd: OddWidth) -> Option<[Vec<u8>; 3]> {
    let k = Rgb2Yuv::new(back);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let px = |x: usize, y: usize| -> [i32; 3] {
        if x >= w {
            return [0; 3];
        }
        std::array::from_fn(|c| i32::from(rgb[(y * w + x) * 3 + c]))
    };
    let luma = (0..w * h)
        .map(|i| k.luma(i32::from(rgb[i * 3]), i32::from(rgb[i * 3 + 1]), i32::from(rgb[i * 3 + 2])) as u8)
        .collect();
    let mut u15 = vec![0i32; cw * h];
    let mut v15 = vec![0i32; cw * h];
    if w % 2 == 1 && odd == OddWidth::Scaled {
        let horizontal = crate::sws::bicubic(w as u32, cw as u32, crate::sws::ONE_HORIZONTAL)?;
        for y in 0..h {
            for x in 0..cw {
                let (mut su, mut sv) = (0i32, 0i32);
                for j in 0..horizontal.size {
                    let col = (horizontal.pos[x] as usize + j).min(w - 1);
                    let (pu, pv) = k.pixel_to_chroma14(px(col, y));
                    let c = horizontal.coeff[x * horizontal.size + j];
                    su += c * pu;
                    sv += c * pv;
                }
                u15[y * cw + x] = (su >> 13).min(32767);
                v15[y * cw + x] = (sv >> 13).min(32767);
            }
        }
    } else {
        for y in 0..h {
            for x in 0..cw {
                let (a, b) = (px(2 * x, y), px(2 * x + 1, y));
                let (u, v) = k.pair_to_chroma15([a[0] + b[0], a[1] + b[1], a[2] + b[2]]);
                u15[y * cw + x] = u;
                v15[y * cw + x] = v;
            }
        }
    }
    Some([
        luma,
        crate::grade::vertical_x86(&u15, cw, h, ch)?,
        crate::grade::vertical_x86(&v15, cw, h, ch)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tables_derive_from_swscales_coefficients() {
        let t = Yuv2Rgb::new(YuvMatrix::Bt601);
        assert_eq!(t.cy, 76_309); // 65536 * 255 / 219
                                  // 104597 * 65536 / 76309, rounded, and the negated green ones.
        assert_eq!((t.crv, t.cbu), (89_830, 113_537));
        assert_eq!((t.cgu, t.cgv), (-22_049, -45_756));
    }

    /// Pictures of random YUV through `yuva420p -> argb` on FFmpeg 6.1 and 9.0,
    /// sampled: `(Y, U, V)` to `(R, G, B)`. The full runs (3072 pixels each, all
    /// three matrices, both versions) had no mismatch; these are the anchors.
    #[test]
    fn yuv_to_rgb_is_what_ffmpeg_measured() {
        type Sample = ((i32, i32, i32), [i32; 3]);
        let cases: [(YuvMatrix, [Sample; 8]); 3] = [
            (
                YuvMatrix::Bt601,
                [
                    ((33, 40, 22), [0, 139, 0]),
                    ((71, 17, 87), [0, 140, 0]),
                    ((236, 106, 142), [255, 252, 210]),
                    ((65, 74, 95), [3, 104, 0]),
                    ((5, 59, 83), [0, 51, 0]),
                    ((40, 26, 97), [0, 93, 0]),
                    ((244, 169, 112), [238, 255, 255]),
                    ((47, 71, 186), [126, 11, 0]),
                ],
            ),
            (
                YuvMatrix::Bt709,
                [
                    ((33, 40, 22), [0, 93, 0]),
                    ((71, 17, 87), [0, 108, 0]),
                    ((236, 106, 142), [255, 252, 208]),
                    ((65, 74, 95), [0, 84, 0]),
                    ((5, 59, 83), [0, 25, 0]),
                    ((40, 26, 97), [0, 65, 0]),
                    ((244, 169, 112), [235, 255, 255]),
                    ((47, 71, 186), [138, 15, 0]),
                ],
            ),
            (
                YuvMatrix::Bt2020,
                [
                    ((33, 40, 22), [0, 103, 0]),
                    ((71, 17, 87), [0, 110, 0]),
                    ((236, 106, 142), [255, 249, 207]),
                    ((65, 74, 95), [0, 87, 0]),
                    ((5, 59, 83), [0, 27, 0]),
                    ((40, 26, 97), [0, 65, 0]),
                    ((244, 169, 112), [237, 255, 255]),
                    ((47, 71, 186), [132, 8, 0]),
                ],
            ),
        ];
        for (matrix, rows) in cases {
            let t = Yuv2Rgb::new(matrix);
            for ((y, u, v), rgb) in rows {
                assert_eq!(t.rgb(y, u, v), rgb, "{matrix:?} Y{y} U{u} V{v}");
            }
        }
        let t = Yuv2Rgb::new(YuvMatrix::Bt601);
        // Black is black; the limited range's white is 253, not 255 (swscale's
        // table rounds down, which is a level of the luma lost in every round trip);
        // beyond the range is clipped, not wrapped.
        assert_eq!(t.rgb(16, 128, 128), [0, 0, 0]);
        assert_eq!(t.rgb(235, 128, 128), [253, 253, 253]);
        assert_eq!(t.rgb(0, 128, 128), [0, 0, 0]);
        assert_eq!(t.rgb(255, 128, 128), [255, 255, 255]);
    }

    /// Flat colours through `argb -> yuva420p` on FFmpeg 6.1 and 9.0: `(R, G, B)`
    /// to `(Y, U, V)` of the first chroma row, the way back as the grade models it (`grade::rgb_to_yuv420`: the x86 vertical kernel).
    /// Luma, U and V equal FFmpeg's.
    #[test]
    fn rgb_to_yuv_is_what_ffmpeg_measured() {
        let back = Rgb2Yuv::new(YuvMatrix::Bt601);
        for ((r, g, b), (y, u, v)) in [
            ((255, 0, 0), (81, 90, 240)),
            ((0, 255, 0), (145, 54, 34)),
            ((0, 0, 255), (41, 240, 110)),
            ((255, 255, 0), (210, 16, 146)),
            ((12, 200, 99), (130, 112, 53)),
            ((255, 255, 255), (235, 128, 128)),
            ((0, 0, 0), (16, 128, 128)),
            ((128, 128, 128), (126, 128, 128)),
            ((200, 30, 160), (98, 160, 194)),
        ] {
            assert_eq!(back.luma(r, g, b), y, "luma of {r},{g},{b}");
            // (A flat picture: the first chroma row is the table's, the others are the vertical filter's interior.)
            let flat = [[r as u8, g as u8, b as u8]; 16 * 16].concat();
            let [_, cu, cv] = crate::grade::rgb_to_yuv420(&flat, 16, 16, YuvMatrix::Bt601);
            assert_eq!((i32::from(cu[0]), i32::from(cv[0])), (u, v), "U, V of {r},{g},{b}");
        }
    }

    #[test]
    fn the_rgb_to_yuv_tables_follow_swscales_derivation() {
        // BT.601 is swscale's explicit table; the others come from the analytic
        // derivation (`0.2126 * 219 / 255 * 2^15` = 5983 for BT.709's red luma).
        let t601 = Rgb2Yuv::new(YuvMatrix::Bt601);
        assert_eq!((t601.ry, t601.gy, t601.by), (8414, 16519, 3208));
        assert_eq!((t601.ru, t601.gu, t601.bu), (-4865, -9528, 14392));
        let t709 = Rgb2Yuv::new(YuvMatrix::Bt709);
        assert_eq!((t709.ry, t709.gy, t709.by), (5983, 20127, 2032));
        assert_eq!((t709.ru, t709.gu, t709.bu), (-3298, -11094, 14392));
        assert_eq!((t709.rv, t709.gv, t709.bv), (14392, -13073, -1320));
        let t2020 = Rgb2Yuv::new(YuvMatrix::Bt2020);
        assert_eq!((t2020.ry, t2020.gy, t2020.by), (7393, 19080, 1669));
        assert_eq!((t2020.rv, t2020.gv, t2020.bv), (14392, -13235, -1158));
        // Each row of luma weights sums to the full-range gain, 219/255 of 2^15.
        for t in [t601, t709, t2020] {
            assert!(((t.ry + t.gy + t.by) - 28_142).abs() <= 1, "{t:?}");
        }
    }

    #[test]
    fn a_saturated_bt709_colour_stays_in_gamut_only_with_its_own_matrix() {
        // SMPTE HD bars' yellow (Y 168, U 44, V 136) is in gamut as BT.709 and
        // out of it as BT.601: the reason the way out of YUV uses the layer's own
        // matrix rather than the composite's.
        let own = Yuv2Rgb::new(YuvMatrix::Bt709).rgb(168, 44, 136);
        assert!(
            own[2] <= 3 && (185..=195).contains(&own[0]) && (185..=195).contains(&own[1]),
            "{own:?}"
        );
        let wrong = Yuv2Rgb::new(YuvMatrix::Bt601).rgb(168, 44, 136);
        assert!(wrong[1] > own[1] + 10, "{wrong:?} vs {own:?}");
    }
}
