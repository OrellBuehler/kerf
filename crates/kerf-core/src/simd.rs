//! Runtime-selected SIMD for the CPU-side per-pixel loops of the preview.
//!
//! Every kernel has a scalar reference that stays in the tree: it is the fallback on a CPU without
//! the instruction set, the oracle the differential tests compare every other path with, and what
//! `KERF_SIMD=scalar` runs. A vector path returns **exactly** what the scalar one returns: these loops
//! feed exact parity with FFmpeg (the title masks `kerf-gpu` blends, the planes `geq` writes), so
//! there is no "close enough" and no fused multiply-add anywhere.
//!
//! [`path`] is the widest instruction set this CPU has (`is_x86_feature_detected!` / `is_aarch64_feature_detected!`,
//! probed once), capped by `KERF_SIMD` (`scalar`, `sse2` / `neon`, `avx2`) and overridden on the
//! current thread by [`with_path`], which the differential tests use to run each [`available`] path.
//!
//! The kernels here are the byte scans: [`position_ne`] / [`rposition_ne`] (the first and last byte
//! that is not a given value) and [`plane_bounds`] (the box a plane's non-zero samples fill) built on
//! them. Measured on x86_64, they run 15 to 65 times as fast as the scalar loops on a 1080p plane.

use std::cell::Cell;
use std::sync::OnceLock;

/// An instruction set a kernel can run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Path {
    /// The reference: plain Rust, always available.
    Scalar,
    /// x86_64 SSE2 (128 bit).
    Sse2,
    /// x86_64 AVX2 (256 bit).
    Avx2,
    /// aarch64 NEON (128 bit).
    Neon,
}

impl Path {
    /// How wide the path is, for `KERF_SIMD`'s cap.
    fn rank(self) -> u8 {
        match self {
            Self::Scalar => 0,
            Self::Sse2 | Self::Neon => 1,
            Self::Avx2 => 2,
        }
    }
}

/// The paths this CPU can run, narrowest first (`Scalar` is always there).
pub fn available() -> Vec<Path> {
    let mut paths = vec![Path::Scalar];
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("sse2") {
            paths.push(Path::Sse2);
        }
        if std::arch::is_x86_feature_detected!("avx2") {
            paths.push(Path::Avx2);
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            paths.push(Path::Neon);
        }
    }
    paths
}

/// The widest of `available` within `cap` (a [`Path::rank`], `None` for no cap).
fn widest(available: &[Path], cap: Option<u8>) -> Path {
    available
        .iter()
        .copied()
        .filter(|p| cap.is_none_or(|c| p.rank() <= c))
        .max_by_key(|p| p.rank())
        .unwrap_or(Path::Scalar)
}

/// `KERF_SIMD`'s cap, case-insensitive and trimmed: `scalar` (also `off`, `none`, `0`) is the scalar path
/// only, `sse2` (also `neon`) caps at the 128-bit paths, `avx2` at 256-bit; anything else is `None`, no cap.
fn parse_cap(value: &str) -> Option<u8> {
    match value.trim().to_ascii_lowercase().as_str() {
        "scalar" | "off" | "none" | "0" => Some(0),
        "sse2" | "neon" => Some(1),
        "avx2" => Some(2),
        _ => None,
    }
}

/// The cap `KERF_SIMD` asks for. A value that is set and not one of [`parse_cap`]'s spellings is no cap,
/// and says so once (this runs once, under [`path`]'s `OnceLock`).
fn env_cap() -> Option<u8> {
    let value = std::env::var("KERF_SIMD").ok().filter(|v| !v.trim().is_empty())?;
    let cap = parse_cap(&value);
    if cap.is_none() {
        tracing::warn!(
            "KERF_SIMD={value:?} is not one of scalar, off, none, 0, sse2, neon, avx2: no cap, the widest SIMD path runs"
        );
    }
    cap
}

thread_local! {
    static FORCED: Cell<Option<Path>> = const { Cell::new(None) };
}

