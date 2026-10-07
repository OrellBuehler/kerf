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
//! exhaustive run on random pictures, and the `opacity` parity cases):
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
//!   RGB, so it is a real 2:1 scale): within one level everywhere.
//! * **alpha** is `lrint(255 * opacity)` scaled by 256/255 on the way back to YUV:
//!   see [`kerf_core::layer_geometry::ffmpeg_alpha`].
//!
//! **Odd sizes are not reproduced, and not drawn**: on a picture with an odd
//! width the pairing of pixels reads one pixel past the line — padding the graph
//! never initialised — and an odd height takes swscale off its unscaled yuv -> rgb
//! path altogether. [`kerf_core::layer_geometry::LayerGeometry::resolve`] refuses such a
//! translucent layer, so that frame goes through FFmpeg.

use kerf_core::YuvMatrix;

/// `ff_yuv2rgb_coeffs`: `{crv, cbu, cgu, cgv}` per matrix.
fn coefficients(m: YuvMatrix) -> [i64; 4] {
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

    /// The 15-bit `(U, V)` of a horizontal pair of RGB pixels given as the *sum* of
    /// the two (swscale's `bgr24ToUV_half`), before the vertical filter.
    pub fn pair_to_chroma15(&self, sum: [i32; 3]) -> (i32, i32) {
        let [r, g, b] = sum;
        let u = (self.ru * r + self.gu * g + self.bu * b + (256 << 15) + (1 << 9)) >> 10;
        let v = (self.rv * r + self.gv * g + self.bv * b + (256 << 15) + (1 << 9)) >> 10;
        (u * 2, v * 2)
    }
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
    /// to `(Y, U, V)`. Luma is exact. A flat picture goes through the vertical
    /// filter unchanged (`(u15 + 64) >> 7`, its weights sum to one), and chroma
    /// lands within a level of FFmpeg's: x86 FFmpeg's vertical scaler is not
    /// bit-exact with the C one this follows (~7% of chroma samples are a level
    /// off on random pictures).
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
            let (u15, v15) = back.pair_to_chroma15([2 * r, 2 * g, 2 * b]);
            assert!((((u15 + 64) >> 7).clamp(0, 255) - u).abs() <= 1, "U of {r},{g},{b}");
            assert!((((v15 + 64) >> 7).clamp(0, 255) - v).abs() <= 1, "V of {r},{g},{b}");
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
