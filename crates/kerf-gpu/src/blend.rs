//! Blend modes, drawn as FFmpeg's `blend` chain draws them (`kerf_core::blend_colour_graph`).
//!
//! A clip with a blend mode is laid onto the composite by a chain of conversions (`engine/cli/blend.rs`):
//! the clip placed opaque on a copy of the composite (`cc`, a picture the compositor already draws), its alpha
//! plane placed alone (`aa`), `alphamerge`, both frames from 8-bit 4:2:0 to `gbrap` by swscale's `neighbor`
//! scaler, `blend` on the three colour planes (the alpha plane is the clip's), back to `yuva420p` by the
//! same scaler, and a last `overlay` of that onto the composite by the clip's alpha. This module is the model of
//! everything after the placement, in integers, held to FFmpeg by `tests/blend_kernels.rs` on noise through
//! the very text the export writes, and by [`kernels`] on the machine that runs it:
//!
//! * **Into RGB** (`yuv2gbrp_full_X_c`, the C path: no SIMD kernel exists for planar RGB out): chroma is
//!   repeated 2x2, `Y' = (Y * 512 - y_off) * y_coeff + 2^21`, `R = Y' + (V - 128) * 512 * v2r`, and so on, in 32-bit
//!   integers **that wrap** (a saturated blue overflows `i32` and is clipped to 0 by the wrap, not to 255), each clipped to
//!   30 bits and shifted down by 22. The coefficients are swscale's own tables for the matrix
//!   (`ff_yuv2rgb_c_init_tables`: BT.601 `9539 13075 -6660 -3209 16525`, BT.709 and BT.2020 the same
//!   derivation). Every one of the 16.7 million (Y, U, V) is the filter's, on both builds.
//! * **The mode** is `vf_blend.c`'s expression in `int`, `top` the clip and `bottom` the composite: `multiply`
//!   `a * b / 255`, `screen` `255 - (255 - a) * (255 - b) / 255`, the two `hardlight`s, `addition`, `darken`,
//!   `lighten`, `difference`, `exclusion` (`a + b - 2 * a * b / 255`). The x86 builds have SIMD for most of these (SSE2 /
//!   AVX2, taken when the opacity is 1); they are the C expressions, measured over every pair of 8-bit values.
//!   Kerf's `Overlay` is FFmpeg's `hardlight` and its `HardLight` is `overlay` (the clip is the top input).
//! * **Out of RGB** (`planar_rgb_to_y`, `planar_rgb_to_uv`, swscale's tables from `fill_rgb2yuv_table`): luma
//!   `((ry r + gy g + by b + 0x80100) >> 9 + 32) >> 6`; **chroma is taken from the bottom row of each 2x2 block, the
//!   sum of its two pixels**: `((ru R + gu G + bu B + 0x800200) >> 10 + 32) >> 6` with `R` the sum of the pair's reds. (`neighbor`
//!   is a point sample, but the converter swscale picks for an RGB source it halves horizontally, `chrSrcHSubSample`, and
//!   the vertical point sample lands on the odd row.) Measured against every 8-bit RGB, both builds, both matrices.
//! * **The overlay** onto the composite is `blend255` per luma sample by the clip's alpha, and per chroma sample
//!   by the average of the block's four alphas ([`chroma_alpha`], the filter's own quirks at the last column and
//!   row), the compositor's own arithmetic for a translucent layer.
//!
//! Whether *this* FFmpeg does that is a fact about the build and the CPU (swscale's kernels differ between them): [`kernels`]
//! runs the chain, per mode and matrix, on noise and keeps the modes whose output came back as [`reference`] computes it
//! (`Compositor::caps` has no mode that was not), the way `grade::kernels` does for the grade.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use kerf_core::{BlendKinds, BlendMode, YuvMatrix};

/// swscale's integer tables for one matrix, limited range YCbCr to full range RGB and back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tables {
    /// `yuv2rgb_y_offset`, `yuv2rgb_y_coeff`, `yuv2rgb_v2r_coeff`, `v2g`, `u2g`, `u2b` (`ff_yuv2rgb_c_init_tables`).
    pub y_off: i32,
    pub y_coeff: i32,
    pub v2r: i32,
    pub v2g: i32,
    pub u2g: i32,
    pub u2b: i32,
    /// `input_rgb2yuv_table`: `ry gy by`, `ru gu bu`, `rv gv bv` (`fill_rgb2yuv_table`, 15 fractional bits).
    pub rgb2yuv: [i32; 9],
}

