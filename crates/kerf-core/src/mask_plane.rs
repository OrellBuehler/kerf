//! The alpha plane a [`Mask`] cuts a clip to, worked out the way FFmpeg's `geq` works it out.
//!
//! The export (and the still) cut a clip to its mask with one `geq` that rewrites the alpha plane
//! only (`engine::cli::mask_filter`); [`mask_alpha_plane`] is that expression evaluated, pure and
//! shared, so a compositor that draws the clip needs no FFmpeg for it. `geq` evaluates a
//! double-precision expression per sample (`X` and `Y` the sample's own column and row, `W` and `H`
//! the plane's size) and stores the result in the 8-bit plane **truncated** (a C conversion from
//! `double`: `255 * 0.999` is 254, `10.9` is 10), so this is the same arithmetic in `f64` and the
//! same truncation, not a formula for the same picture. Verified against the real filter on both
//! FFmpegs (`engine::cli::rendered`, `a_mask_plane_is_the_geq_planes_sample_for_sample`).
//!
//! Evaluating it per frame is what makes `geq` slow (93 ms of CPU for a 960x540 frame, 400 ms at
//! 1080p); a mask does not move, so a plane is made once per `(mask, size)` and kept.

use crate::model::{Mask, MaskShape};

#[cfg(target_arch = "x86_64")]
mod x86;

/// A distance from the edge this far inside (or outside) the shape is decided without the exact
/// expression: the margin dwarfs the `f64` rounding of `hypot` and `max` many times over.
const SURE: f64 = 1e-6;

/// The mask's alpha for every sample of a `w` x `h` picture, row by row: what `geq`'s
/// `a='(keep)*alpha(X,Y)'` writes over an opaque alpha plane. 255 where the clip stays, 0 where it
/// goes, the feather's ramp between them (`inverted` swaps the two).
///
/// Rows run on the widest SIMD path this CPU has ([`crate::simd`]): an ellipse's the same `f64` operations
/// in the same order on 2 or 4 samples at a time, a rectangle's a byte `min` / `max` of a column's alpha
/// and the row's (`Params::rect_row`), so the plane is [`mask_alpha_plane_scalar`]'s, byte for byte.
pub fn mask_alpha_plane(mask: &Mask, w: u32, h: u32) -> Vec<u8> {
    plane(mask, w, h, kernel())
}

/// [`mask_alpha_plane`] on the scalar path: the reference every other path is held to.
pub fn mask_alpha_plane_scalar(mask: &Mask, w: u32, h: u32) -> Vec<u8> {
    plane(mask, w, h, None)
}

/// A SIMD row kernel.
#[derive(Clone, Copy)]
enum Kernel {
    #[cfg(target_arch = "x86_64")]
    Sse2,
    #[cfg(target_arch = "x86_64")]
    Avx2,
}

fn kernel() -> Option<Kernel> {
    match crate::simd::path() {
        #[cfg(target_arch = "x86_64")]
        crate::simd::Path::Avx2 => Some(Kernel::Avx2),
        #[cfg(target_arch = "x86_64")]
        crate::simd::Path::Sse2 => Some(Kernel::Sse2),
        _ => None,
    }
}

/// What a row of the plane needs besides its own `dx` and `dy`.
struct Params {
    shape: MaskShape,
    inverted: bool,
    feathered: bool,
    feather: f64,
    /// An ellipse sample whose `dx^2 + dy^2` is under this is well inside (the ramp is 1 there)...
    near_in: f64,
    /// ...and over this well outside (0).
    near_out: f64,
    /// The alpha of a sample well inside and well outside.
    a_in: u8,
    a_out: u8,
    /// Per bit set of a lane mask (a lane well inside), the alphas of up to four lanes, little endian.
    #[cfg(target_arch = "x86_64")]
    sure: [u32; 16],
}

