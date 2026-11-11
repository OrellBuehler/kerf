//! A `yuv420p` picture taken from one YCbCr matrix to another the way swscale does it, as arithmetic: the scalar
//! reference of the compositor's conversion passes, and the probe that says whether this FFmpeg converts as it does.
//!
//! FFmpeg 9 negotiates the colourspace along the overlay chain (`CompositeColorPolicy::BottomLayerTag`): the first
//! clip's tag is the composite's and an input tagged otherwise gets an auto-inserted `scale` between the clip's chain
//! (its last `format`) and `overlay`. swscale has no YCbCr-to-YCbCr matrix change, so `sws_setColorspaceDetails` builds
//! two contexts and goes **through RGB** ("YUV color matrix differs for YUV->YUV, using intermediate RGB to convert",
//! `utils.c`), unscaled here (the chain has made the picture the size it is overlaid at):
//!
//! * `yuv420p -> bgr24` with the *source's* coefficients: the x86 SSSE3 kernel the grade's way in is
//!   ([`crate::grade::X86Kernel`], held to FFmpeg there);
//! * then `bgr24 -> yuv420p` with the *destination's*, **not** by the generic scaler the grade's way out is: an
//!   unscaled `bgr24 -> yuv420p` with an even width has a converter of its own, `bgr24ToYv12Wrapper` ->
//!   `ff_rgb24toyv12` (`rgb2rgb`), which is C on every row on the FFmpeg 9 pinned here, and on an x86 build with the
//!   inline MMX one (`rgb24toyv12_mmxext`) the first two rows and the last two go through C and every row between
//!   through different arithmetic (two truncations in luma, `pavgb` roundings in chroma):
//!   * C: `Y = ((ry*r + gy*g + by*b) >> 15) + 16`; chroma from the 2x2 block's channel means taken with `>> 2`
//!     (`U = ((ru*rx + gu*gx + bu*bx) >> 15) + 128`).
//!
//! **Where in the chain.** The conversion sits ahead of the chain's last `format`, on the picture the earlier filters
//! (the fit `scale`, `pad`) made: a resized layer is scaled **then** converted (measured: 0 of 10240 luma samples differ
//! with the order the other way round at 9931). A still's graph has no `format`, and its last `scale` converts the matrix in
//! the same pass as its resize, which is not this: it is swscale's cascade at the smaller of the two sizes
//! ([`scaled_to_rgb24`], [`scale_and_change_matrix`], probed by [`scaled`]). **Whose matrix is the composite's** is the chain's, not the frame's: the
//! first clip on an alpha plane (a clip entering through a dissolve, a keyed zoom or position, or a source with an alpha channel: nothing in its chain forces a converter), else the first clip, and every clip
//! of another matrix is converted into it once, wherever on the timeline ([`kerf_core::PlanCanvas::cut_matrix`], measured
//! with three clips in every combination of plain and dissolving). A **dissolving** clip's chain ends in `format=yuva420p`
//! and its last `scale` converts the matrix itself in the same pass as its resize, as in a still: where that scale does not
//! resize the picture it is the unscaled cascade (measured), and where it shrinks it the scaled one, `bgr24` at the
//! destination's size.
//!
//! The coefficients are `Rgb2Yuv`'s ([`crate::roundtrip`]): `fill_rgb2yuv_table` of the destination's matrix.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use kerf_core::YuvMatrix;

use crate::grade::{yuv420_to_rgb, X86Kernel};
use crate::roundtrip::{Rgb2Yuv, Yuv2Rgb};
use crate::scaler;
use crate::sws::{self, Scaler};

/// Which `ff_rgb24toyv12` a build runs: the C one on every row, or the x86 `rgb24toyv12_mmxext`, which does the
/// first two rows and the last two itself in C and every row between in MMX.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    C,
    Mmx,
}

/// `ff_rgb24toyv12_c` and `rgb24toyv12_mmxext` on one `w` x `h` RGB picture (`rgb` is `r, g, b` per pixel, which is
/// `bgr24`'s bytes read the other way round): the planes of a `yuv420p`. Even sides only (the converter's own
/// condition; the plan refuses the rest).
pub fn rgb_to_yuv420_24(rgb: &[u8], w: usize, h: usize, matrix: YuvMatrix, variant: Variant) -> [Vec<u8>; 3] {
    let k = Rgb2Yuv::new(matrix);
    let px = |x: usize, y: usize| -> [i32; 3] { std::array::from_fn(|c| i32::from(rgb[(y * w + x) * 3 + c])) };
    let (cw, ch) = (w / 2, h / 2);
    let mut luma = vec![0u8; w * h];
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    for y in 0..h {
        // The first two rows and the last two go through the C function; the rows between, the MMX loop.
        let c_rows = variant == Variant::C || y < 2 || y + 2 >= h;
        for x in 0..w {
            let [r, g, b] = px(x, y);
            luma[y * w + x] = if c_rows {
                (((k.ry * r + k.gy * g + k.by * b) >> 15) + 16) as u8
            } else {
                let t = ((b * k.by + g * k.gy) >> 8) + ((r * k.ry) >> 8);
                // (`packssdw` to 16 bits, `psraw 7`, `packuswb`, `paddusb 16`.)
                ((t >> 7).clamp(0, 255) + 16).min(255) as u8
            };
        }
    }
    for cy in 0..ch {
        let c_row = variant == Variant::C || cy == 0 || cy + 1 == ch;
        for cx in 0..cw {
            let p = [
                px(2 * cx, 2 * cy),
                px(2 * cx + 1, 2 * cy),
                px(2 * cx, 2 * cy + 1),
                px(2 * cx + 1, 2 * cy + 1),
            ];
            let [rx, gx, bx] = if c_row {
                // `(b11 + b12 + b21 + b22) >> 2`.
                std::array::from_fn(|c| (p[0][c] + p[1][c] + p[2][c] + p[3][c]) >> 2)
            } else {
                // `pavgb` of the two rows, then of the two columns: each rounds up.
                std::array::from_fn(|c| {
                    let top = (p[0][c] + p[2][c] + 1) >> 1;
                    let right = (p[1][c] + p[3][c] + 1) >> 1;
                    (top + right + 1) >> 1
                })
            };
            let chroma = |kr: i32, kg: i32, kb: i32| -> u8 {
                if c_row {
                    // (An `unsigned int` in the C: the low byte of the sum, which the table keeps inside 16..240.)
                    (((kr * rx + kg * gx + kb * bx) >> 15) + 128) as u8
                } else {
                    let t = ((bx * kb + gx * kg) >> 8) + ((rx * kr) >> 8);
                    ((t >> 7).clamp(-128, 127) + 128) as u8
                }
            };
            u[cy * cw + cx] = chroma(k.ru, k.gu, k.bu);
            v[cy * cw + cx] = chroma(k.rv, k.gv, k.bv);
        }
    }
    [luma, u, v]
}

/// The whole change of matrix of an opaque picture: `planes` (`w` x `h`, 4:2:0, even sides) read as `from`, through the
/// x86 `yuv420p -> bgr24` kernel and `bgr24 -> yuv420p` with `to`'s coefficients.
pub fn change_matrix(planes: [&[u8]; 3], w: usize, h: usize, from: YuvMatrix, to: YuvMatrix, variant: Variant) -> [Vec<u8>; 3] {
    rgb_to_yuv420_24(&yuv420_to_rgb(planes, w, h, from), w, h, to, variant)
}

// ---- the scaled conversion -----------------------------------------------------------------------

/// How the vertical pass of a scale that ends in `bgr24` is run for a row: the x86 inline MMX `yuv2bgr24_X`
/// (`YSCALEYUV2RGBX`: `pmulhw` of the 15-bit intermediates and the 12-bit weights, the chroma and luma terms on 16-bit lanes) on every
/// row but the last two, the C `yuv2rgb_X_c_template` (a 19-bit sum, swscale's tables) on those, which `swscale.c` hands back
/// to C (`dstY >= dstH - 2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packed {
    /// The C function on every row.
    C,
    /// MMX on the rows before the last two.
    Mmx,
}

/// The planes of a picture and how large it is.
#[derive(Debug, Clone, Copy)]
pub struct Pic<'a> {
    pub planes: [&'a [u8]; 3],
    pub size: (usize, usize),
}

/// The four tables of the scaled conversion to `bgr24`: the luma and the chroma, horizontally and vertically.
struct Tables {
    hl: sws::Filter,
    hc: sws::Filter,
    vl: sws::Filter,
    vc: sws::Filter,
}