/// swscale's `roundToInt16`.
fn round_to_i16(v: i64) -> i32 {
    let r = (v + (1 << 15)) >> 16;
    if r < -0x7fff {
        -0x8000
    } else if r > 0x7fff {
        0x7fff
    } else {
        r as i32
    }
}

fn rounded_div(a: i64, b: i64) -> i64 {
    (if a >= 0 { a + (b >> 1) } else { a - (b >> 1) }) / b
}

impl Tables {
    /// The tables for `matrix`: `ff_yuv2rgb_coeffs` (`crv, cbu, cgu, cgv`) and what `sws_setColorspaceDetails` makes of them
    /// for limited range YCbCr against full range RGB.
    pub fn new(matrix: YuvMatrix) -> Self {
        let [crv, cbu, cgu, cgv]: [i64; 4] = match matrix {
            YuvMatrix::Bt601 => [104597, 132201, 25675, 53279],
            YuvMatrix::Bt709 => [117489, 138438, 13975, 34925],
            YuvMatrix::Bt2020 => [110013, 140363, 12277, 42626],
        };
        // `ff_yuv2rgb_c_init_tables` with contrast = saturation = 1 << 16, brightness 0, limited range source.
        let one: i64 = 1 << 16;
        let cy = (one * 255 / 219 * one) >> 16;
        let oy: i64 = 16 << 16;
        let scale = |c: i64| (c * one * one) >> 32;
        let (crv_s, cbu_s, cgu_s, cgv_s) = (scale(crv), scale(cbu), scale(-cgu), scale(-cgv));
        // `fill_rgb2yuv_table`: always the limited range table, and BT.601's own literals.
        let rgb2yuv = if matrix == YuvMatrix::Bt601 {
            let f = |x: f64| (x * f64::from(1 << 15) + 0.5) as i32;
            [
                f(0.299 * 219.0 / 255.0),
                f(0.587 * 219.0 / 255.0),
                f(0.114 * 219.0 / 255.0),
                -f(0.169 * 224.0 / 255.0),
                -f(0.331 * 224.0 / 255.0),
                f(0.500 * 224.0 / 255.0),
                f(0.500 * 224.0 / 255.0),
                -f(0.419 * 224.0 / 255.0),
                -f(0.081 * 224.0 / 255.0),
            ]
        } else {
            let (vr, ub, ug, vg) = (crv, cbu, -cgu, -cgv);
            let cy = one * 255 / 219;
            let w = rounded_div(one * one * ug, ub);
            let v = rounded_div(one * one * vg, vr);
            let z = one * one - w - v;
            let cy_ = rounded_div(cy * z, one);
            let cu = rounded_div(ub * z, one);
            let cv = rounded_div(vr * z, one);
            let s = 1i64 << 15;
            [
                -rounded_div(s * v, cy_),
                rounded_div(s * one * one, cy_),
                -rounded_div(s * w, cy_),
                rounded_div(s * v, cu),
                -rounded_div(s * one * one, cu),
                rounded_div(s * (z + w), cu),
                rounded_div(s * (v + z), cv),
                -rounded_div(s * one * one, cv),
                rounded_div(s * w, cv),
            ]
            .map(|x| x as i32)
        };
        Self {
            y_off: round_to_i16(oy * (1 << 9)),
            y_coeff: round_to_i16(cy * (1 << 13)),
            v2r: round_to_i16(crv_s * (1 << 13)),
            v2g: round_to_i16(cgv_s * (1 << 13)),
            u2g: round_to_i16(cgu_s * (1 << 13)),
            u2b: round_to_i16(cbu_s * (1 << 13)),
            rgb2yuv,
        }
    }

    /// One YCbCr sample to RGB: `yuv2gbrp_full_X_c`, in `i32` that wraps.
    pub fn yuv_to_rgb(&self, y: u8, u: u8, v: u8) -> [u8; 3] {
        let yv = i32::from(y) * 512;
        let uv = (i32::from(u) - 128) * 512;
        let vv = (i32::from(v) - 128) * 512;
        let luma = (yv - self.y_off).wrapping_mul(self.y_coeff).wrapping_add(1 << 21);
        let r = luma.wrapping_add(vv.wrapping_mul(self.v2r));
        let g = luma
            .wrapping_add(vv.wrapping_mul(self.v2g))
            .wrapping_add(uv.wrapping_mul(self.u2g));
        let b = luma.wrapping_add(uv.wrapping_mul(self.u2b));
        [r, g, b].map(|c| (c.clamp(0, (1 << 30) - 1) >> 22) as u8)
    }

