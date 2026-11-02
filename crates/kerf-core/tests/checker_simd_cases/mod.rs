//! The cases of `checker_simd.rs` and `checker_simd_fence.rs`, written against the public API only and
//! against oracles of their own (a naive loop, and the `geq` expression evaluated sample by sample with the
//! exact `hypot` everywhere), not the functions under test.

#![allow(dead_code)]

use kerf_core::mask_plane::mask_alpha_plane_scalar;
use kerf_core::model::{Mask, MaskShape};
use kerf_core::simd::{self, Path};

/// xorshift64*.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

pub fn naive_first(h: &[u8], v: u8) -> Option<usize> {
    let mut i = 0;
    while i < h.len() {
        if h[i] != v {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub fn naive_last(h: &[u8], v: u8) -> Option<usize> {
    let mut i = h.len();
    while i > 0 {
        i -= 1;
        if h[i] != v {
            return Some(i);
        }
    }
    None
}

pub fn naive_bounds(plane: &[u8], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    let mut any = false;
    for y in 0..h {
        for x in 0..w {
            if plane[y * w + x] != 0 {
                any = true;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    any.then_some((x0, y0, x1, y1))
}

/// Every kernel on `hay`, against the naive loop and the kept scalar functions.
#[track_caller]
pub fn check_scan(p: Path, hay: &[u8], v: u8) {
    let (f, l) = (naive_first(hay, v), naive_last(hay, v));
    assert_eq!(simd::position_ne(hay, v), f, "position_ne {p:?} len {} v {v}", hay.len());
    assert_eq!(simd::rposition_ne(hay, v), l, "rposition_ne {p:?} len {} v {v}", hay.len());
    assert_eq!(
        simd::position_ne_scalar(hay, v),
        f,
        "position_ne_scalar len {} v {v}",
        hay.len()
    );
    assert_eq!(
        simd::rposition_ne_scalar(hay, v),
        l,
        "rposition_ne_scalar len {} v {v}",
        hay.len()
    );
}

pub const NEEDLES: [u8; 6] = [0, 1, 0x7F, 0x80, 0xFE, 0xFF];

/// The bytes that stand in for "one that differs": a flipped low bit, a flipped sign bit, every bit flipped.
pub fn others(v: u8) -> [u8; 3] {
    [v ^ 1, v ^ 0x80, !v]
}

/// Every length `0..=max_len` as its own allocation (so that a fenced allocator puts the end of the slice
/// against a guard page), a differing byte at every position, then the same with a run of them, then none.
pub fn scan_every_length_every_position(max_len: usize) {
    for p in simd::available() {
        simd::with_path(p, || {
            for v in NEEDLES {
                for n in 0..=max_len {
                    let mut buf = vec![v; n];
                    check_scan(p, &buf, v);
                    for at in 0..n {
                        for o in others(v) {
                            buf[at] = o;
                            assert_eq!(simd::position_ne(&buf, v), Some(at), "first {p:?} n {n} at {at} v {v} o {o}");
                            assert_eq!(simd::rposition_ne(&buf, v), Some(at), "last {p:?} n {n} at {at} v {v} o {o}");
                            assert_eq!(simd::position_ne_scalar(&buf, v), Some(at));
                            assert_eq!(simd::rposition_ne_scalar(&buf, v), Some(at));
                        }
                        buf[at] = v;
                    }
                    let all: Vec<u8> = (0..n).map(|i| others(v)[i % 3]).collect();
                    check_scan(p, &all, v);
                    if n > 0 {
                        assert_eq!(simd::position_ne(&all, v), Some(0));
                        assert_eq!(simd::rposition_ne(&all, v), Some(n - 1));
                    }
                }
            }
        });
    }
}

/// A slice at every offset 0..64 of a bigger buffer whose bytes outside the slice all differ from the needle
/// (a read past either end of the slice finds one and answers outside it), lengths 0..=max_len, a differing byte
/// at the first, second, last, next-to-last, middle and some random places, then none.
pub fn scan_misaligned_sub_slices(max_len: usize) {
    let mut rng = Rng(0xC0FF_EE00_1234_5678);
    for p in simd::available() {
        simd::with_path(p, || {
            for v in [0u8, 0xFF, 0x80, 0x37] {
                for off in 0..64usize {
                    for n in 0..=max_len {
                        let mut buf = vec![!v; 64 + 8 + off + n + 80];
                        let start = 8 + off;
                        buf[start..start + n].fill(v);
                        {
                            let s = &buf[start..start + n];
                            check_scan(p, s, v);
                        }
                        if n == 0 {
                            continue;
                        }
                        let mut spots = vec![0, n / 2, n - 1, n.saturating_sub(2), 1.min(n - 1)];
                        for _ in 0..3 {
                            spots.push(rng.below(n));
                        }
                        for at in spots {
                            for o in others(v) {
                                buf[start + at] = o;
                                let s = &buf[start..start + n];
                                assert_eq!(simd::position_ne(s, v), Some(at), "{p:?} off {off} n {n} at {at} v {v}");
                                assert_eq!(simd::rposition_ne(s, v), Some(at), "{p:?} off {off} n {n} at {at} v {v}");
                            }
                            buf[start + at] = v;
                        }
                    }
                }
            }
        });
    }
}

/// Random runs of differing bytes at random places of random lengths, the answers from the naive loop.
pub fn scan_random(iterations: usize, max_len: usize) {
    let mut rng = Rng(0x1357_9BDF_2468_ACE0);
    for p in simd::available() {
        simd::with_path(p, || {
            for _ in 0..iterations {
                let n = rng.below(max_len + 1);
                let off = rng.below(64);
                let v = [0u8, 255, 0x80, 0x7F, rng.next() as u8][rng.below(5)];
                let mut buf = vec![v; off + n + 64];
                for b in &mut buf[..off] {
                    *b = rng.next() as u8;
                }
                for _ in 0..rng.below(6) {
                    if n > 0 {
                        let at = rng.below(n);
                        buf[off + at] = rng.next() as u8;
                    }
                }
                if rng.below(4) == 0 && n > 0 {
                    let (a, b) = (rng.below(n), rng.below(n));
                    let (a, b) = (a.min(b), a.max(b));
                    for x in &mut buf[off + a..off + b] {
                        *x = rng.next() as u8;
                    }
                }
                check_scan(p, &buf[off..off + n], v);
            }
        });
    }
}

/// Every byte value as the one that differs, at the places a block starts and ends.
pub fn scan_every_value_as_the_odd_one() {
    for p in simd::available() {
        simd::with_path(p, || {
            for v in [0u8, 0x80, 0xFF, 0x41] {
                for n in [1usize, 16, 17, 31, 32, 33, 47, 48, 64, 65, 100, 128, 129] {
                    for at in [0, 1, 15, 16, 31, 32, 63, 64, 65, 127, 128].into_iter().filter(|a| *a < n) {
                        for odd in 0..=255u8 {
                            let mut buf = vec![v; n];
                            buf[at] = odd;
                            check_scan(p, &buf, v);
                        }
                    }
                }
            }
        });
    }
}

/// A big buffer: one differing byte at places around every block edge, then two, then none.
pub fn scan_big(len: usize, pairs: bool) {
    for p in simd::available() {
        simd::with_path(p, || {
            for v in [0u8, 255] {
                let mut buf = vec![v; len];
                assert_eq!((simd::position_ne(&buf, v), simd::rposition_ne(&buf, v)), (None, None), "{p:?}");
                let mut spots: Vec<usize> = vec![0, 1, 2, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 4095, 4096, 4097];
                spots.extend([
                    len - 1,
                    len - 2,
                    len - 16,
                    len - 17,
                    len - 32,
                    len - 33,
                    len - 64,
                    len - 65,
                    len - 128,
                    len / 2,
                    len / 2 + 1,
                ]);
                for (i, &a) in spots.iter().enumerate() {
                    buf[a] = !v;
                    assert_eq!(simd::position_ne(&buf, v), Some(a), "{p:?} len {len} at {a}");
                    assert_eq!(simd::rposition_ne(&buf, v), Some(a), "{p:?} len {len} at {a}");
                    if i % 7 == 0 {
                        assert_eq!(simd::position_ne_scalar(&buf, v), Some(a));
                        assert_eq!(simd::rposition_ne_scalar(&buf, v), Some(a));
                    }
                    for &b in spots.iter().filter(|_| pairs) {
                        if b == a {
                            continue;
                        }
                        buf[b] = !v ^ 0x10;
                        assert_eq!(simd::position_ne(&buf, v), Some(a.min(b)), "{p:?} len {len} at {a},{b}");
                        assert_eq!(simd::rposition_ne(&buf, v), Some(a.max(b)), "{p:?} len {len} at {a},{b}");
                        buf[b] = v;
                    }
                    buf[a] = v;
                }
            }
        });
    }
}

#[track_caller]
pub fn check_bounds(p: Path, plane: &[u8], w: usize, h: usize) {
    let want = naive_bounds(plane, w, h);
    assert_eq!(simd::plane_bounds(plane, w, h), want, "plane_bounds {p:?} {w}x{h}");
    assert_eq!(simd::plane_bounds_scalar(plane, w, h), want, "plane_bounds_scalar {w}x{h}");
}

/// Every one-pixel plane of every small size, every pair of pixels, each at some offset of a bigger buffer whose
/// bytes outside the plane are all non-zero.
pub fn bounds_single_pixels_and_pairs() {
    let mut rng = Rng(0xBAD5_EED5);
    for p in simd::available() {
        simd::with_path(p, || {
            let mut sizes: Vec<(usize, usize)> = Vec::new();
            for w in 1..=9 {
                for h in 1..=9 {
                    sizes.push((w, h));
                }
            }
            for w in [15, 16, 17, 31, 32, 33, 47, 48, 49, 63, 64, 65] {
                for h in [1, 2, 3, 5] {
                    sizes.push((w, h));
                }
            }
            for (w, h) in sizes {
                let n = w * h;
                let off = rng.below(64);
                let mut buf = vec![0xAAu8; 8 + off + n + 80];
                let start = 8 + off;
                for a in 0..n {
                    buf[start..start + n].fill(0);
                    buf[start + a] = 1 + rng.below(255) as u8;
                    let plane = &buf[start..start + n];
                    let (x, y) = (a % w, a / w);
                    assert_eq!(
                        simd::plane_bounds(plane, w, h),
                        Some((x, y, x + 1, y + 1)),
                        "{p:?} {w}x{h} at {a}"
                    );
                    assert_eq!(simd::plane_bounds_scalar(plane, w, h), Some((x, y, x + 1, y + 1)));
                    let pairs: Vec<usize> = if n <= 45 {
                        (a + 1..n).collect()
                    } else {
                        (0..6).map(|_| rng.below(n)).collect()
                    };
                    for b in pairs {
                        if b == a {
                            continue;
                        }
                        let keep = buf[start + b];
                        buf[start + b] = 255;
                        check_bounds(p, &buf[start..start + n], w, h);
                        buf[start + b] = keep;
                    }
                }
            }
        });
    }
}

/// Planes of noise at several densities, rectangles, lines, borders, diagonals, full and empty ones, at widths
/// around every register width and at the sizes of a picture, as sub-slices of a buffer that is non-zero outside.
pub fn bounds_patterns(big: bool) {
    let mut rng = Rng(0x7777_1234_ABCD_0001);
    let mut sizes: Vec<(usize, usize)> = (1..=70).flat_map(|w| [(w, 1), (w, 2), (w, 5), (w, 19)]).collect();
    sizes.extend([(1, 70), (2, 70), (3, 129), (255, 3), (256, 3), (257, 3), (1000, 9), (1920, 4)]);
    if big {
        sizes.extend([(1920, 1080), (3840, 2160), (4097, 31), (8191, 7)]);
    }
    for p in simd::available() {
        simd::with_path(p, || {
            for &(w, h) in &sizes {
                let n = w * h;
                let reps = if n > 100_000 { 2 } else { 12 };
                for _ in 0..reps {
                    let off = rng.below(64);
                    let mut buf = vec![0xEEu8; off + n + 70];
                    let plane = &mut buf[off..off + n];
                    plane.fill(0);
                    match rng.below(8) {
                        0 => {}
                        1 => plane.fill(1 + rng.below(255) as u8),
                        2 => {
                            let k = [1, 2, 8, 64, 4096][rng.below(5)];
                            for v in plane.iter_mut() {
                                if rng.below(k) == 0 {
                                    *v = 1 + rng.below(255) as u8;
                                }
                            }
                        }
                        3 => {
                            let (x0, y0) = (rng.below(w), rng.below(h));
                            let (x1, y1) = (x0 + 1 + rng.below(w - x0), y0 + 1 + rng.below(h - y0));
                            for y in y0..y1 {
                                plane[y * w + x0..y * w + x1].fill(255);
                            }
                        }
                        4 => {
                            let y = rng.below(h);
                            let (a, b) = (rng.below(w), rng.below(w));
                            plane[y * w + a.min(b)..=y * w + a.max(b)].fill(9);
                        }
                        5 => {
                            let x = rng.below(w);
                            for y in 0..h {
                                plane[y * w + x] = 3;
                            }
                        }
                        6 => {
                            for y in 0..h {
                                plane[y * w] = 1;
                                plane[y * w + w - 1] = 2;
                            }
                        }
                        _ => {
                            for i in 0..h.min(w) {
                                plane[i * w + i] = 200;
                            }
                        }
                    }
                    let plane = &buf[off..off + n];
                    assert_eq!(
                        simd::plane_bounds(plane, w, h),
                        simd::plane_bounds_scalar(plane, w, h),
                        "{p:?} {w}x{h}"
                    );
                    if n <= 100_000 {
                        check_bounds(p, plane, w, h);
                    }
                }
            }
        });
    }
}

/// What a plane holds past `w * h` is not read; a plane shorter than `w * h` is a panic on every path.
pub fn bounds_ignore_the_tail_and_refuse_a_short_plane() {
    for p in simd::available() {
        simd::with_path(p, || {
            let mut d = vec![0u8; 5 * 4 + 40];
            d[5 * 4..].fill(1);
            d[7] = 4;
            assert_eq!(simd::plane_bounds(&d, 5, 4), Some((2, 1, 3, 2)), "{p:?}");
            assert_eq!(simd::plane_bounds(&d[..20], 5, 4), Some((2, 1, 3, 2)), "{p:?}");
            for short in [19usize, 0, 4] {
                let r = std::panic::catch_unwind(|| simd::plane_bounds(&vec![7u8; short], 5, 4));
                assert!(r.is_err(), "{p:?} a {short}-sample plane for 5x4 must panic");
            }
            assert_eq!(simd::plane_bounds(&[], 0, 0), None);
            assert_eq!(simd::plane_bounds(&[], 0, 7), None);
            assert_eq!(simd::plane_bounds(&[], 7, 0), None);
            assert_eq!(simd::plane_bounds(&[1, 2, 3], 0, 3), None);
            assert_eq!(simd::plane_bounds(&[1, 2, 3], 3, 0), None);
        });
    }
}

/// A forced path survives a panic in the closure, and an unavailable one is refused without changing anything.
pub fn with_path_restores_after_a_panic() {
    let before = simd::path();
    for p in simd::available() {
        let r = std::panic::catch_unwind(|| {
            simd::with_path(p, || {
                assert_eq!(simd::path(), p);
                panic!("on purpose")
            })
        });
        assert!(r.is_err());
        assert_eq!(simd::path(), before, "{p:?}");
    }
    simd::with_path(Path::Scalar, || {
        let r = std::panic::catch_unwind(|| simd::with_path(Path::Neon, || ()));
        assert!(r.is_err(), "NEON cannot run on x86_64");
        assert_eq!(simd::path(), Path::Scalar);
    });
}

// ---------------------------------------------------------------------------------------------------------
// The mask plane.
// ---------------------------------------------------------------------------------------------------------

/// `geq`'s `a='(keep)*alpha(X,Y)'` over an opaque plane, sample by sample, the exact `hypot` on every sample of an
/// ellipse, nothing shared with `mask_plane.rs`: `lte(d,1)` / `clip((1-d)/f,0,1)`, `(1-inside)`, `*255` truncated.
pub fn geq_plane(mask: &Mask, w: u32, h: u32) -> Vec<u8> {
    let m = mask.normalized();
    let (rw, rh) = (m.width / 2.0, m.height / 2.0);
    let mut out = Vec::with_capacity(w as usize * h as usize);
    for y in 0..h {
        for x in 0..w {
            let dx = (f64::from(x) - m.x * f64::from(w)) / (rw * f64::from(w));
            let dy = (f64::from(y) - m.y * f64::from(h)) / (rh * f64::from(h));
            let d = match m.shape {
                MaskShape::Rect => {
                    let (a, b) = (dx.abs(), dy.abs());
                    if a > b {
                        a
                    } else {
                        b
                    }
                }
                MaskShape::Ellipse => dx.hypot(dy),
            };
            let inside = if m.feather <= 1e-6 {
                if d <= 1.0 {
                    1.0
                } else {
                    0.0
                }
            } else {
                let c = (1.0 - d) / m.feather;
                if c < 0.0 {
                    0.0
                } else if c > 1.0 {
                    1.0
                } else {
                    c
                }
            };
            let keep = if m.inverted { 1.0 - inside } else { inside };
            out.push((keep * 255.0) as u8);
        }
    }
    out
}

/// The first sample that differs, for a message.
fn first_diff(a: &[u8], b: &[u8], w: u32) -> String {
    if a.len() != b.len() {
        return format!("lengths {} vs {}", a.len(), b.len());
    }
    let mut n = 0;
    let mut first = None;
    let mut worst = 0i32;
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if x != y {
            n += 1;
            first.get_or_insert(i);
            worst = worst.max((i32::from(*x) - i32::from(*y)).abs());
        }
    }
    match first {
        None => "equal".into(),
        Some(i) => format!(
            "{n} samples differ, worst {worst}, first ({}, {}): {} vs {}",
            i as u32 % w.max(1),
            i as u32 / w.max(1),
            a[i],
            b[i]
        ),
    }
}

/// `mask_alpha_plane` on `p`, against the scalar function (byte for byte) and, with `exact`, the independent `geq`
/// evaluation.
#[track_caller]
pub fn check_mask(p: Path, m: &Mask, w: u32, h: u32, exact: bool) {
    let got = simd::with_path(p, || kerf_core::mask_alpha_plane(m, w, h));
    let scalar = mask_alpha_plane_scalar(m, w, h);
    assert!(
        got == scalar,
        "{p:?} vs scalar {w}x{h} {m:?}: {}",
        first_diff(&got, &scalar, w)
    );
    if exact {
        let want = geq_plane(m, w, h);
        assert!(
            scalar == want,
            "scalar vs geq {w}x{h} {m:?}: {}",
            first_diff(&scalar, &want, w)
        );
    }
}

const FEATHERS: [f64; 24] = [
    0.0,
    -0.0,
    -1.0,
    1e-300,
    1e-12,
    1e-9,
    9.999999999999999e-7,
    1e-6,
    1.0000000000000002e-6,
    1.000001e-6,
    2e-6,
    1e-5,
    1e-3,
    0.05,
    0.15,
    0.3333333333333333,
    0.5,
    0.9,
    0.999999,
    1.0,
    1.5,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
];

const SIZES: [f64; 14] = [
    0.0,
    -1.0,
    0.005,
    0.01,
    0.010000000000000002,
    0.1,
    0.5,
    0.999,
    1.0,
    1.001,
    2.0,
    2.5,
    f64::NAN,
    f64::INFINITY,
];

const CENTRES: [f64; 12] = [
    0.5,
    0.0,
    1.0,
    0.25,
    0.75,
    1.0 / 3.0,
    2.0 / 3.0,
    -5.0,
    7.0,
    1e308,
    f64::NAN,
    f64::NEG_INFINITY,
];

fn random_mask(rng: &mut Rng) -> Mask {
    let pick = |rng: &mut Rng, list: &[f64], span: f64, lo: f64| match rng.below(3) {
        0 => list[rng.below(list.len())],
        _ => lo + rng.unit() * span,
    };
    Mask {
        shape: if rng.below(2) == 0 {
            MaskShape::Rect
        } else {
            MaskShape::Ellipse
        },
        x: pick(rng, &CENTRES, 1.4, -0.2),
        y: pick(rng, &CENTRES, 1.4, -0.2),
        width: pick(rng, &SIZES, 2.2, 0.0),
        height: pick(rng, &SIZES, 2.2, 0.0),
        feather: pick(rng, &FEATHERS, 1.2, -0.1),
        inverted: rng.below(2) == 0,
    }
}

fn mask(shape: MaskShape, x: f64, y: f64, width: f64, height: f64, feather: f64, inverted: bool) -> Mask {
    Mask {
        shape,
        x,
        y,
        width,
        height,
        feather,
        inverted,
    }
}

pub const SHAPES: [MaskShape; 2] = [MaskShape::Rect, MaskShape::Ellipse];

/// Every feather, both shapes, both senses, on a mask whose edge falls on samples (d is exactly 1 there), at every
/// width from 1 to 70 and a few heights.
pub fn mask_every_feather_every_width() {
    for p in simd::available() {
        for shape in SHAPES {
            for feather in FEATHERS {
                for inverted in [false, true] {
                    for (w, h) in (1..=70u32).flat_map(|w| [(w, 1u32), (w, 3), (w, 8)]) {
                        let m = mask(shape, 0.5, 0.5, 0.5, 0.5, feather, inverted);
                        check_mask(p, &m, w, h, true);
                    }
                }
            }
        }
    }
}

/// A product of centres and sizes (the model clamps what is out of range, NaN and infinity to something finite).
pub fn mask_centres_and_sizes(w: u32, h: u32) {
    for p in simd::available() {
        for shape in SHAPES {
            for &x in &CENTRES {
                for &width in &SIZES {
                    let m = mask(
                        shape,
                        x,
                        1.0 - x.clamp(0.0, 1.0) * 0.3,
                        width,
                        1.7 - width.clamp(0.0, 1.2),
                        0.2,
                        x.to_bits() & 1 == 0,
                    );
                    check_mask(p, &m, w, h, true);
                    let m = mask(shape, 0.5, x, width, 0.37, 0.0, false);
                    check_mask(p, &m, w, h, true);
                }
            }
        }
    }
}

/// Seeded random masks of every kind at random small sizes (and at the widths either side of a register).
pub fn mask_random(count: usize, max_side: u32) {
    let mut rng = Rng(0xFEED_FACE_0BAD_F00D);
    for p in simd::available() {
        for _ in 0..count {
            let m = random_mask(&mut rng);
            for _ in 0..3 {
                let w = 1 + rng.below(max_side as usize) as u32;
                let h = 1 + rng.below(max_side.min(40) as usize) as u32;
                check_mask(p, &m, w, h, true);
            }
        }
    }
}

/// Fine sweeps of the feather and the size, the inverted senses included: `(1 - x) * 255` against
/// `255 - x * 255` is where a fused multiply-add shows, and where a truncation of a value an ulp under an integer does.
pub fn mask_dense_feather_sweep() {
    let mut rng = Rng(0x0DDB_A11);
    for p in simd::available() {
        for shape in SHAPES {
            for i in 0..400 {
                let feather = if i < 200 {
                    1e-6 + f64::from(i) * 0.005
                } else {
                    1e-6 + rng.unit()
                };
                for inverted in [false, true] {
                    let m = mask(
                        shape,
                        rng.unit(),
                        rng.unit(),
                        0.02 + rng.unit() * 1.9,
                        0.02 + rng.unit() * 1.9,
                        feather,
                        inverted,
                    );
                    let (w, h) = (7 + rng.below(90) as u32, 5 + rng.below(30) as u32);
                    check_mask(p, &m, w, h, true);
                }
            }
        }
    }
}

/// The widths and heights from 1 to 70 (and the odd ones around 100) in both directions, for a handful of masks.
pub fn mask_every_size() {
    let masks = [
        mask(MaskShape::Rect, 0.5, 0.5, 0.5, 0.5, 0.0, false),
        mask(MaskShape::Rect, 0.3, 0.6, 0.9, 0.4, 0.2, true),
        mask(MaskShape::Ellipse, 0.5, 0.5, 0.5, 0.5, 0.0, false),
        mask(MaskShape::Ellipse, 0.4, 0.55, 1.3, 0.8, 0.15, false),
        mask(MaskShape::Ellipse, 0.6, 0.4, 0.7, 1.1, 0.7, true),
        mask(MaskShape::Rect, 0.0, 1.0, 2.0, 2.0, 1.0, true),
    ];
    for p in simd::available() {
        for m in &masks {
            for w in (1..=70u32).chain([97, 99, 100, 101, 127, 128, 129]) {
                for h in (1..=70u32).step_by(3).chain([1, 2, 3, 4, 5, 6, 7, 8, 9]) {
                    check_mask(p, m, w, h, w * h < 2000);
                }
            }
        }
    }
}

/// Picture sizes: 4K, a sliver, a column, and widths that are not a multiple of anything.
pub fn mask_picture_sizes(sizes: &[(u32, u32)], count: usize) {
    let mut rng = Rng(0x4B4B_4B4B);
    let fixed = [
        mask(MaskShape::Rect, 0.5, 0.5, 0.5, 0.5, 0.0, false),
        mask(MaskShape::Rect, 0.5, 0.5, 0.8, 0.6, 0.15, true),
        mask(MaskShape::Ellipse, 0.5, 0.5, 0.5, 0.5, 0.0, false),
        mask(MaskShape::Ellipse, 0.45, 0.55, 0.7, 0.9, 0.15, false),
        mask(MaskShape::Ellipse, 0.45, 0.55, 0.7, 0.9, 0.15, true),
    ];
    for p in simd::available() {
        for &(w, h) in sizes {
            for m in &fixed {
                check_mask(p, m, w, h, w as u64 * h as u64 <= 4_200_000);
            }
            for _ in 0..count {
                let m = random_mask(&mut rng);
                check_mask(p, &m, w, h, w as u64 * h as u64 <= 4_200_000);
            }
        }
    }
}

/// A zero-sized picture is an empty plane on every path.
pub fn mask_empty_pictures() {
    for p in simd::available() {
        simd::with_path(p, || {
            for m in [Mask::default(), mask(MaskShape::Ellipse, 0.5, 0.5, 0.5, 0.5, 0.0, true)] {
                assert!(kerf_core::mask_alpha_plane(&m, 0, 0).is_empty());
                assert!(kerf_core::mask_alpha_plane(&m, 0, 9).is_empty());
                assert!(kerf_core::mask_alpha_plane(&m, 9, 0).is_empty());
            }
        });
    }
}

/// A plane drawn on one thread with a path forced, another thread with none: the same bytes.
pub fn mask_from_threads_that_pick_for_themselves() {
    let m = mask(MaskShape::Ellipse, 0.45, 0.55, 0.7, 0.9, 0.15, true);
    let want = mask_alpha_plane_scalar(&m, 403, 77);
    let handles: Vec<_> = (0..4)
        .map(|_| std::thread::spawn(move || kerf_core::mask_alpha_plane(&m, 403, 77)))
        .collect();
    for h in handles {
        assert!(h.join().unwrap() == want);
    }
}
