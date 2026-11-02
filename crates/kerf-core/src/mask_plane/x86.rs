//! The mask plane's row kernels on x86_64: SSE2 (two samples at a time) and AVX2 (four).
//!
//! Each does what [`Params::px`] does, with the same `f64` operations in the same order (no fused
//! multiply-add, `max` / `min` / `clamp` on numbers only: `plane` keeps NaNs on the scalar path) and
//! the same truncating conversion, on `N` columns at once. A sample the vector code cannot decide
//! (an ellipse's band around its edge, which needs the libm `hypot`) and the columns left over after
//! the last whole group go through [`Params::px`] itself.

use core::arch::x86_64::*;

use super::Params;

/// Store the first `N` (2 or 4) bytes of `bits`, little endian, at `to`.
///
/// # Safety
/// `to` must be valid for `N` bytes of writes.
#[inline(always)]
unsafe fn store<const N: usize>(to: *mut u8, bits: u32) {
    // SAFETY: the caller owns `N` bytes at `to`; the write is unaligned.
    unsafe {
        if N == 4 {
            to.cast::<u32>().write_unaligned(bits);
        } else {
            to.cast::<u16>().write_unaligned(bits as u16);
        }
    }
}

macro_rules! kernels {
    (
        $rect:ident, $ellipse:ident, $feature:literal, $lanes:expr,
        $set1:ident, $loadu:ident, $add:ident, $sub:ident, $mul:ident, $div:ident, $max:ident, $min:ident,
        $and:ident, $andnot:ident, $or:ident, $le:ident, $lt:ident, $gt:ident, $cvtt:ident, $movemask:ident
    ) => {
        /// A rect's row: `ay` is the row's `|dy|`.
        #[target_feature(enable = $feature)]
        pub(super) fn $rect(p: &Params, ay: f64, dx: &[f64], out: &mut [u8]) {
            const N: usize = $lanes;
            let n = dx.len().min(out.len());
            let (dx, out) = (&dx[..n], &mut out[..n]);
            let (zero, one, c255, sign) = ($set1(0.0), $set1(1.0), $set1(255.0), $set1(-0.0));
            let (ay_v, feather) = ($set1(ay), $set1(p.feather));
            let mut i = 0;
            while i + N <= n {
                // SAFETY: `i + N <= n` and `dx` is `n` long, so the load stays in `dx`.
                let x = unsafe { $loadu(dx.as_ptr().add(i)) };
                let d = $max($andnot(sign, x), ay_v);
                let inside = if p.feathered {
                    $min($max($div($sub(one, d), feather), zero), one)
                } else {
                    $and($le!(d, one), one)
                };
                let keep = if p.inverted { $sub(one, inside) } else { inside };
                let q = $cvtt($mul(keep, c255));
                let bits = _mm_cvtsi128_si32(_mm_packus_epi16(_mm_packs_epi32(q, q), q)) as u32;
                // SAFETY: `i + N <= n` and `out` is `n` long, so the store stays in `out`.
                unsafe { store::<N>(out.as_mut_ptr().add(i), bits) };
                i += N;
            }
            for k in i..n {
                out[k] = p.px(dx[k], 0.0, ay);
            }
        }

        /// An ellipse's row: `dy` is the row's own.
        #[target_feature(enable = $feature)]
        pub(super) fn $ellipse(p: &Params, dy: f64, dx: &[f64], out: &mut [u8]) {
            const N: usize = $lanes;
            let n = dx.len().min(out.len());
            let (dx, out) = (&dx[..n], &mut out[..n]);
            let (dy2, near_in, near_out) = ($set1(dy * dy), $set1(p.near_in), $set1(p.near_out));
            let mut i = 0;
            while i + N <= n {
                // SAFETY: `i + N <= n` and `dx` is `n` long, so the load stays in `dx`.
                let x = unsafe { $loadu(dx.as_ptr().add(i)) };
                let near = $add($mul(x, x), dy2);
                let inner = $lt!(near, near_in);
                let outer = $gt!(near, near_out);
                if $movemask($or(inner, outer)) == (1 << N) - 1 {
                    // SAFETY: `i + N <= n` and `out` is `n` long, so the store stays in `out`.
                    unsafe { store::<N>(out.as_mut_ptr().add(i), p.sure[$movemask(inner) as usize]) };
                } else {
                    for k in i..i + N {
                        out[k] = p.px(dx[k], dy, 0.0);
                    }
                }
                i += N;
            }
            for k in i..n {
                out[k] = p.px(dx[k], dy, 0.0);
            }
        }
    };
}

macro_rules! sse2_le {
    ($a:expr, $b:expr) => {
        _mm_cmple_pd($a, $b)
    };
}
macro_rules! sse2_lt {
    ($a:expr, $b:expr) => {
        _mm_cmplt_pd($a, $b)
    };
}
macro_rules! sse2_gt {
    ($a:expr, $b:expr) => {
        _mm_cmpgt_pd($a, $b)
    };
}
macro_rules! avx2_le {
    ($a:expr, $b:expr) => {
        _mm256_cmp_pd::<_CMP_LE_OQ>($a, $b)
    };
}
macro_rules! avx2_lt {
    ($a:expr, $b:expr) => {
        _mm256_cmp_pd::<_CMP_LT_OQ>($a, $b)
    };
}
macro_rules! avx2_gt {
    ($a:expr, $b:expr) => {
        _mm256_cmp_pd::<_CMP_GT_OQ>($a, $b)
    };
}

kernels!(
    rect_row_sse2,
    ellipse_row_sse2,
    "sse2",
    2,
    _mm_set1_pd,
    _mm_loadu_pd,
    _mm_add_pd,
    _mm_sub_pd,
    _mm_mul_pd,
    _mm_div_pd,
    _mm_max_pd,
    _mm_min_pd,
    _mm_and_pd,
    _mm_andnot_pd,
    _mm_or_pd,
    sse2_le,
    sse2_lt,
    sse2_gt,
    _mm_cvttpd_epi32,
    _mm_movemask_pd
);
kernels!(
    rect_row_avx2,
    ellipse_row_avx2,
    "avx2",
    4,
    _mm256_set1_pd,
    _mm256_loadu_pd,
    _mm256_add_pd,
    _mm256_sub_pd,
    _mm256_mul_pd,
    _mm256_div_pd,
    _mm256_max_pd,
    _mm256_min_pd,
    _mm256_and_pd,
    _mm256_andnot_pd,
    _mm256_or_pd,
    avx2_le,
    avx2_lt,
    avx2_gt,
    _mm256_cvttpd_epi32,
    _mm256_movemask_pd
);