    /// The luma of one RGB sample (`planar_rgb_to_y`, then the 14-bit to 8-bit step).
    pub fn luma(&self, rgb: [u8; 3]) -> u8 {
        let [r, g, b] = rgb.map(i32::from);
        let t = self.rgb2yuv[0] * r + self.rgb2yuv[1] * g + self.rgb2yuv[2] * b + (0x801 << 8);
        ((((t >> 9) + 32) >> 6).clamp(0, 255)) as u8
    }

    /// The chroma of a block from the pair of RGB samples in its bottom row (`planar_rgb_to_uv`'s horizontal-pair form).
    pub fn chroma(&self, pair: [[u8; 3]; 2]) -> [u8; 2] {
        let sum = |c: usize| i32::from(pair[0][c]) + i32::from(pair[1][c]);
        let (r, g, b) = (sum(0), sum(1), sum(2));
        let one = |k: usize| {
            let t = self.rgb2yuv[k] * r + self.rgb2yuv[k + 1] * g + self.rgb2yuv[k + 2] * b + (0x4001 << 9);
            (((t >> 10) + 32) >> 6).clamp(0, 255) as u8
        };
        [one(3), one(6)]
    }

    /// The tables as the shader's uniform holds them: the six conversion constants, then the nine of the way back.
    pub fn words(&self) -> [i32; 16] {
        let mut w = [0; 16];
        w[..6].copy_from_slice(&[self.y_off, self.y_coeff, self.v2r, self.v2g, self.u2g, self.u2b]);
        w[6..15].copy_from_slice(&self.rgb2yuv);
        w
    }
}

/// The mode's number in the shader. FFmpeg's `blend` has the clip as its top input, and kerf's `Overlay` is its `hardlight`.
pub fn mode_index(mode: BlendMode) -> Option<u32> {
    Some(match mode {
        BlendMode::Normal => return None,
        BlendMode::Multiply => 0,
        BlendMode::Screen => 1,
        BlendMode::Overlay => 2,
        BlendMode::HardLight => 3,
        BlendMode::Add => 4,
        BlendMode::Darken => 5,
        BlendMode::Lighten => 6,
        BlendMode::Difference => 7,
        BlendMode::Exclusion => 8,
    })
}

/// One channel of the blend: `vf_blend.c`'s expression, `top` the clip and `bottom` the composite.
pub fn channel(mode: BlendMode, top: u8, bottom: u8) -> u8 {
    let (a, b) = (i32::from(top), i32::from(bottom));
    let multiply = |x: i32, a: i32, b: i32| x * ((a * b) / 255);
    let screen = |x: i32, a: i32, b: i32| 255 - x * ((255 - a) * (255 - b) / 255);
    let v = match mode {
        BlendMode::Normal => a,
        BlendMode::Multiply => multiply(1, a, b),
        BlendMode::Screen => screen(1, a, b),
        // FFmpeg's `hardlight` (the clip on top: `B` decides).
        BlendMode::Overlay => {
            if b < 128 {
                multiply(2, b, a)
            } else {
                screen(2, b, a)
            }
        }
        // FFmpeg's `overlay`: `A` decides.
        BlendMode::HardLight => {
            if a < 128 {
                multiply(2, a, b)
            } else {
                screen(2, a, b)
            }
        }
        BlendMode::Add => 255.min(a + b),
        BlendMode::Darken => a.min(b),
        BlendMode::Lighten => a.max(b),
        BlendMode::Difference => (a - b).abs(),
        BlendMode::Exclusion => a + b - 2 * a * b / 255,
    };
    v as u8
}

/// `overlay`'s `FAST_DIV255(d * (255 - a) + s * a)`.
pub fn blend255(d: u8, s: u8, a: u8) -> u8 {
    ((((u32::from(d) * (255 - u32::from(a)) + u32::from(s) * u32::from(a)) + 128) * 257) >> 16) as u8
}

