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
//! * **vignette** is `vignette` with its defaults (`vf_vignette.c`: angle `PI/5`, held as a `float` by
//!   `av_clipf`; centre `w/2`, `h/2`; forward, `eval=init`, dither on) on a `yuv420p` picture. A factor per
//!   luma sample, `cos(angle * d / dmax)^4` in `double` (`d` from the sample's offset to the centre, each
//!   axis truncated to an integer; 0 past `dmax`, half the diagonal) stored as a `float` ([`vignette_map`]);
//!   luma becomes `(int)(f * s + dither)` and chroma `(int)(f * (s - 127) + 127 + dither)` (chroma reads
//!   the factor of its block's top-left luma sample), the products and the `+ 127` in `float`, the dither
//!   added in `double`. The **dither** is an LCG (`state * 1664525 + 1013904223`, from 0), one step per sample,
//!   Y then U then V in raster order, that **runs on across frames**: sample `i` of the filter's `n`th
//!   frame is dithered by state number `n * samples_per_frame + i` ([`lcg_at`], its period is 2^32).
//!   [`vignette_luma`] and [`vignette_chroma`] do the arithmetic in integers (the `float` roundings
//!   made exactly, so no backend's contraction into a fused multiply-add can change a level), which is
//!   what `vignette.wgsl` does; [`vignette_planes`] is the filter on a frame. Exact, held to FFmpeg's
//!   output byte for byte.
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

/// `vignette`'s LCG: `state * A + C`, from 0.
pub const LCG_A: u32 = 1_664_525;
pub const LCG_C: u32 = 1_013_904_223;

/// The LCG's state after `i` steps from 0: the dither of sample `i` of the filter's run is this over 2^32.
/// By halving (`O(log i)`), as the shader does for each sample.
pub fn lcg_at(i: u32) -> u32 {
    let (mut acc_a, mut acc_c) = (1u32, 0u32);
    let (mut a, mut c) = (LCG_A, LCG_C);
    let mut i = i;
    while i > 0 {
        if i & 1 == 1 {
            acc_a = acc_a.wrapping_mul(a);
            acc_c = acc_c.wrapping_mul(a).wrapping_add(c);
        }
        c = c.wrapping_mul(a.wrapping_add(1));
        a = a.wrapping_mul(a);
        i >>= 1;
    }
    acc_c
}

/// The samples `vignette` steps its dither through for a `w` x `h` `yuv420p` frame.
pub fn vignette_samples(w: u32, h: u32) -> u64 {
    let (cw, ch) = (u64::from(w.div_ceil(2)), u64::from(h.div_ceil(2)));
    u64::from(w) * u64::from(h) + 2 * cw * ch
}

/// `vignette`'s factor for each luma sample of a `w` x `h` picture (`update_context` with the defaults),
/// row by row: computed in `double` and stored as `float`, as the filter's `fmap` is.
pub fn vignette_map(w: u32, h: u32) -> Vec<f32> {
    // `av_clipf` clips the angle as a `float`: `PI/5` one `float` rounding up, which takes the factor a `float`
    // step lower on some samples (a level, where the sum sits just over an integer).
    let angle = f64::from((std::f64::consts::PI / 5.0) as f32);
    let (x0, y0) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
    let dmax = (f64::from(w) / 2.0).hypot(f64::from(h) / 2.0);
    let mut map = Vec::with_capacity(w as usize * h as usize);
    for y in 0..h {
        // `const int yy = (y - s->y0) * s->yscale`: truncated toward zero, the scale 1 (`setsar=1` comes first).
        let yy = (f64::from(y) - y0) as i32;
        for x in 0..w {
            let xx = (f64::from(x) - x0) as i32;
            let d = f64::from(xx).hypot(f64::from(yy)) / dmax;
            let v = if d > 1.0 {
                0.0
            } else {
                let c = (angle * d).cos();
                (c * c) * (c * c)
            };
            map.push(v as f32);
        }
    }
    map
}

/// `m` (non-zero) rounded to 24 significant bits, half to even, as a `float` holds it: the mantissa and
/// how far it was shifted (`m ≈ mantissa << shift`).
fn round24(m: u32) -> (u32, i32) {
    let len = 32 - m.leading_zeros();
    if len <= 24 {
        return (m, 0);
    }
    let sh = len - 24;
    let mut q = m >> sh;
    let rem = m & ((1u32 << sh) - 1);
    let half = 1u32 << (sh - 1);
    if rem > half || (rem == half && q & 1 == 1) {
        q += 1;
    }
    if q == 1 << 24 {
        return (q >> 1, sh as i32 + 1);
    }
    (q, sh as i32)
}