impl Params {
    /// One sample: `dx` its column's distance, `dy` its row's, `ay` the row's `|dy|`.
    fn px(&self, dx: f64, dy: f64, ay: f64) -> u8 {
        let d = match self.shape {
            MaskShape::Rect => dx.abs().max(ay),
            MaskShape::Ellipse => {
                // Only the band around the edge needs the exact `hypot`: well inside the shape
                // the ramp is 1 and well outside it is 0, whatever the last bit of `d` is.
                let near = dx * dx + dy * dy;
                if near < self.near_in {
                    return self.a_in;
                }
                if near > self.near_out {
                    return self.a_out;
                }
                dx.hypot(dy)
            }
        };
        let inside = if self.feathered {
            // `clip((1-d)/f,0,1)`
            ((1.0 - d) / self.feather).clamp(0.0, 1.0)
        } else if d <= 1.0 {
            1.0
        } else {
            0.0
        };
        keep_alpha(inside, self.inverted)
    }

    /// One sample's `keep` in `f64`, what `px` truncates after `* 255`: the same operations in the same order, so
    /// `(keep * 255.0) as u8` is `px`'s (held by a test over every shape and feather).
    fn keep(&self, dx: f64, dy: f64, ay: f64) -> f64 {
        let d = match self.shape {
            MaskShape::Rect => dx.abs().max(ay),
            MaskShape::Ellipse => {
                let near = dx * dx + dy * dy;
                if near < self.near_in {
                    return if self.inverted { 0.0 } else { 1.0 };
                }
                if near > self.near_out {
                    return if self.inverted { 1.0 } else { 0.0 };
                }
                dx.hypot(dy)
            }
        };
        let inside = if self.feathered {
            ((1.0 - d) / self.feather).clamp(0.0, 1.0)
        } else if d <= 1.0 {
            1.0
        } else {
            0.0
        };
        if self.inverted {
            1.0 - inside
        } else {
            inside
        }
    }

    fn row_scalar(&self, dy: f64, dx: &[f64], out: &mut [u8]) {
        let ay = dy.abs();
        for (px, dx) in out.iter_mut().zip(dx) {
            *px = self.px(*dx, dy, ay);
        }
    }

    fn row(&self, kernel: Option<Kernel>, dy: f64, dx: &[f64], out: &mut [u8]) {
        match (kernel, self.shape) {
            #[cfg(target_arch = "x86_64")]
            // SAFETY: the kernel is `Avx2` only where `simd::path()` found AVX2.
            (Some(Kernel::Avx2), MaskShape::Ellipse) => unsafe { x86::ellipse_row_avx2(self, dy, dx, out) },
            #[cfg(target_arch = "x86_64")]
            // SAFETY: the kernel is `Sse2` only where `simd::path()` found SSE2.
            (Some(Kernel::Sse2), MaskShape::Ellipse) => unsafe { x86::ellipse_row_sse2(self, dy, dx, out) },
            _ => self.row_scalar(dy, dx, out),
        }
    }

    /// A rectangle's row from `col`, the alpha of each column's offset alone (`px` with a row offset of 0),
    /// and `r`, the alpha of the row's offset alone. A sample's offset is the larger of its two, and the
    /// alpha is a monotone function of the offset (each step of `1 - d`, `/ feather`, `clamp`, `* 255`
    /// and the truncating cast keeps the order, and `inverted` turns it round), so the alpha of the larger
    /// offset is the smaller of the two alphas, or the larger when inverted. Exactly `px`'s, the `f64`
    /// arithmetic done once per column and once per row.
    #[cfg(target_arch = "x86_64")]
    fn rect_row(&self, kernel: Kernel, col: &[u8], r: u8, out: &mut [u8]) {
        match (kernel, self.inverted) {
            // SAFETY: the kernel is `Avx2` only where `simd::path()` found AVX2.
            (Kernel::Avx2, false) => unsafe { x86::min_row_avx2(col, r, out) },
            // SAFETY: as above.
            (Kernel::Avx2, true) => unsafe { x86::max_row_avx2(col, r, out) },
            // SAFETY: the kernel is `Sse2` only where `simd::path()` found SSE2.
            (Kernel::Sse2, false) => unsafe { x86::min_row_sse2(col, r, out) },
            // SAFETY: as above.
            (Kernel::Sse2, true) => unsafe { x86::max_row_sse2(col, r, out) },
        }
    }
}

