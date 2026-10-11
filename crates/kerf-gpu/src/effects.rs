//! Ports of the FFmpeg filters behind Kerf's video effects, as scalar references and the
//! parameters the shaders take (`shaders/effects.wgsl`).
//!
//! * **blur** is `gblur` (`vf_gblur.c`): a first-order recursive (IIR) Gaussian, run rightwards then
//!   leftwards along every row and downwards then upwards along every column, on each plane in
//!   `float`, then scaled and rounded with `lrintf`. [`Gaussian`] holds the three numbers
//!   `set_params` derives from the sigma, [`blur_plane`] is the filter on one plane. The result
//!   is FFmpeg's to a level on a handful of samples, not to the bit: the x86 build runs the
//!   horizontal pass in AVX2 / AVX-512 with another association of the same sums (and a fused
//!   multiply-add), so FFmpeg's own output depends on the CPU it runs on by the last bits of a
//!   `float`, which `lrintf` turns into a level where a value sits on a half.
//! * **sharpen** is `unsharp` with a 5x5 matrix on luma (`vf_unsharp.c`): integer arithmetic, a
//!   binomial blur (`[1 4 6 4 1]` in each direction, summed exactly) and `s + (((s - blur) * amount)
//!   >> 16)`. [`unsharp_amount`] is the integer `amount` the filter derives from the option;
//!   [`sharpen_plane`] is the filter on one plane. Exact, on every build.
//! * **grayscale** is `hue=s=0`: U and V to 128, luma untouched; **invert** is `negate`: `255 - v` on
//!   all three planes. Both are tables ([`GRAYSCALE`], [`INVERT`]), composed with `eq`'s.
//!
//! None of it is on the alpha plane (FFmpeg runs these ahead of `format=yuva420p`).

/// `gblur`'s numbers for one sigma (the same in both directions: Kerf never sets `sigmaV`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gaussian {
    /// The recursion's coefficient.
    pub nu: f32,
    /// The edge samples' extra weight (`boundaryscale`).
    pub boundary: f32,
    /// The gain applied once at the end: the horizontal and the vertical `postscale`, multiplied.
    pub post: f32,
}

/// `set_params` of `vf_gblur.c`, in the filter's own precision: `sigma` is an option of type
/// `float`; `lambda` and `dnu` are computed in `double`; the three results are stored as `float`
/// and any that is not a normal number (zero, subnormal, infinite, NaN) is replaced
/// (`postscale` and `boundaryscale` by 1, `nu` by 0). `None` for a sigma `gblur` refuses.
pub fn gaussian(sigma: f64) -> Option<Gaussian> {
    if !sigma.is_finite() || !(0.0..=1024.0).contains(&sigma) {
        return None;
    }
    let sigma = sigma as f32;
    let steps = 1.0f64;
    let lambda = f64::from(sigma * sigma) / (2.0 * steps);
    let dnu = (1.0 + 2.0 * lambda - (1.0 + 4.0 * lambda).sqrt()) / (2.0 * lambda);
    let mut post = (dnu / lambda).powf(steps) as f32;
    let mut boundary = (1.0 / (1.0 - dnu)) as f32;
    let mut nu = dnu as f32;
    if !post.is_normal() {
        post = 1.0;
    }
    if !boundary.is_normal() {
        boundary = 1.0;
    }
    if !nu.is_normal() {
        nu = 0.0;
    }
    // (`filter_postscale` multiplies the horizontal and the vertical factor, two floats.)
    Some(Gaussian {
        nu,
        boundary,
        post: post * post,
    })
}

/// `gblur` on one 8-bit plane: the recursion in `float`, in `horiz_slice_c`'s and
/// `do_vertical_columns`' order, then `postscale`, clip to 0..255, `lrintf`.
pub fn blur_plane(src: &[u8], w: usize, h: usize, g: &Gaussian) -> Vec<u8> {
    assert_eq!(src.len(), w * h);
    let mut buf: Vec<f32> = src.iter().map(|v| f32::from(*v)).collect();
    for row in buf.chunks_exact_mut(w) {
        row[0] *= g.boundary;
        for x in 1..w {
            row[x] += g.nu * row[x - 1];
        }
        row[w - 1] *= g.boundary;
        for x in (1..w).rev() {
            row[x - 1] += g.nu * row[x];
        }
    }
    for x in 0..w {
        buf[x] *= g.boundary;
        for y in 1..h {
            buf[y * w + x] += g.nu * buf[(y - 1) * w + x];
        }
        buf[(h - 1) * w + x] *= g.boundary;
        for y in (1..h).rev() {
            buf[(y - 1) * w + x] += g.nu * buf[y * w + x];
        }
    }
    buf.iter()
        .map(|v| (v * g.post).clamp(0.0, 255.0).round_ties_even() as u8)
        .collect()
}

/// The integer `amount` `unsharp` derives from `luma_amount` (`set_filter_param`: the option is a
/// `float`, scaled by 65536 and truncated), or `None` for one the filter's option refuses (it takes -2
/// to 5).
pub fn unsharp_amount(amount: f64) -> Option<i32> {
    if !amount.is_finite() || !(-2.0..=5.0).contains(&amount) {
        return None;
    }
    Some((f64::from(amount as f32) * 65536.0) as i32)
}

