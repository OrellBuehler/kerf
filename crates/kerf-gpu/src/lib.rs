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
//!   linear-light round trip to disagree about.
//! * **The matrix is fixed, not read from the stream.** The composite is
//!   converted with **BT.601, limited range** ([`kerf_core::YuvMatrix::Bt601`], a
//!   field of the plan): the composited frame carries no colorspace tag, swscale
//!   reads that as BT.601, and a BT.709 source gets BT.601 coefficients in
//!   FFmpeg's still too (measured: tagged BT.709, tagged BT.601 and untagged files
//!   convert identically, on FFmpeg 6.1 and 9.0). Matching it is the point; the
//!   stream's matrix is used in exactly one place, the RGB round trip of a
//!   translucent layer (below), because there FFmpeg converts *the layer* with its
//!   frame's own tag. Chroma is replicated 2x2 at the final conversion, as
//!   swscale's unscaled path does, not interpolated. Sources are asked for limited
//!   range explicitly (`scale=out_range=tv`): FFmpeg 9 no longer converts a
//!   full-range JPEG for `-pix_fmt` alone.
//! * **`eq`.** Colour correction runs on the Y / U / V planes through vf_eq's own
//!   tables ([`eq`]), before any RGB exists — it is not an RGB operation, and
//!   Kerf's "temperature" is a power function on the chroma planes. The black bars
//!   of a letterbox are part of the frame it sees ([`geometry`]: `pad` hands
//!   `overlay` a full-canvas frame), so they are graded too and cover what is below.
//! * **Scaler.** swscale's bicubic ([`sws`]): its filter tables ported from
//!   `initFilter` — the integer weights, the window start truncated toward zero,
//!   the border folding, the near-zero tap trimming — and its integer arithmetic
//!   in the shader (15-bit horizontal intermediate clipped at the top, vertical
//!   pass rounded at bit 19). A formula for "bicubic" is not enough: the table's
//!   quirks show along every border, and busy footage shows them everywhere.
//!   Planes are scaled independently at their own size and rounded to 8 bits
//!   between stages, like FFmpeg's filters. The committed evidence is
//!   `tests/parity.rs::the_scaler_matches_ffmpegs_scale_plane_by_plane`: every
//!   plane within one level of `ffmpeg -vf scale`, on test patterns, noise and a
//!   checkerboard, scaled down and up, on FFmpeg 6.1 and 9.0.
//! * **Opacity below 1** takes FFmpeg's RGB round trip ([`roundtrip`]): the layer
//!   goes `yuva420p -> argb -> yuva420p`, because `colorchannelmixer` only takes
//!   RGB, with its own matrix out and BT.601 back, and an alpha plane that ends up
//!   at `round(lrint(255 * op) * 256 / 255)` ([`geometry::ffmpeg_alpha`]).
//!   Reproduced in integer arithmetic; a translucent layer of odd size is refused.
//! * **Geometry.** FFmpeg's integer rounding (`crop`, `scale`, `pad`, `overlay`,
//!   `rotate`) is reproduced in [`geometry`], so layers land on the same pixels —
//!   including the chroma block an odd layer's last pixel shares with the pixel
//!   just past it.
//! * **What the GPU does not draw is refused, loudly.** Per frame the plan says no
//!   (`gpu_supported`), and the decode and the compositor refuse what only they can
//!   see ([`GpuError::Unsupported`]): a picture that decodes at another size than
//!   the probe said (an EXIF orientation), an alpha channel, a ratio the scaler
//!   needs two passes for, a translucent odd layer. The caller's answer to every
//!   one is the FFmpeg path.
//! * **A GPU failure is an error, not a panic.** wgpu panics by default on a bad
//!   call; here every unit of GPU work runs in error scopes and a lost device is
//!   tracked ([`gpu`]). After [`GpuError::DeviceLost`] the owner builds a new
//!   [`Gpu`] and a new [`Compositor`] on it.

pub mod compositor;
pub mod eq;
pub mod geometry;
pub mod gpu;
pub mod roundtrip;
pub mod source;
pub mod sws;

pub use compositor::{Compositor, RenderTimings, RgbaFrame};
pub use gpu::{Gpu, GpuError, GpuOptions};
pub use source::{decode_args, decode_layer, decode_layers, YuvFrame};