impl Tables {
    /// The tables for a `src` -> `dst` scale, or `None` where the model does not follow it: an odd or empty side, a ratio beyond one pass
    /// (no table), or a luma vertical table of one or two taps (swscale hands those rows to `yuv2packed1` / `yuv2packed2`).
    fn of((sw, sh): (usize, usize), (dw, dh): (usize, usize)) -> Option<Self> {
        if sw % 2 != 0 || sh % 2 != 0 || dw % 2 != 0 || dh % 2 != 0 || sw == 0 || sh == 0 || dw == 0 || dh == 0 {
            return None;
        }
        let (ah, av) = (sws::SIMD_ALIGN_HORIZONTAL, sws::SIMD_ALIGN_VERTICAL);
        let table = |src: usize, dst: usize, one: i32, align: usize| {
            sws::table_aligned(Scaler::Bicubic, src as u32, dst as u32, one, align)
        };
        let tables = Self {
            hl: table(sw, dw, sws::ONE_HORIZONTAL, ah)?,
            hc: table(sw / 2, dw / 2, sws::ONE_HORIZONTAL, ah)?,
            vl: table(sh, dh, sws::ONE_VERTICAL, av)?,
            vc: table(sh / 2, dh, sws::ONE_VERTICAL, av)?,
        };
        (tables.vl.size > 2).then_some(tables)
    }
}

/// Whether the model follows a `src` -> `dst` scale at all ([`scaled_to_rgb24`] answers `None` for the rest): the sizes the plan may
/// draw as the cascade have to be ones it does (`kerf_core::layer_geometry::ScaleStage::cascade_refusal` is held to this). Builds
/// the tables, not the picture.
pub fn follows(src: (usize, usize), dst: (usize, usize)) -> bool {
    Tables::of(src, dst).is_some()
}