/// `overlay`'s alpha for the chroma sample `(bx, by)` of a layer that is `w` x `h` luma samples with a full alpha plane
/// (`blend_plane` for `yuva420p` over `yuv420p`): the four alphas of the block averaged, where it has a neighbour to its
/// right and below; the last chroma column and row average what they have, and **a last column ignores its own second luma
/// column**, a quirk of the filter.
pub fn chroma_alpha(alpha: &[u8], w: usize, h: usize, bx: usize, by: usize) -> u8 {
    let at = |x: usize, y: usize| u32::from(alpha[y * w + x]);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (x, y) = (bx * 2, by * 2);
    let (right, below) = (bx + 1 < cw, by + 1 < ch);
    if right && below {
        return ((at(x, y) + at(x + 1, y) + at(x, y + 1) + at(x + 1, y + 1)) >> 2) as u8;
    }
    let ah = if right { (at(x, y) + at(x + 1, y)) >> 1 } else { at(x, y) };
    let av = if below { (at(x, y) + at(x, y + 1)) >> 1 } else { at(x, y) };
    ((av + ah) >> 1) as u8
}

/// An 8-bit 4:2:0 picture, even sided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planes {
    pub w: usize,
    pub h: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Planes {
    pub fn from_frame(bytes: &[u8], w: usize, h: usize) -> Option<Self> {
        let (cw, ch) = (w / 2, h / 2);
        if bytes.len() != w * h + 2 * cw * ch {
            return None;
        }
        let (y, rest) = bytes.split_at(w * h);
        let (u, v) = rest.split_at(cw * ch);
        Some(Self {
            w,
            h,
            y: y.to_vec(),
            u: u.to_vec(),
            v: v.to_vec(),
        })
    }

    pub fn to_frame(&self) -> Vec<u8> {
        [&self.y[..], &self.u[..], &self.v[..]].concat()
    }
}

/// What the chain makes of `base` (the composite), `laid` (the composite with the clip laid on opaque) and the clip's
/// luma-resolution `alpha` plane: the composite after the blend, plane for plane. `base` and `laid` are one even size.
pub fn reference(mode: BlendMode, matrix: YuvMatrix, base: &Planes, laid: &Planes, alpha: &[u8]) -> Planes {
    let tables = Tables::new(matrix);
    let (w, h) = (base.w, base.h);
    let cw = w / 2;
    let rgb_at =
        |p: &Planes, x: usize, y: usize| tables.yuv_to_rgb(p.y[y * w + x], p.u[(y / 2) * cw + x / 2], p.v[(y / 2) * cw + x / 2]);
    let mixed = |x: usize, y: usize| -> [u8; 3] {
        let (top, bottom) = (rgb_at(laid, x, y), rgb_at(base, x, y));
        [0, 1, 2].map(|c| channel(mode, top[c], bottom[c]))
    };
    let mut out = base.clone();
    for y in 0..h {
        for x in 0..w {
            let luma = tables.luma(mixed(x, y));
            out.y[y * w + x] = blend255(base.y[y * w + x], luma, alpha[y * w + x]);
        }
    }
    for by in 0..h / 2 {
        for bx in 0..cw {
            let pair = [mixed(bx * 2, by * 2 + 1), mixed(bx * 2 + 1, by * 2 + 1)];
            let [u, v] = tables.chroma(pair);
            let a = chroma_alpha(alpha, w, h, bx, by);
            out.u[by * cw + bx] = blend255(base.u[by * cw + bx], u, a);
            out.v[by * cw + bx] = blend255(base.v[by * cw + bx], v, a);
        }
    }
    out
}

// ---- does this FFmpeg draw the chain as the model does? ---------------------------------------------------------------

/// The probe picture's size.
const PROBE: (usize, usize) = (48, 32);
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Every mode the compositor draws, in the probe's order.
const MODES: [BlendMode; 9] = [
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Overlay,
    BlendMode::HardLight,
    BlendMode::Add,
    BlendMode::Darken,
    BlendMode::Lighten,
    BlendMode::Difference,
    BlendMode::Exclusion,
];

/// The matrices the probe tries: what `blend_chains` names for a composite that is untagged (BT.601 by default) or tagged.
const MATRICES: [(YuvMatrix, Option<&str>); 2] = [(YuvMatrix::Bt601, None), (YuvMatrix::Bt709, Some("bt709"))];

struct State {
    measured: Option<[BlendKinds; 2]>,
    failures: u32,
    retry_at: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State {
    measured: None,
    failures: 0,
    retry_at: None,
});

