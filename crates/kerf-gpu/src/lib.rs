//! `kerf-gpu` — Kerf's wgpu compositor.
//!
//! A [`kerf_core::RenderPlan`] says what is on screen at a timeline time;
//! [`Compositor`] draws it on the GPU and reads the pixels back. FFmpeg stays the
//! source of truth until parity is proven: the plan's `gpu_supported()` decides
//! per frame whether the GPU draws it at all, and `tests/parity.rs` holds the
//! compositor to FFmpeg's own still of the same timeline at the same time.
//!
//! # Decisions the parity harness forced
//!
//! * **Colour.** FFmpeg's still blends `yuv420p` pictures *in YUV* (`overlay` mixes
//!   the encoded values directly) onto a `yuv420p` black base and converts the
//!   single result to RGB once. The compositor does the same: its canvas holds
//!   Y, U, V and alpha, each layer is brought to 8-bit planes exactly as FFmpeg
//!   would, blended per channel, and the finished frame is converted to RGB once.
//!   (A first version converted each layer to RGB and blended there — equal for
//!   in-gamut colour, wrong for the legal-but-out-of-gamut YUV that saturated
//!   test patterns and super-whites are, because the clamp comes before the blend.)
//!   Everything stays in encoded gamma, FFmpeg's own space: there is no
//!   linear-light round trip to disagree about. The matrix is **BT.601 limited
//!   range**, not the stream's: the composited frame carries no colorspace tag,
//!   swscale reads that as BT.601, and a BT.709 source gets BT.601 coefficients in
//!   FFmpeg's still too (measured: tagged BT.709, tagged BT.601 and untagged
//!   files convert identically, on FFmpeg 6.1 and 9.0). Matching it is the point;
//!   the matrix is a field of the plan so changing the policy later is one line.
//!   Chroma is replicated 2x2 at that final conversion, as swscale's unscaled
//!   path does, not interpolated. Sources are asked for limited range explicitly
//!   (`scale=out_range=tv`): FFmpeg 9 no longer converts a full-range JPEG for
//!   `-pix_fmt` alone.
//! * **`eq`.** Colour correction runs on the Y / U / V planes through vf_eq's own
//!   tables ([`eq`]), before any RGB exists — it is not an RGB operation, and
//!   Kerf's "temperature" is a power function on the chroma planes.
//! * **Scaler.** swscale's bicubic (B = 0, C = 0.6), implemented in the shader as
//!   two separable passes per plane with the kernel stretched when shrinking.
//!   Verified against `ffmpeg -vf scale` on luma and chroma, odd sizes included,
//!   to within one level — cheap enough, and the only way a downscale's edges
//!   agree. Planes are scaled independently at their own size and rounded to 8
//!   bits between stages, like FFmpeg's filters.
//! * **Geometry.** FFmpeg's integer rounding (`crop`, `scale`, `pad`, `overlay`,
//!   `rotate`) is reproduced in [`geometry`], so layers land on the same pixels.

pub mod compositor;
pub mod eq;
pub mod geometry;
pub mod gpu;
pub mod source;

pub use compositor::{Compositor, RenderTimings, RgbaFrame};
pub use gpu::{Gpu, GpuError, GpuOptions};
pub use source::{decode_args, decode_layer, decode_layers, YuvFrame};