/// What a `yuv420p` picture of `src` size turns into when swscale scales it to `dst` and converts it to `bgr24` in the one
/// context (`sws_scale` with a `bgr24` destination): `Some(rgb)`, `r, g, b` per pixel, or `None` for a size the model does not
/// follow (an odd side; a luma vertical filter of one or two taps, which swscale hands to other output functions; a ratio
/// beyond one pass). A chroma filter of one tap (a 2:1 shrink of the rows) is the general output function's too.
///
/// * Horizontal: the luma `src.0 -> dst.0` and the chroma `src.0 / 2 -> dst.0 / 2` (the destination RGB has no chroma
///   of its own; one sample serves two pixels), 15 bits, bicubic tables of an x86 build (filter sizes rounded up to 4).
/// * Vertical: the luma `src.1 -> dst.1` and the chroma **`src.1 / 2 -> dst.1`** (an RGB destination has a chroma row for
///   every row), tables rounded up to 2 taps.
/// * Out: `kernel` per row ([`Packed`]).
pub fn scaled_to_rgb24(pic: Pic, dst: (usize, usize), matrix: YuvMatrix, kernel: Packed) -> Option<Vec<u8>> {
    let (sw, sh) = pic.size;
    let (dw, dh) = dst;
    let Tables { hl, hc, vl, vc } = Tables::of(pic.size, dst)?;
    let (scw, sch, dcw) = (sw / 2, sh / 2, dw / 2);
    let [py, pu, pv] = pic.planes;
    if py.len() != sw * sh || pu.len() != scw * sch || pv.len() != scw * sch {
        return None;
    }
    // The horizontal pass, per row of the source: 15 bits, clipped at the top.
    let hscale = |plane: &[u8], pw: usize, rows: usize, out_w: usize, f: &sws::Filter| -> Vec<i32> {
        let mut mid = vec![0i32; out_w * rows];
        for y in 0..rows {
            for x in 0..out_w {
                let acc: i32 = (0..f.size)
                    .map(|j| {
                        let col = (f.pos[x] as usize + j).min(pw - 1);
                        f.coeff[x * f.size + j] * i32::from(plane[y * pw + col])
                    })
                    .sum();
                mid[y * out_w + x] = (acc >> 7).min(32767);
            }
        }
        mid
    };
    let my = hscale(py, sw, sh, dw, &hl);
    let mu = hscale(pu, scw, sch, dcw, &hc);
    let mv = hscale(pv, scw, sch, dcw, &hc);
    let mmx = X86Kernel::new(matrix);
    let c = Yuv2Rgb::new(matrix);
    let mut out = Vec::with_capacity(dw * dh * 3);
    for i in 0..dh {
        let simd = kernel == Packed::Mmx && i + 2 < dh;
        for x in 0..dw {
            let lum =
                |_: ()| (0..vl.size).map(|j| (vl.coeff[i * vl.size + j], my[(vl.pos[i] as usize + j).min(sh - 1) * dw + x]));
            let chr = |plane: &[i32]| {
                (0..vc.size)
                    .map(|j| {
                        (
                            vc.coeff[i * vc.size + j],
                            plane[(vc.pos[i] as usize + j).min(sch - 1) * dcw + x / 2],
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let (u_taps, v_taps) = (chr(&mu), chr(&mv));
            if simd {
                // (`vRounder` is 4 on every lane.)
                let acc = |taps: &mut dyn Iterator<Item = (i32, i32)>| 4 + taps.map(|(c, s)| (c * s) >> 16).sum::<i32>();
                let y8 = acc(&mut lum(()));
                let u8_ = acc(&mut u_taps.iter().copied());
                let v8 = acc(&mut v_taps.iter().copied());
                out.extend(mmx.rgb_lanes(y8, u8_, v8));
            } else {
                let acc = |taps: &mut dyn Iterator<Item = (i32, i32)>| {
                    (1i32 << 18).wrapping_add(taps.map(|(c, s)| c.wrapping_mul(s)).fold(0i32, i32::wrapping_add))
                };
                let y = acc(&mut lum(())) >> 19;
                let u = acc(&mut u_taps.iter().copied()) >> 19;
                let v = acc(&mut v_taps.iter().copied()) >> 19;
                out.extend(c.rgb_unclipped(y, u, v).map(|c| c as u8));
            }
        }
    }
    Some(out)
}

/// The whole change of matrix of a `yuv420p` picture swscale scales to `dst` in the same `scale`: **through the cascade**
/// ([`scaled_to_rgb24`], then `ff_rgb24toyv12` on the `bgr24` intermediate at the destination's size), which is what
/// a destination smaller than the source gives (`sws_setColorspaceDetails` makes the intermediate the smaller of the two).
pub fn scale_and_change_matrix(
    pic: Pic,
    dst: (usize, usize),
    from: YuvMatrix,
    to: YuvMatrix,
    kernel: Packed,
) -> Option<[Vec<u8>; 3]> {
    if pic.size.0 * pic.size.1 <= dst.0 * dst.1 {
        return None;
    }
    let rgb = scaled_to_rgb24(pic, dst, from, kernel)?;
    Some(rgb_to_yuv420_24(&rgb, dst.0, dst.1, to, Variant::C))
}

/// The tables of the **enlarging** cascade: the horizontal ones (luma `sw -> dw`, chroma `sw -> dw / 2`: an RGB source has a chroma
/// sample for every pixel: `sws_init_context` halves an RGB source's chroma only for a destination whose chroma is not wider than half
/// the source, which an enlargement never is) and the vertical ones (luma `sh -> dh`, chroma `sh -> dh / 2`), the bilinear scaler's, as an
/// x86 build pads them.
pub struct EnlargeTables {
    pub hl: sws::Filter,
    pub hc: sws::Filter,
    pub vl: sws::Filter,
    pub vc: sws::Filter,
}

impl EnlargeTables {
    /// The tables for a `src` -> `dst` enlargement (both axes grow, even sides), or `None` where the model does not follow it.
    pub fn of((sw, sh): (usize, usize), (dw, dh): (usize, usize)) -> Option<Self> {
        if sw % 2 != 0 || sh % 2 != 0 || dw % 2 != 0 || dh % 2 != 0 || sw == 0 || sh == 0 || sw >= dw || sh >= dh {
            return None;
        }
        let (ah, av) = (sws::SIMD_ALIGN_HORIZONTAL, sws::SIMD_ALIGN_VERTICAL);
        let table = |src: usize, dst: usize, one: i32, align: usize| {
            sws::table_aligned(Scaler::Bilinear, src as u32, dst as u32, one, align)
        };
        Some(Self {
            hl: table(sw, dw, sws::ONE_HORIZONTAL, ah)?,
            hc: table(sw, dw / 2, sws::ONE_HORIZONTAL, ah)?,
            vl: table(sh, dh, sws::ONE_VERTICAL, av)?,
            vc: table(sh, dh / 2, sws::ONE_VERTICAL, av)?,
        })
    }
}

/// Whether the model follows a `src` -> `dst` enlargement ([`enlarge_and_change_matrix`] answers `None` for the rest).
pub fn follows_enlarged(src: (usize, usize), dst: (usize, usize)) -> bool {
    EnlargeTables::of(src, dst).is_some()
}

/// The vertical pass of the scaler over a 15-bit plane, to a byte (`yuv2planeX`): the x86 kernel on every row but the last `c_rows`, the C
/// one (a 19-bit sum) on those, which `swscale.c` hands back to C (the last two rows of luma, the last of a 4:2:0 chroma plane).
fn vertical_bytes(mid: &[i32], width: usize, rows: usize, out_rows: usize, filter: &sws::Filter, c_rows: usize) -> Vec<u8> {
    let start = crate::scaler::vertical_start(filter.size);
    let mut out = vec![0u8; width * out_rows];
    for i in 0..out_rows {
        let simd = filter.size > 1 && i + c_rows < out_rows;
        for x in 0..width {
            let taps = (0..filter.size).map(|j| {
                let row = (filter.pos[i] as usize + j).min(rows - 1);
                (filter.coeff[i * filter.size + j], mid[row * width + x])
            });
            out[i * width + x] = if simd {
                let acc: i32 = start + taps.map(|(c, s)| (c * s) >> 16).sum::<i32>();
                (acc >> 3).clamp(0, 255) as u8
            } else {
                let acc: i64 = (64 << 12) + taps.map(|(c, s)| i64::from(c) * i64::from(s)).sum::<i64>();
                (acc >> 19).clamp(0, 255) as u8
            };
        }
    }
    out
}

/// `bgr24 -> yuv420p` **scaled up** by the generic scaler with `to`'s coefficients: the second context of swscale's cascade when the
/// destination is the larger picture. `rgb` is `r, g, b` per pixel (`bgr24`'s bytes the other way round), `src` its size, `dst` the
/// planes' (even sides, [`EnlargeTables`]).
///
/// * Input: the colour of every pixel as 14 bits (`bgr24ToY_c` and `bgr24ToUV_c`: `(ry*r + gy*g + by*b + (32 << 14) + (1 << 8)) >> 9`, chroma
///   with `256 << 14`), one chroma sample for each pixel.
/// * Horizontal: `hScale16To15` on them (an RGB source is read as 16 bits, and the sum is shifted by 13), the bilinear x86 tables, clipped at the top.
/// * Vertical: [`vertical_bytes`], the chroma rows from the source's full height.
pub fn rgb_scale_up(rgb: &[u8], src: (usize, usize), dst: (usize, usize), to: YuvMatrix) -> Option<[Vec<u8>; 3]> {
    let (sw, sh) = src;
    let (dw, dh) = dst;
    if rgb.len() != sw * sh * 3 {
        return None;
    }
    let EnlargeTables { hl, hc, vl, vc } = EnlargeTables::of(src, dst)?;
    let k = Rgb2Yuv::new(to);
    let n = sw * sh;
    let (mut y14, mut u14, mut v14) = (vec![0i32; n], vec![0i32; n], vec![0i32; n]);
    for i in 0..n {
        let p = [0, 1, 2].map(|c| i32::from(rgb[i * 3 + c]));
        y14[i] = (k.ry * p[0] + k.gy * p[1] + k.by * p[2] + (32 << 14) + (1 << 8)) >> 9;
        let (u, v) = k.pixel_to_chroma14(p);
        u14[i] = u;
        v14[i] = v;
    }
    let horizontal = |plane: &[i32], filter: &sws::Filter, out_w: usize| -> Vec<i32> {
        let mut mid = vec![0i32; out_w * sh];
        for y in 0..sh {
            for x in 0..out_w {
                let acc: i32 = (0..filter.size)
                    .map(|j| {
                        let col = (filter.pos[x] as usize + j).min(sw - 1);
                        filter.coeff[x * filter.size + j] * plane[y * sw + col]
                    })
                    .sum();
                mid[y * out_w + x] = (acc >> 13).min(32767);
            }
        }
        mid
    };
    let (my, mu, mv) = (
        horizontal(&y14, &hl, dw),
        horizontal(&u14, &hc, dw / 2),
        horizontal(&v14, &hc, dw / 2),
    );
    Some([
        vertical_bytes(&my, dw, sh, dh, &vl, 2),
        vertical_bytes(&mu, dw / 2, sh, dh / 2, &vc, 1),
        vertical_bytes(&mv, dw / 2, sh, dh / 2, &vc, 1),
    ])
}

/// The whole change of matrix of a `yuv420p` picture swscale **enlarges** to `dst` in the same `scale` (the blurred fill's up-scale, flags
/// `bilinear`): through the cascade's other half, `bgr24` at the **source's** size (`sws_setColorspaceDetails`: the smaller of the two),
/// `yuv420p -> bgr24` unscaled with `from`'s coefficients (the x86 kernel of [`change_matrix`]'s way in) and then [`rgb_scale_up`] to the
/// destination. `None` for a size the model does not follow.
pub fn enlarge_and_change_matrix(pic: Pic, dst: (usize, usize), from: YuvMatrix, to: YuvMatrix) -> Option<[Vec<u8>; 3]> {
    let (w, h) = pic.size;
    if !follows_enlarged(pic.size, dst) || pic.planes[0].len() != w * h || pic.planes[1].len() != w * h / 4 {
        return None;
    }
    let rgb = yuv420_to_rgb(pic.planes, w, h, from);
    rgb_scale_up(&rgb, pic.size, dst, to)
}

// ---- the probe -----------------------------------------------------------------------------------

/// Whether this FFmpeg converts a layer into the composite's matrix as [`change_matrix`] does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernels {
    /// A picture through the overlay chain's conversion came back as the model computes it.
    Match,
    /// It did not: the chain that differs.
    Differs(&'static str),
    /// Not known: not measured yet, or FFmpeg would not run or timed out (asked again after a backoff).
    Unknown,
}

impl Kernels {
    /// The compositor converts a layer into the composite's matrix on this machine.
    pub fn converts(self) -> bool {
        self == Self::Match
    }
}

/// The probe picture: tall enough for the MMX loop's rows (it leaves the first two and the last two to C).
const PROBE: (usize, usize) = (256, 16);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

fn tag(matrix: &str) -> &'static str {
    match matrix {
        "bt709" => ",setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709",
        "bt2020nc" => ",setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt2020-10",
        _ => "",
    }
}

/// The probe picture: a polynomial of the coordinates, mod 256, in all three planes (not `random()`, which restarts
/// for every slice thread), `matrix` as its tag.
fn source(matrix: &str) -> String {
    source_sized(matrix, PROBE)
}

/// [`source`] at another size.
fn source_sized(matrix: &str, (w, h): (usize, usize)) -> String {
    format!(
        "nullsrc=s={w}x{h}:r=1:d=1,format=yuv420p,\
         geq=lum='mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29,256)':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17,256)'{}",
        tag(matrix)
    )
}

/// The overlay chain of an export with two clips: a `bottom` picture, and above it a `top` one that covers it, tagged
/// otherwise; the frame is the top picture as FFmpeg takes it into the bottom's matrix.
fn overlay_graph(bottom: &str, top: &str) -> String {
    let (w, h) = PROBE;
    format!(
        "color=c=black:s={w}x{h}:r=1:d=1,format=yuv420p[vbase];{},format=yuv420p[v0];[vbase][v0]overlay[o0];{},format=yuv420p[v1];[o0][v1]overlay",
        source(bottom),
        source(top)
    )
}

/// The overlay chain of an export with **three clips on one track**, laid side by side on one canvas so that every clip's
/// picture is seen: a plain BT.709 clip, then a clip whose chain ends in `yuva420p` (what a dissolve's chain ends in) with no
/// tag, then a plain BT.709 clip again. The inputs are **files** (`[0:v]` and `[2:v]` the tagged one, `[1:v]` the untagged
/// one): a tag set in the graph by `setparams` is another constraint in the negotiation and moves who decides (measured: the
/// first clip does, then). The composite's matrix is the tag of the first clip on an alpha plane (the middle one: nothing in
/// its chain forces a converter), and the plain clips are converted into it ([`kerf_core::PlanCanvas::cut_matrix`] is that
/// rule: `Planner` reads it off the graph's shape, and this is the check that this FFmpeg negotiates it so).
fn decider_graph() -> String {
    let (w, h) = PROBE;
    // The export's identity fit of a picture that fills the frame.
    let fit = format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1");
    format!(
        "color=c=black:s={}x{h}:r=1:d=1,format=yuv420p[vbase];[0:v]{fit},fps=1,format=yuv420p[v0];[vbase][v0]overlay=x=0:y=0[o0];\
         [1:v]{fit},fps=1,format=yuva420p[v1];[o0][v1]overlay=x={w}:y=0[o1];[2:v]{fit},fps=1,format=yuv420p[v2];[o1][v2]overlay=x={}:y=0",
        3 * w,
        2 * w
    )
}

/// A scratch directory under the temp dir, removed when dropped.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Option<Self> {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kerf-gpu-convert-probe-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).ok()?;
        Some(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The probe picture as a lossless one-frame file (FFV1 in Matroska) carrying `matrix` as its tag: what the export's inputs are.
fn make_file(path: &std::path::Path, matrix: &str) -> Option<()> {
    // (Two of them at once.)
    make_file_sized(path, matrix, PROBE, 2)
}

/// [`make_file`] at another size, with `share` files being made at once.
fn make_file_sized(path: &std::path::Path, matrix: &str, size: (usize, usize), share: usize) -> Option<()> {
    let mut args = args_for(&[], &source_sized(matrix, size));
    // (`args` writes a rawvideo pipe; a file is the same graph into an encoder.)
    args.truncate(args.iter().position(|a| a == "-map")?);
    args.extend(["-map", "[o]", "-frames:v", "1", "-c:v", "ffv1", "-pix_fmt", "yuv420p", "-y"].map(String::from));
    args.push(path.to_string_lossy().into_owned());
    kerf_core::limit_frame_args(&mut args, share);
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(&args);
    crate::source::run_child(cmd, 1 << 16, PROBE_TIMEOUT).ok().map(drop)
}

/// Tile `k` (of three side by side) of a `yuv420p` frame of `3 * PROBE.0` x `PROBE.1`, as the planes of a `PROBE` picture.
fn tile(frame: &[u8], k: usize) -> Vec<u8> {
    let (w, h) = PROBE;
    let (y, rest) = frame.split_at(3 * w * h);
    let (u, v) = rest.split_at(3 * w * h / 4);
    let rows = |plane: &[u8], pw: usize, rows: usize| -> Vec<u8> {
        (0..rows)
            .flat_map(|r| plane[r * 3 * pw + k * pw..r * 3 * pw + (k + 1) * pw].iter().copied())
            .collect()
    };
    let mut out = rows(y, w, h);
    out.extend(rows(u, w / 2, h / 2));
    out.extend(rows(v, w / 2, h / 2));
    out
}

fn args(graph: &str) -> Vec<String> {
    args_for(&[], graph)
}

/// [`args`] over input `files`.
fn args_for(files: &[std::path::PathBuf], graph: &str) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin"]
        .into_iter()
        .map(String::from)
        .collect();
    for f in files {
        a.extend(["-i".to_string(), f.to_string_lossy().into_owned()]);
    }
    a.push("-filter_complex".to_string());
    a.push(format!("{graph}[o]"));
    a.extend(
        [
            "-map",
            "[o]",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "pipe:1",
        ]
        .map(String::from),
    );
    a
}

fn planes_of(frame: &[u8]) -> [&[u8]; 3] {
    let (w, h) = PROBE;
    let (y, rest) = frame.split_at(w * h);
    let (u, v) = rest.split_at(w * h / 4);
    [y, u, v]
}

fn measure() -> Option<Kernels> {
    let (w, h) = PROBE;
    let yuv = w * h * 3 / 2;
    // The cheap round first, three short children at once: the source, the picture's conversion into the bottom layer's matrix
    // (two pairs of matrices). A machine that converts another way settles here, `Differs` for good, without the files below.
    // (graph, the length of its frame)
    let graphs = [
        (source("none"), yuv),
        (overlay_graph("bt709", "none"), yuv),
        (overlay_graph("bt2020nc", "bt709"), yuv),
    ];
    let share = graphs.len();
    // The graphs are `-filter_complex` ones: that is how the product writes them, and where the negotiation happens.
    let frames: Vec<Option<Vec<u8>>> = std::thread::scope(|scope| {
        let runs: Vec<_> = graphs
            .iter()
            .map(|(graph, len)| {
                scope.spawn(move || {
                    let mut args = args(graph);
                    kerf_core::limit_frame_args(&mut args, share);
                    let mut cmd = kerf_core::ffmpeg_command();
                    cmd.args(&args);
                    crate::source::run_child(cmd, len + 1, PROBE_TIMEOUT).ok()
                })
            })
            .collect();
        runs.into_iter().map(|run| run.join().ok().flatten()).collect()
    });
    let [Some(source), Some(opaque), Some(opaque_2020)] = &frames[..] else {
        return None;
    };
    if source.len() != yuv {
        return None;
    }
    let src = planes_of(source);
    let want = |from, to, variant| change_matrix(src, w, h, from, to, variant).concat();
    // The overlay chain converts the top picture into the bottom's matrix.
    if *opaque != want(YuvMatrix::Bt601, YuvMatrix::Bt709, Variant::C)
        || *opaque_2020 != want(YuvMatrix::Bt709, YuvMatrix::Bt2020, Variant::C)
    {
        return Some(if *opaque == want(YuvMatrix::Bt601, YuvMatrix::Bt709, Variant::Mmx) {
            Kernels::Differs("an opaque picture goes through the MMX rgb24toyv12")
        } else {
            Kernels::Differs("an opaque picture's conversion into the bottom layer's matrix")
        });
    }
    // Only a machine that converts as modelled is asked who decides: two tagged files (what an export's inputs are), then the
    // three-clip graph over them. A failure here is `Unknown` (asked again after a backoff), not a verdict.
    let scratch = Scratch::new()?;
    let (tagged, untagged) = (scratch.0.join("tagged.mkv"), scratch.0.join("untagged.mkv"));
    let made: [Option<()>; 2] = std::thread::scope(|scope| {
        [(&tagged, "bt709"), (&untagged, "none")]
            .map(|(path, matrix)| scope.spawn(move || make_file(path, matrix)))
            .map(|m| m.join().ok().flatten())
    });
    if made.iter().any(Option::is_none) {
        return None;
    }
    let three = {
        let mut args = args_for(&[tagged.clone(), untagged, tagged], &decider_graph());
        kerf_core::limit_frame_args(&mut args, 1);
        let mut cmd = kerf_core::ffmpeg_command();
        cmd.args(&args);
        crate::source::run_child(cmd, 3 * yuv + 1, PROBE_TIMEOUT).ok()?
    };
    if three.len() != 3 * yuv {
        return None;
    }
    // Whose matrix is the composite's, in the shape the planner's decider reads: the first clip on an alpha plane (the middle
    // one, untagged = BT.601) keeps its picture, and the two plain BT.709 clips are converted into BT.601.
    let into_601 = want(YuvMatrix::Bt709, YuvMatrix::Bt601, Variant::C);
    if tile(&three, 0) != into_601 || tile(&three, 1) != *source || tile(&three, 2) != into_601 {
        return Some(Kernels::Differs(
            "the composite's matrix is not the first clip on an alpha plane's, as the planner's decider reads the graph",
        ));
    }
    Some(Kernels::Match)
}

// ---- the scaled conversion's probe ---------------------------------------------------------------

/// Whether this FFmpeg's `scale`, when it resizes a picture and takes it from one matrix to another in the one call, runs the
/// cascade [`scale_and_change_matrix`] follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scaled {
    /// The probe's scaled pictures, through the export's own chains, came back as the model computes them, **the blurred fill's too**
    /// (its up-scale is the cascade's enlarging half: [`enlarge_and_change_matrix`]).
    Match,
    /// The pictures that shrink came back as the model computes them and the fill behind a converted layer did not: what differs. A
    /// layer is drawn without a fill, and one with a fill is FFmpeg's.
    Shrinks(&'static str),
    /// They did not: what differs.
    Differs(&'static str),
    /// Not known: not measured yet, or FFmpeg would not run or timed out (asked again after a backoff).
    Unknown,
}

impl Scaled {
    /// The compositor draws a layer whose last scale resizes it and converts its matrix on this machine.
    pub fn converts(self) -> bool {
        matches!(self, Self::Match | Self::Shrinks(_))
    }

    /// ... and the blurred fill behind such a layer, whose up-scale converts it too.
    pub fn fills(self) -> bool {
        self == Self::Match
    }
}

/// The canvas of the scaled probe's graphs: two tiles side by side, the first the clip that decides the composite's matrix, the
/// second the clip whose fit `scale` resizes it and converts its matrix.
const CANVAS: (usize, usize) = (64, 48);

/// The scaled probe's pictures, by the clip that is resized: `(size of the file, the picture the fit scale makes of it)`. A
/// square, which a letterbox fits, and a tall one.
const RESIZED: [((usize, usize), (usize, usize)); 2] = [((160, 160), (48, 48)), ((96, 192), (24, 48))];

/// The export's chains for a decider and a resized clip over the canvas: the chains of [`decider_graph`], `plane` (the clips are
/// on the alpha plane: `fps`, `format=yuva420p`, a dissolving clip's) or not (a still's: the overlay asks for `yuva420p` itself).
fn scaled_graph(plane: bool) -> String {
    let (w, h) = CANVAS;
    let fit = |w, h| format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1");
    let tail = if plane { ",fps=1,format=yuva420p" } else { "" };
    format!(
        "color=c=black:s={}x{h}:r=1:d=1,format=yuv420p[vbase];[0:v]{}{tail}[v0];[vbase][v0]overlay=x=0:y=0[o0];[1:v]{}{tail}[v1];[o0][v1]overlay=x={w}:y=0",
        2 * w,
        fit(w, h),
        fit(w, h)
    )
}

/// Frame `k` (of two side by side, `canvas` each) of a `yuv420p` picture, cut to the window at `(x, y)` of size `(w, h)`, as planes.
fn window(frame: &[u8], canvas: (usize, usize), k: usize, (x, y): (usize, usize), (w, h): (usize, usize)) -> [Vec<u8>; 3] {
    let (cw, ch) = canvas;
    let (fw, fh) = (2 * cw, ch);
    let (luma, rest) = frame.split_at(fw * fh);
    let (u, v) = rest.split_at(fw * fh / 4);
    let cut = |plane: &[u8], pw: usize, x0: usize, y0: usize, w: usize, h: usize| -> Vec<u8> {
        (y0..y0 + h)
            .flat_map(|r| plane[r * pw + x0..r * pw + x0 + w].iter().copied())
            .collect()
    };
    let x0 = k * cw + x;
    [
        cut(luma, fw, x0, y, w, h),
        cut(u, fw / 2, x0 / 2, y / 2, w / 2, h / 2),
        cut(v, fw / 2, x0 / 2, y / 2, w / 2, h / 2),
    ]
}

/// The canvas of the fill's probe graphs: two tiles side by side, the first the clip that decides the composite's matrix, the second a
/// clip over a **blurred fill** (`Fit::Blur`): a thumbnail of 16x12 (`2 * floor(128 / 16)` by `2 * floor(96 / 16)`), a box blur of radius 1,
/// and an up-scale of 8:1 to the frame.
const FILL_CANVAS: (usize, usize) = (128, 96);

/// The picture of the clip over the fill: portrait, so that the fit leaves bars (it is fitted to 48x96, a shrink the cascade follows).
const FILL_PICTURE: (usize, usize) = (90, 180);

/// The export's chains for a decider and a clip over a blurred fill, in the canvas of [`FILL_CANVAS`]: the fill is
/// [`kerf_core::blur_fill_chain`], the very text the export writes (`split` to the `overlay` that lays the fitted picture over the blurred
/// copy), followed by `setsar=1` and, for a dissolving clip's chain (`plane`), `fps` and `format=yuva420p`. A still's has neither.
fn filled_graph(plane: bool) -> String {
    let (w, h) = FILL_CANVAS;
    let tail = if plane { ",fps=1,format=yuva420p" } else { "" };
    let fit = format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1");
    format!(
        "color=c=black:s={}x{h}:r=1:d=1,format=yuv420p[vbase];[0:v]{fit}{tail}[v0];[vbase][v0]overlay=x=0:y=0[o0];[1:v]{},setsar=1{tail}[v1];[o0][v1]overlay=x={w}:y=0",
        2 * w,
        kerf_core::blur_fill_chain(w as u32, h as u32, "pf"),
    )
}

/// What the frame of the clip over a blurred fill is, as the cascade's two halves compute it: the picture, fitted and converted by
/// [`scale_and_change_matrix`], laid over the fill, which is the thumbnail (the picture scaled to cover it by the bilinear scaler, cropped,
/// box-blurred) converted by [`enlarge_and_change_matrix`] on its way up to the frame. `None` for a geometry the model does not follow.
fn filled_frame(source: &[u8], from: YuvMatrix, to: YuvMatrix) -> Option<[Vec<u8>; 3]> {
    let (fw, fh) = FILL_CANVAS;
    let (sw, sh) = FILL_PICTURE;
    let fill = kerf_core::blur_fill::BlurFill::resolve((sw as u32, sh as u32), (fw as u32, fh as u32)).ok()?;
    let (tw, th) = fill.thumb();
    let (y, rest) = source.split_at(sw * sh);
    let (u, v) = rest.split_at(sw * sh / 4);
    // Each plane at its own size: scaled to the cover, cut to the thumbnail's window, blurred.
    let thumb = |plane: &[u8], chroma: bool| -> Option<Vec<u8>> {
        let half = |n: u32| if chroma { n / 2 } else { n } as usize;
        let cover = scaler::scale_plane(
            plane,
            (half(sw as u32), half(sh as u32)),
            (half(fill.cover.0), half(fill.cover.1)),
            Scaler::Bilinear,
            scaler::Kernel::X86,
            if chroma { 1 } else { 2 },
        )?;
        let (cw, cx, cy, w, h) = (half(fill.cover.0), half(fill.crop.x), half(fill.crop.y), half(tw), half(th));
        let cut: Vec<u8> = (cy..cy + h)
            .flat_map(|r| cover[r * cw + cx..r * cw + cx + w].iter().copied())
            .collect();
        crate::fill::box_blur(&cut, w, h, fill.radius as usize, 2)
    };
    let planes = [thumb(y, false)?, thumb(u, true)?, thumb(v, true)?];
    let mut frame = enlarge_and_change_matrix(
        Pic {
            planes: [&planes[0], &planes[1], &planes[2]],
            size: (tw as usize, th as usize),
        },
        (fw, fh),
        from,
        to,
    )?;
    // The picture's fit: `decrease` into the frame, centred (an even offset).
    let fitted = (sw * fh / sh, fh);
    let pic = scale_and_change_matrix(
        Pic {
            planes: [y, u, v],
            size: (sw, sh),
        },
        fitted,
        from,
        to,
        Packed::Mmx,
    )?;
    let at = ((fw - fitted.0) / 2 & !1, (fh - fitted.1) / 2 & !1);
    for (pi, (plane, copy)) in frame.iter_mut().zip(&pic).enumerate() {
        let shift = usize::from(pi > 0);
        let (pw, ph) = (fitted.0 >> shift, fitted.1 >> shift);
        let (ax, ay) = (at.0 >> shift, at.1 >> shift);
        let stride = fw >> shift;
        for r in 0..ph {
            plane[(ay + r) * stride + ax..(ay + r) * stride + ax + pw].copy_from_slice(&copy[r * pw..(r + 1) * pw]);
        }
    }
    Some(frame)
}

/// Which graph a run of the scaled probe is, and which picture it holds the result to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// The fit `scale` of a clip that shrinks (`RESIZED[n]`), the cascade's first half.
    Resized(usize),
    /// A clip over a blurred fill: the fit `scale` and the fill's up-scale, the cascade's two halves.
    Filled,
}

/// One of the scaled probe's runs: the file of the clip that decides, the file that is resized, and which graph.
struct ScaledRun {
    decider: std::path::PathBuf,
    resized: std::path::PathBuf,
    /// The matrix of the decider's tag and the resized clip's (what `want` converts between).
    from: YuvMatrix,
    to: YuvMatrix,
    shape: Shape,
    plane: bool,
}

fn measure_scaled() -> Option<Scaled> {
    // The cascade is the conversion's second half; a build that does not convert as modelled has nothing to follow.
    match unscaled() {
        Kernels::Match => {}
        Kernels::Unknown => return None,
        Kernels::Differs(_) => {
            return Some(Scaled::Differs(
                "this FFmpeg's conversion between matrices is not the model's",
            ))
        }
    }
    let scratch = Scratch::new()?;
    // (name, tag, size): the deciders at the canvas' size, the resized clips at the files' sizes of `RESIZED`.
    let big = |shape: usize, name: &str, tag: &str| {
        (
            scratch.0.join(format!("{name}-{shape}.mkv")),
            tag.to_string(),
            RESIZED[shape].0,
        )
    };
    let small = |name: &str, tag: &str| (scratch.0.join(format!("{name}-small.mkv")), tag.to_string(), CANVAS);
    // ... and the fill's: deciders at its canvas' size, the picture over the fill at `FILL_PICTURE`.
    let filled = |name: &str, tag: &str, size| (scratch.0.join(format!("{name}-fill.mkv")), tag.to_string(), size);
    let files = [
        small("d709", "bt709"),
        small("d601", "none"),
        big(0, "r601", "none"),
        big(0, "r709", "bt709"),
        big(0, "r2020", "bt2020nc"),
        big(1, "r601", "none"),
        filled("d709", "bt709", FILL_CANVAS),
        filled("d601", "none", FILL_CANVAS),
        filled("p709", "bt709", FILL_PICTURE),
        filled("p601", "none", FILL_PICTURE),
    ];
    let share = files.len();
    let made: Vec<Option<()>> = std::thread::scope(|scope| {
        files
            .iter()
            .map(|(path, tag, size)| scope.spawn(move || make_file_sized(path, tag, *size, share)))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|m| m.join().ok().flatten())
            .collect()
    });
    if made.iter().any(Option::is_none) {
        return None;
    }
    let path = |i: usize| files[i].0.clone();
    let run = |decider, resized, from, to, shape, plane| ScaledRun {
        decider: path(decider),
        resized: path(resized),
        from,
        to,
        shape,
        plane,
    };
    use YuvMatrix::{Bt2020, Bt601, Bt709};
    let runs = [
        // The chains of a dissolving clip (an alpha plane), each direction and BT.2020, and the tall picture.
        run(0, 2, Bt601, Bt709, Shape::Resized(0), true),
        run(1, 3, Bt709, Bt601, Shape::Resized(0), true),
        run(0, 4, Bt2020, Bt709, Shape::Resized(0), true),
        run(0, 5, Bt601, Bt709, Shape::Resized(1), true),
        // ... and the still's, where the overlay itself asks for `yuva420p`.
        run(0, 2, Bt601, Bt709, Shape::Resized(0), false),
        run(1, 3, Bt709, Bt601, Shape::Resized(0), false),
        // The fill behind such a clip, both directions, the dissolving chain and the still's.
        run(6, 9, Bt601, Bt709, Shape::Filled, true),
        run(7, 8, Bt709, Bt601, Shape::Filled, true),
        run(6, 9, Bt601, Bt709, Shape::Filled, false),
        run(7, 8, Bt709, Bt601, Shape::Filled, false),
    ];
    // The reference pictures: the planes of a resized clip's source, and the picture over the fill.
    let pictures = [
        ("none", RESIZED[0].0),
        ("bt709", RESIZED[0].0),
        ("bt2020nc", RESIZED[0].0),
        ("none", RESIZED[1].0),
        ("none", FILL_PICTURE),
    ];
    let share = pictures.len();
    let sources: Vec<Option<Vec<u8>>> = std::thread::scope(|scope| {
        let jobs: Vec<_> = pictures
            .into_iter()
            .map(|(tag, size)| {
                scope.spawn(move || {
                    let mut args = args(&source_sized(tag, size));
                    kerf_core::limit_frame_args(&mut args, share);
                    let mut cmd = kerf_core::ffmpeg_command();
                    cmd.args(&args);
                    crate::source::run_child(cmd, size.0 * size.1 * 3 / 2 + 1, PROBE_TIMEOUT).ok()
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().ok().flatten()).collect()
    });
    let share = runs.len();
    let frames: Vec<Option<Vec<u8>>> = std::thread::scope(|scope| {
        let jobs: Vec<_> = runs
            .iter()
            .map(|run| {
                scope.spawn(move || {
                    let (graph, (w, h)) = match run.shape {
                        Shape::Resized(_) => (scaled_graph(run.plane), CANVAS),
                        Shape::Filled => (filled_graph(run.plane), FILL_CANVAS),
                    };
                    let mut args = args_for(&[run.decider.clone(), run.resized.clone()], &graph);
                    kerf_core::limit_frame_args(&mut args, share);
                    let mut cmd = kerf_core::ffmpeg_command();
                    cmd.args(&args);
                    crate::source::run_child(cmd, 2 * w * h * 3 / 2 + 1, PROBE_TIMEOUT).ok()
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().ok().flatten()).collect()
    });
    // The pictures that shrink first (their verdict is `Differs`); the fill's is `Shrinks`, the rest still draws.
    let mut filled_differs = None;
    for (run, frame) in runs.iter().zip(&frames) {
        let frame = frame.as_ref()?;
        let (picture_size, canvas) = match run.shape {
            Shape::Resized(shape) => (RESIZED[shape].0, CANVAS),
            Shape::Filled => (FILL_PICTURE, FILL_CANVAS),
        };
        if frame.len() != 2 * canvas.0 * canvas.1 * 3 / 2 {
            return None;
        }
        let source = match (run.from, run.shape) {
            (_, Shape::Filled) => &sources[4],
            (YuvMatrix::Bt601, Shape::Resized(0)) => &sources[0],
            (YuvMatrix::Bt709, Shape::Resized(0)) => &sources[1],
            (YuvMatrix::Bt2020, Shape::Resized(0)) => &sources[2],
            _ => &sources[3],
        }
        .as_ref()?;
        // (A child that exits 0 with a short frame is a probe that could not run, not a difference: asked again after the backoff.)
        if source.len() != picture_size.0 * picture_size.1 * 3 / 2 {
            return None;
        }
        let (y, rest) = source.split_at(picture_size.0 * picture_size.1);
        let (u, v) = rest.split_at(picture_size.0 * picture_size.1 / 4);
        let pic = Pic {
            planes: [y, u, v],
            size: picture_size,
        };
        if run.shape == Shape::Filled {
            let got = window(frame, canvas, 1, (0, 0), canvas);
            if filled_frame(source, run.from, run.to).as_ref() != Some(&got) && filled_differs.is_none() {
                filled_differs = Some(if run.plane {
                    "a dissolving clip's blurred fill is not the cascade's enlarging half the model computes"
                } else {
                    "a still's blurred fill is not the cascade's enlarging half the model computes"
                });
            }
            continue;
        }
        let Shape::Resized(shape) = run.shape else {
            unreachable!("the fill's runs are judged above")
        };
        let picture = RESIZED[shape].1;
        let (cw, ch) = canvas;
        let at = ((cw - picture.0) / 2, (ch - picture.1) / 2);
        let got = window(frame, canvas, 1, at, picture);
        let Some(want) = scale_and_change_matrix(pic, picture, run.from, run.to, Packed::Mmx) else {
            return Some(Scaled::Differs("the probe's own picture is one the model does not follow"));
        };
        if got != want {
            let c_only = scale_and_change_matrix(pic, picture, run.from, run.to, Packed::C);
            return Some(Scaled::Differs(if c_only.as_ref() == Some(&got) {
                "a resized picture is converted with the C output functions on every row"
            } else if run.plane {
                "a dissolving clip's resized picture is not the cascade the model computes"
            } else {
                "a still's resized picture is not the cascade the model computes"
            }));
        }
    }
    Some(filled_differs.map_or(Scaled::Match, Scaled::Shrinks))
}

static SCALED: Mutex<State<Scaled>> = Mutex::new(State::new());

/// Whether this FFmpeg's `scale` that resizes a picture and converts its matrix in the one call is the cascade
/// [`scale_and_change_matrix`] follows. Runs the probe on its first call (the conversion's first; then the export's
/// own chains over a decider and a clip that is resized, a few `ffmpeg` children bounded by 20 s each, with no lock held): a
/// blocking thread's, ahead of the frame that needs it ([`crate::prepare`]). [`Scaled::Unknown`] is not remembered: the probe is run
/// again after a backoff.
pub fn scaled() -> Scaled {
    cached(&SCALED, Instant::now(), measure_scaled)
}

/// What is known without running anything: the measured answer, else [`Scaled::Unknown`]. What a plan's caps are made from
/// ([`crate::Compositor::caps`]).
pub fn scaled_measured() -> Scaled {
    lock(&SCALED).measured.unwrap_or(Scaled::Unknown)
}

/// What a probe of this module has found, and when it may be asked again.
struct State<T> {
    measured: Option<T>,
    failures: u32,
    retry_at: Option<Instant>,
}

impl<T> State<T> {
    const fn new() -> Self {
        Self {
            measured: None,
            failures: 0,
            retry_at: None,
        }
    }
}

/// The answer of a probe of this module: what it says while it does not know, and what is logged when it settles on a difference
/// or cannot run.
trait Verdict: Copy {
    const UNKNOWN: Self;
    fn settled(self);
    fn failed(failures: u32, retry_in_secs: u64);
}

impl Verdict for Kernels {
    const UNKNOWN: Self = Self::Unknown;

    fn settled(self) {
        if let Self::Differs(why) = self {
            tracing::warn!(
                why,
                "this FFmpeg converts a layer into the composite's matrix another way than the GPU does; \
                 stacks of mixed matrices are drawn by FFmpeg"
            );
        }
    }

    fn failed(failures: u32, retry_in_secs: u64) {
        tracing::warn!(
            failures,
            retry_in_secs,
            "could not measure FFmpeg's conversion between colour matrices; stacks of mixed matrices are not drawn on the GPU until it can"
        );
    }
}

impl Verdict for Scaled {
    const UNKNOWN: Self = Self::Unknown;

    fn settled(self) {
        match self {
            Self::Differs(why) => tracing::warn!(
                why,
                "the GPU does not draw a layer FFmpeg resizes as it converts its matrix on this FFmpeg (its scale does not cascade as \
                 the model does, or its conversion is not the model's); such layers are drawn by FFmpeg"
            ),
            Self::Shrinks(why) => tracing::warn!(
                why,
                "the GPU does not draw a blurred fill behind a layer FFmpeg converts into the composite's matrix on this FFmpeg (the fill's \
                 up-scale does not convert as the model does); such layers are drawn by FFmpeg"
            ),
            Self::Match | Self::Unknown => {}
        }
    }

    fn failed(failures: u32, retry_in_secs: u64) {
        tracing::warn!(
            failures,
            retry_in_secs,
            "could not measure FFmpeg's scale that resizes a picture and converts its matrix; such layers are not drawn on the GPU until it can"
        );
    }
}

static STATE: Mutex<State<Kernels>> = Mutex::new(State::new());

fn lock<T>(state: &Mutex<State<T>>) -> std::sync::MutexGuard<'_, State<T>> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether this FFmpeg converts a layer into the composite's matrix as [`change_matrix`] does (and, where it does, whether its `scale`
/// that resizes as it converts is the cascade [`scale_and_change_matrix`] follows: [`scaled`]). The first call runs
/// the probe: three short `ffmpeg` children at once, and only where those match two that make the tagged files and then the three-clip graph over them (each bounded by 20 s) with no lock held, a blocking thread's, ahead of
/// the frame that needs it ([`crate::prepare`]). [`Kernels::Unknown`] is not remembered: the probe is run again after a
/// backoff.
pub fn kernels() -> Kernels {
    let kernels = unscaled();
    // Where the conversion matches, the same pass measures the scale that resizes as it converts ([`scaled`], which needs it).
    if kernels == Kernels::Match {
        let _ = scaled();
    }
    kernels
}

/// [`kernels`] without the scaled probe.
fn unscaled() -> Kernels {
    cached(&STATE, Instant::now(), measure)
}

/// What is known without running anything: the measured answer, else [`Kernels::Unknown`]. What a plan's caps are made
/// from ([`crate::Compositor::caps`]): a plan does not wait for a probe.
pub fn measured() -> Kernels {
    lock(&STATE).measured.unwrap_or(Kernels::Unknown)
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300))
}

fn cached<T: Verdict>(state: &Mutex<State<T>>, now: Instant, measure: impl FnOnce() -> Option<T>) -> T {
    {
        let s = lock(state);
        if let Some(k) = s.measured {
            return k;
        }
        if s.retry_at.is_some_and(|t| now < t) {
            return T::UNKNOWN;
        }
    }
    let measured = measure();
    let mut s = lock(state);
    if let Some(k) = s.measured {
        return k;
    }
    match measured {
        Some(k) => {
            k.settled();
            s.measured = Some(k);
            k
        }
        None if s.retry_at.is_some_and(|t| t > now) => T::UNKNOWN,
        None => {
            s.failures += 1;
            let wait = backoff(s.failures);
            s.retry_at = Some(Instant::now() + wait);
            T::failed(s.failures, wait.as_secs());
            T::UNKNOWN
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u32) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (s >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn the_mmx_variant_differs_from_the_c_one_only_between_the_first_and_last_rows() {
        let (w, h) = (16, 16);
        let rgb = noise(w * h * 3, 7);
        let c = rgb_to_yuv420_24(&rgb, w, h, YuvMatrix::Bt709, Variant::C);
        let mmx = rgb_to_yuv420_24(&rgb, w, h, YuvMatrix::Bt709, Variant::Mmx);
        assert_ne!(c[0], mmx[0]);
        // The first two and last two luma rows, and the first and last chroma row, are the C function's in both.
        assert_eq!(c[0][..2 * w], mmx[0][..2 * w]);
        assert_eq!(c[0][(h - 2) * w..], mmx[0][(h - 2) * w..]);
        assert_eq!(c[1][..w / 2], mmx[1][..w / 2]);
        assert_eq!(c[1][(h / 2 - 1) * (w / 2)..], mmx[1][(h / 2 - 1) * (w / 2)..]);
    }

    /// The plan says the fill's up-scale is the cascade's enlarging half (`BlurFill::enlarge_refusal`) only where the model follows it
    /// ([`follows_enlarged`]), for the frames a delivery is made at and the thumbnails the engine makes of them.
    #[test]
    fn the_plan_judges_the_fills_up_scale_by_what_the_model_follows() {
        use kerf_core::blur_fill::BlurFill;
        let mut judged = 0;
        for w in (4..=400).step_by(14) {
            for h in (4..=400).step_by(14) {
                for (iw, ih) in [(90, 180), (180, 90), (101, 99), (1080, 1920)] {
                    let Ok(fill) = BlurFill::resolve((iw, ih), (w, h)) else {
                        continue;
                    };
                    let (tw, th) = fill.thumb();
                    if fill.enlarge_refusal().is_none() {
                        assert!(
                            follows_enlarged((tw as usize, th as usize), (w as usize, h as usize)),
                            "{tw}x{th} -> {w}x{h}: the plan draws it, the model does not follow it"
                        );
                        judged += 1;
                    } else if w % 2 == 0 && h % 2 == 0 {
                        // (An even frame the thumbnail is not smaller than: not an enlargement.)
                        assert!(tw >= w || th >= h, "{tw}x{th} -> {w}x{h}");
                    }
                }
            }
        }
        assert!(judged > 100, "{judged}");
    }

    /// An enlargement that converts is not the conversion of the enlarged picture: the cascade's intermediate is the thumbnail's own `bgr24`
    /// (one chroma sample per pixel, scaled up by the generic scaler), so a compositor that scaled the planes up and then converted them, as
    /// it does a plain layer's whole frame, would draw the fill off (which is why the fill behind a dissolving or still layer is the passes
    /// of `fill_convert.wgsl`, not the plane scaler followed by `convert_matrix`).
    #[test]
    fn an_enlargement_that_converts_is_not_the_conversion_of_the_enlarged_picture() {
        let (w, h, dw, dh) = (16usize, 12usize, 128usize, 96usize);
        let y = noise(w * h, 31);
        let u = noise(w * h / 4, 32);
        let v = noise(w * h / 4, 33);
        let cascade = enlarge_and_change_matrix(
            Pic {
                planes: [&y, &u, &v],
                size: (w, h),
            },
            (dw, dh),
            YuvMatrix::Bt709,
            YuvMatrix::Bt601,
        )
        .expect("a size the model follows");
        let up = |plane: &[u8], (sw, sh): (usize, usize), (tw, th): (usize, usize), c_rows| {
            scaler::scale_plane(plane, (sw, sh), (tw, th), Scaler::Bilinear, scaler::Kernel::X86, c_rows).expect("a table")
        };
        let (big_y, big_u, big_v) = (
            up(&y, (w, h), (dw, dh), 2),
            up(&u, (w / 2, h / 2), (dw / 2, dh / 2), 1),
            up(&v, (w / 2, h / 2), (dw / 2, dh / 2), 1),
        );
        let after = change_matrix(
            [&big_y, &big_u, &big_v],
            dw,
            dh,
            YuvMatrix::Bt709,
            YuvMatrix::Bt601,
            Variant::C,
        );
        let differing = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(x, y)| x != y).count();
        assert!(differing(&cascade[0], &after[0]) > dw * dh / 2, "luma");
        assert!(differing(&cascade[1], &after[1]) > dw * dh / 8, "U");
        assert!(differing(&cascade[2], &after[2]) > dw * dh / 8, "V");
    }

    /// The model refuses what swscale's `sws_setColorspaceDetails` would not make an enlargement of: the same size or less on an axis, odd sides.
    #[test]
    fn the_enlarging_model_follows_only_even_pictures_that_grow_on_both_axes() {
        assert!(follows_enlarged((16, 12), (128, 96)));
        assert!(follows_enlarged((4, 4), (32, 18)));
        for (src, dst) in [
            ((16, 12), (16, 96)),
            ((16, 12), (128, 12)),
            ((16, 12), (8, 6)),
            ((16, 12), (129, 96)),
            ((16, 12), (128, 97)),
            ((15, 12), (128, 96)),
            ((16, 11), (128, 96)),
            ((0, 12), (128, 96)),
        ] {
            assert!(!follows_enlarged(src, dst), "{src:?} -> {dst:?}");
        }
    }

    /// The plan says a scale is the cascade (`ScaleStage::cascade_refusal`) only where the model follows it ([`follows`]: the tables, not
    /// a picture): no layer is drawn that the passes would then refuse. And `follows` is what [`scaled_to_rgb24`] answers by.
    #[test]
    fn the_plan_judges_the_cascade_by_what_the_model_follows() {
        use kerf_core::layer_geometry::{Rect, ScaleStage};
        let mut judged = 0;
        for sw in (6..=96).step_by(10) {
            for sh in (6..=96).step_by(10) {
                for dw in (4..=96).step_by(6) {
                    for dh in (4..=96).step_by(6) {
                        let stage = ScaleStage {
                            src: Rect::whole(sw, sh),
                            scaled: (dw, dh),
                            keep: Rect::whole(dw, dh),
                        };
                        if stage.cascade_refusal().is_none() {
                            assert!(
                                follows((sw as usize, sh as usize), (dw as usize, dh as usize)),
                                "{sw}x{sh} -> {dw}x{dh}: the plan draws it, the model does not follow it"
                            );
                            judged += 1;
                        }
                    }
                }
            }
        }
        assert!(judged > 100, "{judged}");
    }

    #[test]
    fn the_model_answers_by_follows() {
        for (src, dst) in [
            ((16, 12), (8, 6)),
            ((16, 12), (16, 6)),
            ((16, 12), (16, 12)),
            ((17, 12), (8, 6)),
            ((16, 12), (7, 6)),
            ((16, 12), (0, 6)),
        ] {
            let planes = [
                vec![0u8; src.0 * src.1],
                vec![0u8; src.0 * src.1 / 4],
                vec![0u8; src.0 * src.1 / 4],
            ];
            let pic = Pic {
                planes: [&planes[0], &planes[1], &planes[2]],
                size: src,
            };
            assert_eq!(
                scaled_to_rgb24(pic, dst, YuvMatrix::Bt709, Packed::Mmx).is_some(),
                follows(src, dst),
                "{src:?} -> {dst:?}"
            );
        }
    }

    #[test]
    fn a_scale_that_grows_or_leaves_the_rows_alone_is_not_the_cascade() {
        use kerf_core::layer_geometry::{Rect, ScaleStage};
        let stage = |src: (u32, u32), dst: (u32, u32)| ScaleStage {
            src: Rect::whole(src.0, src.1),
            scaled: dst,
            keep: Rect::whole(dst.0, dst.1),
        };
        assert!(stage((640, 360), (320, 180)).cascade_refusal().is_none());
        assert!(stage((1080, 1920), (170, 302)).cascade_refusal().is_none());
        assert!(
            stage((640, 360), (100, 360)).cascade_refusal().is_some(),
            "luma rows not scaled"
        );
        assert!(stage((640, 360), (960, 540)).cascade_refusal().is_some(), "grows");
        assert!(stage((1080, 1920), (1920, 1080)).cascade_refusal().is_some(), "same area");
        assert!(stage((641, 360), (320, 180)).cascade_refusal().is_some(), "odd source");
        assert!(stage((640, 360), (321, 180)).cascade_refusal().is_some(), "odd result");
        let cropped = ScaleStage {
            keep: Rect {
                x: 2,
                y: 0,
                w: 100,
                h: 180,
            },
            ..stage((640, 360), (320, 180))
        };
        assert!(cropped.cascade_refusal().is_some(), "cropped result");
    }

    #[test]
    fn a_black_picture_is_black_in_every_matrix() {
        let (w, h) = (8, 8);
        let (y, u, v) = (vec![16u8; w * h], vec![128u8; w * h / 4], vec![128u8; w * h / 4]);
        for from in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
            for to in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
                let out = change_matrix([&y, &u, &v], w, h, from, to, Variant::C);
                assert_eq!(out[0], y, "{from:?} -> {to:?}");
                assert_eq!(out[1], u);
                assert_eq!(out[2], v);
            }
        }
    }

    #[test]
    fn a_measured_answer_is_kept_and_an_unknown_one_waits() {
        let state = Mutex::new(State::new());
        let now = Instant::now();
        assert_eq!(cached(&state, now, || None), Kernels::Unknown);
        // Asked again inside the backoff: not measured again.
        assert_eq!(
            cached(&state, now, || panic!("measured inside the backoff")),
            Kernels::Unknown
        );
        assert_eq!(
            cached(&state, now + Duration::from_secs(6), || Some(Kernels::Match)),
            Kernels::Match
        );
        assert_eq!(
            cached(&state, now + Duration::from_secs(7), || panic!("measured twice")),
            Kernels::Match
        );
        assert!(Kernels::Match.converts() && !Kernels::Unknown.converts() && !Kernels::Differs("x").converts());
    }
    /// The scaled probe is kept like the conversion's: a measured answer for good, an unknown one only until the backoff has run, and
    /// a difference is no failure.
    #[test]
    fn a_measured_cascade_is_kept_and_an_unknown_one_waits() {
        let state = Mutex::new(State::<Scaled>::new());
        let now = Instant::now();
        assert_eq!(cached(&state, now, || None), Scaled::Unknown);
        assert_eq!(cached(&state, now, || panic!("measured inside the backoff")), Scaled::Unknown);
        let differs = Scaled::Differs("a cascade of another kind");
        assert_eq!(cached(&state, now + Duration::from_secs(6), || Some(differs)), differs);
        assert_eq!(
            cached(&state, now + Duration::from_secs(7), || panic!("measured twice")),
            differs
        );
        assert!(Scaled::Match.converts() && !differs.converts() && !Scaled::Unknown.converts());
        // (Nothing has been measured in a unit test: the process answers `Unknown`, which the caps leave out.)
        assert!(!scaled_measured().converts());
    }

    #[test]
    fn a_tile_of_three_is_the_probe_picture_in_its_columns() {
        let (w, h) = PROBE;
        // Every sample holds its tile's number: 3w x h luma, then the two 3w/2 x h/2 chroma planes.
        let plane = |pw: usize, ph: usize| -> Vec<u8> { (0..pw * ph).map(|i| ((i % pw) / (pw / 3)) as u8).collect() };
        let mut frame = plane(3 * w, h);
        frame.extend(plane(3 * w / 2, h / 2));
        frame.extend(plane(3 * w / 2, h / 2));
        for k in 0..3 {
            let t = tile(&frame, k);
            assert_eq!(t.len(), w * h * 3 / 2);
            assert!(t.iter().all(|&v| usize::from(v) == k), "tile {k}");
        }
    }
}
