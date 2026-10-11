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

/// A distance from the edge this far inside (or outside) the shape is decided without the exact
/// expression: the margin dwarfs the `f64` rounding of `hypot` and `max` many times over.
const SURE: f64 = 1e-6;

/// The mask's alpha for every sample of a `w` x `h` picture, row by row: what `geq`'s
/// `a='(keep)*alpha(X,Y)'` writes over an opaque alpha plane. 255 where the clip stays, 0 where it
/// goes, the feather's ramp between them (`inverted` swaps the two).
pub fn mask_alpha_plane(mask: &Mask, w: u32, h: u32) -> Vec<u8> {
    let m = mask.normalized();
    let (wf, hf) = (f64::from(w), f64::from(h));
    let (rw, rh) = (m.width / 2.0, m.height / 2.0);
    // `(X-cx*W)/(rw*W)`: the same operations in the same order, once per column and per row.
    let dx: Vec<f64> = (0..w).map(|x| (f64::from(x) - m.x * wf) / (rw * wf)).collect();
    let dy: Vec<f64> = (0..h).map(|y| (f64::from(y) - m.y * hf) / (rh * hf)).collect();
    let feathered = m.feather > 1e-6;
    // Inside this distance the ramp is 1 (the feather starts `feather` inside the edge).
    let full = ((if feathered { 1.0 - m.feather } else { 1.0 }) - SURE).max(0.0);
    let mut out = vec![0u8; (w as usize) * (h as usize)];
    for (y, row) in out.chunks_exact_mut(w.max(1) as usize).enumerate() {
        let ay = dy[y].abs();
        for (x, px) in row.iter_mut().enumerate() {
            let d = match m.shape {
                MaskShape::Rect => dx[x].abs().max(ay),
                MaskShape::Ellipse => {
                    // Only the band around the edge needs the exact `hypot`: well inside the shape
                    // the ramp is 1 and well outside it is 0, whatever the last bit of `d` is.
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
                // `clip((1-d)/f,0,1)`
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