/// `unsharp` with a 5x5 matrix on one 8-bit plane: the binomial blur (the filter's two delay-line
/// stages per direction sum to `[1 4 6 4 1]`, 256 in all, edges replicated), then
/// `s + (((s - ((blur + 128) >> 8)) * amount) >> 16)`, clipped.
pub fn sharpen_plane(src: &[u8], w: usize, h: usize, amount: i32) -> Vec<u8> {
    assert_eq!(src.len(), w * h);
    const K: [i32; 5] = [1, 4, 6, 4, 1];
    let at = |x: i64, y: i64| i32::from(src[y.clamp(0, h as i64 - 1) as usize * w + x.clamp(0, w as i64 - 1) as usize]);
    let mut out = Vec::with_capacity(src.len());
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let mut sum = 0;
            for (j, ky) in K.iter().enumerate() {
                for (i, kx) in K.iter().enumerate() {
                    sum += ky * kx * at(x + i as i64 - 2, y + j as i64 - 2);
                }
            }
            let s = at(x, y);
            let res = s + (((s - ((sum + 128) >> 8)) * amount) >> 16);
            out.push(res.clamp(0, 255) as u8);
        }
    }
    out
}

/// Per-plane tables (Y, U, V): what `hue=s=0` does (`vf_hue.c`'s chroma table at zero saturation is
/// `(0 + 0 + (1 << 15) + (128 << 16)) >> 16`).
pub const GRAYSCALE: [fn(u8) -> u8; 3] = [|v| v, |_| 128, |_| 128];

/// ... and `negate`'s `255 - v` (`negate8`), on all three planes.
pub const INVERT: [fn(u8) -> u8; 3] = [|v| 255 - v; 3];

/// The tables of [`GRAYSCALE`] / [`INVERT`] as rows of 256.
pub fn table(planes: [fn(u8) -> u8; 3]) -> [[u8; 256]; 3] {
    std::array::from_fn(|p| std::array::from_fn(|i| planes[p](i as u8)))
}

/// `next` applied after `first`, plane by plane.
pub fn then(first: &[[u8; 256]; 3], next: &[[u8; 256]; 3]) -> [[u8; 256]; 3] {
    std::array::from_fn(|p| std::array::from_fn(|i| next[p][usize::from(first[p][i])]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_plane_stays_flat_under_a_blur() {
        for sigma in [0.5, 2.0, 6.0, 25.0] {
            let g = gaussian(sigma).unwrap();
            for level in [0u8, 16, 128, 235, 255] {
                let out = blur_plane(&vec![level; 37 * 23], 37, 23, &g);
                assert!(out.iter().all(|v| *v == level), "sigma {sigma}, level {level}");
            }
        }
    }

    #[test]
    fn a_blur_spreads_an_impulse_symmetrically_and_conserves_the_mass() {
        let g = gaussian(3.0).unwrap();
        let (w, h) = (41, 41);
        let mut plane = vec![0u8; w * h];
        plane[20 * w + 20] = 255;
        let out = blur_plane(&plane, w, h, &g);
        let at = |x: usize, y: usize| i32::from(out[y * w + x]);
        assert_eq!((at(17, 20), at(23, 20)), (at(23, 20), at(17, 20)));
        assert!(at(20, 20) > at(21, 20) && at(21, 20) > at(23, 20));
        let sum: i32 = out.iter().map(|v| i32::from(*v)).sum();
        assert!((sum - 255).abs() <= 12, "the mass is 255, got {sum}");
    }

    #[test]
    fn sigma_zero_is_the_identity_and_out_of_range_is_refused() {
        let g = gaussian(0.0).unwrap();
        assert_eq!((g.nu, g.boundary, g.post), (0.0, 1.0, 1.0));
        let plane: Vec<u8> = (0..30).map(|i| (i * 8) as u8).collect();
        assert_eq!(blur_plane(&plane, 6, 5, &g), plane);
        assert!(gaussian(-1.0).is_none() && gaussian(1025.0).is_none() && gaussian(f64::NAN).is_none());
    }

    #[test]
    fn the_amount_is_the_floats_truncated_product() {
        assert_eq!(unsharp_amount(1.0), Some(65536));
        // 0.7f32 is 0.699999988, times 65536 is 45875.19.
        assert_eq!(unsharp_amount(0.7), Some(45875));
        assert_eq!(unsharp_amount(-0.7), Some(-45875));
        assert_eq!(unsharp_amount(0.0), Some(0));
        assert!(unsharp_amount(5.1).is_none() && unsharp_amount(-2.1).is_none());
    }

    #[test]
    fn sharpening_leaves_a_flat_plane_and_raises_an_edge() {
        let flat = vec![100u8; 20 * 10];
        assert_eq!(sharpen_plane(&flat, 20, 10, 65536), flat);
        let mut edge = vec![50u8; 20 * 10];
        for y in 0..10 {
            for x in 10..20 {
                edge[y * 20 + x] = 150;
            }
        }
        let out = sharpen_plane(&edge, 20, 10, 65536);
        assert!(
            out[5 * 20 + 9] < 50 && out[5 * 20 + 10] > 150,
            "overshoot either side of the edge"
        );
        assert_eq!(out[5 * 20 + 2], 50);
        // Amount 0 copies; a negative amount blurs.
        assert_eq!(sharpen_plane(&edge, 20, 10, 0), edge);
        let soft = sharpen_plane(&edge, 20, 10, -65536);
        assert!(soft[5 * 20 + 9] > 50 && soft[5 * 20 + 10] < 150);
    }

    #[test]
    fn the_tables_compose_in_order() {
        let g = table(GRAYSCALE);
        let n = table(INVERT);
        assert_eq!((g[0][77], g[1][77], g[2][200]), (77, 128, 128));
        assert_eq!((n[0][0], n[1][255], n[2][100]), (255, 0, 155));
        // Gray, then invert: luma negated, chroma at 255 - 128.
        let both = then(&g, &n);
        assert_eq!((both[0][10], both[1][10], both[2][10]), (245, 127, 127));
        let other = then(&n, &g);
        assert_eq!((other[0][10], other[1][10], other[2][10]), (245, 128, 128));
    }
}