/// The path kernels run on: this thread's [`with_path`], else the widest this CPU has within
/// `KERF_SIMD` (read once).
pub fn path() -> Path {
    static CHOSEN: OnceLock<Path> = OnceLock::new();
    FORCED
        .with(Cell::get)
        .unwrap_or_else(|| *CHOSEN.get_or_init(|| widest(&available(), env_cap())))
}

/// Run `f` with every kernel on this thread taking `path`, for the differential tests. Panics if the
/// CPU cannot run it.
#[doc(hidden)]
pub fn with_path<R>(path: Path, f: impl FnOnce() -> R) -> R {
    assert!(available().contains(&path), "{path:?} cannot run on this CPU");
    struct Restore(Option<Path>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCED.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(FORCED.with(|c| c.replace(Some(path))));
    f()
}

/// The index of the first byte of `hay` that is not `v`.
pub fn position_ne(hay: &[u8], v: u8) -> Option<usize> {
    match path() {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: the path is `Avx2` only where AVX2 was detected.
        Path::Avx2 if hay.len() >= 32 => unsafe { x86::first_ne_avx2(hay, v) },
        #[cfg(target_arch = "x86_64")]
        // SAFETY: the path is `Sse2` or `Avx2` only where SSE2 was detected.
        Path::Sse2 | Path::Avx2 if hay.len() >= 16 => unsafe { x86::first_ne_sse2(hay, v) },
        #[cfg(target_arch = "aarch64")]
        // SAFETY: the path is `Neon` only where NEON was detected.
        Path::Neon if hay.len() >= 16 => unsafe { neon::first_ne(hay, v) },
        _ => position_ne_scalar(hay, v),
    }
}

/// The index of the last byte of `hay` that is not `v`.
pub fn rposition_ne(hay: &[u8], v: u8) -> Option<usize> {
    match path() {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: the path is `Avx2` only where AVX2 was detected.
        Path::Avx2 if hay.len() >= 32 => unsafe { x86::last_ne_avx2(hay, v) },
        #[cfg(target_arch = "x86_64")]
        // SAFETY: the path is `Sse2` or `Avx2` only where SSE2 was detected.
        Path::Sse2 | Path::Avx2 if hay.len() >= 16 => unsafe { x86::last_ne_sse2(hay, v) },
        #[cfg(target_arch = "aarch64")]
        // SAFETY: the path is `Neon` only where NEON was detected.
        Path::Neon if hay.len() >= 16 => unsafe { neon::last_ne(hay, v) },
        _ => rposition_ne_scalar(hay, v),
    }
}

/// [`position_ne`], the reference.
pub fn position_ne_scalar(hay: &[u8], v: u8) -> Option<usize> {
    hay.iter().position(|b| *b != v)
}

/// [`rposition_ne`], the reference.
pub fn rposition_ne_scalar(hay: &[u8], v: u8) -> Option<usize> {
    hay.iter().rposition(|b| *b != v)
}

/// The bounding box `(x0, y0, x1, y1)` (end exclusive) of the non-zero samples of a `w` x `h` plane,
/// `None` if there are none. Panics if `plane` is shorter than `w * h`, or `w * h` overflows.
pub fn plane_bounds(plane: &[u8], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    if path() == Path::Scalar {
        return plane_bounds_scalar(plane, w, h);
    }
    if w == 0 || h == 0 {
        return None;
    }
    let plane = &plane[..w.checked_mul(h).expect("a plane's size overflows usize")];
    // The first and last non-zero samples of the whole plane are in the first and last non-empty
    // rows: they give those rows, and a column each that the rows between can only widen.
    let first = position_ne(plane, 0)?;
    let last = rposition_ne(plane, 0)?;
    let (y0, y1) = (first / w, last / w + 1);
    let (mut x0, mut x1) = (first % w, last % w + 1);
    for row in plane[y0 * w..y1 * w].chunks_exact(w) {
        if x0 == 0 && x1 == w {
            break;
        }
        if let Some(f) = position_ne(&row[..x0], 0) {
            x0 = f;
        }
        if let Some(l) = rposition_ne(&row[x1..], 0) {
            x1 += l + 1;
        }
    }
    Some((x0, y0, x1, y1))
}

/// [`plane_bounds`], the reference: every row's first and last non-zero sample.
pub fn plane_bounds_scalar(plane: &[u8], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let mut b: Option<(usize, usize, usize, usize)> = None;
    for y in 0..h {
        let row = &plane[y * w..(y + 1) * w];
        let (Some(first), Some(last)) = (row.iter().position(|v| *v != 0), row.iter().rposition(|v| *v != 0)) else {
            continue;
        };
        b = Some(match b {
            None => (first, y, last + 1, y + 1),
            Some((x0, y0, x1, _)) => (x0.min(first), y0, x1.max(last + 1), y + 1),
        });
    }
    b
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use core::arch::x86_64::*;

    /// A first/last "byte that is not `v`" pair for one register width. Whole `LANES * UNROLL` blocks
    /// are compared with the equality masks and-ed together (one branch a block), then single
    /// registers; the part that is left is one register that overlaps what was already compared (a
    /// byte there that differs is the first/last one all the same: everything it overlaps is equal).
    macro_rules! scans {
        (
            $first:ident, $last:ident, $feature:literal, $lanes:expr, $full:expr, $unroll:expr,
            $load:ident, $set1:ident, $eq:ident, $and:ident, $mask:ident
        ) => {
            /// Callers check that the CPU has the feature.
            #[target_feature(enable = $feature)]
            pub(super) fn $first(hay: &[u8], v: u8) -> Option<usize> {
                const L: usize = $lanes;
                const U: usize = $unroll;
                const FULL: u32 = $full;
                let n = hay.len();
                if n < L {
                    return super::position_ne_scalar(hay, v);
                }
                let want = $set1(v as i8);
                let mut i = 0;
                while i + L * U <= n {
                    let mut eqs = [want; U];
                    let mut all = $set1(-1);
                    for (k, eq) in eqs.iter_mut().enumerate() {
                        let c = &hay[i + k * L..i + (k + 1) * L];
                        // SAFETY: `c` is `L` bytes, the width of the load.
                        *eq = $eq(unsafe { $load(c.as_ptr().cast()) }, want);
                        all = $and(all, *eq);
                    }
                    if $mask(all) as u32 != FULL {
                        for (k, eq) in eqs.iter().enumerate() {
                            let m = $mask(*eq) as u32;
                            if m != FULL {
                                return Some(i + k * L + (!m & FULL).trailing_zeros() as usize);
                            }
                        }
                    }
                    i += L * U;
                }
                while i + L <= n {
                    let c = &hay[i..i + L];
                    // SAFETY: `c` is `L` bytes, the width of the load.
                    let m = $mask($eq(unsafe { $load(c.as_ptr().cast()) }, want)) as u32;
                    if m != FULL {
                        return Some(i + (!m & FULL).trailing_zeros() as usize);
                    }
                    i += L;
                }
                if i < n {
                    let j = n - L;
                    let c = &hay[j..];
                    // SAFETY: `c` is `L` bytes, the width of the load.
                    let m = $mask($eq(unsafe { $load(c.as_ptr().cast()) }, want)) as u32;
                    if m != FULL {
                        return Some(j + (!m & FULL).trailing_zeros() as usize);
                    }
                }
                None
            }

            /// Callers check that the CPU has the feature.
            #[target_feature(enable = $feature)]
            pub(super) fn $last(hay: &[u8], v: u8) -> Option<usize> {
                const L: usize = $lanes;
                const U: usize = $unroll;
                const FULL: u32 = $full;
                let n = hay.len();
                if n < L {
                    return super::rposition_ne_scalar(hay, v);
                }
                let want = $set1(v as i8);
                let mut end = n;
                while end >= L * U {
                    let mut eqs = [want; U];
                    let mut all = $set1(-1);
                    for (k, eq) in eqs.iter_mut().enumerate() {
                        let c = &hay[end - (k + 1) * L..end - k * L];
                        // SAFETY: `c` is `L` bytes, the width of the load.
                        *eq = $eq(unsafe { $load(c.as_ptr().cast()) }, want);
                        all = $and(all, *eq);
                    }
                    if $mask(all) as u32 != FULL {
                        for (k, eq) in eqs.iter().enumerate() {
                            let m = $mask(*eq) as u32;
                            if m != FULL {
                                return Some(end - (k + 1) * L + 31 - (!m & FULL).leading_zeros() as usize);
                            }
                        }
                    }
                    end -= L * U;
                }
                while end >= L {
                    let c = &hay[end - L..end];
                    // SAFETY: `c` is `L` bytes, the width of the load.
                    let m = $mask($eq(unsafe { $load(c.as_ptr().cast()) }, want)) as u32;
                    if m != FULL {
                        return Some(end - L + 31 - (!m & FULL).leading_zeros() as usize);
                    }
                    end -= L;
                }
                if end > 0 {
                    let c = &hay[..L];
                    // SAFETY: `c` is `L` bytes, the width of the load.
                    let m = $mask($eq(unsafe { $load(c.as_ptr().cast()) }, want)) as u32;
                    if m != FULL {
                        return Some(31 - (!m & FULL).leading_zeros() as usize);
                    }
                }
                None
            }
        };
    }

    scans!(
        first_ne_sse2,
        last_ne_sse2,
        "sse2",
        16,
        0xFFFF,
        4,
        _mm_loadu_si128,
        _mm_set1_epi8,
        _mm_cmpeq_epi8,
        _mm_and_si128,
        _mm_movemask_epi8
    );
    scans!(
        first_ne_avx2,
        last_ne_avx2,
        "avx2",
        32,
        0xFFFF_FFFF,
        2,
        _mm256_loadu_si256,
        _mm256_set1_epi8,
        _mm256_cmpeq_epi8,
        _mm256_and_si256,
        _mm256_movemask_epi8
    );
}

/// [`position_ne`] over chunks of 16 bytes, four to a block, for a vector unit that can say whether a chunk
/// is all `v` (`all_equal`, given 16 bytes) but has no byte mask (NEON): the first block or chunk that
/// is not all `v` is searched with the scalar scan. The part left at the end is the last 16 bytes, which
/// overlap bytes already known to be `v`.
#[cfg(any(target_arch = "aarch64", test))]
fn first_ne_by_chunks(hay: &[u8], v: u8, all_equal: impl Fn(&[u8]) -> bool) -> Option<usize> {
    const L: usize = 16;
    let n = hay.len();
    if n < L {
        return position_ne_scalar(hay, v);
    }
    let mut i = 0;
    while i + 4 * L <= n {
        let c = &hay[i..i + 4 * L];
        if !(all_equal(&c[..L]) && all_equal(&c[L..2 * L]) && all_equal(&c[2 * L..3 * L]) && all_equal(&c[3 * L..])) {
            return position_ne_scalar(c, v).map(|k| i + k);
        }
        i += 4 * L;
    }
    while i + L <= n {
        let c = &hay[i..i + L];
        if !all_equal(c) {
            return position_ne_scalar(c, v).map(|k| i + k);
        }
        i += L;
    }
    if i < n {
        let j = n - L;
        return position_ne_scalar(&hay[j..], v).map(|k| j + k);
    }
    None
}

/// [`first_ne_by_chunks`], from the end.
#[cfg(any(target_arch = "aarch64", test))]
fn last_ne_by_chunks(hay: &[u8], v: u8, all_equal: impl Fn(&[u8]) -> bool) -> Option<usize> {
    const L: usize = 16;
    let n = hay.len();
    if n < L {
        return rposition_ne_scalar(hay, v);
    }
    let mut end = n;
    while end >= 4 * L {
        let c = &hay[end - 4 * L..end];
        if !(all_equal(&c[3 * L..]) && all_equal(&c[2 * L..3 * L]) && all_equal(&c[L..2 * L]) && all_equal(&c[..L])) {
            return rposition_ne_scalar(c, v).map(|k| end - 4 * L + k);
        }
        end -= 4 * L;
    }
    while end >= L {
        let c = &hay[end - L..end];
        if !all_equal(c) {
            return rposition_ne_scalar(c, v).map(|k| end - L + k);
        }
        end -= L;
    }
    if end > 0 {
        return rposition_ne_scalar(&hay[..L], v);
    }
    None
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use core::arch::aarch64::*;

    /// Whether the 16 bytes of `c` are all `v`. NEON is part of every aarch64 target's baseline.
    fn all_equal(c: &[u8], v: u8) -> bool {
        assert_eq!(c.len(), 16);
        // SAFETY: `c` is 16 bytes, the width of the load; NEON is a baseline feature of aarch64.
        unsafe { vminvq_u8(vceqq_u8(vld1q_u8(c.as_ptr()), vdupq_n_u8(v))) == 0xFF }
    }

    /// Callers check that the CPU has NEON.
    #[target_feature(enable = "neon")]
    pub(super) fn first_ne(hay: &[u8], v: u8) -> Option<usize> {
        super::first_ne_by_chunks(hay, v, |c| all_equal(c, v))
    }

    /// Callers check that the CPU has NEON.
    #[target_feature(enable = "neon")]
    pub(super) fn last_ne(hay: &[u8], v: u8) -> Option<usize> {
        super::last_ne_by_chunks(hay, v, |c| all_equal(c, v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// xorshift64*: a repeatable stream, no dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// `n` bytes of `v` with `k` bytes (0 to a few, anywhere, anything) that differ.
    fn sparse(rng: &mut Rng, n: usize, v: u8) -> Vec<u8> {
        let mut d = vec![v; n];
        for _ in 0..rng.below(4) {
            if n > 0 {
                d[rng.below(n)] = rng.next() as u8;
            }
        }
        d
    }

    #[test]
    fn every_path_is_listed_narrowest_first_and_scalar_is_always_there() {
        let paths = available();
        assert_eq!(paths[0], Path::Scalar);
        assert!(paths.windows(2).all(|w| w[0].rank() <= w[1].rank()), "{paths:?}");
        #[cfg(target_arch = "x86_64")]
        assert!(paths.contains(&Path::Sse2));
    }

    #[test]
    fn the_cap_picks_the_widest_path_within_it() {
        let all = [Path::Scalar, Path::Sse2, Path::Avx2];
        assert_eq!(widest(&all, None), Path::Avx2);
        assert_eq!(widest(&all, parse_cap("avx2")), Path::Avx2);
        assert_eq!(widest(&all, parse_cap("SSE2")), Path::Sse2);
        assert_eq!(widest(&all, parse_cap("scalar")), Path::Scalar);
        for spelling in ["off", "none", "0", " Scalar\n"] {
            assert_eq!(widest(&all, parse_cap(spelling)), Path::Scalar, "{spelling:?}");
        }
        assert_eq!(widest(&all, parse_cap("Neon")), Path::Sse2);
        assert_eq!(parse_cap(""), None);
        assert_eq!(widest(&all, parse_cap("banana")), Path::Avx2, "no cap");
        assert_eq!(
            widest(&[Path::Scalar, Path::Sse2], parse_cap("avx2")),
            Path::Sse2,
            "never wider than the CPU"
        );
        assert_eq!(widest(&[Path::Scalar, Path::Neon], parse_cap("sse2")), Path::Neon);
        assert_eq!(widest(&[], None), Path::Scalar);
    }

    #[test]
    fn with_path_forces_a_path_on_this_thread_and_restores_the_last() {
        let before = path();
        with_path(Path::Scalar, || {
            assert_eq!(path(), Path::Scalar);
            for p in available() {
                with_path(p, || assert_eq!(path(), p));
                assert_eq!(path(), Path::Scalar);
            }
        });
        assert_eq!(path(), before);
        let unavailable = [Path::Sse2, Path::Avx2, Path::Neon]
            .into_iter()
            .find(|p| !available().contains(p));
        if let Some(p) = unavailable {
            assert!(std::panic::catch_unwind(|| with_path(p, || ())).is_err());
            assert_eq!(path(), before);
        }
    }

    #[test]
    fn a_forced_path_does_not_leak_to_other_threads() {
        with_path(Path::Scalar, || {
            let other = std::thread::spawn(path).join().unwrap();
            assert_eq!(
                other,
                widest(&available(), std::env::var("KERF_SIMD").ok().as_deref().and_then(parse_cap))
            );
        });
    }

    /// Every width from 1 to 67 (through each register width and tail) and a few large ones, from every
    /// start offset in a block (loads are unaligned), for several fill values, with 0 to 3 odd bytes.
    #[test]
    fn the_scans_find_what_the_scalar_scans_find_on_every_path() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let widths: Vec<usize> = (0..=67)
            .chain([95, 96, 97, 127, 128, 129, 255, 256, 257, 1000, 4099, 65_536 + 13])
            .collect();
        for p in available() {
            with_path(p, || {
                for &n in &widths {
                    for v in [0u8, 255, 7] {
                        for offset in [0usize, 1, 3, 15, 16, 31, 33] {
                            for _ in 0..6 {
                                let buf = sparse(&mut rng, n + offset, v);
                                let hay = &buf[offset..];
                                assert_eq!(
                                    position_ne(hay, v),
                                    position_ne_scalar(hay, v),
                                    "{p:?} n={n} v={v} offset={offset}"
                                );
                                assert_eq!(
                                    rposition_ne(hay, v),
                                    rposition_ne_scalar(hay, v),
                                    "{p:?} n={n} v={v} offset={offset}"
                                );
                            }
                        }
                    }
                }
            });
        }
    }

    #[test]
    fn a_differing_byte_is_found_at_every_position_of_every_width_on_every_path() {
        for p in available() {
            with_path(p, || {
                for n in 1..=70usize {
                    for at in 0..n {
                        let mut d = vec![255u8; n];
                        d[at] = 254;
                        assert_eq!(position_ne(&d, 255), Some(at), "{p:?} n={n}");
                        assert_eq!(rposition_ne(&d, 255), Some(at), "{p:?} n={n}");
                        d[at] = 255;
                        assert_eq!(position_ne(&d, 255), None, "{p:?} n={n}");
                        assert_eq!(rposition_ne(&d, 255), None, "{p:?} n={n}");
                    }
                }
                let big = 100_003;
                for at in [
                    0,
                    1,
                    31,
                    32,
                    63,
                    64,
                    65,
                    127,
                    4096,
                    big - 65,
                    big - 33,
                    big - 32,
                    big - 2,
                    big - 1,
                ] {
                    let mut d = vec![0u8; big];
                    d[at] = 1;
                    assert_eq!(
                        (position_ne(&d, 0), rposition_ne(&d, 0)),
                        (Some(at), Some(at)),
                        "{p:?} at={at}"
                    );
                }
            });
        }
    }

    /// The control flow of the NEON scans (blocks of four chunks, chunks, the overlapping end) with a
    /// portable "is this chunk all `v`", on any CPU: everything of it but the four intrinsics of `neon::all_equal`.
    #[test]
    fn the_chunked_scans_find_what_the_scalar_scans_find() {
        let mut rng = Rng(0xA5A5_1234_5678_9ABC);
        let all_equal = |v: u8| {
            move |c: &[u8]| {
                assert_eq!(c.len(), 16);
                c.iter().all(|b| *b == v)
            }
        };
        for n in (0..=200usize).chain([255, 256, 257, 1000, 4099]) {
            for v in [0u8, 255, 7] {
                for _ in 0..40 {
                    let d = sparse(&mut rng, n, v);
                    assert_eq!(
                        first_ne_by_chunks(&d, v, all_equal(v)),
                        position_ne_scalar(&d, v),
                        "n={n} v={v}"
                    );
                    assert_eq!(
                        last_ne_by_chunks(&d, v, all_equal(v)),
                        rposition_ne_scalar(&d, v),
                        "n={n} v={v}"
                    );
                }
            }
        }
        for n in 1..=70usize {
            for at in 0..n {
                let mut d = vec![9u8; n];
                d[at] = 8;
                assert_eq!(first_ne_by_chunks(&d, 9, all_equal(9)), Some(at), "n={n}");
                assert_eq!(last_ne_by_chunks(&d, 9, all_equal(9)), Some(at), "n={n}");
            }
        }
    }

    #[test]
    fn two_differing_bytes_are_the_first_and_the_last() {
        let mut rng = Rng(77);
        for p in available() {
            with_path(p, || {
                for n in [2usize, 3, 16, 17, 31, 32, 33, 63, 64, 65, 129, 1000, 70_001] {
                    for _ in 0..200 {
                        let (a, b) = (rng.below(n), rng.below(n));
                        let mut d = vec![0u8; n];
                        d[a] = 9;
                        d[b] = 200;
                        assert_eq!(position_ne(&d, 0), Some(a.min(b)), "{p:?} n={n}");
                        assert_eq!(rposition_ne(&d, 0), Some(a.max(b)), "{p:?} n={n}");
                    }
                }
            });
        }
    }

    /// A plane of a few rectangles of non-zero samples (or noise, or nothing, or everything).
    fn plane(rng: &mut Rng, w: usize, h: usize) -> Vec<u8> {
        let mut d = vec![0u8; w * h];
        match rng.below(5) {
            0 => {}
            1 => d.iter_mut().for_each(|v| *v = 1 + rng.below(255) as u8),
            2 => {
                for _ in 0..rng.below(4) {
                    d[rng.below(w * h)] = 1 + rng.below(255) as u8;
                }
            }
            _ => {
                for _ in 0..1 + rng.below(3) {
                    let (x0, y0) = (rng.below(w), rng.below(h));
                    let (x1, y1) = (x0 + 1 + rng.below(w - x0), y0 + 1 + rng.below(h - y0));
                    for y in y0..y1 {
                        for x in x0..x1 {
                            if rng.below(3) != 0 {
                                d[y * w + x] = 1 + rng.below(255) as u8;
                            }
                        }
                    }
                }
            }
        }
        d
    }

    #[test]
    fn the_bounds_of_a_plane_are_the_scalar_bounds_on_every_path() {
        let mut rng = Rng(0xD1B5_4A32_D192_ED03);
        let sizes: Vec<(usize, usize)> = (1..=67)
            .flat_map(|w| [(w, 1), (w, 3), (w, 18)])
            .chain([(1, 67), (2, 67), (1920, 40), (257, 301), (3, 1000)])
            .collect();
        for p in available() {
            with_path(p, || {
                for &(w, h) in &sizes {
                    for _ in 0..8 {
                        let d = plane(&mut rng, w, h);
                        assert_eq!(plane_bounds(&d, w, h), plane_bounds_scalar(&d, w, h), "{p:?} {w}x{h}");
                    }
                }
                assert_eq!(plane_bounds(&[], 0, 0), None);
                assert_eq!(plane_bounds(&[], 0, 5), None);
                assert_eq!(plane_bounds(&[], 5, 0), None);
            });
        }
    }

    #[test]
    fn a_plane_whose_size_overflows_is_a_panic_on_every_path() {
        for p in available() {
            let overflowed = std::panic::catch_unwind(|| with_path(p, || plane_bounds(&[1u8; 8], usize::MAX, 2)));
            assert!(overflowed.is_err(), "{p:?}");
        }
    }

    #[test]
    fn the_bounds_ignore_what_the_plane_holds_past_its_size() {
        let mut d = vec![0u8; 10 * 4 + 7];
        d[10 * 4 + 3] = 1;
        d[12] = 5;
        for p in available() {
            assert_eq!(with_path(p, || plane_bounds(&d, 10, 4)), Some((2, 1, 3, 2)), "{p:?}");
        }
    }
}
