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
//! * **The composite's matrix is the FFmpeg's, probed.** The conversion to RGB is
//!   limited range with the matrix of [`kerf_core::YuvMatrix`], a field of the plan,
//!   and which matrix FFmpeg uses depends on the FFmpeg. [`kerf_core::composite_color_policy`]
//!   renders a tagged and an untagged clip through the real still graph once per
//!   process and reads which behaviour it has ([`kerf_core::CompositeColorPolicy`]):
//!   - **`FixedBt601`** (FFmpeg 6.1: the black base carries no colourspace, so the
//!     composite is read as BT.601 whatever the layers were tagged — BT.709, BT.601
//!     and untagged files convert identically);
//!   - **`BottomLayerTag`** (FFmpeg 9.0: colourspace is negotiated along the overlay
//!     chain, the bottom layer's tag becomes the composite's, and a layer tagged
//!     otherwise is converted into it by a scaler stage). The compositor draws a
//!     stack whose layers share one matrix class with that matrix — BT.709, BT.2020,
//!     or BT.601 (`smpte170m`, `bt470bg` and untagged are one class) — and **refuses
//!     a mixed stack**, a layer whose matrix is unknown, and an RGB picture in a
//!     stack that is not BT.601, because the arithmetic of the conversion into the
//!     bottom layer's matrix was not reproduced (float and fixed-point models of it
//!     were off by up to 26 levels).
//!
//!   Everything is judged by the plan, from the policy, before anything is decoded.
//!   An asset that never recorded its pixel format has an unknown matrix and is
//!   treated like a mixed tag wherever the matrix matters. Chroma is replicated 2x2
//!   at the final conversion, as swscale's unscaled path does, not interpolated.
//!   Sources are asked for limited range explicitly (`scale=out_range=tv`): FFmpeg 9
//!   no longer converts a full-range JPEG for `-pix_fmt` alone.
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
//!   plane within one level of `ffmpeg -vf scale` up to about 4:1, scaled down and
//!   up, within two to three levels on 8:1 to 20:1 downscales of noise and test
//!   patterns and within five at 40:1 (x86 swscale's vertical scaler is not
//!   bit-exact with the C one this follows), identically on FFmpeg 6.1 and 9.0. A
//!   shrink steeper than 40:1 ([`kerf_core::MAX_SHRINK`]) is not measured and is
//!   refused. FFmpeg scales a picture in the
//!   format it has; the decode reduces it to 8-bit 4:2:0 first, which agrees for a
//!   shrink and not for an **enlargement** of a picture that is not 4:2:0 (a 2x
//!   enlargement of 4:4:4 is 27 levels off, of RGB 69), so the plan refuses that
//!   from the pixel format.
//! * **Opacity below 1** takes FFmpeg's RGB round trip ([`roundtrip`]): the layer
//!   goes `yuva420p -> argb -> yuva420p`, because `colorchannelmixer` only takes
//!   RGB. Out of YCbCr with the layer's own matrix (swscale's converter, exact), back
//!   with the composite's (luma exact, chroma within a level of the pair-averaged,
//!   vertically scaled one), and an alpha plane that ends up at
//!   `round(lrint(255 * op) * 256 / 255)` ([`geometry::ffmpeg_alpha`]). Reproduced
//!   in integer arithmetic; a translucent layer of odd size is refused, as is one
//!   whose matrix is unknown.
//! * **Geometry.** FFmpeg's integer rounding (`crop`, `scale`, `pad`, `overlay`,
//!   `rotate`) is reproduced in [`geometry`], so layers land on the same pixels —
//!   including the chroma block an odd layer's last pixel shares with the pixel
//!   just past it.
//! * **What the GPU does not draw is refused, loudly.** Per frame the plan says no
//!   ([`kerf_core::RenderPlan::gpu_supported_at`], which also takes the render size:
//!   an enlargement of a picture whose chroma is not 4:2:0, a translucent layer of
//!   odd size) and the decode and the compositor refuse what only they can see
//!   ([`GpuError::Unsupported`]): a picture that decodes at another size than the
//!   probe said (an EXIF orientation), a pixel format that is not on the allow-list
//!   of known-opaque ones, a ratio the scaler needs two passes for. The caller's
//!   answer to every one is the FFmpeg path.
//! * **A GPU failure is an error, not a panic.** wgpu panics by default on a bad
//!   call; here every unit of GPU work runs in error scopes and a lost device is
//!   tracked ([`gpu`]). After [`GpuError::DeviceLost`] the owner builds a new
//!   [`Gpu`] and a new [`Compositor`] on it. Reading the frame back waits a bounded
//!   time (`READBACK_TIMEOUT`) and reports a timeout instead of hanging the caller.

pub mod compositor;
pub mod eq;
pub mod gpu;
pub mod roundtrip;
pub mod source;
pub mod sws;

pub use compositor::{Compositor, RenderTimings, RgbaFrame};
pub use gpu::{Gpu, GpuError, GpuOptions};
/// FFmpeg's integer layer geometry (it lives in kerf-core: the plan needs it too).
pub use kerf_core::layer_geometry as geometry;
pub use source::{decode_args, decode_layer, decode_layers, YuvFrame};