fn lock() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The modes this FFmpeg draws as the model does, **per matrix**: `[BT.601, BT.709]` (BT.2020 stacks are refused). The first call runs the probe (a few
/// short `ffmpeg` children, bounded), on a blocking thread, ahead of the frame that needs it ([`crate::prepare`]); the
/// answer is kept for good. A probe that could not run (`None` in the cache) is retried after a backoff and until then
/// draws nothing.
pub fn kernels() -> [BlendKinds; 2] {
    {
        let s = lock();
        if let Some(k) = s.measured {
            return k;
        }
        if s.retry_at.is_some_and(|t| Instant::now() < t) {
            return [BlendKinds::NONE; 2];
        }
    }
    let measured = measure();
    let mut s = lock();
    if let Some(k) = s.measured {
        return k;
    }
    match measured {
        Some(kinds) => {
            if kinds.iter().any(|k| *k != BlendKinds::ALL) {
                tracing::warn!(
                    ?kinds,
                    "this FFmpeg's blend chain is not the one the GPU follows for some modes; they are drawn by FFmpeg"
                );
            }
            s.measured = Some(kinds);
            kinds
        }
        None => {
            s.failures += 1;
            let wait = Duration::from_secs((5u64 << s.failures.saturating_sub(1).min(6)).min(300));
            s.retry_at = Some(Instant::now() + wait);
            tracing::warn!(
                failures = s.failures,
                retry_in_secs = wait.as_secs(),
                "could not measure FFmpeg's blend chain; blended layers are not drawn on the GPU until it can"
            );
            [BlendKinds::NONE; 2]
        }
    }
}

/// What is known without running anything: [`kernels`]'s answer once there is one, else nothing.
pub fn measured() -> [BlendKinds; 2] {
    lock().measured.unwrap_or([BlendKinds::NONE; 2])
}

/// Pins for tests and for a host that knows its FFmpeg: every mode on every matrix.
pub fn all_modes() -> [BlendKinds; 2] {
    [BlendKinds::ALL; 2]
}

/// A scratch directory holding the probe's pictures, removed when dropped.
struct ProbeDir(PathBuf);

impl ProbeDir {
    fn new() -> std::io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kerf-gpu-blend-probe-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for ProbeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Integer noise that does not depend on the platform: a xorshift stream.
struct Noise(u64);

impl Noise {
    fn byte(&mut self) -> u8 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 24) as u8
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }
}

/// The probe's pictures: the composite (noise in all three planes), the composite with the clip laid on it (more noise)
/// and the clip's alpha plane (noise, with runs of 0 and 255 so that both of `overlay`'s shortcuts are met).
fn probe_pictures() -> (Planes, Planes, Vec<u8>) {
    let (w, h) = PROBE;
    let mut noise = Noise(0x2545_f491_4f6c_dd1d);
    let frame = |noise: &mut Noise| Planes {
        w,
        h,
        y: noise.bytes(w * h),
        u: noise.bytes(w * h / 4),
        v: noise.bytes(w * h / 4),
    };
    let base = frame(&mut noise);
    let laid = frame(&mut noise);
    (base, laid, probe_alpha(&mut noise, w * h))
}

fn probe_alpha(noise: &mut Noise, n: usize) -> Vec<u8> {
    noise
        .bytes(n)
        .into_iter()
        .enumerate()
        .map(|(i, a)| match i % 7 {
            0 => 0,
            1 => 255,
            _ => a,
        })
        .collect()
}

/// The clip the placement probe lays: a `yuva420p` picture smaller than the composite, noise in all four planes.
const CLIP: (usize, usize) = (32, 16);

/// Where the probe lays the clip, in luma samples (even: the layer's origin is): off each edge, off a corner, and inside.
const PLACES: [(i32, i32); 6] = [(-8, -6), (8, -4), (-6, 4), (20, 10), (16, 20), (4, 6)];

