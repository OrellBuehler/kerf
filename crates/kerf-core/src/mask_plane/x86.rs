//! The mask plane's row kernels on x86_64: SSE2 (two samples at a time) and AVX2 (four).
//!
//! An ellipse's row does what [`Params::px`] does, with the same `f64` operations in the same order (no
//! fused multiply-add) on `N` columns at once: a sample the vector code cannot decide (the band around the
//! edge, which needs the libm `hypot`) and the columns left over after the last whole group go through
//! [`Params::px`] itself. A rectangle's row is a byte `min` or `max` of a column's ramp and the row's
//! (`min_row`, `max_row`: the ramp of the larger offset is the smaller of the two ramps, see [`Params::rect_row`]).

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

macro_rules! ellipse_rows {
    (
        $ellipse:ident, $feature:literal, $lanes:expr,
        $set1:ident, $loadu:ident, $add:ident, $mul:ident, $or:ident, $lt:ident, $gt:ident, $movemask:ident
    ) => {
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

macro_rules! byte_rows {
    (
        $row:ident, $feature:literal, $lanes:expr,
        $loadu:ident, $storeu:ident, $set1:ident, $op:ident, $tail:path
    ) => {
        /// `out[x] = op(col[x], r)` for every column.
        #[target_feature(enable = $feature)]
        pub(super) fn $row(col: &[u8], r: u8, out: &mut [u8]) {
            const N: usize = $lanes;
            let n = col.len().min(out.len());
            let (col, out) = (&col[..n], &mut out[..n]);
            let r_v = $set1(r as i8);
            let mut i = 0;
            while i + N <= n {
                // SAFETY: `i + N <= n` and `col` is `n` long, so the load stays in `col`.
                let c = unsafe { $loadu(col.as_ptr().add(i).cast()) };
                // SAFETY: `i + N <= n` and `out` is `n` long, so the store stays in `out`.
                unsafe { $storeu(out.as_mut_ptr().add(i).cast(), $op(c, r_v)) };
                i += N;
            }
            for k in i..n {
                out[k] = $tail(col[k], r);
            }
        }
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

ellipse_rows!(
    ellipse_row_sse2,
    "sse2",
    2,
    _mm_set1_pd,
    _mm_loadu_pd,
    _mm_add_pd,
    _mm_mul_pd,
    _mm_or_pd,
    sse2_lt,
    sse2_gt,
    _mm_movemask_pd
);
ellipse_rows!(
    ellipse_row_avx2,
    "avx2",
    4,
    _mm256_set1_pd,
    _mm256_loadu_pd,
    _mm256_add_pd,
    _mm256_mul_pd,
    _mm256_or_pd,
    avx2_lt,
    avx2_gt,
    _mm256_movemask_pd
);

byte_rows!(
    min_row_sse2,
    "sse2",
    16,
    _mm_loadu_si128,
    _mm_storeu_si128,
    _mm_set1_epi8,
    _mm_min_epu8,
    u8::min
);
byte_rows!(
    max_row_sse2,
    "sse2",
    16,
    _mm_loadu_si128,
    _mm_storeu_si128,
    _mm_set1_epi8,
    _mm_max_epu8,
    u8::max
);
byte_rows!(
    min_row_avx2,
    "avx2",
    32,
    _mm256_loadu_si256,
    _mm256_storeu_si256,
    _mm256_set1_epi8,
    _mm256_min_epu8,
    u8::min
);
byte_rows!(
    max_row_avx2,
    "avx2",
    32,
    _mm256_loadu_si256,
    _mm256_storeu_si256,
    _mm256_set1_epi8,
    _mm256_max_epu8,
    u8::max
);