/// What a plane of `m` needs besides each sample's own offsets, and those offsets: `dx` per column and `dy` per row.
fn params_of(m: &Mask, w: u32, h: u32) -> (Params, Vec<f64>, Vec<f64>) {
    let (wf, hf) = (f64::from(w), f64::from(h));
    let (rw, rh) = (m.width / 2.0, m.height / 2.0);
    // `(X-cx*W)/(rw*W)`: the same operations in the same order, once per column and per row.
    let dx: Vec<f64> = (0..w).map(|x| (f64::from(x) - m.x * wf) / (rw * wf)).collect();
    let dy: Vec<f64> = (0..h).map(|y| (f64::from(y) - m.y * hf) / (rh * hf)).collect();
    let feathered = m.feather > 1e-6;
    // Inside this distance the ramp is 1 (the feather starts `feather` inside the edge).
    let full = ((if feathered { 1.0 - m.feather } else { 1.0 }) - SURE).max(0.0);
    let (a_in, a_out) = (keep_alpha(1.0, m.inverted), keep_alpha(0.0, m.inverted));
    let params = Params {
        shape: m.shape,
        inverted: m.inverted,
        feathered,
        feather: m.feather,
        near_in: full.powi(2),
        near_out: (1.0 + SURE).powi(2),
        a_in,
        a_out,
        #[cfg(target_arch = "x86_64")]
        sure: std::array::from_fn(|bits| {
            u32::from_le_bytes(std::array::from_fn(|k| if bits >> k & 1 == 1 { a_in } else { a_out }))
        }),
    };
    (params, dx, dy)
}

fn plane(mask: &Mask, w: u32, h: u32, kernel: Option<Kernel>) -> Vec<u8> {
    let m = mask.normalized();
    let (params, dx, dy) = params_of(&m, w, h);
    // The vector kernels are written for numbers (`max` and `clamp` of a NaN differ between the two).
    let kernel = kernel.filter(|_| dx.iter().chain(&dy).all(|v| v.is_finite()) && m.feather.is_finite());
    let mut out = vec![0u8; (w as usize) * (h as usize)];
    let rows = out.chunks_exact_mut(w.max(1) as usize).enumerate();
    #[cfg(target_arch = "x86_64")]
    if let (MaskShape::Rect, Some(kernel)) = (params.shape, kernel) {
        // (`px` with a column's offset and none for the row is the alpha at that offset, and the other way round.)
        let col: Vec<u8> = dx.iter().map(|dx| params.px(*dx, 0.0, 0.0)).collect();
        for (y, row) in rows {
            params.rect_row(kernel, &col, params.px(0.0, dy[y], dy[y].abs()), row);
        }
        return out;
    }
    for (y, row) in rows {
        params.row(kernel, dy[y], &dx, row);
    }
    out
}

/// The mask's alpha under a **keyed opacity**: the export writes both in one `geq`, `a='(keep)*(opacity(T))*alpha(X,Y)'`, over an opaque
/// alpha plane (`alpha(X,Y)` is 255.0), which evaluates left to right in `f64` and stores the product truncated:
/// `((keep * opacity) * 255.0) as u8`. Unlike [`mask_alpha_plane`]'s `keep * 255` the product needs the keep itself, not its byte, so
/// every sample is worked out (no SIMD row: a plane is made for the frame, the opacity moves every one). `opacity` is the curve's
/// value in `f64` ([`crate::PlanLayer::keyed_opacity`]); at 1.0 this is [`mask_alpha_plane`], byte for byte.
pub fn mask_alpha_plane_keyed(mask: &Mask, w: u32, h: u32, opacity: f64) -> Vec<u8> {
    let m = mask.normalized();
    let (params, dx, dy) = params_of(&m, w, h);
    let mut out = vec![0u8; (w as usize) * (h as usize)];
    for (row, dy) in out.chunks_exact_mut(w.max(1) as usize).zip(&dy) {
        let ay = dy.abs();
        for (px, dx) in row.iter_mut().zip(&dx) {
            *px = ((params.keep(*dx, *dy, ay) * opacity) * 255.0) as u8;
        }
    }
    out
}