/// A `float` factor as an integer mantissa and its power of two (`f = m * 2^e`); `None` for 0.
fn factor(f: f32) -> Option<(u32, i32)> {
    let bits = f.to_bits();
    let exp = ((bits >> 23) & 0xff) as i32;
    (exp != 0).then(|| ((bits & 0x7f_ffff) | 0x80_0000, exp - 127 - 23))
}

/// `(int)(v + state / 2^32)` for `v = m * 2^e`, `0 <= v <= 255` with no bit below 2^-32: the integer part,
/// and one more when the fraction and the dither reach 1 (the `double` sum is exact here).
fn floor_dithered(m: u32, e: i32, state: u32) -> u8 {
    if m == 0 {
        return 0;
    }
    let (int, frac) = if e >= 0 {
        (m << e, 0)
    } else {
        (m >> -e, m & ((1u32 << -e) - 1))
    };
    // The fraction in 32 bits (`e >= -32` here: every value has a bit at 2^-25 or above).
    let f32bits = if e >= 0 { 0 } else { frac << (32 + e) };
    let carry = u32::from(f32bits != 0 && state >= f32bits.wrapping_neg());
    (int + carry).min(255) as u8
}

/// One luma sample through `vignette`: `(int)(f * s + dither)`, the product in `float`.
pub fn vignette_luma(f: f32, s: u8, state: u32) -> u8 {
    let Some((mf, ef)) = factor(f) else {
        return 0;
    };
    let p = mf * u32::from(s);
    if p == 0 {
        return 0;
    }
    let (m, sh) = round24(p);
    floor_dithered(m, ef + sh, state)
}

/// One chroma sample through `vignette`: `(int)(f * (s - 127) + 127 + dither)`, the product and the
/// sum in `float`.
pub fn vignette_chroma(f: f32, s: u8, state: u32) -> u8 {
    let d = i32::from(s) - 127;
    let Some((mf, ef)) = factor(f).filter(|_| d != 0) else {
        return 127;
    };
    let (mq, sh) = round24(mf * d.unsigned_abs());
    let eq = ef + sh;
    // `q + 127` exactly, in units of 2^eq (`eq` is -25 at the least, so 127 << 25 still fits).
    let unit = 127u32 << -eq;
    let sum = if d > 0 { unit + mq } else { unit - mq };
    if sum == 0 {
        return 0;
    }
    let (m, sh) = round24(sum);
    floor_dithered(m, eq + sh, state)
}