/// The composite with `clip` (`yuv420p` planes and an alpha plane) laid on `base` at `(x, y)` as the chain lays it: the clip's colours
/// held to the legal range by `lutyuv`'s `clipval` (luma 16 to 235, chroma 16 to 240) and put where it lies, chroma by blocks; and
/// the clip's alpha there and 0 elsewhere. `x` and `y` are even.
fn lay(base: &Planes, clip: &Planes, alpha: &[u8], x: i32, y: i32) -> (Planes, Vec<u8>) {
    let mut laid = base.clone();
    let mut plane_alpha = vec![0u8; base.w * base.h];
    for cy in 0..clip.h {
        for cx in 0..clip.w {
            let (dx, dy) = (x + cx as i32, y + cy as i32);
            if (0..base.w as i32).contains(&dx) && (0..base.h as i32).contains(&dy) {
                let (dx, dy) = (dx as usize, dy as usize);
                laid.y[dy * base.w + dx] = clip.y[cy * clip.w + cx].clamp(16, 235);
                plane_alpha[dy * base.w + dx] = alpha[cy * clip.w + cx];
            }
        }
    }
    let (cw, ccw) = (base.w / 2, clip.w / 2);
    for ky in 0..clip.h / 2 {
        for kx in 0..ccw {
            let (dx, dy) = (x / 2 + kx as i32, y / 2 + ky as i32);
            if (0..cw as i32).contains(&dx) && (0..(base.h / 2) as i32).contains(&dy) {
                let (d, c) = (dy as usize * cw + dx as usize, ky * ccw + kx);
                laid.u[d] = clip.u[c].clamp(16, 240);
                laid.v[d] = clip.v[c].clamp(16, 240);
            }
        }
    }
    (laid, plane_alpha)
}

/// The graph of one mode and matrix: `[0]` the clip laid on the composite, `[1]` its alpha plane, `[2]` the composite. A tagged
/// composite (the bottom layer's matrix, which FFmpeg 9 carries through the overlays: it converts the blend's output into it at the
/// last `overlay` otherwise) is tagged here too, as the export's frames are.
fn graph(mode: BlendMode, tag: Option<&str>) -> Option<String> {
    let name = mode.ffmpeg()?;
    let tagged = tag.map_or_else(String::new, |t| format!("setparams=colorspace={t}:range=tv,"));
    Some(format!(
        "[0:v]{tagged}null[cc];[1:v]null[aa];[2:v]{tagged}split[b][m];{}",
        kerf_core::blend_colour_graph(name, tag)
    ))
}

/// The whole chain, the clip placed by the export's own `overlay`: `[0]` the composite, `[1]` the clip (`yuva420p`).
fn placed_graph(mode: BlendMode, tag: Option<&str>, (x, y): (i32, i32)) -> Option<String> {
    let name = mode.ffmpeg()?;
    let tagged = tag.map_or_else(String::new, |t| format!("setparams=colorspace={t}:range=tv,"));
    Some(format!(
        "[0:v]{tagged}format=yuv420p[base];[1:v]{tagged}format=yuva420p[clip];{}",
        kerf_core::blend_probe_graph(name, &format!("overlay=x={x}:y={y}:eof_action=pass"), tag)
    ))
}

/// One child of the probe: a graph, the pictures it reads, and the picture it must come back as.
struct Job {
    /// The matrix (an index of [`MATRICES`]) it is a probe of.
    matrix: usize,
    /// A mode of the colour chain, or `None` for a placement (which holds for every mode).
    mode: Option<BlendMode>,
    graph: String,
    inputs: Vec<(&'static str, &'static str, (usize, usize))>,
    want: Planes,
}

