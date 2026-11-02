//! The placement pass's scalar reference: FFmpeg's `pad`, `perspective` (`interpolation=linear`) and
//! `crop` over a `yuva420p` picture, sample by sample, as `vf_perspective.c`'s `resample_linear` does it.
//!
//! The tables of sample positions are `kerf_core::perspective`'s ([`PlacementMap`]: the filter's matrix in
//! `f64` and its `lrint`); this is the resample that reads them. A plane's sample takes the table entry of
//! its top-left luma pixel (`pv[(x << hsub) + (y << vsub) * w]`, then `>> hsub`: no half-sample centring),
//! splits it into a whole part and 8 bits of fraction, and mixes the four neighbours with integer weights
//! (`(sub_v_inv * (sub_u_inv * a + sub_u * b) + sub_v * (...) + 2^15) >> 16`). Reads outside the padded
//! frame are clamped to its edge, which is what the filter's two edge branches come to (each ignores the
//! fraction of the axis it clamps, and a clamped pair of taps is one sample, so the general formula over
//! clamped taps gives the same number). Past the picture the frame is `pad`'s: `black@0`, that is
//! Y 16, U = V = 128 and alpha 0.
//!
//! `src/shaders/placement.wgsl` is this function on the GPU; `tests/placement.rs` holds this one to the
//! real filters (both FFmpegs) and the shader to this one.

use kerf_core::perspective::{PlacementFrame, PlacementMap, SUB_PIXEL_BITS};

/// `pad=color=black@0` on `yuva420p`.
pub const PAD_Y: u8 = 16;
pub const PAD_C: u8 = 128;
pub const PAD_A: u8 = 0;

/// A 4:2:0 picture with an alpha plane (or one uniform alpha) as the placement receives it.
#[derive(Debug, Clone, Copy)]
pub struct Picture<'a> {
    pub size: (u32, u32),
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    /// The alpha plane (luma-sized); `None` is the opaque `format=yuva420p` adds, `opaque` everywhere.
    pub a: Option<&'a [u8]>,
    pub opaque: u8,
}

/// The canvas-sized planes the placement hands `overlay`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub a: Vec<u8>,
}

/// The chroma plane's side for a luma side.
fn half(n: u32) -> u32 {
    n.div_ceil(2)
}

/// The padded frame's plane `plane` (0 Y, 1 U, 2 V, 3 A) at `(x, y)` of that plane, `pad`'s colour past the picture.
fn padded(frame: &PlacementFrame, pic: &Picture, plane: usize, x: u32, y: u32) -> u8 {
    let sub = u32::from(plane == 1 || plane == 2);
    let (mx, my) = (frame.margin.0 >> sub, frame.margin.1 >> sub);
    let (pw, ph) = (frame.picture.0 >> sub, frame.picture.1 >> sub);
    let (fill, stride) = match plane {
        0 => (PAD_Y, pic.size.0),
        1 | 2 => (PAD_C, half(pic.size.0)),
        _ => (PAD_A, pic.size.0),
    };
    if x < mx || y < my || x >= mx + pw || y >= my + ph {
        return fill;
    }
    let at = ((y - my) * stride + (x - mx)) as usize;
    match plane {
        0 => pic.y[at],
        1 => pic.u[at],
        2 => pic.v[at],
        _ => pic.a.map_or(pic.opaque, |a| a[at]),
    }
}

/// One plane of the placement. `plane` is 0 Y, 1 U, 2 V, 3 alpha; the result is `canvas` (halved for U and V).
fn place_plane(map: &PlacementMap, pic: &Picture, plane: usize) -> Vec<u8> {
    let frame = &map.frame;
    let sub = u32::from(plane == 1 || plane == 2);
    let (fw, fh) = (frame.frame.0 >> sub, frame.frame.1 >> sub);
    let (ow, oh) = (frame.canvas.0 >> sub, frame.canvas.1 >> sub);
    let one = 1i32 << SUB_PIXEL_BITS;
    let mut out = Vec::with_capacity(ow as usize * oh as usize);
    for y in 0..oh {
        for x in 0..ow {
            // The table is the luma plane's, and a chroma sample reads the entry of its top-left luma pixel.
            let (u, v) = map.entry(x << sub, y << sub);
            let (u, v) = (u >> sub, v >> sub);
            let (su, sv) = (u & (one - 1), v & (one - 1));
            let (iu, iv) = (u >> SUB_PIXEL_BITS, v >> SUB_PIXEL_BITS);
            let tap = |dx: i32, dy: i32| {
                let tx = (iu + dx).clamp(0, fw as i32 - 1) as u32;
                let ty = (iv + dy).clamp(0, fh as i32 - 1) as u32;
                i32::from(padded(frame, pic, plane, tx, ty))
            };
            let (a, b, c, d) = (tap(0, 0), tap(1, 0), tap(0, 1), tap(1, 1));
            let sum = (one - sv) * ((one - su) * a + su * b) + sv * ((one - su) * c + su * d);
            out.push(((sum + (1 << (2 * SUB_PIXEL_BITS - 1))) >> (2 * SUB_PIXEL_BITS)).clamp(0, 255) as u8);
        }
    }
    out
}

/// The picture placed by `map`: what `pad,perspective,crop` hand the `overlay`.
pub fn reference(map: &PlacementMap, pic: &Picture) -> Placed {
    Placed {
        y: place_plane(map, pic, 0),
        u: place_plane(map, pic, 1),
        v: place_plane(map, pic, 2),
        a: place_plane(map, pic, 3),
    }
}