/// `(keep)*alpha(X,Y)` over an opaque plane, stored as `geq` stores it: `keep * 255`, truncated.
fn keep_alpha(inside: f64, inverted: bool) -> u8 {
    let keep = if inverted { 1.0 - inside } else { inside };
    (keep * 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(shape: MaskShape, feather: f64, inverted: bool) -> Mask {
        Mask {
            shape,
            x: 0.5,
            y: 0.5,
            width: 0.5,
            height: 0.5,
            feather,
            inverted,
        }
    }

    /// The keyed plane at an opacity of 1 is the plane `geq` writes with no curve, byte for byte, for every shape, feather and inversion
    /// (a mask off centre, bigger than the frame, thin): `keep * 1.0 * 255.0` is `keep * 255.0`, and `Params::keep` is `px`'s keep.
    #[test]
    fn the_keyed_plane_at_full_opacity_is_the_plain_plane() {
        for shape in [MaskShape::Rect, MaskShape::Ellipse] {
            for feather in [0.0, 1e-7, 0.05, 0.3, 1.0] {
                for inverted in [false, true] {
                    for (x, y, width, height) in [(0.5, 0.5, 0.5, 0.5), (0.31, 0.62, 1.7, 0.4), (0.9, 0.1, 0.05, 2.0)] {
                        let m = Mask {
                            shape,
                            x,
                            y,
                            width,
                            height,
                            feather,
                            inverted,
                        };
                        for (w, h) in [(40, 22), (97, 61), (1, 1), (2, 130)] {
                            assert_eq!(
                                mask_alpha_plane_keyed(&m, w, h, 1.0),
                                mask_alpha_plane_scalar(&m, w, h),
                                "{m:?} at {w}x{h}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Under a curve below 1 the product is `((keep * opacity) * 255.0) as u8` and not one of its other orders: a rectangle's feather
    /// at an opacity where the three orders of the three factors truncate apart (found by search over the 255 grid, the hundredths and
    /// the thousandths; column 129 of a 640x360 frame at `192/255` is 189 in this order and 190 in `keep * (opacity * 255)`, column 134 179 and 180 in either of the others).
    #[test]
    fn the_keyed_plane_multiplies_keep_by_opacity_before_the_255() {
        let m = Mask {
            shape: MaskShape::Rect,
            x: 0.5,
            y: 0.5,
            width: 0.6,
            height: 0.9,
            feather: 0.5,
            inverted: true,
        };
        let opacity = 192.0 / 255.0;
        let plane = mask_alpha_plane_keyed(&m, 640, 360, opacity);
        let at = |x: usize| plane[180 * 640 + x];
        assert_eq!((at(129), at(132), at(134)), (189, 183, 179));
        // A curve at 0 is nothing anywhere, one that holds 1 is the plain plane.
        assert!(mask_alpha_plane_keyed(&m, 64, 36, 0.0).iter().all(|a| *a == 0));
    }

    #[test]
    fn a_hard_rectangle_is_all_or_nothing_and_centred() {
        let p = mask_alpha_plane(&mask(MaskShape::Rect, 0.0, false), 40, 20);
        assert!(p.iter().all(|a| *a == 0 || *a == 255));
        let at = |x: usize, y: usize| p[y * 40 + x];
        assert_eq!((at(20, 10), at(0, 0), at(39, 19)), (255, 0, 0));
        // 0.5 of 40 is 20 wide: columns 10..=30 are within d <= 1 (the edge sample is kept).
        assert_eq!((at(9, 10), at(10, 10), at(30, 10), at(31, 10)), (0, 255, 255, 0));
        assert_eq!((at(20, 4), at(20, 5), at(20, 15), at(20, 16)), (0, 255, 255, 0));
    }

    #[test]
    fn a_feather_ramps_inside_the_edge_and_never_grows_the_shape() {
        let p = mask_alpha_plane(&mask(MaskShape::Rect, 0.5, false), 100, 100);
        let row: Vec<u8> = (0..100).map(|x| p[50 * 100 + x]).collect();
        // Full strength from half the half-size in, zero at and beyond the edge.
        assert_eq!(row[50], 255);
        assert_eq!(row[75], 0, "the edge sample (d = 1) has a ramp value of 0");
        assert!(row[65] > 0 && row[65] < 255);
        assert_eq!(row[60], 255, "a feather of 0.5 starts half-way in");
        assert!(row[..=25].iter().all(|a| *a == 0));
        // Monotonic from the edge to the centre.
        assert!(row[26..=50].windows(2).all(|w| w[0] <= w[1]), "{row:?}");
    }

    #[test]
    fn an_ellipse_is_round_and_inversion_is_its_complement_to_the_truncation() {
        let e = mask(MaskShape::Ellipse, 0.2, false);
        let inv = mask(MaskShape::Ellipse, 0.2, true);
        let (a, b) = (mask_alpha_plane(&e, 64, 48), mask_alpha_plane(&inv, 64, 48));
        assert_eq!(a[24 * 64 + 32], 255);
        assert_eq!(a[0], 0);
        // (1 - keep) * 255 and keep * 255 are each truncated: they sum to 254 or 255.
        for (x, y) in a.iter().zip(&b) {
            let s = u16::from(*x) + u16::from(*y);
            assert!(s == 255 || s == 254, "{x} + {y}");
        }
    }

    #[test]
    fn the_fast_paths_agree_with_the_exact_expression_everywhere() {
        for feather in [0.0, 0.1, 0.5, 1.0] {
            for inverted in [false, true] {
                let m = Mask {
                    shape: MaskShape::Ellipse,
                    x: 0.43,
                    y: 0.57,
                    width: 0.7,
                    height: 0.35,
                    feather,
                    inverted,
                };
                let (w, h) = (97u32, 61u32);
                let fast = mask_alpha_plane(&m, w, h);
                let (rw, rh) = (m.width / 2.0, m.height / 2.0);
                for y in 0..h {
                    for x in 0..w {
                        let dx = (f64::from(x) - m.x * f64::from(w)) / (rw * f64::from(w));
                        let dy = (f64::from(y) - m.y * f64::from(h)) / (rh * f64::from(h));
                        let d = dx.hypot(dy);
                        let inside = if feather > 1e-6 {
                            ((1.0 - d) / feather).clamp(0.0, 1.0)
                        } else if d <= 1.0 {
                            1.0
                        } else {
                            0.0
                        };
                        assert_eq!(
                            fast[(y * w + x) as usize],
                            keep_alpha(inside, inverted),
                            "({x}, {y}) feather {feather} inverted {inverted}"
                        );
                    }
                }
            }
        }
    }

    /// `mask_alpha_plane` as it was before it had vector paths, verbatim: the oracle for the scalar
    /// path since the rows were split out and for every vector path.
    fn reference_plane(mask: &Mask, w: u32, h: u32) -> Vec<u8> {
        let m = mask.normalized();
        let (wf, hf) = (f64::from(w), f64::from(h));
        let (rw, rh) = (m.width / 2.0, m.height / 2.0);
        let dx: Vec<f64> = (0..w).map(|x| (f64::from(x) - m.x * wf) / (rw * wf)).collect();
        let dy: Vec<f64> = (0..h).map(|y| (f64::from(y) - m.y * hf) / (rh * hf)).collect();
        let feathered = m.feather > 1e-6;
        let full = ((if feathered { 1.0 - m.feather } else { 1.0 }) - SURE).max(0.0);
        let mut out = vec![0u8; (w as usize) * (h as usize)];
        for (y, row) in out.chunks_exact_mut(w.max(1) as usize).enumerate() {
            let ay = dy[y].abs();
            for (x, px) in row.iter_mut().enumerate() {
                let d = match m.shape {
                    MaskShape::Rect => dx[x].abs().max(ay),
                    MaskShape::Ellipse => {
                        let near = dx[x] * dx[x] + dy[y] * dy[y];
                        if near < full.powi(2) {
                            *px = keep_alpha(1.0, m.inverted);
                            continue;
                        }
                        if near > (1.0 + SURE).powi(2) {
                            *px = keep_alpha(0.0, m.inverted);
                            continue;
                        }
                        dx[x].hypot(dy[y])
                    }
                };
                let inside = if feathered {
                    ((1.0 - d) / m.feather).clamp(0.0, 1.0)
                } else if d <= 1.0 {
                    1.0
                } else {
                    0.0
                };
                *px = keep_alpha(inside, m.inverted);
            }
        }
        out
    }

    /// A repeatable stream of masks: every shape, feathers either side of the 1e-6 threshold and past 1,
    /// centres and sizes off the frame and out of range (`normalized` clamps them), inverted or not.
    fn masks(n: usize) -> Vec<Mask> {
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move || {
            seed ^= seed >> 12;
            seed ^= seed << 25;
            seed ^= seed >> 27;
            (seed.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
        };
        let feathers = [0.0, 1e-9, 1e-6, 2e-6, 0.05, 0.15, 0.5, 1.0, 1.5];
        (0..n)
            .map(|i| Mask {
                shape: if i % 2 == 0 { MaskShape::Rect } else { MaskShape::Ellipse },
                x: next() * 1.4 - 0.2,
                y: next() * 1.4 - 0.2,
                width: next() * 2.5,
                height: next() * 2.5,
                feather: if i % 3 == 0 {
                    next()
                } else {
                    feathers[(next() * 9.0) as usize % 9]
                },
                inverted: next() < 0.5,
            })
            .collect()
    }

    #[test]
    fn the_scalar_path_is_the_function_it_replaced() {
        for m in masks(40) {
            for (w, h) in [(1, 1), (7, 5), (64, 48), (97, 61)] {
                assert_eq!(mask_alpha_plane_scalar(&m, w, h), reference_plane(&m, w, h), "{m:?} {w}x{h}");
            }
        }
    }

    #[test]
    fn every_path_draws_the_plane_the_scalar_path_draws_at_every_width() {
        let masks = masks(12);
        for p in crate::simd::available() {
            crate::simd::with_path(p, || {
                for w in (1..=67).chain([128, 255, 256, 257]) {
                    for h in [1, 2, 3, 18, 40] {
                        for m in &masks {
                            assert_eq!(mask_alpha_plane(m, w, h), reference_plane(m, w, h), "{p:?} {m:?} {w}x{h}");
                        }
                    }
                }
            });
        }
    }

    #[test]
    fn every_path_draws_the_plane_the_scalar_path_draws_at_the_sizes_of_a_picture() {
        // (The reference is slow unoptimised: 1080p only for the first masks, the rest at 480x270.)
        let masks = masks(24);
        for p in crate::simd::available() {
            crate::simd::with_path(p, || {
                for (i, m) in masks.iter().enumerate() {
                    let big: &[(u32, u32)] = if i < 6 { &[(1920, 1080)] } else { &[] };
                    for &(w, h) in big.iter().chain(&[(480, 270), (401, 7), (3, 600)]) {
                        assert_eq!(mask_alpha_plane(m, w, h), reference_plane(m, w, h), "{p:?} {m:?} {w}x{h}");
                    }
                }
            });
        }
    }

    #[test]
    fn an_empty_picture_is_an_empty_plane_on_every_path() {
        for p in crate::simd::available() {
            crate::simd::with_path(p, || {
                assert!(mask_alpha_plane(&Mask::default(), 0, 10).is_empty());
                assert!(mask_alpha_plane(&Mask::default(), 10, 0).is_empty());
            });
        }
    }

    #[test]
    fn the_sizes_are_normalised_like_the_graph_does() {
        // A zero-sized mask would blank the clip; `normalized` gives it a visible minimum.
        let m = Mask {
            width: 0.0,
            height: 0.0,
            ..Mask::default()
        };
        assert!(mask_alpha_plane(&m, 200, 200).iter().any(|a| *a > 0));
    }
}
