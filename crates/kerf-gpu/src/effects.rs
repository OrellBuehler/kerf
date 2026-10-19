//! Ports of the FFmpeg filters behind Kerf's video effects, as scalar references and the
//! parameters the shaders take (`shaders/planeops.wgsl`: tables and `unsharp`; `shaders/blur.wgsl` and
//! `blur_out.wgsl`: `gblur`).
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
//! * **chroma key** is `chromakey` (8-bit, after `format=yuva420p`): an alpha for each luma sample from
//!   the 3x3 window of chroma samples around it, the mean of nine `sqrt((du² + dv²) / (255² * 2))` from the
//!   key's U, V ([`chroma_key_uv`]) summed in `double`, then `(mean - similarity) / blend` clipped and
//!   scaled to 255 (or a hard 0 / 255 at a blend of 0.0001 or less). Outside the picture a window entry
//!   keeps what the filter last wrote there in the same slice, the key colour at a slice's start, so a
//!   slice's first sample depends on the graph's thread count ([`chroma_key_source`]). The shader sums the
//!   terms in integers ([`chroma_key_add`]) and counts the thresholds the sum reaches
//!   ([`chroma_key_thresholds`]), which hold the rest of the `double` arithmetic; [`chroma_key_plane`] is
//!   the filter as written. Exact.
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

/// A chroma key as the filter holds it: the key colour's U and V ([`chroma_key_uv`]) and its two
/// options, `float`s.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromaKeyOptions {
    pub uv: [u8; 2],
    pub similarity: f32,
    pub blend: f32,
}

/// `chromakey`'s key colour in the picture's chroma (`RGB_TO_U` / `RGB_TO_V`: BT.601 in 10-bit fixed
/// point, `lrint` coefficients, the `>> 10` flooring).
pub fn chroma_key_uv(rgb: [u8; 3]) -> [u8; 2] {
    let [r, g, b] = rgb.map(i32::from);
    let u = ((-173 * r - 339 * g + 512 * b + 511) >> 10) + 128;
    let v = ((512 * r - 429 * g - 83 * b + 511) >> 10) + 128;
    [u as u8, v as u8]
}

/// How many distances squared a chroma sample can be from the key: `du² + dv²` for `du`, `dv` in
/// `-255..=255`, at most `2 * 255²`.
pub const CHROMA_KEY_TERMS: usize = 2 * 255 * 255 + 1;

/// One sample's term of the key's distance sum: `sqrt(d2 / (255 * 255 * 2))` in `double`.
pub fn chroma_key_term(d2: u32) -> f64 {
    (f64::from(d2) / (255.0 * 255.0 * 2.0)).sqrt()
}

/// Every [`chroma_key_term`] as `double` bits, `d2` from 0: the table the shader sums from.
pub fn chroma_key_terms() -> Vec<u64> {
    (0..CHROMA_KEY_TERMS as u32).map(|d2| chroma_key_term(d2).to_bits()).collect()
}

/// The alpha `chromakey` makes of a sample's distance sum (nine terms, added in `double` in their
/// order): the mean, then `(mean - similarity) / blend` clipped to 0..1 and scaled to 255 (truncated),
/// or with a blend of 0.0001 or less 255 past the similarity and 0 within it. The option values are
/// `float`s.
pub fn chroma_key_alpha(sum: f64, similarity: f32, blend: f32) -> u8 {
    let diff = sum / 9.0;
    let (similarity, blend) = (f64::from(similarity), f64::from(blend));
    if blend > 0.0001 {
        (((diff - similarity) / blend).clamp(0.0, 1.0) * 255.0) as u8
    } else if diff > similarity {
        255
    } else {
        0
    }
}