/// A `w` x `h` `yuv420p` frame through `vignette`, the `n`th frame the filter has seen.
pub fn vignette_planes(y: &[u8], u: &[u8], v: &[u8], w: u32, h: u32, n: u64) -> [Vec<u8>; 3] {
    let map = vignette_map(w, h);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut i = (n.wrapping_mul(vignette_samples(w, h))) as u32;
    let mut out_y = Vec::with_capacity(y.len());
    for row in 0..h {
        for x in 0..w {
            let at = (row * w + x) as usize;
            out_y.push(vignette_luma(map[at], y[at], lcg_at(i)));
            i = i.wrapping_add(1);
        }
    }
    let mut chroma = |plane: &[u8]| {
        let mut out = Vec::with_capacity(plane.len());
        for row in 0..ch {
            for x in 0..cw {
                let f = map[((row * 2) * w + x * 2) as usize];
                out.push(vignette_chroma(f, plane[(row * cw + x) as usize], lcg_at(i)));
                i = i.wrapping_add(1);
            }
        }
        out
    };
    let out_u = chroma(u);
    let out_v = chroma(v);
    [out_y, out_u, out_v]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lcg_is_jumped_to_where_stepping_gets() {
        let mut state = 0u32;
        for i in 0..5000u32 {
            assert_eq!(lcg_at(i), state, "step {i}");
            state = state.wrapping_mul(LCG_A).wrapping_add(LCG_C);
        }
        // Its period is 2^32: an index past it is the same state.
        assert_eq!(lcg_at(7), lcg_at(7));
        assert_eq!(lcg_at(1), LCG_C);
    }

    /// What the C code computes, in the C code's own precision.
    fn direct(f: f32, s: u8, state: u32, chroma: bool) -> u8 {
        let dv = f64::from(state) / 4_294_967_296.0;
        let v = if chroma {
            f64::from(f * (f32::from(s) - 127.0) + 127.0) + dv
        } else {
            f64::from(f * f32::from(s)) + dv
        };
        (v as i32).clamp(0, 255) as u8
    }

    #[test]
    fn the_integer_arithmetic_is_the_float_and_double_arithmetic_of_the_filter() {
        // The factors of a real map (and the ends of their range), every sample value, and dithers that
        // fall either side of each fraction.
        let mut factors: Vec<f32> = vignette_map(97, 55).into_iter().step_by(7).collect();
        factors.extend([0.0, 1.0, 0.999_999_94, 0.428_9, f32::from_bits(1.0f32.to_bits() - 1)]);
        let mut seen = 0u64;
        for &f in &factors {
            for s in 0..=255u8 {
                for chroma in [false, true] {
                    let base = f64::from(if chroma {
                        f * (f32::from(s) - 127.0) + 127.0
                    } else {
                        f * f32::from(s)
                    });
                    let frac = base - base.floor();
                    let edge = ((1.0 - frac) * 4_294_967_296.0) as u64;
                    for state in [0u64, 1, edge.saturating_sub(1), edge, edge + 1, u64::from(u32::MAX), 1 << 31] {
                        let state = state.min(u64::from(u32::MAX)) as u32;
                        let got = if chroma {
                            vignette_chroma(f, s, state)
                        } else {
                            vignette_luma(f, s, state)
                        };
                        assert_eq!(
                            got,
                            direct(f, s, state, chroma),
                            "f {f:e} s {s} chroma {chroma} state {state}"
                        );
                        seen += 1;
                    }
                }
            }
        }
        assert!(seen > 300_000);
    }

    #[test]
    #[ignore = "needs ffmpeg"]
    fn the_reference_is_ffmpegs_vignette_frame_after_frame() {
        // A flat 640x360 picture of Y 81, U and V 166 has seven samples in ten frames whose sum sits a few
        // millionths over an integer: they hold the factor to the `float` angle's.
        for (w, h, frames, flat) in [(96u32, 54u32, 4u64, false), (97, 55, 4, false), (640, 360, 10, true)] {
            let size = format!("{w}x{h}");
            let src = if flat {
                let (luma, chroma) = ((w * h) as usize, (w.div_ceil(2) * h.div_ceil(2)) as usize);
                let frame: Vec<u8> = [vec![81; luma], vec![166; 2 * chroma]].concat();
                frame.repeat(frames as usize)
            } else {
                let out = kerf_core::ffmpeg_command()
                    .args(["-v", "error", "-f", "lavfi", "-i"])
                    .arg(format!("testsrc2=size=128x72:rate=30:duration=1,scale={w}:{h}"))
                    .args(["-frames:v", &frames.to_string(), "-pix_fmt", "yuv420p", "-f", "rawvideo", "-"])
                    .output()
                    .expect("ffmpeg");
                assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
                out.stdout
            };
            let mut child = kerf_core::ffmpeg_command()
                .args([
                    "-v", "error", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", &size, "-r", "30", "-i", "-",
                ])
                .args(["-vf", "vignette", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("ffmpeg");
            let mut stdin = child.stdin.take().expect("stdin");
            let input = src.clone();
            let feed = std::thread::spawn(move || std::io::Write::write_all(&mut stdin, &input));
            let out = child.wait_with_output().expect("ffmpeg");
            feed.join().unwrap().expect("fed");
            let per = vignette_samples(w, h) as usize;
            assert_eq!(out.stdout.len(), per * frames as usize);
            let (luma, chroma) = ((w * h) as usize, (w.div_ceil(2) * h.div_ceil(2)) as usize);
            for n in 0..frames {
                let at = n as usize * per;
                let planes = |b: &[u8]| {
                    (
                        b[at..at + luma].to_vec(),
                        b[at + luma..at + luma + chroma].to_vec(),
                        b[at + luma + chroma..at + per].to_vec(),
                    )
                };
                let (y, u, v) = planes(&src);
                let (fy, fu, fv) = planes(&out.stdout);
                let [gy, gu, gv] = vignette_planes(&y, &u, &v, w, h, n);
                assert!(gy == fy && gu == fu && gv == fv, "{w}x{h} frame {n}");
            }
        }
    }

    #[test]
    fn the_map_is_one_in_the_middle_falls_off_and_is_zero_past_half_the_diagonal() {
        let (w, h) = (64u32, 36u32);
        let map = vignette_map(w, h);
        let at = |x: u32, y: u32| map[(y * w + x) as usize];
        assert_eq!(at(32, 18), 1.0);
        assert!(at(0, 0) > 0.0 && at(0, 0) < at(16, 9) && at(16, 9) < at(32, 18));
        // The corner is at half the diagonal: cos(PI/5)^4.
        let corner = (std::f64::consts::PI / 5.0).cos().powi(4) as f32;
        assert!((at(0, 0) - corner).abs() < 1e-3, "{}", at(0, 0));
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