fn measure() -> Option<[BlendKinds; 2]> {
    let dir = ProbeDir::new().ok()?;
    let (w, h) = PROBE;
    let (base, laid, alpha) = probe_pictures();
    let mut noise = Noise(0x1234_5678_9abc_def1);
    let clip = Planes {
        w: CLIP.0,
        h: CLIP.1,
        y: noise.bytes(CLIP.0 * CLIP.1),
        u: noise.bytes(CLIP.0 * CLIP.1 / 4),
        v: noise.bytes(CLIP.0 * CLIP.1 / 4),
    };
    let clip_alpha = probe_alpha(&mut noise, CLIP.0 * CLIP.1);
    let files = [
        ("laid.yuv", laid.to_frame()),
        ("alpha.gray", alpha.clone()),
        ("base.yuv", base.to_frame()),
        ("clip.yuva", [clip.to_frame(), clip_alpha.clone()].concat()),
    ];
    for (name, data) in &files {
        std::fs::write(dir.0.join(name), data).ok()?;
    }
    let mut jobs: Vec<Job> = Vec::new();
    for (mi, (matrix, tag)) in MATRICES.iter().enumerate() {
        for mode in MODES {
            let Some(graph) = graph(mode, *tag) else { continue };
            jobs.push(Job {
                matrix: mi,
                mode: Some(mode),
                graph,
                inputs: vec![
                    ("laid.yuv", "yuv420p", PROBE),
                    ("alpha.gray", "gray", PROBE),
                    ("base.yuv", "yuv420p", PROBE),
                ],
                want: reference(mode, *matrix, &base, &laid, &alpha),
            });
        }
        // The clip placed by the export's own `overlay` (a frame with an alpha plane laid on one that has none, and its alpha laid on a
        // transparent one), here and there on and off the composite: FFmpeg 4.4 draws nothing for a clip above the frame's top edge.
        let places: &[(i32, i32)] = if mi == 0 { &PLACES } else { &PLACES[..1] };
        for place in places {
            let mode = BlendMode::Multiply;
            let Some(graph) = placed_graph(mode, *tag, *place) else {
                continue;
            };
            let (laid, plane_alpha) = lay(&base, &clip, &clip_alpha, place.0, place.1);
            jobs.push(Job {
                matrix: mi,
                mode: None,
                graph,
                inputs: vec![("base.yuv", "yuv420p", PROBE), ("clip.yuva", "yuva420p", CLIP)],
                want: reference(mode, *matrix, &base, &laid, &plane_alpha),
            });
        }
    }
    let share = jobs.len();
    let outputs: Vec<Option<Vec<u8>>> = std::thread::scope(|scope| {
        let runs: Vec<_> = jobs
            .iter()
            .map(|job| {
                let dir = &dir.0;
                scope.spawn(move || {
                    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin"].map(String::from).to_vec();
                    for (name, pix, (iw, ih)) in &job.inputs {
                        args.extend(["-f", "rawvideo", "-pix_fmt", pix, "-s", &format!("{iw}x{ih}"), "-i"].map(String::from));
                        args.push(dir.join(name).to_string_lossy().into_owned());
                    }
                    args.extend(["-filter_complex".to_string(), job.graph.clone()]);
                    args.extend(
                        [
                            "-map",
                            "[out]",
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
                    kerf_core::limit_frame_args(&mut args, share);
                    if job.mode.is_none() {
                        // FFmpeg 4.4's `overlay` slices a clip above the frame's top edge wrongly when it runs on several threads
                        // (its alpha plane loses rows), as an export's graph does: the placement is probed with them.
                        for flag in ["-filter_threads", "-filter_complex_threads"] {
                            while let Some(at) = args.iter().position(|a| a == flag) {
                                args.drain(at..=(at + 1).min(args.len() - 1));
                            }
                            args.splice(0..0, [flag.to_string(), "8".to_string()]);
                        }
                    }
                    let mut cmd = kerf_core::ffmpeg_command();
                    cmd.args(&args);
                    crate::source::run_child(cmd, w * h * 3 / 2 + 1, PROBE_TIMEOUT).ok()
                })
            })
            .collect();
        runs.into_iter().map(|run| run.join().ok().flatten()).collect()
    });
    let mut kinds = [BlendKinds::ALL; 2];
    let mut unknown = false;
    for (job, out) in jobs.iter().zip(&outputs) {
        let Some(got) = out.as_deref().and_then(|o| Planes::from_frame(o, w, h)) else {
            unknown = true;
            continue;
        };
        if got != job.want {
            kinds[job.matrix] = match job.mode {
                Some(mode) => kinds[job.matrix].without(BlendKinds::of(mode)),
                None => BlendKinds::NONE,
            };
        }
    }
    // A difference is conclusive wherever the rest stands; a child that did not answer is not.
    (!unknown || kinds.iter().any(|k| *k != BlendKinds::ALL)).then_some(kinds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tables_are_what_swscale_computes() {
        // `ff_yuv2rgb_c_init_tables` for BT.601, and `fill_rgb2yuv_table`'s literals (checked against both builds).
        let t = Tables::new(YuvMatrix::Bt601);
        assert_eq!(
            (t.y_off, t.y_coeff, t.v2r, t.v2g, t.u2g, t.u2b),
            (8192, 9539, 13075, -6660, -3209, 16525)
        );
        assert_eq!(t.rgb2yuv, [8414, 16519, 3208, -4865, -9528, 14392, 14392, -12061, -2332]);
        let t = Tables::new(YuvMatrix::Bt709);
        assert_eq!((t.v2r, t.v2g, t.u2g, t.u2b), (14686, -4366, -1747, 17305),);
        assert_eq!(t.rgb2yuv, [5983, 20127, 2032, -3298, -11094, 14392, 14392, -13073, -1320]);
    }

    #[test]
    fn black_white_and_a_saturated_blue_are_what_the_converter_makes_of_them() {
        let t = Tables::new(YuvMatrix::Bt601);
        assert_eq!(t.yuv_to_rgb(16, 128, 128), [0, 0, 0]);
        assert_eq!(t.yuv_to_rgb(235, 128, 128), [255, 255, 255]);
        // Just inside 32 bits a saturated blue is 255; a level more and the sum overflows: the wrap is the filter's, and it
        // clips to 0 where a saturation would say 255.
        assert_eq!(t.yuv_to_rgb(235, 255, 128)[2], 255);
        assert_eq!(t.yuv_to_rgb(237, 254, 128)[2], 0);
        assert_eq!(t.luma([0, 0, 0]), 16);
        assert_eq!(t.luma([255, 255, 255]), 235);
        assert_eq!(t.chroma([[255, 255, 255], [255, 255, 255]]), [128, 128]);
    }

    #[test]
    fn every_mode_is_the_c_expression() {
        use BlendMode::*;
        // (top, bottom) -> out, from vf_blend.c with MAX 255.
        for (mode, top, bottom, out) in [
            (Multiply, 200, 100, 78),
            (Screen, 200, 100, 222),
            (Overlay, 200, 100, 156),
            (Overlay, 100, 200, 255 - 2 * (55 * 155 / 255)),
            (HardLight, 100, 200, 2 * (100 * 200 / 255)),
            (HardLight, 200, 100, 255 - 2 * (55 * 155 / 255)),
            (Add, 200, 100, 255),
            (Darken, 200, 100, 100),
            (Lighten, 200, 100, 200),
            (Difference, 100, 200, 100),
            (Exclusion, 200, 100, 200 + 100 - 2 * 200 * 100 / 255),
        ] {
            assert_eq!(channel(mode, top, bottom), out as u8, "{mode:?} {top} {bottom}");
        }
        // Nothing leaves the byte.
        for mode in MODES {
            for a in 0..=255u8 {
                for b in (0..=255u8).step_by(5) {
                    let v = i32::from(channel(mode, a, b));
                    assert!((0..=255).contains(&v));
                }
            }
        }
    }

    #[test]
    fn overlay_is_exact_at_zero_and_full_alpha() {
        for d in 0..=255u8 {
            assert_eq!(blend255(d, 77, 0), d);
            assert_eq!(blend255(d, 77, 255), 77);
        }
    }

    #[test]
    fn the_chroma_alpha_of_the_last_column_ignores_its_second_luma_column() {
        // 4x4 luma: two chroma columns and rows.
        #[rustfmt::skip]
        let alpha = [
            10, 20, 100, 200,
            30, 40, 60, 80,
            1, 2, 3, 4,
            5, 6, 7, 8,
        ];
        // An inner block: the four alphas, `>> 2`.
        assert_eq!(chroma_alpha(&alpha, 4, 4, 0, 0), ((10 + 20 + 30 + 40) >> 2) as u8);
        // The last column: only the block's first luma column, and (it has a row below) averaged with the row under it.
        let (ah, av) = (100, (100 + 60) >> 1);
        assert_eq!(chroma_alpha(&alpha, 4, 4, 1, 0), ((av + ah) >> 1) as u8);
        // The last row: its right neighbour is averaged, and nothing below.
        let (ah, av) = ((1 + 2) >> 1, 1);
        assert_eq!(chroma_alpha(&alpha, 4, 4, 0, 1), ((av + ah) >> 1) as u8);
        // The corner: the first sample alone.
        assert_eq!(chroma_alpha(&alpha, 4, 4, 1, 1), 3);
    }

    #[test]
    fn the_probe_graph_is_the_exports_text() {
        let g = graph(BlendMode::Overlay, Some("bt709")).expect("a mode");
        assert!(
            g.contains("blend=c0_mode=hardlight:c1_mode=hardlight:c2_mode=hardlight"),
            "{g}"
        );
        assert!(g.contains("in_color_matrix=bt709:in_range=tv"), "{g}");
        assert!(graph(BlendMode::Normal, None).is_none());
    }
}