/// For each alpha level 1..=255 the least distance sum (`double` bits) that reaches it, `u64::MAX` for
/// one no sum reaches. Every step of [`chroma_key_alpha`] is monotone in the sum and a non-negative
/// `double`'s bits are in its order, so a sample's alpha is the number of thresholds its sum's bits
/// reach: the shader adds the nine terms exactly and counts, and never divides.
pub fn chroma_key_thresholds(similarity: f32, blend: f32) -> Vec<u64> {
    let top = 9.0f64.to_bits();
    let alpha = |bits: u64| u32::from(chroma_key_alpha(f64::from_bits(bits), similarity, blend));
    (1..=255u32)
        .map(|k| {
            if alpha(top) < k {
                return u64::MAX;
            }
            let (mut lo, mut hi) = (0u64, top);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if alpha(mid) >= k {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            lo
        })
        .collect()
}

/// The sum of two non-negative `double`s (`[hi, lo]` bits, normal or zero), rounded to nearest even, in
/// 32-bit integers: what `chromakey.wgsl` does, step for step (WGSL has no `f64`).
pub fn chroma_key_add(a: [u32; 2], b: [u32; 2]) -> [u32; 2] {
    if a == [0, 0] {
        return b;
    }
    if b == [0, 0] {
        return a;
    }
    let (a, b) = if (a[0] >> 20) >= (b[0] >> 20) { (a, b) } else { (b, a) };
    let mut e = a[0] >> 20;
    let d = e - (b[0] >> 20);
    // Each mantissa with its implicit bit, shifted up so its top is bit 62: ten bits below to round in.
    let wide = |x: [u32; 2]| -> [u32; 2] {
        let h = (x[0] & 0xf_ffff) | 0x10_0000;
        [(h << 10) | (x[1] >> 22), x[1] << 10]
    };
    let ma = wide(a);
    let mut mb = wide(b);
    // `b` down to `a`'s exponent, what falls off kept as a sticky bit.
    if d >= 64 {
        mb = [0, 1];
    } else if d >= 32 {
        let s = d - 32;
        let lost = mb[1] != 0 || (s > 0 && (mb[0] & ((1u32 << s) - 1)) != 0);
        mb = [0, (if s == 0 { mb[0] } else { mb[0] >> s }) | u32::from(lost)];
    } else if d > 0 {
        let lost = (mb[1] & ((1u32 << d) - 1)) != 0;
        mb = [mb[0] >> d, (mb[1] >> d) | (mb[0] << (32 - d)) | u32::from(lost)];
    }
    let lo = ma[1].wrapping_add(mb[1]);
    let carry = u32::from(lo < ma[1]);
    let mut sum = [ma[0] + mb[0] + carry, lo];
    if sum[0] & 0x8000_0000 != 0 {
        let lost = sum[1] & 1;
        sum = [sum[0] >> 1, (sum[1] >> 1) | (sum[0] << 31) | lost];
        e += 1;
    }
    let rest = sum[1] & 0x3ff;
    let mut keep = [sum[0] >> 10, (sum[1] >> 10) | (sum[0] << 22)];
    if rest > 0x200 || (rest == 0x200 && keep[1] & 1 == 1) {
        keep[1] = keep[1].wrapping_add(1);
        if keep[1] == 0 {
            keep[0] += 1;
        }
        if keep[0] == 0x20_0000 {
            keep = [0x10_0000, 0];
            e += 1;
        }
    }
    [(e << 20) | (keep[0] & 0xf_ffff), keep[1]]
}

/// Where entry `(xo, yo)` of `chromakey`'s 3x3 window holds its value from at luma sample `(x, y)` of a
/// `w` x `h` picture in the slice that starts at row `s0`: the neighbour itself when it is in the
/// picture; else what the entry kept from the last sample of the slice that wrote it (the filter skips
/// the write and the array runs on): the same row's column `w - 1` on the right edge, else the previous
/// row's last write (column `w - 2` for the left entry, `w - 1` otherwise, one row up from the
/// neighbour); `None` where nothing in the slice wrote it, the key colour the slice began with.
/// For `w`, `h` of 2 or more.
pub fn chroma_key_source(x: u32, y: u32, xo: u32, yo: u32, w: u32, h: u32, s0: u32) -> Option<(u32, u32)> {
    let (cx, cy) = (i64::from(x) + i64::from(xo) - 1, i64::from(y) + i64::from(yo) - 1);
    let (w, h) = (i64::from(w), i64::from(h));
    if (0..w).contains(&cx) && (0..h).contains(&cy) {
        return Some((cx as u32, cy as u32));
    }
    if (0..h).contains(&cy) && cx >= w {
        return Some(((w - 1) as u32, cy as u32));
    }
    let col = if xo == 0 { w - 2 } else { w - 1 };
    let row = i64::from(y) + i64::from(yo) - 2;
    (i64::from(y) > i64::from(s0) && row >= 0).then_some((col as u32, row as u32))
}

/// The row slice `j` of `n` starts at (`ff_slice_pos`).
pub fn chroma_key_slice_start(h: u32, j: u32, n: u32) -> u32 {
    (u64::from(h) * u64::from(j) / u64::from(n)) as u32
}

/// `chromakey`'s alpha plane for a `w` x `h` `yuva420p` picture's chroma `u`, `v` (each `ceil(w/2)` x
/// `ceil(h/2)`): `vf_chromakey.c` as written, slice by slice (`slices` threads: the slice count is
/// `min(h, threads)`, and each slice starts its window over at the key colour).
pub fn chroma_key_plane(u: &[u8], v: &[u8], w: u32, h: u32, key: &ChromaKeyOptions, slices: u32) -> Vec<u8> {
    let ChromaKeyOptions {
        uv: key,
        similarity,
        blend,
    } = *key;
    let cw = w.div_ceil(2) as usize;
    let mut alpha = vec![0u8; w as usize * h as usize];
    let n = slices.clamp(1, h.max(1));
    for j in 0..n {
        let (s0, s1) = (chroma_key_slice_start(h, j, n), chroma_key_slice_start(h, j + 1, n));
        let (mut wu, mut wv) = ([key[0]; 9], [key[1]; 9]);
        for y in s0..s1 {
            for x in 0..w {
                for yo in 0..3u32 {
                    for xo in 0..3u32 {
                        let (cx, cy) = (i64::from(x) + i64::from(xo) - 1, i64::from(y) + i64::from(yo) - 1);
                        if (0..i64::from(w)).contains(&cx) && (0..i64::from(h)).contains(&cy) {
                            let at = (cy as usize >> 1) * cw + (cx as usize >> 1);
                            wu[(yo * 3 + xo) as usize] = u[at];
                            wv[(yo * 3 + xo) as usize] = v[at];
                        }
                    }
                }
                let mut sum = 0.0f64;
                for i in 0..9 {
                    let du = i32::from(wu[i]) - i32::from(key[0]);
                    let dv = i32::from(wv[i]) - i32::from(key[1]);
                    sum += chroma_key_term((du * du + dv * dv) as u32);
                }
                alpha[(y * w + x) as usize] = chroma_key_alpha(sum, similarity, blend);
            }
        }
    }
    alpha
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(x: [u32; 2]) -> u64 {
        (u64::from(x[0]) << 32) | u64::from(x[1])
    }

    fn halves(x: u64) -> [u32; 2] {
        [(x >> 32) as u32, x as u32]
    }

    /// A deterministic stream of numbers for the tests below.
    fn stream(seed: u64) -> impl FnMut() -> u64 {
        let mut state = seed;
        move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state >> 11
        }
    }

    #[test]
    fn the_key_colour_is_ffmpegs_fixed_point_bt601() {
        assert_eq!(chroma_key_uv([0, 128, 0]), [86, 74]);
        assert_eq!(chroma_key_uv([0, 0, 0]), [128, 128]);
        assert_eq!(chroma_key_uv([255, 255, 255]), [128, 128]);
        assert_eq!(chroma_key_uv([0, 0, 255]), [255, 107]);
        assert_eq!(chroma_key_uv([255, 0, 0]), [85, 255]);
    }

    #[test]
    fn the_integer_sum_is_the_double_sum() {
        let terms = chroma_key_terms();
        let mut next = stream(3);
        // Nine terms at a time, as the filter adds them.
        for _ in 0..200_000 {
            let (mut sum, mut emulated) = (0.0f64, [0u32; 2]);
            for _ in 0..9 {
                let t = terms[next() as usize % CHROMA_KEY_TERMS];
                sum += f64::from_bits(t);
                emulated = chroma_key_add(emulated, halves(t));
                assert_eq!(bits(emulated), sum.to_bits(), "{sum:e}");
            }
        }
        // Any two positive doubles, exponents near and far apart, ties included.
        for _ in 0..2_000_000 {
            let draw = |n: u64| ((900 + n % 200) << 52) | (n >> 8 & 0xf_ffff_ffff_ffff);
            let (a, b) = (draw(next()), draw(next()));
            let b = if next().is_multiple_of(4) {
                (a & !0xff) | (b & 0xff)
            } else {
                b
            };
            let want = (f64::from_bits(a) + f64::from_bits(b)).to_bits();
            assert_eq!(bits(chroma_key_add(halves(a), halves(b))), want, "{a:x} + {b:x}");
        }
        assert_eq!(bits(chroma_key_add([0, 0], halves(1.5f64.to_bits()))), 1.5f64.to_bits());
    }

    #[test]
    fn the_thresholds_count_the_filters_alpha() {
        let mut next = stream(5);
        for (similarity, blend) in [
            (0.15f32, 0.1f32),
            (0.01, 0.5),
            (0.3, 0.0),
            (0.2, 0.000_05),
            (1.0, 1.0),
            (0.000_01, 0.0001),
            (0.4, 0.25),
        ] {
            let th = chroma_key_thresholds(similarity, blend);
            assert_eq!(th.len(), 255);
            assert!(th.windows(2).all(|w| w[0] <= w[1]));
            let alpha = |sum: u64| th.iter().filter(|t| sum >= **t).count();
            let mut sums: Vec<u64> = (0..100_000).map(|_| next() % 9.0f64.to_bits()).collect();
            sums.extend(th.iter().filter(|t| **t != u64::MAX).flat_map(|t| [t - 1, *t, t + 1]));
            for sum in sums {
                assert_eq!(
                    alpha(sum),
                    usize::from(chroma_key_alpha(f64::from_bits(sum), similarity, blend)),
                    "{similarity} {blend} {:e}",
                    f64::from_bits(sum)
                );
            }
        }
    }

    /// The plane the shader's way: each entry where [`chroma_key_source`] says, the integer sum, the count of
    /// thresholds reached.
    fn by_rule(u: &[u8], v: &[u8], w: u32, h: u32, options: &ChromaKeyOptions, slices: u32) -> Vec<u8> {
        let ChromaKeyOptions {
            uv: key,
            similarity,
            blend,
        } = *options;
        let terms = chroma_key_terms();
        let th = chroma_key_thresholds(similarity, blend);
        let cw = w.div_ceil(2);
        let n = slices.clamp(1, h);
        let mut out = Vec::new();
        for y in 0..h {
            let j = (0..n).rev().find(|&j| chroma_key_slice_start(h, j, n) <= y).unwrap();
            let s0 = chroma_key_slice_start(h, j, n);
            for x in 0..w {
                let mut sum = [0u32; 2];
                for yo in 0..3 {
                    for xo in 0..3 {
                        let (cu, cv) = match chroma_key_source(x, y, xo, yo, w, h, s0) {
                            Some((sx, sy)) => {
                                let at = ((sy >> 1) * cw + (sx >> 1)) as usize;
                                (u[at], v[at])
                            }
                            None => (key[0], key[1]),
                        };
                        let (du, dv) = (i32::from(cu) - i32::from(key[0]), i32::from(cv) - i32::from(key[1]));
                        sum = chroma_key_add(sum, halves(terms[(du * du + dv * dv) as usize]));
                    }
                }
                out.push(th.iter().filter(|t| bits(sum) >= **t).count() as u8);
            }
        }
        out
    }

    #[test]
    fn the_window_rule_and_the_thresholds_are_the_filter_as_written() {
        let mut next = stream(9);
        for w in 2..=13u32 {
            for h in 2..=11u32 {
                let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
                // Chroma near the key, so the alpha is graded and every window entry counts.
                let key = [86u8, 74u8];
                let near = |c: u8, n: u64| (i32::from(c) + (n % 61) as i32 - 30) as u8;
                let u: Vec<u8> = (0..cw * ch).map(|_| near(key[0], next())).collect();
                let v: Vec<u8> = (0..cw * ch).map(|_| near(key[1], next())).collect();
                for slices in [1, 2, 3, 5, 7, 16] {
                    for (similarity, blend) in [(0.02f32, 0.2f32), (0.05, 0.0)] {
                        let options = ChromaKeyOptions {
                            uv: key,
                            similarity,
                            blend,
                        };
                        assert_eq!(
                            by_rule(&u, &v, w, h, &options, slices),
                            chroma_key_plane(&u, &v, w, h, &options, slices),
                            "{w}x{h} {slices} slices {options:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "needs ffmpeg"]
    fn the_reference_is_ffmpegs_chromakey_at_any_thread_count() {
        let run = |args: &[&str], input: Option<Vec<u8>>| -> Vec<u8> {
            let mut cmd = kerf_core::ffmpeg_command();
            cmd.args(["-v", "error"]).args(args);
            cmd.stdin(if input.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped());
            let mut child = cmd.spawn().expect("ffmpeg");
            let feed = input.map(|data| {
                let mut stdin = child.stdin.take().expect("stdin");
                std::thread::spawn(move || std::io::Write::write_all(&mut stdin, &data))
            });
            let out = child.wait_with_output().expect("ffmpeg");
            if let Some(feed) = feed {
                feed.join().unwrap().expect("fed");
            }
            assert!(out.status.success());
            out.stdout
        };
        for (w, h) in [(96u32, 54u32), (97, 55)] {
            let size = format!("{w}x{h}");
            for source in [
                format!("testsrc2=size=128x72:rate=30:duration=1,scale={w}:{h}"),
                format!("color=c=green:size=128x72:rate=30:duration=1,noise=alls=60:allf=t,scale={w}:{h}:flags=neighbor"),
            ] {
                let src = run(
                    &[
                        "-f",
                        "lavfi",
                        "-i",
                        &source,
                        "-frames:v",
                        "1",
                        "-pix_fmt",
                        "yuv420p",
                        "-f",
                        "rawvideo",
                        "-",
                    ],
                    None,
                );
                let (luma, chroma) = ((w * h) as usize, (w.div_ceil(2) * h.div_ceil(2)) as usize);
                let (u, v) = (&src[luma..luma + chroma], &src[luma + chroma..luma + 2 * chroma]);
                for (color, similarity, blend) in [("green", 0.15, 0.1), ("green", 0.01, 0.5), ("0x20c040", 0.2, 0.0)] {
                    let rgb = kerf_core::color_rgb(color).expect("colour");
                    for threads in [1u32, 3, 5] {
                        let filter = format!("format=yuva420p,chromakey={color}:{similarity}:{blend}");
                        let out = run(
                            &[
                                "-filter_threads",
                                &threads.to_string(),
                                "-f",
                                "rawvideo",
                                "-pix_fmt",
                                "yuv420p",
                                "-s",
                                &size,
                                "-i",
                                "-",
                                "-vf",
                                &filter,
                                "-f",
                                "rawvideo",
                                "-pix_fmt",
                                "yuva420p",
                                "-",
                            ],
                            Some(src.clone()),
                        );
                        let alpha = &out[luma + 2 * chroma..2 * luma + 2 * chroma];
                        let options = ChromaKeyOptions {
                            uv: chroma_key_uv(rgb),
                            similarity: similarity as f32,
                            blend: blend as f32,
                        };
                        let want = chroma_key_plane(u, v, w, h, &options, threads);
                        assert!(
                            alpha == want.as_slice(),
                            "{w}x{h} {source} {color} {similarity} {blend} {threads} threads"
                        );
                    }
                }
            }
        }
    }

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
        // The mass is 255 before rounding; the tails below half a level round to 0 (a third of it here).
        let sum: i32 = out.iter().map(|v| i32::from(*v)).sum();
        assert!(
            (200..=262).contains(&sum),
            "the mass is 255 less the rounded-off tails, got {sum}"
        );
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
