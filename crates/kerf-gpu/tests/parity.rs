//! Parity: the GPU compositor against FFmpeg's own still of the same timeline.
//!
//! Every case builds a timeline from synthesized media, renders it at several
//! times through FFmpeg (`export_still`, a PNG at the full delivery canvas — no
//! JPEG noise, no preview downscale) and through the GPU (`RenderPlan` ->
//! `Compositor`) at the same size, and compares them.
//!
//! Needs `ffmpeg` on `PATH` (or `KERF_FFMPEG`) and an adapter. The adapter is the
//! software one unless `KERF_GPU_ADAPTER=hardware|auto`, so the numbers do not
//! depend on the machine's GPU. `#[ignore]`d; run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features -- --ignored
//! ```
//!
//! # What "match" means
//!
//! The two renderers are not bit-identical, and the harness says where. The
//! scaler is swscale's own tables in swscale's integer arithmetic and agrees with
//! `ffmpeg -vf scale` to one level on every plane up to about 4:1, to two or
//! three levels on 8:1 to 20:1 downscales and to five at 40:1 (the steepest the plan
//! accepts)
//! (`the_scaler_matches_ffmpegs_scale_plane_by_plane`); `eq` is byte-exact; an
//! opaque layer's blend is FFmpeg's in YUV. What remains is a hard coloured edge
//! landing on a different pixel (chroma is averaged over 2x2 in one's alpha blend
//! and per pixel in the other's, a rotated edge is stepped in one and sits at a
//! fraction of a pixel in the other) and swscale's YUV -> RGB conversion, which
//! truncates and carries a mean bias of up to a level that the shader does not
//! copy. So a frame is judged in two parts:
//!
//! * the **edge band** — every pixel within [`BAND`] px of a strong gradient in
//!   the *reference* (a step of more than [`EDGE_STEP`] levels in any channel
//!   between neighbours, which is every layer boundary and every hard edge of
//!   the test pattern; [`EDGE_STEP_ROTATED`] for a case with a rotated layer) —
//!   is excluded from the strict check, and
//! * everywhere else (the *flat* region) the GPU must hold **PSNR >= 40 dB** and a
//!   **max per-channel error <= 8/255**.
//!
//! The band must not swallow the frame: **at least [`FLAT_SHARE_MIN`] of the
//! pixels must be judged by the strict check**, or the check is empty. A case
//! whose picture is edge everywhere (noise, a 1-px checkerboard) says so by taking
//! [`BUSY`] limits instead: the whole image, held to the flat region's own error
//! bounds. Those are the cases where a cheaper scaler than swscale's bicubic
//! (bilinear, a box filter) has no flat area to hide in.
//!
//! The whole-image PSNR (band included) is held to its own floor
//! ([`PSNR_ALL_MIN`], lower for a case with a rotated layer): it exists to catch a
//! layer in the wrong place, which the band alone would excuse.
//!
//! Frames the GPU path must **refuse** (`frames_the_gpu_would_draw_wrong_are_refused`)
//! are cases too: the plan or the render says no, and FFmpeg still renders them.
//!
//! On failure (or always, with `KERF_PARITY_KEEP=1`) the reference, the GPU frame
//! and an amplified diff are written to `target/parity/`. `KERF_PARITY_EXPLORE=1`
//! prints every figure and fails nothing, for measuring a change case by case.
//!
//! # Which FFmpeg
//!
//! The suite passes on FFmpeg 6.1.1 (the distro build CI's `system` leg runs) and
//! on the pinned 9.0.2 build the Windows and macOS bundles ship (the `pinned` leg),
//! and the two compose a frame's colour differently. The harness reads the policy
//! the same way the product does (`kerf_core::composite_color_policy`, probed from
//! the real still graph) and the plan takes it as input:
//!
//! * 6.1.1 is **`FixedBt601`**: the composite is converted as BT.601 whatever the
//!   layers were tagged. Every stack is drawable, mixed tags included, and the
//!   mixed cases are judged strictly.
//! * 9.0.2 is **`BottomLayerTag`**: the bottom layer's tag is the composite's
//!   matrix. Stacks of one matrix (BT.709, BT.2020, BT.601 / untagged), opaque or
//!   translucent, single or layered, are drawn and judged strictly
//!   (`the_composite_follows_the_matrix_ffmpeg_negotiates`,
//!   `translucent_layers_take_the_round_trip_with_the_right_matrices`); stacks of
//!   mixed tags are asserted **refused by the plan** (`check_mixed`), because the
//!   conversion FFmpeg makes of the other layers is not reproduced.
//!
//! * **`Unknown`** is what the probe answers when it could not read the FFmpeg (it
//!   would not run, timed out, or its probe clip lost its tag): neither policy is a
//!   safe guess, because they differ on exactly BT.709 and BT.2020 footage. The plan
//!   then draws only stacks that are BT.601 throughout and refuses the rest. The
//!   harness never runs against it by accident — `policy()` stops if the probe
//!   comes back `Unknown` — and forces it where it is the subject
//!   (`an_unmeasured_policy_draws_only_what_every_ffmpeg_agrees_on`).
//!
//! Either way the case is *judged*: a figure that is not FFmpeg's is a failure
//! when the plan said it could draw the frame, and a refusal is asserted rather
//! than assumed.
//!
//! # Known divergences (none hidden by a threshold)
//!
//! * **Opacity below 1** is reproduced, not approximated: FFmpeg takes the layer
//!   through RGB (`colorchannelmixer` has no YUV mode) and `roundtrip.rs` /
//!   `roundtrip.wgsl` follow it in integer arithmetic. Out of YUV is exact on
//!   random pictures (the layer's own matrix); luma back is exact; chroma back is
//!   within one level (x86 FFmpeg's vertical scaler is not bit-exact with the C one
//!   this follows). The way out uses the layer's own matrix and the way back the
//!   composite's. Translucent layers of an **odd size are refused** (by the plan,
//!   once the render size is known): FFmpeg's chroma pairing reads uninitialised
//!   padding past an odd picture. A translucent layer whose matrix is unknown (an
//!   asset that never recorded its pixel format) is refused too.
//! * **A crop of a picture whose chroma is finer than 4:2:0's** is placed exactly
//!   only where the two grids agree. The first `crop` rounds to the picture's
//!   *native* chroma grid (even for 4:2:0, even columns for 4:2:2, nothing for 4:4:4,
//!   gray or RGB); the chain converts to 4:2:0 in its *last* `scale`, so `pad` and the
//!   Cover crop of a one-`scale` chain are on the 4:2:0 grid for every source, while a
//!   Cover crop followed by the transform's own `scale` is on the native one
//!   (`crop_rounds_to_the_native_chroma_grid_and_pad_and_cover_to_the_420_one`). The
//!   compositor's planes are 4:2:0: luma lands exactly, but the chroma of a 4:2:2,
//!   4:4:4 or RGB picture positioned between two 4:2:0 samples is a pixel off (30 to
//!   38 dB over the frame), so the plan refuses it. Gray has no chroma to misplace
//!   and is drawn exactly.
//! * **Colour correction and full range.** The decode converts a full-range
//!   (`yuvj420p`) picture to limited range first; FFmpeg 9 grades the raw full-range
//!   values and converts last (34 to 44 dB apart, every knob; FFmpeg 6 agrees). A
//!   graded `yuvj` picture is refused, as is a graded layer in a stack with one under
//!   a negotiating policy (`grading_a_full_range_picture_is_refused_and_ungraded_it_is_drawn`).
//! * **Resizing a picture that is not 4:2:0 (8/10-bit) or gray** (4:2:2, 4:4:4, RGB,
//!   12-bit, or a format never recorded) is refused, in both directions. FFmpeg scales
//!   in the format the picture has, the decode reduces it to 8-bit 4:2:0 first, and the
//!   chroma is then interpolated from different samples: enlarging was 15 to 69 levels
//!   off, and a *shrink* as mild as 1.05x to 1.5x reads flat max 8 to 9 (4:2:2 at 0.9x
//!   is over the limit) and up to 32 levels on edges, the kernels differing most near
//!   a ratio of 1. No band of ratios was measured strictly inside the limits on both
//!   FFmpegs for busy chroma, so none is claimed
//!   (`resizing_a_picture_ffmpeg_scales_in_another_format_is_refused`). A picture left
//!   at its size is drawn for every format.
//! * **A moving zoom is the last stage of FFmpeg's chain** (the still and the export alike), so a
//!   grade, effects, a mask and the clip's fades act on the picture at its fit size and are magnified
//!   with it; the compositor draws that order (`a_moving_zoom_is_the_last_stage_of_the_still_too`,
//!   `tests/motion_parity.rs`). The two it does not order that way, a rotation and an opacity below 1 in
//!   front of the zoom, are refused (`refused/moving-zoom-behind-*`). A rotation and an opacity moving
//!   over a zoom that holds are drawn (`time/keyframes+rotation+opacity`).
//! * **Masks and effects** (`masks_cut_a_layer_to_a_shape`, `effects_are_drawn_as_their_filters_draw_them`,
//!   `the_effect_filters_match_ffmpeg_plane_by_plane`): a mask's alpha is `geq`'s, blended per sample (luma)
//!   and over each 2x2 block's average (chroma) like `overlay`; grayscale, invert and sharpen are exact;
//!   a blur is `gblur`'s float recursion in the portable order, one level off on a few samples in a hundred
//!   thousand (FFmpeg's own SIMD orders the same sums otherwise). A mask on a letterbox is cut from the
//!   whole padded frame, as `geq` is. A vignette, a chroma key, a mask on a rotated or translucent layer, a
//!   blur or a sharpen on a letterbox, an invert / blur / sharpen on a full-range picture (FFmpeg runs it on
//!   the raw values and converts the range afterwards: an invert 8 levels off, a sharpen's overshoot 26)
//!   and an invert of a gray picture (its chroma stays 128 in FFmpeg) are refused. On a 10-bit, 4:2:2,
//!   4:4:4, RGB or gray picture the effects and masks are drawn at the plain source's own figures.
//! * **`geq` on FFmpeg 6.1 copies a plane's last column and row from the ones before them** (its bilinear
//!   read clamps to `w - 2` / `h - 2`; 9.0 copies them), so a masked picture's right-most column and bottom
//!   row differ between the two FFmpegs (up to 224 levels of chroma at the bottom row of a frame whose picture
//!   changes there). `kerf_core::geq_edge()` measures which the build in use does and the compositor draws
//!   it, so the mask cases hold on both (`a_mask_hands_the_planes_on_as_the_geq_that_clamps_its_read_does`).
//! * **A dip to white ahead of a moving zoom** is refused: the zoom's `scale` takes the RGB frame the fade
//!   leaves and converts it in the same pass (up to 14 levels off a draw that scales the converted picture on
//!   6.1.1, 3 on 9.0.2).
//! * **A shrink steeper than 40:1** (`kerf_core::MAX_SHRINK`) is refused: past what
//!   the scaler comparison measures, swscale's x86 vertical scaler drifts further
//!   from the C arithmetic the shader follows (a 58:1 shrink of a 4K test pattern
//!   read 16 levels off in RGB).
//! * **A rotated edge** is a fixed-point stair-step in FFmpeg and a float sample
//!   here; the interior agrees to a level. FFmpeg's own `rotate` also leaves a few
//!   green pixels along the edge of a neutral layer (chroma it never wrote); the
//!   GPU does not copy them (`a_rotated_neutral_layer_has_no_colour_fringe`).
//! * **swscale's YUV -> RGB** truncates, a mean bias of up to a level (the flat
//!   mean error column in the report); the shader rounds.
//! * **Text** is FFmpeg's own `drawtext` raster blended with `drawtext`'s own arithmetic
//!   (`tests/text.rs` holds that blend to `drawtext` sample for sample, the `text/` cases and
//!   `the_text_pass_is_the_reference_blend_plane_by_plane` hold the shader to it). Text is edge
//!   everywhere, so a text case is judged against the same picture *without* the text
//!   (`check_text`: no allowance over it, but for three measured cases that say why), not by
//!   the band. What the compositor cannot reproduce is refused
//!   (`text_the_gpu_cannot_reproduce_is_refused`): a synthetic-bold border, text under 12 px
//!   (glyphs that overlap are blended once as a mask and in turn by `drawtext`), a face the
//!   plan cannot name.
//! * **A 10-bit source that is also scaled**: FFmpeg's `scale` converts and scales
//!   in one pass (with ordered dither), the GPU path decodes to 8 bits first.
//!   Measured unscaled only (`source/10-bit`).

#![allow(clippy::print_stderr)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use kerf_core::{
    export_still, Asset, Clip, Color, CompositeColorPolicy, Container, Delivery, ExportOptions, Fit, ImageFormat, Keyframe,
    Project, RateControl, RenderPlan, StreamKind, Subsampling, Timeline, Track, Transform, Transition, TransitionKind,
    Unsupported,
};
use kerf_gpu::geometry::LayerGeometry;
use kerf_gpu::{Compositor, Gpu, GpuOptions};

// ---- thresholds (final values; the recorded numbers are at the bottom) ------

/// Pixels this close to a strong reference gradient are the edge band.
const BAND: usize = 2;
/// A neighbour-to-neighbour step (any channel, in 0..255) that counts as an
/// edge: 24 levels, about 10% of the range. It is what defines the band, so the
/// higher it is the fewer pixels are excused — 24 is what every case without a
/// rotated layer is judged at.
const EDGE_STEP: i32 = 24;
/// ...and 12 for a case with a rotated layer. `rotate` writes a stair-stepped
/// edge from a fixed-point source position; once the layer is also faded (the
/// keyframed case runs at ~65% opacity) the edge shows at two thirds of its
/// contrast, under 24 levels, and the stair-step — a few levels of difference at
/// the pixels along it, measured at 10 — would be judged as a mismatch in the
/// flat region. At 12 the same pixels are in the band; the limits did not move.
const EDGE_STEP_ROTATED: i32 = 12;
/// Required PSNR over the flat region.
const PSNR_FLAT_MIN: f64 = 40.0;
/// Allowed max per-channel error over the flat region.
const MAX_FLAT: i32 = 8;
/// Whole-image PSNR floor, edge band included. It exists for what the band
/// excuses: a layer one row off (a missing line of picture) is invisible to the
/// flat check and costs several dB here — the very first run of the 9:16
/// letterbox case read 31 dB before FFmpeg's `pad` quirk was reproduced.
const PSNR_ALL_MIN: f64 = 40.0;
/// ...and for a case with a rotated layer. FFmpeg's `rotate` writes a hard,
/// stair-stepped edge from a fixed-point source position; the GPU samples the
/// same bilinear footprint at a float one. Every pixel of the border can differ
/// by a whole step where the picture meets a different colour, which is what
/// holds the whole-image figure near 33 dB while the interior stays above 46.
const PSNR_ALL_MIN_ROTATED: f64 = 30.0;

/// The least share of a frame that must lie outside the edge band, so the strict
/// flat check judges at least half of every ordinary case. The recorded cases sit
/// between 57% and 100% flat (the busiest, the 202x100 Cover frames of the `grid/`
/// cases, 56.9%); a case below this is [`BUSY`] and says so.
const FLAT_SHARE_MIN: f64 = 0.5;

/// The scaler comparison's bounds, on the planes themselves (levels of 255). Up to
/// about 4:1 no sample differs from `ffmpeg -vf scale` by more than one level, on
/// any source. A shrink of 8:1 or more reduces each output sample from dozens of
/// inputs, and swscale's x86 vertical scaler rounds each of them slightly
/// differently from the C arithmetic this follows: measured 2-3 on noise and test
/// patterns at 8:1 to 20:1 and up to 5 at 40:1, identically on FFmpeg 6.1.1 and
/// 9.0.2, so those are the bounds there (`SCALER_MAX_EXTREME`, `SCALER_MAX_STEEP`)
/// and it is claimed no further: the plan refuses a steeper shrink than 40:1.
/// The mean bounds are what the one-level differences add up to: smooth footage
/// (measured <= 0.06) is almost exact; on a checkerboard — 219 levels of
/// contrast per pixel — x86 FFmpeg's non-bit-exact vertical scaler is a level
/// low on over half the samples (measured 0.57), the same bias at every
/// amplitude, which is why the busy bound is the looser one.
const SCALER_MAX: i32 = 1;
const SCALER_MAX_EXTREME: i32 = 3;
const SCALER_SMOOTH_MEAN: f64 = 0.1;
const SCALER_BUSY_MEAN: f64 = 0.75;
/// ...and for a shrink of 8:1 or more, where even a smooth test pattern lands
/// every output sample on dozens of inputs (measured at most 0.52, on a 20:1
/// shrink of the test pattern).
const SCALER_EXTREME_MEAN: f64 = 0.6;
/// A shrink of 32:1 up to the 40:1 the plan stops at (`kerf_core::MAX_SHRINK`):
/// noise reads up to 5 levels off a plane, 0.85 on average.
const SCALER_MAX_STEEP: i32 = 5;
const SCALER_STEEP_MEAN: f64 = 1.0;

/// How far from neutral (the spread of a pixel's three channels, levels of 255) a
/// rotated mid-grey layer over black may get anywhere in the frame: rounding in
/// 4:2:0 chroma, nothing like a green rim (spread > 100 before the fix).
const FRINGE_MAX: u8 = 4;

/// What a case may relax, and why. Every relaxation is a named constant above.
#[derive(Clone, Copy)]
struct Limits {
    psnr_flat: f64,
    max_flat: i32,
    psnr_all: f64,
    /// At least this share of the frame must be judged by the strict flat check
    /// (i.e. outside the edge band). Without it a busy picture — all edge —
    /// would pass on an empty comparison.
    flat_share: f64,
    /// A bound on the worst whole-image error, for a case that has no flat
    /// region to hold the max to.
    max_all: Option<i32>,
    /// What counts as an edge for this case's band.
    edge_step: i32,
}

const STRICT: Limits = Limits {
    psnr_flat: PSNR_FLAT_MIN,
    max_flat: MAX_FLAT,
    psnr_all: PSNR_ALL_MIN,
    flat_share: FLAT_SHARE_MIN,
    max_all: None,
    edge_step: EDGE_STEP,
};

/// A source that is edge everywhere (noise, a 1-px checkerboard): the band
/// covers the picture, so the strict check is the *whole image* — held to the
/// flat region's own error bounds, with no flat-share requirement. This is the
/// case where a cheaper scaler than swscale's bicubic (bilinear, a box filter)
/// cannot hide: there is no flat area to retreat into.
const BUSY: Limits = Limits {
    flat_share: 0.0,
    max_all: Some(MAX_FLAT),
    ..STRICT
};

const ROTATED: Limits = Limits {
    psnr_all: PSNR_ALL_MIN_ROTATED,
    edge_step: EDGE_STEP_ROTATED,
    ..STRICT
};

// ---- environment ------------------------------------------------------------

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| Compositor::new(gpu()).expect("a compositor"))
}

/// One frame source for the whole suite, as the app would have: every case's frames are decoded
/// through it as well, and must be the one-shot decode's byte for byte.
fn frame_source() -> &'static kerf_gpu::FrameSource {
    static S: OnceLock<Arc<kerf_gpu::FrameSource>> = OnceLock::new();
    S.get_or_init(|| kerf_gpu::FrameSource::new(kerf_gpu::FrameSourceConfig::default()))
}

thread_local! {
    /// A policy a test forces on its own thread (see [`with_policy`]).
    static FORCED_POLICY: std::cell::Cell<Option<CompositeColorPolicy>> = const { std::cell::Cell::new(None) };
}

/// How this FFmpeg picks the composite's matrix (probed from the real graph, once).
/// The harness never runs against a guess: if the probe could not read this FFmpeg,
/// every case would be judged against `Unknown`'s refusals rather than against the
/// behaviour under test, so it stops here instead (a test that wants `Unknown`
/// forces it with [`with_policy`]).
fn policy() -> CompositeColorPolicy {
    if let Some(forced) = FORCED_POLICY.with(std::cell::Cell::get) {
        return forced;
    }
    let measured = kerf_core::composite_color_policy();
    assert_ne!(
        measured,
        CompositeColorPolicy::Unknown,
        "the composite colour probe could not read this FFmpeg ({}): the parity cases would be judged against a guess",
        kerf_core::ffmpeg_path()
    );
    measured
}

/// Run `f` with `policy` in force on this thread, whatever the FFmpeg is.
fn with_policy<T>(forced: CompositeColorPolicy, f: impl FnOnce() -> T) -> T {
    let before = FORCED_POLICY.with(|p| p.replace(Some(forced)));
    let out = f();
    FORCED_POLICY.with(|p| p.set(before));
    out
}

/// Why the compositor would refuse `plan` at `size`: what *it* says it draws (`Compositor::caps`),
/// not the plan's size-free A0 answer, which refuses every text overlay.
fn reasons(plan: &RenderPlan, size: (u32, u32)) -> Vec<String> {
    plan.reasons(&compositor().caps(), size)
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn target_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .expect("target/")
        .to_path_buf()
}

fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

// ---- synthesized media --------------------------------------------------------

struct Media {
    testsrc: Asset,
    bars: Asset,
    gradient: Asset,
    portrait: Asset,
    still: Asset,
    /// Sources a probe cannot tell from ordinary video, but whose decode path
    /// differs: 10-bit, 4:4:4, full-range JPEG, odd-sized, and a phone-style
    /// rotated one (a landscape frame plus a display matrix).
    tenbit: Asset,
    /// The same 10-bit picture tagged full range: it probes as `yuv420p10le` too.
    tenbit_pc: Asset,
    yuv444: Asset,
    fullrange: Asset,
    /// A JPEG still: full-range YUV in a file FFmpeg reads as an image.
    jpeg: Asset,
    odd: Asset,
    rotated: Asset,
    /// A 480x270 JPEG whose EXIF orientation says "turn 90 degrees": ffprobe
    /// reports 480x270, every decode comes out 270x480.
    exif: Asset,
    /// Lossless yuva420p, the left half opaque and the right half transparent.
    alpha: Asset,
    /// Busy sources: per-pixel noise and a 1-px checkerboard (0.2 s of video).
    noise: Asset,
    /// Noise at 1280x720, for the extreme downscales.
    noise_hd: Asset,
    checker: Asset,
    /// A 640x360 still that never ends, to sit under a clip that does.
    still_long: Asset,
    /// Flat mid-grey and black: neutral colour, so any chroma in a result is a bug.
    grey: Asset,
    black: Asset,
    /// The colour bars tagged BT.2020 (non-constant luminance): the matrix a
    /// translucent layer is taken out of YUV with is the stream's own.
    bars2020: Asset,
    /// `bars`, `testsrc` and `gradient` carry no colour tag. These are the same
    /// kinds of picture tagged BT.709 and BT.2020, for the cases that are about
    /// what FFmpeg does with a tag.
    bars709: Asset,
    testsrc709: Asset,
    gradient709: Asset,
    testsrc2020: Asset,
    gradient2020: Asset,
    /// The test pattern in formats that are not 8-bit 4:2:0 — what FFmpeg scales
    /// natively and the compositor, which works on 4:2:0, does not.
    yuv422: Asset,
    bgr0: Asset,
    gray: Asset,
    /// An RGB PNG the size of the suite's usual canvas (640x360): fitting it into
    /// that canvas is not an enlargement, which `still` (480x270) is.
    png640: Asset,
}

/// The asset a real import would make: the file, probed by the same code the
/// app imports with (so a stream says what the probe says, `pix_fmt` and all —
/// which is how the plan learns a picture has alpha, and what the decode is
/// checked against).
fn probed(path: &Path) -> Asset {
    Project::probe_asset(path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()))
}

fn media() -> &'static Media {
    static M: OnceLock<Media> = OnceLock::new();
    M.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-media");
        std::fs::create_dir_all(&dir).unwrap();
        // Lossless H.264 (the pixels FFmpeg and the GPU decode are then the
        // synthesized ones exactly), short GOP, 2 seconds at 30 fps.
        let video = |name: &str, lavfi: &str| -> PathBuf {
            let p = dir.join(name);
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                lavfi,
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-qp",
                "0",
                "-g",
                "10",
                "-pix_fmt",
                "yuv420p",
                p.to_str().unwrap(),
            ]);
            p
        };
        let testsrc = video("testsrc.mp4", "testsrc2=size=640x360:rate=30:duration=2");
        // `smptehdbars` tags itself BT.709; the suite's base picture is untagged, so
        // that the cases which are not about colour tags are the same under every
        // FFmpeg (which tag a stack's composite takes is the version's business).
        let untagged = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
        let bt709 = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709";
        let bt2020 = "setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt709";
        let bars = video("bars.mp4", &format!("smptehdbars=size=640x360:rate=30:duration=2,{untagged}"));
        let bars709 = video("bars709.mp4", &format!("smptehdbars=size=640x360:rate=30:duration=2,{bt709}"));
        let testsrc709 = video("testsrc709.mp4", &format!("testsrc2=size=640x360:rate=30:duration=2,{bt709}"));
        let testsrc2020 = video("testsrc2020.mp4", &format!("testsrc2=size=640x360:rate=30:duration=2,{bt2020}"));
        let gradients = "gradients=size=640x360:rate=30:duration=2:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:x0=0:y0=0:x1=640:y1=360:speed=0.00001";
        // (`gradients` makes RGB: the conversion to 4:2:0 has to come before the tag,
        // or it replaces it with "unspecified".)
        let gradient709 = video("gradient709.mp4", &format!("{gradients},format=yuv420p,{bt709}"));
        let gradient2020 = video("gradient2020.mp4", &format!("{gradients},format=yuv420p,{bt2020}"));
        let gradient = video(
            "gradient.mp4",
            "gradients=size=640x360:rate=30:duration=2:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:x0=0:y0=0:x1=640:y1=360:speed=0.00001",
        );
        let portrait = video("portrait.mp4", "testsrc2=size=360x640:rate=30:duration=2");
        let still = dir.join("still.png");
        ffmpeg(&["-f", "lavfi", "-i", "testsrc2=size=480x270", "-frames:v", "1", still.to_str().unwrap()]);
        // Lossless intermediates keep the pixels exact where H.264 cannot carry
        // the format (4:4:4 and an odd size need FFV1; full range is MJPEG).
        let ffv1 = |name: &str, lavfi: &str, pix_fmt: &str, codec: &[&str]| -> PathBuf {
            let p = dir.join(name);
            let mut args = vec!["-f", "lavfi", "-i", lavfi, "-c:v"];
            args.extend_from_slice(codec);
            args.extend(["-pix_fmt", pix_fmt, p.to_str().unwrap()]);
            ffmpeg(&args);
            p
        };
        let tenbit = dir.join("tenbit.mp4");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30:duration=2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-qp",
            "0",
            "-pix_fmt",
            "yuv420p10le",
            tenbit.to_str().unwrap(),
        ]);
        let tenbit_pc = dir.join("tenbit_pc.mp4");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30:duration=2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-qp",
            "0",
            "-color_range",
            "pc",
            "-pix_fmt",
            "yuv420p10le",
            tenbit_pc.to_str().unwrap(),
        ]);
        let yuv444 = ffv1("yuv444.mkv", "testsrc2=size=640x360:rate=30:duration=2", "yuv444p", &["ffv1"]);
        let fullrange = ffv1(
            "fullrange.mkv",
            "testsrc2=size=640x360:rate=30:duration=2",
            "yuvj420p",
            &["mjpeg", "-qscale:v", "1"],
        );
        // (Sources refuse odd sizes; a scale produces one.)
        let odd = ffv1(
            "odd.mkv",
            "testsrc2=size=642x362:rate=30:duration=2,scale=641:361",
            "yuv420p",
            &["ffv1"],
        );
        // The display matrix is a container property: remux a landscape file.
        let landscape = video("landscape.mp4", "testsrc2=size=640x360:rate=30:duration=2");
        let rotated_path = dir.join("rotated.mp4");
        ffmpeg(&[
            "-display_rotation",
            "90",
            "-i",
            landscape.to_str().unwrap(),
            "-c",
            "copy",
            rotated_path.to_str().unwrap(),
        ]);
        let jpeg = dir.join("still.jpg");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=480x270",
            "-frames:v",
            "1",
            "-q:v",
            "2",
            jpeg.to_str().unwrap(),
        ]);
        // EXIF orientation 6 in an APP1 segment right after SOI: a landscape JPEG
        // that displays as a portrait one.
        let exif = dir.join("exif6.jpg");
        let mut bytes = std::fs::read(&jpeg).unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "a JPEG");
        let app1: Vec<u8> = [
            &[0xFF, 0xE1, 0x00, 0x22][..],
            b"Exif\0\0",
            &[b'I', b'I', 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00], // TIFF header
            &[0x01, 0x00],                                     // one entry
            &[0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00], // Orientation = 6
            &[0x00, 0x00, 0x00, 0x00],                         // no next IFD
        ]
        .concat();
        bytes.splice(2..2, app1);
        std::fs::write(&exif, bytes).unwrap();
        // Left half opaque, right half transparent.
        let alpha = ffv1(
            "alpha.mkv",
            "testsrc2=size=640x360:rate=30:duration=2,format=yuva420p,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(gt(X,W/2),0,255)'",
            "yuva420p",
            &["ffv1"],
        );
        // Busy footage, as video (a PNG would be converted RGB -> 4:2:0 inside
        // FFmpeg's scaler in one step and by the decode here in another, which
        // is a difference of the *source*, not of the compositor).
        let noise = video(
            "noise.mp4",
            "nullsrc=size=640x360:rate=30:duration=0.2,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'",
        );
        let noise_hd = video(
            "noise_hd.mp4",
            "nullsrc=size=1280x720:rate=30:duration=0.1,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'",
        );
        let checker = video(
            "checker.mp4",
            "nullsrc=size=640x360:rate=30:duration=0.2,format=yuv420p,geq=lum='if(eq(mod(X+Y,2),0),235,16)':cb=128:cr=128",
        );
        let still_long = dir.join("still-long.png");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "gradients=size=640x360:rate=1:duration=1:c0=0x20a040:c1=0xc04080:nb_colors=2:seed=3:speed=0.00001",
            "-frames:v",
            "1",
            // (`gradients` makes RGBA, which a plan rightly refuses as having alpha.)
            "-pix_fmt",
            "rgb24",
            still_long.to_str().unwrap(),
        ]);
        let bars2020 = video("bars2020.mp4", &format!("smptebars=size=640x360:rate=30:duration=2,{bt2020}"));
        let grey = video("grey.mp4", "color=c=0x808080:s=640x360:r=30:d=2");
        let black = video("black.mp4", "color=c=black:s=640x360:r=30:d=2");
        let png640 = dir.join("still640.png");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360",
            "-frames:v",
            "1",
            png640.to_str().unwrap(),
        ]);
        let yuv422 = ffv1("yuv422.mkv", "testsrc2=size=640x360:rate=30:duration=2", "yuv422p", &["ffv1"]);
        let bgr0 = ffv1("bgr0.mkv", "testsrc2=size=640x360:rate=30:duration=2", "bgr0", &["ffv1"]);
        let gray = ffv1("gray.mkv", "testsrc2=size=640x360:rate=30:duration=2", "gray", &["ffv1"]);
        Media {
            bars2020: probed(&bars2020),
            bars709: probed(&bars709),
            testsrc709: probed(&testsrc709),
            gradient709: probed(&gradient709),
            testsrc2020: probed(&testsrc2020),
            gradient2020: probed(&gradient2020),
            png640: probed(&png640),
            yuv422: probed(&yuv422),
            bgr0: probed(&bgr0),
            gray: probed(&gray),
            grey: probed(&grey),
            black: probed(&black),
            tenbit: probed(&tenbit),
            tenbit_pc: probed(&tenbit_pc),
            yuv444: probed(&yuv444),
            fullrange: probed(&fullrange),
            jpeg: probed(&jpeg),
            odd: probed(&odd),
            rotated: probed(&rotated_path),
            exif: probed(&exif),
            alpha: probed(&alpha),
            noise: probed(&noise),
            noise_hd: probed(&noise_hd),
            checker: probed(&checker),
            still_long: probed(&still_long),
            testsrc: probed(&testsrc),
            bars: probed(&bars),
            gradient: probed(&gradient),
            portrait: probed(&portrait),
            still: probed(&still),
        }
    })
}

fn timeline(tracks: Vec<Vec<Clip>>, format: Option<Delivery>) -> Timeline {
    Timeline {
        tracks: tracks
            .into_iter()
            .enumerate()
            .map(|(i, clips)| Track {
                clips,
                ..Track::new(StreamKind::Video, format!("V{}", i + 1))
            })
            .collect(),
        overlays: Vec::new(),
        markers: Vec::new(),
        format,
        master: Default::default(),
    }
}

fn clip(a: &Asset, source_in: f64, source_out: f64, start: f64) -> Clip {
    Clip::new(a.id, source_in, source_out, start)
}

// ---- comparison ---------------------------------------------------------------

struct Metrics {
    psnr_flat: f64,
    max_flat: i32,
    mean_flat: f64,
    psnr_all: f64,
    max_all: i32,
    edge_fraction: f64,
}

impl Metrics {
    fn flat_share(&self) -> f64 {
        1.0 - self.edge_fraction
    }
}

fn psnr(sq_err: f64, n: usize) -> f64 {
    if sq_err == 0.0 || n == 0 {
        return 99.0;
    }
    10.0 * (255.0 * 255.0 / (sq_err / n as f64)).log10()
}

/// The pixels within `BAND` of a strong gradient in `reference` (RGB, packed).
fn edge_band(reference: &[u8], w: usize, h: usize, edge_step: i32) -> Vec<bool> {
    let px = |x: usize, y: usize, c: usize| i32::from(reference[(y * w + x) * 3 + c]);
    let mut seed = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut step = 0;
            for c in 0..3 {
                if x + 1 < w {
                    step = step.max((px(x + 1, y, c) - px(x, y, c)).abs());
                }
                if y + 1 < h {
                    step = step.max((px(x, y + 1, c) - px(x, y, c)).abs());
                }
            }
            if step > edge_step {
                // A step is between two pixels; both are on the edge.
                seed[y * w + x] = true;
                if x + 1 < w {
                    seed[y * w + x + 1] |= (0..3).any(|c| (px(x + 1, y, c) - px(x, y, c)).abs() > edge_step);
                }
                if y + 1 < h {
                    seed[(y + 1) * w + x] |= (0..3).any(|c| (px(x, y + 1, c) - px(x, y, c)).abs() > edge_step);
                }
            }
        }
    }
    // Dilate by BAND in x, then in y.
    let mut wide = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if seed[y * w + x] {
                for xx in x.saturating_sub(BAND)..=(x + BAND).min(w - 1) {
                    wide[y * w + xx] = true;
                }
            }
        }
    }
    let mut band = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if wide[y * w + x] {
                for yy in y.saturating_sub(BAND)..=(y + BAND).min(h - 1) {
                    band[yy * w + x] = true;
                }
            }
        }
    }
    band
}

fn compare(reference: &[u8], gpu_rgb: &[u8], w: usize, h: usize, edge_step: i32) -> (Metrics, Vec<bool>) {
    let band = edge_band(reference, w, h, edge_step);
    let (mut se_flat, mut n_flat, mut sum_flat, mut max_flat) = (0.0f64, 0usize, 0.0f64, 0i32);
    let (mut se_all, mut max_all) = (0.0f64, 0i32);
    for (i, &in_band) in band.iter().enumerate() {
        for c in 0..3 {
            let d = i32::from(reference[i * 3 + c]) - i32::from(gpu_rgb[i * 3 + c]);
            let sq = f64::from(d * d);
            se_all += sq;
            max_all = max_all.max(d.abs());
            if !in_band {
                se_flat += sq;
                sum_flat += f64::from(d.abs());
                n_flat += 1;
                max_flat = max_flat.max(d.abs());
            }
        }
    }
    let m = Metrics {
        psnr_flat: psnr(se_flat, n_flat),
        max_flat,
        mean_flat: if n_flat == 0 { 0.0 } else { sum_flat / n_flat as f64 },
        psnr_all: psnr(se_all, w * h * 3),
        max_all,
        edge_fraction: band.iter().filter(|b| **b).count() as f64 / (w * h) as f64,
    };
    (m, band)
}

fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect()
}

fn write_png(path: &Path, rgb: &[u8], w: usize, h: usize) {
    let size = format!("{w}x{h}");
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-s",
            &size,
            "-i",
            "pipe:0",
        ])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(rgb).unwrap();
    }
    assert!(child.wait().unwrap().success());
}

/// The runtime self-test (`kerf_gpu::selftest`, run on a user's machine at the GPU path's first
/// use) judges by these limits and by this file's comparison; a number changed here and not
/// there would make it stricter or looser than the suite it stands in for.
#[test]
fn the_runtime_self_test_judges_by_the_limits_and_the_comparison_of_this_harness() {
    use kerf_gpu::selftest as st;
    assert_eq!((st::BAND, st::EDGE_STEP, st::MAX_FLAT), (BAND, EDGE_STEP, MAX_FLAT));
    assert_eq!(
        (st::PSNR_FLAT_MIN, st::PSNR_ALL_MIN, st::FLAT_SHARE_MIN),
        (STRICT.psnr_flat, STRICT.psnr_all, STRICT.flat_share)
    );
    assert_eq!(
        (STRICT.max_flat, STRICT.edge_step, STRICT.max_all),
        (MAX_FLAT, EDGE_STEP, None)
    );
    // The same picture pair, judged by both.
    let (w, h) = (97, 61);
    let mut seed = 0x9e37_79b9_u32;
    let mut next = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 24) as u8
    };
    let reference: Vec<u8> = (0..w * h * 3)
        .map(|i| if (i / 3 % w) < w / 3 { 40 } else { 40 + (i % 5) as u8 * 30 })
        .collect();
    let gpu: Vec<u8> = reference.iter().map(|v| v.saturating_add(next() % 4)).collect();
    let (mine, _) = compare(&reference, &gpu, w, h, STRICT.edge_step);
    let theirs = st::compare(&reference, &gpu, w, h);
    assert_eq!(
        (
            mine.psnr_flat,
            mine.max_flat,
            mine.mean_flat,
            mine.psnr_all,
            mine.max_all,
            mine.edge_fraction
        ),
        (
            theirs.psnr_flat,
            theirs.max_flat,
            theirs.mean_flat,
            theirs.psnr_all,
            theirs.max_all,
            theirs.edge_fraction
        )
    );
}

// ---- the check ----------------------------------------------------------------

fn report() -> &'static Mutex<BTreeMap<String, String>> {
    static R: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The layers' frames from the suite's one [`frame_source`]. The suite asks for a few files from
/// every test thread at once, and "every decode run is in use" is a legitimate answer to that (an
/// app renders that frame through FFmpeg), so the harness asks again.
fn shared_frames(layers: &[kerf_core::PlanLayer]) -> Result<Vec<Option<Arc<kerf_gpu::YuvFrame>>>, kerf_gpu::GpuError> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match frame_source().frames(layers, kerf_gpu::Hint::Scrub) {
            Err(kerf_gpu::GpuError::Busy(_)) if std::time::Instant::now() < until => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            other => return other,
        }
    }
}

/// Where two decoded frames differ, for a failure message.
fn describe_difference(a: Option<&kerf_gpu::YuvFrame>, b: Option<&kerf_gpu::YuvFrame>) -> String {
    match (a, b) {
        (Some(a), Some(b)) => {
            let first = |x: &[u8], y: &[u8]| x.iter().zip(y).position(|(p, q)| p != q);
            format!(
                "{}x{} against {}x{}, first luma difference at {:?}, first chroma at {:?} / {:?}",
                a.width,
                a.height,
                b.width,
                b.height,
                first(&a.y, &b.y),
                first(&a.u, &b.u),
                first(&a.v, &b.v)
            )
        }
        (a, b) => format!(
            "{} against {}",
            a.map_or("no frame", |_| "a frame"),
            b.map_or("no frame", |_| "a frame")
        ),
    }
}

/// Render `tl` at each of `times` through both renderers and hold them to
/// `limits`. Returns the failures (empty when every time passes) after printing
/// a line per time.
fn check(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64], limits: Limits) {
    check_with(case, tl, assets, times, limits, &ExportOptions::default());
}

/// [`check`] for a render with options (the frame rate the plan counts frames in, say).
fn check_with(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64], limits: Limits, opts: &ExportOptions) -> Vec<i32> {
    let out_dir = target_dir().join("parity");
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    // A file-name-safe form of the case name (`pip/scaled+offset`).
    let slug: String = case
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let mut failures = Vec::new();
    let mut maxes = Vec::new();
    for &t in times {
        let plan = RenderPlan::at(tl, assets, opts, t, policy()).expect("plan");
        let size = plan.size(u32::MAX);
        let why = reasons(&plan, size);
        assert!(
            why.is_empty(),
            "{case} @ {t}: the plan says the GPU cannot draw this at {size:?} (canvas {}x{}): {why:?}",
            plan.canvas.width,
            plan.canvas.height
        );
        let (w, h) = (size.0 as usize, size.1 as usize);

        let ref_png = scratch.join(format!("{slug}-{t}.png"));
        export_still(tl, assets, opts, t, &ref_png, ImageFormat::Png, 0).expect("FFmpeg still");
        let reference = ffmpeg(&[
            "-i",
            ref_png.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ]);
        let _ = std::fs::remove_file(&ref_png);
        assert_eq!(reference.len(), w * h * 3, "{case} @ {t}: the reference is not {w}x{h}");

        let (frame, timings) = compositor().render_plan(&plan, size).expect("GPU render");
        assert_eq!((frame.width as usize, frame.height as usize), (w, h));
        // The layers' frames from long-lived runs: identical to the one-shot decode's, not merely close.
        let shared = shared_frames(&plan.layers).unwrap_or_else(|e| panic!("{case} @ {t}: the frame source: {e}"));
        let oneshot = kerf_gpu::decode_layers(&plan.layers).expect("decode");
        for (n, (a, b)) in shared.iter().zip(&oneshot).enumerate() {
            assert!(
                a.as_deref() == b.as_ref(),
                "{case} @ {t}: layer {n} ({}, source time {}): the frame source's frame is not the one-shot decode's: {}",
                plan.layers[n].path,
                plan.layers[n].source_time,
                describe_difference(a.as_deref(), b.as_ref())
            );
        }
        let gpu_rgb = rgba_to_rgb(&frame.data);

        let (m, band) = compare(&reference, &gpu_rgb, w, h, limits.edge_step);
        maxes.push(m.max_all);
        let line = format!(
            "{case:<28} t={t:<7} {w}x{h} layers={} flat: PSNR {:>5.1} dB  max {:>2}  mean {:.2} | all: PSNR {:>5.1} dB  max {:>3} | edge band {:>4.1}%  (decode {:.0} ms, composite {:.0} ms)",
            plan.layers.len(),
            m.psnr_flat,
            m.max_flat,
            m.mean_flat,
            m.psnr_all,
            m.max_all,
            m.edge_fraction * 100.0,
            timings.decode.as_secs_f64() * 1e3,
            timings.composite.as_secs_f64() * 1e3,
        );
        eprintln!("{line}");
        report().lock().unwrap().insert(format!("{case} {t:08.4}"), line);

        let ok = m.psnr_flat >= limits.psnr_flat
            && m.max_flat <= limits.max_flat
            && m.psnr_all >= limits.psnr_all
            && m.flat_share() >= limits.flat_share
            && limits.max_all.is_none_or(|bound| m.max_all <= bound);
        if !ok || std::env::var_os("KERF_PARITY_KEEP").is_some() {
            std::fs::create_dir_all(&out_dir).unwrap();
            let stem = format!("{slug}-{t}");
            write_png(&out_dir.join(format!("{stem}-ref.png")), &reference, w, h);
            write_png(&out_dir.join(format!("{stem}-gpu.png")), &gpu_rgb, w, h);
            // Diff: |ref - gpu| x 8, with the edge band tinted blue.
            let diff: Vec<u8> = (0..w * h)
                .flat_map(|i| {
                    let d = |c: usize| (i32::from(reference[i * 3 + c]) - i32::from(gpu_rgb[i * 3 + c])).unsigned_abs();
                    let mag = (d(0).max(d(1)).max(d(2)) * 8).min(255) as u8;
                    if band[i] {
                        [mag / 2, mag / 2, mag.max(60)]
                    } else {
                        [mag, mag, mag]
                    }
                })
                .collect();
            write_png(&out_dir.join(format!("{stem}-diff.png")), &diff, w, h);
            if !ok {
                failures.push(format!(
                    "{case} @ {t}s: flat PSNR {:.1} (>= {}), flat max {} (<= {}), whole PSNR {:.1} (>= {}), flat share {:.0}% (>= {:.0}%), whole max {} (<= {:?}) — images in {}",
                    m.psnr_flat,
                    limits.psnr_flat,
                    m.max_flat,
                    limits.max_flat,
                    m.psnr_all,
                    limits.psnr_all,
                    m.flat_share() * 100.0,
                    limits.flat_share * 100.0,
                    m.max_all,
                    limits.max_all,
                    out_dir.display()
                ));
            }
        }
    }
    write_report();
    // `KERF_PARITY_EXPLORE=1` prints every figure and judges nothing: for
    // measuring a change case by case instead of stopping at the first miss.
    if std::env::var_os("KERF_PARITY_EXPLORE").is_some() {
        for f in &failures {
            eprintln!("WOULD FAIL: {f}");
        }
        return maxes;
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    maxes
}

fn write_report() {
    let mut text = String::new();
    for line in report().lock().unwrap().values() {
        let _ = writeln!(text, "{line}");
    }
    let dir = target_dir().join("parity");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("report.txt"), text);
}

// ---- cases ----------------------------------------------------------------------

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn single_clip_at_the_footage_size() {
    let m = media();
    let tl = timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None);
    check(
        "single/testsrc2",
        &tl,
        std::slice::from_ref(&m.testsrc),
        &[0.0, 0.5, 1.2345],
        STRICT,
    );
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)]], None);
    check("single/smptehdbars", &tl, std::slice::from_ref(&m.bars), &[0.0, 1.0], STRICT);
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)]], None);
    check("single/gradient", &tl, std::slice::from_ref(&m.gradient), &[0.0, 1.0], STRICT);
}

/// `Compositor::render_plan_with` end to end: the picture composited from a `FrameSource`'s
/// frames (runs, a cache, layers sharing a decode) is the picture composited from the one-shot
/// decodes, bit for bit, and so on a second ask (served by the cache), on a `Forward` ask (a run
/// read on) and with the times visited backwards. The suite's other cases compare the *frames*
/// of the two paths; this is the render.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn rendering_through_the_frame_source_is_the_picture_the_one_shot_decodes_give() {
    let m = media();
    let mut over = clip(&m.testsrc, 0.0, 2.0, 0.0);
    over.transform = Transform {
        scale: 0.7,
        rotation: 17.0,
        crop_left: 0.1,
        pos_x: -0.1,
        ..Transform::default()
    };
    let mut half = clip(&m.bars, 0.0, 2.0, 0.0);
    half.transform.opacity = 0.5;
    let mut fast = clip(&m.testsrc, 0.2, 2.0, 1.0);
    fast.speed = 2.0;
    let cases = [
        (
            "single",
            timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None),
            vec![m.testsrc.clone()],
            vec![0.0, 0.5, 1.2345],
        ),
        (
            "contain/9x16",
            timeline(
                vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
                Some(Delivery::new(360, 640, Fit::Contain)),
            ),
            vec![m.testsrc.clone()],
            vec![0.5, 1.2],
        ),
        (
            "transform over gradient",
            timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![over]], None),
            vec![m.gradient.clone(), m.testsrc.clone()],
            vec![0.5, 1.5],
        ),
        (
            "opacity",
            timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)], vec![half]], None),
            vec![m.testsrc.clone(), m.bars.clone()],
            vec![0.0, 1.0],
        ),
        (
            "speed 2x, and the layers of one shot",
            timeline(vec![vec![fast], vec![clip(&m.testsrc, 0.2, 2.0, 1.0)]], None),
            vec![m.testsrc.clone()],
            vec![1.0, 1.4, 1.7],
        ),
    ];
    let mut captioned = vec![overlay("A caption, with a box: 50%", 0.0, 2.0), overlay("Title", 0.5, 1.5)];
    (captioned[0].size, captioned[0].pos_y, captioned[0].bg) = (0.05, 0.88, Some("black@0.5".into()));
    (captioned[1].size, captioned[1].bold, captioned[1].color) = (0.1, true, "yellow@0.9".into());
    let cases = {
        let mut cases = cases.to_vec();
        cases.push((
            "text over a transform",
            with_text(
                timeline(
                    vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
                    None,
                ),
                captioned,
            ),
            vec![m.gradient.clone(), m.testsrc.clone()],
            vec![0.25, 1.0],
        ));
        cases
    };
    let compositor = compositor();
    let source = kerf_gpu::FrameSource::new(kerf_gpu::FrameSourceConfig::default());
    assert!(source.runs_enabled(), "the run path is off on this ffmpeg");
    for (name, tl, assets, times) in &cases {
        let mut order: Vec<f64> = times.clone();
        order.extend(times.iter().rev());
        for (n, &t) in order.iter().enumerate() {
            let plan = RenderPlan::at(tl, assets, &ExportOptions::default(), t, policy()).expect("plan");
            let size = plan.size(u32::MAX);
            let (reference, _) = compositor.render_plan(&plan, size).expect("one-shot render");
            let (again, _) = compositor.render_plan(&plan, size).expect("one-shot render");
            assert_eq!(
                reference.data, again.data,
                "{name} @ {t}: the compositor is not deterministic, which this comparison needs"
            );
            let hint = if n % 2 == 0 {
                kerf_gpu::Hint::Scrub
            } else {
                kerf_gpu::Hint::Forward { fps: 30.0 }
            };
            let (with, _) = compositor
                .render_plan_with(&plan, size, &source, hint)
                .unwrap_or_else(|e| panic!("{name} @ {t}: {e}"));
            assert_eq!((with.width, with.height), (reference.width, reference.height));
            assert!(
                with.data == reference.data,
                "{name} @ {t} ({hint:?}): render_plan_with is not the picture render_plan draws"
            );
            // The presenter's route: the frame left on the GPU, then read back, is the same picture.
            let (texture, _) = compositor
                .render_plan_texture_with(&plan, size, &source, hint)
                .unwrap_or_else(|e| panic!("{name} @ {t}: {e}"));
            assert_eq!((texture.width, texture.height), (reference.width, reference.height));
            let on_gpu = texture.read_back(&gpu()).expect("read the texture back");
            assert!(
                on_gpu.data == reference.data,
                "{name} @ {t} ({hint:?}): render_plan_texture_with is not the picture render_plan draws"
            );
        }
    }
    let stats = source.stats();
    assert!(
        stats.spawned > 0 && stats.cache.hits > 0,
        "the source was not exercised: {stats:?}"
    );
    eprintln!("render_plan_with: {stats:?}");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_gap_is_black() {
    let m = media();
    let tl = timeline(vec![vec![clip(&m.testsrc, 0.0, 1.0, 0.0)]], None);
    // Past the clip: no layers, a black canvas at the footage size.
    check("gap", &tl, std::slice::from_ref(&m.testsrc), &[1.5], STRICT);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn contain_and_cover_into_a_vertical_frame() {
    let m = media();
    let assets = std::slice::from_ref(&m.testsrc);
    let contain = timeline(
        vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(360, 640, Fit::Contain)),
    );
    check("contain/9x16", &contain, assets, &[0.5, 1.2], STRICT);
    let cover = timeline(
        vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(360, 640, Fit::Cover)),
    );
    check("cover/9x16", &cover, assets, &[0.5, 1.2], STRICT);
    // A portrait source into a landscape frame, both ways round.
    let pa = std::slice::from_ref(&m.portrait);
    let tl = timeline(
        vec![vec![clip(&m.portrait, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(640, 360, Fit::Contain)),
    );
    check("contain/portrait-in-16x9", &tl, pa, &[0.7], STRICT);
    let tl = timeline(
        vec![vec![clip(&m.portrait, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(640, 360, Fit::Cover)),
    );
    check("cover/portrait-in-16x9", &tl, pa, &[0.7], STRICT);
    // Upscaling: the 640x360 source into a larger frame.
    let up = timeline(
        vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(960, 540, Fit::Contain)),
    );
    check("contain/upscale-960x540", &up, assets, &[0.5], STRICT);
    let down = timeline(
        vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(320, 180, Fit::Contain)),
    );
    check("contain/downscale-320x180", &down, assets, &[0.5], STRICT);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn two_overlapping_tracks() {
    let m = media();
    let mut top = clip(&m.testsrc, 0.0, 2.0, 0.0);
    top.transform = Transform {
        scale: 0.4,
        pos_x: 0.3,
        pos_y: -0.25,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![top]], None);
    check(
        "pip/scaled+offset",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        &[0.0, 1.0],
        STRICT,
    );

    // The later track is on top even when it starts later; before it starts
    // only the base shows.
    let late = clip(&m.testsrc, 0.0, 1.0, 1.0);
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![late]], None);
    check(
        "tracks/late-top-clip",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        &[0.5, 1.5],
        STRICT,
    );

    // Fit scale and the transform's own scale are two scalers in cascade (a
    // vertical frame, then a half-size picture-in-picture).
    let mut pip = clip(&m.testsrc, 0.0, 2.0, 0.0);
    pip.transform = Transform {
        scale: 0.6,
        pos_y: 0.2,
        ..Transform::default()
    };
    let tl = timeline(
        vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![pip]],
        Some(Delivery::new(360, 640, Fit::Cover)),
    );
    check(
        "pip/cover-then-scale",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        &[0.5],
        STRICT,
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn scaled_rotated_and_cropped() {
    let m = media();
    let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
    c.transform = Transform {
        scale: 0.7,
        rotation: 17.0,
        crop_left: 0.1,
        crop_top: 0.05,
        crop_right: 0.05,
        pos_x: -0.1,
        pos_y: 0.05,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![c]], None);
    check(
        "transform/scale+rotate+crop",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        &[0.5, 1.5],
        ROTATED,
    );

    let mut rot = clip(&m.bars, 0.0, 2.0, 0.0);
    rot.transform.rotation = 90.0;
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![rot]], None);
    check(
        "transform/rotate-90",
        &tl,
        &[m.gradient.clone(), m.bars.clone()],
        &[0.5],
        STRICT,
    );

    let mut crop = clip(&m.testsrc, 0.0, 2.0, 0.0);
    crop.transform = Transform {
        crop_left: 0.25,
        crop_right: 0.1,
        crop_top: 0.2,
        crop_bottom: 0.15,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![crop]], None);
    check("transform/crop-only", &tl, std::slice::from_ref(&m.testsrc), &[0.5], STRICT);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn odd_sizes_and_layers_that_leave_the_canvas() {
    let m = media();
    let base = clip(&m.gradient, 0.0, 2.0, 0.0);
    let over = |t: Transform, a: &Asset| {
        let mut c = clip(a, 0.0, 2.0, 0.0);
        c.transform = t;
        c
    };
    // A portrait picture fitted into 16:9 is 203 x 360 and half of that is 101 x
    // 180: an odd width, which `overlay` and chroma have to agree on.
    let tl = timeline(
        vec![
            vec![base.clone()],
            vec![over(
                Transform {
                    scale: 0.5,
                    pos_x: 0.2,
                    ..Transform::default()
                },
                &m.portrait,
            )],
        ],
        None,
    );
    check(
        "geometry/odd-sized-layer",
        &tl,
        &[m.gradient.clone(), m.portrait.clone()],
        &[0.5],
        STRICT,
    );
    // The same, turned: the rotated box is odd in both directions.
    let tl = timeline(
        vec![
            vec![base.clone()],
            vec![over(
                Transform {
                    scale: 0.5,
                    rotation: 33.0,
                    ..Transform::default()
                },
                &m.portrait,
            )],
        ],
        None,
    );
    check(
        "geometry/odd-rotated-box",
        &tl,
        &[m.gradient.clone(), m.portrait.clone()],
        &[0.5],
        ROTATED,
    );
    // Zoomed past the frame and panned: the layer starts left of the canvas.
    let tl = timeline(
        vec![
            vec![base.clone()],
            vec![over(
                Transform {
                    scale: 1.6,
                    pos_x: 0.3,
                    pos_y: -0.2,
                    ..Transform::default()
                },
                &m.testsrc,
            )],
        ],
        None,
    );
    check(
        "geometry/zoom-off-canvas",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        &[0.5],
        STRICT,
    );
    // Entirely off the canvas: nothing to draw, the base shows.
    let tl = timeline(
        vec![
            vec![base],
            vec![over(
                Transform {
                    scale: 0.5,
                    pos_x: 2.0,
                    ..Transform::default()
                },
                &m.testsrc,
            )],
        ],
        None,
    );
    check(
        "geometry/fully-off-canvas",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        &[0.5],
        STRICT,
    );
    // A user crop, then Cover's centre crop on top of it.
    let tl = timeline(
        vec![vec![over(
            Transform {
                crop_left: 0.2,
                crop_right: 0.05,
                crop_top: 0.1,
                ..Transform::default()
            },
            &m.testsrc,
        )]],
        Some(Delivery::new(360, 640, Fit::Cover)),
    );
    check(
        "geometry/crop-then-cover",
        &tl,
        std::slice::from_ref(&m.testsrc),
        &[0.5],
        STRICT,
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_rotated_neutral_layer_has_no_colour_fringe() {
    let m = media();
    // Mid-grey turned over black: every pixel of the result is a grey. FFmpeg's
    // `rotate` leaves chroma outside the picture at zero (a deep green) under an
    // alpha that is not quite zero there, and a few of its own pixels show it.
    // The compositor must not copy that: reading chroma outside the picture as
    // zero made a green rim on every rotated edge.
    for rotation in [17.0, 33.0, 45.0, -25.0, 5.0] {
        let mut c = clip(&m.grey, 0.0, 2.0, 0.0);
        c.transform.rotation = rotation;
        c.transform.scale = 0.7;
        let tl = timeline(vec![vec![clip(&m.black, 0.0, 2.0, 0.0)], vec![c]], None);
        let assets = [m.black.clone(), m.grey.clone()];
        let plan = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 0.5, policy()).expect("plan");
        let size = plan.size(u32::MAX);
        let (frame, _) = compositor().render_plan(&plan, size).expect("GPU render");
        let spread = |rgb: &[u8]| {
            rgb.chunks(3)
                .map(|p| p.iter().max().unwrap() - p.iter().min().unwrap())
                .max()
                .unwrap_or(0)
        };
        let gpu = spread(&rgba_to_rgb(&frame.data));
        // FFmpeg's own, for reference (it is not perfectly neutral either).
        let png = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-ref").join("fringe.png");
        export_still(&tl, &assets, &ExportOptions::default(), 0.5, &png, ImageFormat::Png, 0).expect("FFmpeg still");
        let reference = ffmpeg(&["-i", png.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
        let _ = std::fs::remove_file(&png);
        let ffmpeg_spread = spread(&reference);
        eprintln!("fringe/grey-rotated-{rotation:<5} worst channel spread: GPU {gpu}, FFmpeg {ffmpeg_spread}");
        // The input is neutral, so the output must be: that is the contract, not
        // "as bad as FFmpeg" (its own frame has a few green pixels along the edge,
        // spread ~100, for the very reason above — they sit in the edge band).
        assert!(
            gpu <= FRINGE_MAX,
            "rotation {rotation}: the GPU frame has a colour fringe (spread {gpu}; FFmpeg's has {ffmpeg_spread})"
        );
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn opacity() {
    let m = media();
    let mut top = clip(&m.testsrc, 0.0, 2.0, 0.0);
    top.transform.opacity = 0.5;
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![top]], None);
    check("opacity/0.5", &tl, &[m.bars.clone(), m.testsrc.clone()], &[0.0, 1.0], STRICT);

    // The roles swapped: saturated bars on top, whose out-of-gamut colour is
    // clipped by the RGB round trip FFmpeg's `colorchannelmixer` forces.
    let mut top = clip(&m.bars, 0.0, 2.0, 0.0);
    top.transform.opacity = 0.5;
    let tl = timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)], vec![top]], None);
    check(
        "opacity/bars-0.5-over-testsrc2",
        &tl,
        &[m.testsrc.clone(), m.bars.clone()],
        &[0.5],
        STRICT,
    );

    let mut top = clip(&m.gradient, 0.0, 2.0, 0.0);
    top.transform = Transform {
        opacity: 0.3,
        scale: 0.6,
        rotation: -25.0,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![top]], None);
    check(
        "opacity/0.3+scale+rotate",
        &tl,
        &[m.bars.clone(), m.gradient.clone()],
        &[0.5],
        ROTATED,
    );

    // Alone, over black — the layer's own picture is what is compared.
    for (name, a, color) in [
        ("bars-0.65-alone", &m.bars, Color::default()),
        (
            "testsrc2-0.65+brightness0.2",
            &m.testsrc,
            Color {
                brightness: 0.2,
                ..Color::default()
            },
        ),
        (
            "testsrc2-0.65+saturation1.9",
            &m.testsrc,
            Color {
                saturation: 1.9,
                ..Color::default()
            },
        ),
    ] {
        let mut c = clip(a, 0.0, 2.0, 0.0);
        c.transform.opacity = 0.65;
        c.color = color;
        let tl = timeline(vec![vec![c]], None);
        check(&format!("opacity/{name}"), &tl, std::slice::from_ref(a), &[0.5], STRICT);
    }

    // A BT.2020-tagged picture (saturated bars) at 0.6: out of YUV with its own
    // matrix, back as BT.601.
    let mut c = clip(&m.bars2020, 0.0, 2.0, 0.0);
    c.transform.opacity = 0.6;
    let tl = timeline(vec![vec![c]], None);
    check(
        "opacity/bt2020-bars-0.6",
        &tl,
        std::slice::from_ref(&m.bars2020),
        &[0.5],
        STRICT,
    );

    // A translucent clip with an odd source size and a crop.
    let mut c = clip(&m.odd, 0.0, 2.0, 0.0);
    c.transform = Transform {
        opacity: 0.7,
        crop_left: 0.1,
        crop_bottom: 0.15,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None);
    check(
        "opacity/odd-source-cropped-0.7",
        &tl,
        &[m.bars.clone(), m.odd.clone()],
        &[0.5],
        STRICT,
    );

    // A fade-in keyframed on a graded clip, sampled mid-fade (opacity 0.5).
    let mut fade = clip(&m.testsrc, 0.0, 2.0, 0.0);
    fade.color = Color {
        brightness: 0.1,
        saturation: 1.4,
        ..Color::default()
    };
    let key = |time: f64, opacity: f64| Keyframe {
        easing: Default::default(),
        time,
        scale: 1.0,
        pos_x: 0.0,
        pos_y: 0.0,
        rotation: 0.0,
        opacity,
    };
    fade.keyframes = vec![key(0.0, 0.0), key(1.0, 1.0)];
    let tl = timeline(vec![vec![fade]], None);
    check(
        "opacity/keyframed-fade+grade",
        &tl,
        std::slice::from_ref(&m.testsrc),
        &[0.5],
        STRICT,
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn colour_adjustments() {
    let m = media();
    let case = |name: &str, color: Color, asset: &Asset| {
        let mut c = clip(asset, 0.0, 2.0, 0.0);
        c.color = color;
        let tl = timeline(vec![vec![c]], None);
        check(name, &tl, std::slice::from_ref(asset), &[0.5], STRICT);
    };
    // All four knobs: gamma != 1 on luma, so the pow table.
    case(
        "colour/brightness+contrast+saturation+gamma",
        Color {
            brightness: 0.08,
            contrast: 1.2,
            saturation: 1.4,
            gamma: 1.25,
            temperature: 0.0,
        },
        &m.testsrc,
    );
    // Contrast + saturation only: gamma is 1 everywhere, so FFmpeg's fixed-point
    // `process` path.
    case(
        "colour/contrast+saturation-only",
        Color {
            brightness: 0.0,
            contrast: 1.3,
            saturation: 0.6,
            gamma: 1.0,
            temperature: 0.0,
        },
        &m.bars,
    );
    // Warm and cool: the chroma-plane power function.
    case(
        "colour/warm",
        Color {
            temperature: 0.6,
            ..Color::default()
        },
        &m.gradient,
    );
    case(
        "colour/cool+brightness",
        Color {
            temperature: -0.8,
            brightness: -0.05,
            ..Color::default()
        },
        &m.testsrc,
    );
    // Colour on a transformed clip: the tables apply to the scaled planes.
    let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
    c.color = Color {
        saturation: 1.6,
        gamma: 0.8,
        temperature: 0.3,
        ..Color::default()
    };
    c.transform = Transform {
        scale: 0.8,
        pos_x: 0.1,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![c]], None);
    check(
        "colour/on-a-transformed-clip",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        &[0.5],
        STRICT,
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn still_images() {
    let m = media();
    // The PNG alone: the frame is the picture's own shape.
    let tl = timeline(vec![vec![clip(&m.still, 0.0, 5.0, 0.0)]], None);
    check("still/alone", &tl, std::slice::from_ref(&m.still), &[0.0, 2.5], STRICT);
    // A JPEG is full-range YUV, which has to be brought to the limited range the
    // composite works in (FFmpeg does it in the graph; the decode does it here).
    let tl = timeline(vec![vec![clip(&m.jpeg, 0.0, 5.0, 0.0)]], None);
    check("still/jpeg", &tl, std::slice::from_ref(&m.jpeg), &[1.0], STRICT);
    // On top of video, scaled: a JPEG (4:2:0). A PNG is RGB, and resizing a picture
    // that is not 4:2:0 is refused — so a PNG goes *under* the video, at its own size,
    // and a PNG pip is asserted refused.
    let pip = Transform {
        scale: 0.5,
        pos_x: -0.2,
        pos_y: -0.2,
        ..Transform::default()
    };
    let mut pic = clip(&m.jpeg, 0.0, 5.0, 0.0);
    pic.transform = pip;
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![pic]], None);
    check(
        "still/pip-over-video",
        &tl,
        &[m.gradient.clone(), m.jpeg.clone()],
        &[1.0],
        STRICT,
    );
    let mut video_pip = clip(&m.gradient, 0.0, 2.0, 0.0);
    video_pip.transform = pip;
    let tl = timeline(vec![vec![clip(&m.png640, 0.0, 5.0, 0.0)], vec![video_pip]], None);
    check(
        "still/png-under-video-pip",
        &tl,
        &[m.png640.clone(), m.gradient.clone()],
        &[1.0],
        STRICT,
    );
    let mut png_pip = clip(&m.png640, 0.0, 5.0, 0.0);
    png_pip.transform = pip;
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![png_pip]], None);
    check_refused(
        "still/png-pip-over-video",
        &tl,
        &[m.gradient.clone(), m.png640.clone()],
        1.0,
        Refusal::Plan("scales a"),
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn source_time_speed_reverse_and_keyframes() {
    let m = media();
    // testsrc2 carries a frame counter, so a wrong source time is a different
    // picture — these cases fail loudly if the GPU decodes the wrong frame.
    let mut fast = clip(&m.testsrc, 0.2, 2.0, 1.0);
    fast.speed = 2.0;
    let tl = timeline(vec![vec![fast]], None);
    check(
        "time/speed-2x-offset",
        &tl,
        std::slice::from_ref(&m.testsrc),
        &[1.0, 1.4, 1.7],
        STRICT,
    );

    let mut rev = clip(&m.testsrc, 0.0, 2.0, 0.0);
    rev.speed = -1.0;
    let tl = timeline(vec![vec![rev]], None);
    check("time/reversed", &tl, std::slice::from_ref(&m.testsrc), &[0.3, 1.1], STRICT);

    // Keyframed transform: sampled at the same clip time by both renderers. The zoom moves and
    // so does the position, with nothing in front of the zoom (a moving zoom is the *last*
    // stage of FFmpeg's chain, so a grade, a rotation or a fade of opacity is applied to the
    // picture at its fit size and magnified; the plan refuses that, see below).
    let key = |time: f64, scale: f64, pos: (f64, f64), rotation: f64, opacity: f64| Keyframe {
        easing: Default::default(),
        time,
        scale,
        pos_x: pos.0,
        pos_y: pos.1,
        rotation,
        opacity,
    };
    let mut kf = clip(&m.testsrc, 0.0, 2.0, 0.5);
    kf.keyframes = vec![key(0.0, 1.0, (0.0, 0.0), 0.0, 1.0), key(1.5, 0.5, (0.2, -0.1), 0.0, 1.0)];
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![kf]], None);
    check(
        "time/keyframes",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        &[0.5, 1.25, 2.0],
        STRICT,
    );
    // The rotation and the opacity keyed too, over a zoom that holds still: both are sampled at
    // the clip's time and the order is the one the GPU has.
    let mut kf = clip(&m.testsrc, 0.0, 2.0, 0.5);
    kf.keyframes = vec![key(0.0, 0.5, (0.0, 0.0), 0.0, 1.0), key(1.5, 0.5, (0.2, -0.1), 10.0, 0.6)];
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![kf]], None);
    check(
        "time/keyframes+rotation+opacity",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        &[0.5, 1.25, 2.0],
        ROTATED,
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn sources_that_are_not_plain_8_bit_420() {
    let m = media();
    for (name, a, t) in [
        ("source/10-bit", &m.tenbit, 0.5),
        ("source/4:4:4", &m.yuv444, 0.5),
        ("source/full-range-jpeg", &m.fullrange, 0.5),
        ("source/odd-641x361", &m.odd, 0.5),
        ("source/rotated-by-metadata", &m.rotated, 0.5),
    ] {
        let tl = timeline(vec![vec![clip(a, 0.0, 2.0, 0.0)]], None);
        check(name, &tl, std::slice::from_ref(a), &[t], STRICT);
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_letterboxed_layer_covers_what_is_below_it() {
    let m = media();
    // `pad` hands `overlay` a full-canvas frame, black bars included: a portrait
    // clip over a landscape one hides the base entirely, it does not let the
    // base show through the sides.
    let tl = timeline(
        vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![clip(&m.portrait, 0.0, 2.0, 0.0)]],
        None,
    );
    check(
        "letterbox/portrait-over-landscape",
        &tl,
        &[m.bars.clone(), m.portrait.clone()],
        &[0.5],
        STRICT,
    );
    // ...and the same in a 9:16 project: a landscape clip over a portrait one.
    let tl = timeline(
        vec![vec![clip(&m.portrait, 0.0, 2.0, 0.0)], vec![clip(&m.bars, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(360, 640, Fit::Contain)),
    );
    check(
        "letterbox/landscape-over-portrait-9x16",
        &tl,
        &[m.portrait.clone(), m.bars.clone()],
        &[0.5],
        STRICT,
    );
    // The bars are part of the frame `eq` sees: a brightened clip has brightened
    // bars (Y 16 -> 16 + 0.15 * 219, U / V through the chroma tables).
    let mut graded = clip(&m.portrait, 0.0, 2.0, 0.0);
    graded.color.brightness = 0.15;
    let tl = timeline(vec![vec![graded]], Some(Delivery::new(640, 360, Fit::Contain)));
    check(
        "letterbox/graded-single-clip",
        &tl,
        std::slice::from_ref(&m.portrait),
        &[0.5],
        STRICT,
    );
    // Graded bars on top of another track, saturation and temperature in play.
    let mut graded = clip(&m.portrait, 0.0, 2.0, 0.0);
    graded.color = Color {
        saturation: 1.7,
        temperature: 0.5,
        contrast: 1.1,
        ..Color::default()
    };
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![graded]], None);
    check(
        "letterbox/graded-over-gradient",
        &tl,
        &[m.gradient.clone(), m.portrait.clone()],
        &[0.5],
        STRICT,
    );
    // An odd fitted picture (203 rows, of which `pad` keeps 202).
    let tl = timeline(
        vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(360, 640, Fit::Contain)),
    );
    check(
        "letterbox/odd-fit-over-gradient",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        &[0.5],
        STRICT,
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn an_odd_layer_in_a_frame_with_an_odd_chroma_line() {
    let m = media();
    // 722x640 with a half-size 640x360 clip is a 361x203 layer: both sides odd,
    // so `overlay`'s last chroma block reaches a pixel past it on both axes. The
    // whole-image floor used to read 38.9 dB here.
    let mut pip = clip(&m.testsrc, 0.0, 2.0, 0.0);
    pip.transform = Transform {
        scale: 0.5,
        pos_x: 0.1,
        ..Transform::default()
    };
    let tl = timeline(
        vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![pip]],
        Some(Delivery::new(722, 640, Fit::Contain)),
    );
    check(
        "pip/odd-361x203-in-722x640",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        &[0.5],
        STRICT,
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn busy_sources_are_held_to_the_scaler_not_to_a_flat_region() {
    let m = media();
    // Noise and a 1-px checkerboard, scaled by ratios that are not whole numbers
    // (down 0.75 and 0.6, up 1.5): every output pixel depends on the exact kernel
    // (taps, phase, stretch on shrink). The edge band covers these pictures, so
    // the whole image is the evidence (`BUSY`).
    for (name, a) in [("noise", &m.noise), ("checker", &m.checker)] {
        for (w, h) in [(480u32, 270u32), (384, 216), (960, 540)] {
            let tl = timeline(vec![vec![clip(a, 0.0, 0.2, 0.0)]], Some(Delivery::new(w, h, Fit::Contain)));
            check(&format!("busy/{name}-to-{w}x{h}"), &tl, std::slice::from_ref(a), &[0.1], BUSY);
        }
    }
    // The transform's own scale after the fit scale: two kernels in cascade.
    let mut pip = clip(&m.noise, 0.0, 0.2, 0.0);
    pip.transform = Transform {
        scale: 0.7,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![pip]], None);
    check(
        "busy/noise-scaled-0.7",
        &tl,
        &[m.gradient.clone(), m.noise.clone()],
        &[0.1],
        BUSY,
    );
}

/// What a refused frame must be refused for.
enum Refusal {
    /// The plan itself says no (`gpu_supported_at`), with this in its reasons.
    Plan(&'static str),
    /// The plan cannot know, the render finds out (the decode, or the layer's
    /// geometry): `Unsupported` with this in it.
    Render(&'static str),
}

/// FFmpeg still renders the frame (that is the fallback), and the GPU path says
/// no — by the plan or by the decode — instead of drawing it wrong.
fn check_refused(case: &str, tl: &Timeline, assets: &[Asset], t: f64, expect: Refusal) {
    check_refused_with(case, tl, assets, t, expect, &ExportOptions::default());
}

/// [`check_refused`] for a render with options.
fn check_refused_with(case: &str, tl: &Timeline, assets: &[Asset], t: f64, expect: Refusal, opts: &ExportOptions) {
    let plan = RenderPlan::at(tl, assets, opts, t, policy()).expect("plan");
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    let png = scratch.join(format!("refused-{}.png", case.replace('/', "_")));
    export_still(tl, assets, opts, t, &png, ImageFormat::Png, 0).expect("the FFmpeg fallback renders it");
    let _ = std::fs::remove_file(&png);
    match expect {
        Refusal::Plan(reason) => {
            let reasons = reasons(&plan, plan.size(u32::MAX));
            assert!(!reasons.is_empty(), "{case}: the plan should refuse");
            let reasons = reasons.join("; ");
            assert!(reasons.contains(reason), "{case}: {reasons:?} does not say {reason:?}");
        }
        Refusal::Render(reason) => {
            let why = reasons(&plan, plan.size(u32::MAX));
            assert!(why.is_empty(), "{case}: {why:?}");
            let err = compositor()
                .render_plan(&plan, plan.size(u32::MAX))
                .expect_err("the render should refuse");
            assert!(
                matches!(&err, kerf_gpu::GpuError::Unsupported(why) if why.contains(reason)),
                "{case}: {err}"
            );
        }
    }
    eprintln!("{case:<40} refused as expected, FFmpeg renders it");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn frames_the_gpu_would_draw_wrong_are_refused() {
    let m = media();
    // An EXIF-oriented JPEG: probed 480x270, decoded 270x480.
    assert_eq!(
        (m.exif.streams[0].width, m.exif.streams[0].height),
        (Some(480), Some(270)),
        "the probe does not apply the EXIF orientation"
    );
    let tl = timeline(vec![vec![clip(&m.exif, 0.0, 5.0, 0.0)]], None);
    check_refused(
        "refused/exif-orientation",
        &tl,
        std::slice::from_ref(&m.exif),
        1.0,
        Refusal::Render("probed as"),
    );

    // Video with an alpha channel: the plan knows from the probed pixel format.
    assert_eq!(m.alpha.streams[0].pix_fmt.as_deref(), Some("yuva420p"));
    let tl = timeline(
        vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![clip(&m.alpha, 0.0, 2.0, 0.0)]],
        None,
    );
    check_refused(
        "refused/alpha-video",
        &tl,
        &[m.bars.clone(), m.alpha.clone()],
        0.5,
        Refusal::Plan("alpha"),
    );
    // An asset saved before the pixel format was recorded: the decode checks the
    // alpha plane itself. (Under a negotiating FFmpeg the plan refuses it sooner —
    // the composite's matrix depends on a tag it cannot know — so the decode is
    // asked directly.)
    let mut unknown = m.alpha.clone();
    unknown.streams[0].pix_fmt = None;
    let tl = timeline(
        vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![clip(&unknown, 0.0, 2.0, 0.0)]],
        None,
    );
    let assets = [m.bars.clone(), unknown];
    let plan = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 0.5, policy()).expect("plan");
    let err = kerf_gpu::decode_layer(&plan.layers[1]).expect_err("the decode should find the transparency");
    assert!(
        matches!(&err, kerf_gpu::GpuError::Unsupported(why) if why.contains("transparency")),
        "{err}"
    );
    match policy() {
        CompositeColorPolicy::FixedBt601 => check_refused(
            "refused/alpha-video-unknown-pix-fmt",
            &tl,
            &assets,
            0.5,
            Refusal::Render("transparency"),
        ),
        CompositeColorPolicy::BottomLayerTag => check_refused(
            "refused/alpha-video-unknown-pix-fmt (negotiated)",
            &tl,
            &assets,
            0.5,
            Refusal::Plan("matrix is unknown"),
        ),
        CompositeColorPolicy::Unknown => unreachable!("policy() never answers Unknown"),
    }
    // A translucent layer with an odd side (0.33 of a 640x360 is 211x118): FFmpeg's
    // RGB round trip reads uninitialised padding past the picture there.
    let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
    c.transform = Transform {
        scale: 0.33,
        opacity: 0.6,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![c]], None);
    check_refused(
        "refused/translucent-odd-layer",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        0.5,
        Refusal::Plan("odd size"),
    );
    // A moving zoom with a rotation or a fade of opacity in front of it: FFmpeg (the still and the
    // export alike) runs the zoom last, so those act on the picture at its fit size and are
    // magnified with it, an order the compositor does not have for these two (a grade, a mask, the
    // effects and the fades it does: `a_moving_zoom_is_the_last_stage_of_the_still_too`).
    let key = |time: f64, scale: f64, rotation: f64, opacity: f64| Keyframe {
        easing: Default::default(),
        time,
        scale,
        pos_x: 0.0,
        pos_y: 0.0,
        rotation,
        opacity,
    };
    let mut zoom = clip(&m.testsrc, 0.0, 2.0, 0.5);
    zoom.keyframes = vec![key(0.0, 1.0, 0.0, 1.0), key(1.5, 0.5, 10.0, 0.6)];
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![zoom]], None);
    check_refused(
        "refused/moving-zoom-behind-rotation",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        1.25,
        Refusal::Plan("moving zoom"),
    );
    let mut faint = clip(&m.testsrc, 0.0, 2.0, 0.5);
    faint.keyframes = vec![key(0.0, 1.0, 0.0, 0.6), key(1.5, 0.5, 0.0, 0.6)];
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![faint]], None);
    check_refused(
        "refused/moving-zoom-behind-an-opacity",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        1.25,
        Refusal::Plan("moving zoom"),
    );
    // What the compositor does not draw of an effect or a mask: a vignette (its dither is a counter that runs
    // on from frame to frame), a chroma key (an alpha from a float colour distance), a mask on a turned layer
    // or one below full opacity, a blur or a sharpen on the padded frame of a letterbox, a value the filters refuse.
    let effect = |e: kerf_core::VideoEffect| {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.effects = vec![e];
        timeline(vec![vec![c]], None)
    };
    check_refused(
        "refused/vignette",
        &effect(kerf_core::VideoEffect::Vignette),
        std::slice::from_ref(&m.testsrc),
        0.5,
        Refusal::Plan("vignette"),
    );
    check_refused(
        "refused/chroma-key",
        &effect(kerf_core::VideoEffect::ChromaKey {
            color: "green".into(),
            similarity: 0.1,
            blend: 0.1,
        }),
        std::slice::from_ref(&m.testsrc),
        0.5,
        Refusal::Plan("chroma key"),
    );
    // (A value the filter's own option refuses fails in FFmpeg too: the plan refuses the frame so that FFmpeg
    // reports it, which is all there is to check.)
    for (name, e, reason) in [
        ("blur sigma", kerf_core::VideoEffect::Blur { sigma: 5000.0 }, "gblur refuses"),
        (
            "sharpen amount",
            kerf_core::VideoEffect::Sharpen { amount: 8.0 },
            "unsharp refuses",
        ),
    ] {
        let tl = effect(e);
        let plan = RenderPlan::at(
            &tl,
            std::slice::from_ref(&m.testsrc),
            &ExportOptions::default(),
            0.5,
            policy(),
        )
        .expect("plan");
        let why = reasons(&plan, plan.size(u32::MAX)).join("; ");
        assert!(why.contains(reason), "refused/{name}-out-of-range: {why:?}");
        eprintln!(
            "{:<40} refused as expected (FFmpeg refuses the value too)",
            format!("refused/{name}-out-of-range")
        );
    }
    let masked = |edit: &dyn Fn(&mut Clip), format: Option<Delivery>| {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.mask = Some(a_mask(kerf_core::MaskShape::Rect, 0.1, false));
        edit(&mut c);
        timeline(vec![vec![c]], format)
    };
    check_refused(
        "refused/mask-on-a-rotated-layer",
        &masked(&|c| c.transform.rotation = 12.0, None),
        std::slice::from_ref(&m.testsrc),
        0.5,
        Refusal::Plan("rotated layer"),
    );
    check_refused(
        "refused/mask-below-full-opacity",
        &masked(&|c| c.transform.opacity = 0.6, None),
        std::slice::from_ref(&m.testsrc),
        0.5,
        Refusal::Plan("below full opacity"),
    );
    let mut blurred = clip(&m.testsrc, 0.0, 2.0, 0.0);
    blurred.effects = vec![kerf_core::VideoEffect::Blur { sigma: 3.0 }];
    check_refused(
        "refused/blur-on-a-letterbox",
        &timeline(vec![vec![blurred]], Some(Delivery::new(360, 640, Fit::Contain))),
        std::slice::from_ref(&m.testsrc),
        0.5,
        Refusal::Plan("letterboxed"),
    );
    // A black or white fade on a layer below full opacity: `colorchannelmixer` leaves the picture in
    // RGBA and `fade` runs on that, alpha plane included, ahead of the conversion back to `yuva420p`
    // (the first measurement of the Motion harness: Y 153 levels off). The frames the fade is
    // running on are FFmpeg's; the dissolve's ramp, which touches the alpha alone, is drawn
    // (`motion_parity.rs`).
    let mut glass = clip(&m.testsrc, 0.0, 2.0, 0.0);
    glass.transform.opacity = 0.6;
    glass.fade_in = 1.0;
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![glass]], None);
    check_refused(
        "refused/fade-on-a-translucent-layer",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        0.3,
        Refusal::Plan("below full opacity"),
    );
    // A dip to white is drawn through the RGB round trip on a plain chain; on an alpha plane FFmpeg
    // converts through another RGB format, and a black fade of the same clip ahead of it is a filter
    // of its own in the YUV.
    let white_in = |edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.transition_in = Some(Transition {
            kind: TransitionKind::DipToWhite,
            duration: 1.0,
        });
        edit(&mut c);
        timeline(vec![vec![c]], None)
    };
    check_refused(
        "refused/dip-to-white-on-a-rotated-layer",
        &white_in(&|c| c.transform.rotation = 10.0),
        std::slice::from_ref(&m.testsrc),
        0.2,
        Refusal::Plan("alpha plane"),
    );
    check_refused(
        "refused/dip-to-white-with-a-black-fade",
        &white_in(&|c| c.fade_in = 0.8),
        std::slice::from_ref(&m.testsrc),
        0.2,
        Refusal::Plan("black fade"),
    );
}

/// The full-range (`yuvj`) picture under a fade, a dip or a dissolve: FFmpeg 6 converts the range at the
/// decode, as the compositor does, and the case is drawn strictly; FFmpeg 9 fades the raw full-range
/// values (black 16) and converts the range afterwards (measured 17 levels in Y and 16 in UV off), so the
/// plan refuses it. Under either, a picture the scaler *resizes* is refused while a fade runs on it
/// (`resized`): swscale converts the range inside the resize (flat error 14 on a 1.5x enlargement
/// on FFmpeg 6). `negotiated` says whether FFmpeg 9 refuses the picture under a running fade at all (a `yuvj` or a
/// deeper-than-8-bit one: its range is not told by its name). The frames the plan does not refuse (the case lists
/// some on purpose) are drawn.
fn check_full_range(
    case: &str,
    tl: &Timeline,
    assets: &[Asset],
    times: &[f64],
    opts: &ExportOptions,
    resized: bool,
    negotiated: bool,
) {
    let mut refused = 0;
    for &t in times {
        let plan = RenderPlan::at(tl, assets, opts, t, policy()).expect("plan");
        if reasons(&plan, plan.size(u32::MAX)).is_empty() {
            check_with(case, tl, assets, &[t], STRICT, opts);
        } else {
            // Which reason, by variant: FFmpeg 6 only refuses what the scaler resizes, FFmpeg 9 any picture
            // under a running fade (the two messages share their words).
            let why = plan.reasons(&compositor().caps(), plan.size(u32::MAX));
            let want = |u: &Unsupported| match policy() {
                CompositeColorPolicy::FixedBt601 => matches!(u, Unsupported::FullRangeResized(..)),
                _ => matches!(u, Unsupported::FullRangeNegotiated(_)),
            };
            assert!(
                why.iter().any(want),
                "{case} @ {t}: not refused for the expected reason: {why:?}"
            );
            check_refused_with(
                &format!("{case} (refused)"),
                tl,
                assets,
                t,
                Refusal::Plan("full-range picture"),
                opts,
            );
            refused += 1;
        }
    }
    let wanted = resized || (negotiated && policy() != CompositeColorPolicy::FixedBt601);
    assert_eq!(refused > 0, wanted, "{case}: {refused} of {} frames refused", times.len());
}

/// A dip to white goes through `yuv420p -> rgb24 -> yuv420p`, and the compositor draws that for the
/// pictures it was measured on (8-bit limited-range 4:2:0, untagged or BT.709): 2 levels of the
/// planes (3 on BT.709), well inside the strict bounds. The pictures it is not drawn for were all
/// measured off by more than the bound — RGB 11 / 12, 10-bit 9 / 16, 4:4:4 16 / 17, 4:2:2 9 / 13 — and so
/// was a letterbox (FFmpeg pads before the round trip, so its bars go white too: Y 126 against 16 at
/// half way), a picture with an odd side (the round trip reads past it) and a layer the scaler
/// resizes (the `scale` filter converts to RGB itself). The dip to black on the same pictures is
/// exact, and is drawn.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_dip_to_white_is_drawn_where_its_round_trip_was_measured_and_refused_elsewhere() {
    let m = media();
    let opts = ExportOptions {
        fps: Some(30.0),
        ..ExportOptions::default()
    };
    let dip = |a: &Asset, kind: TransitionKind, edit: &dyn Fn(&mut Clip), format: Option<Delivery>| {
        let mut c = clip(a, 0.0, 1.5, 0.0);
        c.transition_in = Some(Transition { kind, duration: 1.0 });
        edit(&mut c);
        timeline(vec![vec![c]], format)
    };
    let (white, black) = (TransitionKind::DipToWhite, TransitionKind::DipToBlack);
    let times = [0.05, 0.2, 0.4];
    let none = |_: &mut Clip| {};
    // Drawn: the BT.709 picture.
    let one = |a: &Asset| std::slice::from_ref(a).to_vec();
    check_with(
        "dip-white/bt709-tagged",
        &dip(&m.testsrc709, white, &none, None),
        &one(&m.testsrc709),
        &times,
        STRICT,
        &opts,
    );
    // A smaller layer that is not resized (a crop) is drawn, its edge included; one the scaler resizes
    // is refused: the `scale` filter converts to RGB itself (Y 19 / UV 28 off at sharp edges).
    check_with(
        "dip-white/cropped-layer",
        &dip(
            &m.testsrc,
            white,
            &|c| {
                c.transform.crop_left = 0.25;
                c.transform.crop_right = 0.25;
            },
            None,
        ),
        &one(&m.testsrc),
        &times,
        STRICT,
        &opts,
    );
    check_refused(
        "refused/dip-to-white-half-size-layer",
        &dip(&m.testsrc, white, &|c| c.transform.scale = 0.5, None),
        &one(&m.testsrc),
        0.2,
        Refusal::Plan("scaler resizes"),
    );
    check_with(
        "dip-black/half-size-layer",
        &dip(&m.testsrc, black, &|c| c.transform.scale = 0.5, None),
        &one(&m.testsrc),
        &times,
        STRICT,
        &opts,
    );
    // Refused: a letterboxed layer, and a layer of odd size (319x179 in a 640x360 canvas).
    let tall = Some(Delivery::new(640, 360, Fit::Contain));
    check_refused(
        "refused/dip-to-white-letterboxed",
        &dip(&m.portrait, white, &none, tall),
        &one(&m.portrait),
        0.2,
        Refusal::Plan("letterboxed"),
    );
    check_with(
        "dip-black/letterboxed",
        &dip(&m.portrait, black, &none, tall),
        &one(&m.portrait),
        &times,
        STRICT,
        &opts,
    );
    check_refused(
        "refused/dip-to-white-odd-layer",
        &dip(&m.testsrc, white, &|c| c.transform.scale = 0.4985, None),
        &one(&m.testsrc),
        0.2,
        Refusal::Plan("odd size 319x179"),
    );
    // A matrix with no measured round trip.
    check_refused(
        "refused/dip-to-white-bt2020",
        &dip(&m.testsrc2020, white, &none, None),
        &one(&m.testsrc2020),
        0.2,
        Refusal::Plan("colour matrix"),
    );
    // Refused: every picture that is not 8-bit limited-range 4:2:0; the dip to black on it is drawn.
    for (name, a) in [
        ("rgb-png", &m.png640),
        ("10-bit", &m.tenbit),
        ("4:4:4", &m.yuv444),
        ("4:2:2", &m.yuv422),
        ("bgr0", &m.bgr0),
        ("gray", &m.gray),
    ] {
        check_refused(
            &format!("refused/dip-to-white-{name}"),
            &dip(a, white, &none, None),
            &one(a),
            0.2,
            Refusal::Plan("not 8-bit limited-range"),
        );
        // (a 10-bit picture is maybe full range under FFmpeg 9, so its dip to black is refused there)
        check_full_range(
            &format!("dip-black/{name}"),
            &dip(a, black, &none, None),
            &one(a),
            &times,
            &opts,
            false,
            name == "10-bit",
        );
    }
    for (name, a) in [("full-range-mjpeg", &m.fullrange), ("full-range-jpeg-still", &m.jpeg)] {
        check_refused(
            &format!("refused/dip-to-white-{name}"),
            &dip(a, white, &none, None),
            &one(a),
            0.2,
            Refusal::Plan("not 8-bit limited-range"),
        );
    }
}

/// A fade, a dip and a dissolve on a full-range picture: see [`check_full_range`].
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_full_range_picture_fades_as_the_ffmpeg_in_use_converts_its_range() {
    let m = media();
    eprintln!("composite colour policy of this FFmpeg: {:?}", policy());
    let opts = ExportOptions {
        fps: Some(30.0),
        ..ExportOptions::default()
    };
    // (the JPEG still is 480x270 and the canvas of a cut that starts with the bars 640x360: resized)
    for (name, a, resized) in [("mjpeg", &m.fullrange, false), ("jpeg-still", &m.jpeg, true)] {
        let mut fading = clip(a, 0.0, 1.5, 0.0);
        fading.fade_in = 0.5;
        fading.fade_out = 0.5;
        let tl = timeline(vec![vec![fading]], None);
        check_full_range(
            &format!("full-range/fade-in-out {name}"),
            &tl,
            std::slice::from_ref(a),
            &[0.0, 0.1, 0.25, 1.1, 1.45],
            &opts,
            false,
            true,
        );
        let mut dipped = clip(a, 0.0, 1.5, 1.0);
        dipped.transition_in = Some(Transition {
            kind: TransitionKind::DipToBlack,
            duration: 0.6,
        });
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 1.0, 0.0), dipped]], None);
        check_full_range(
            &format!("full-range/dip-black {name}"),
            &tl,
            &[m.bars.clone(), a.clone()],
            &[0.8, 1.1, 1.25],
            &opts,
            resized,
            true,
        );
        let mut dissolved = clip(a, 0.0, 1.5, 1.0);
        dissolved.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.6,
        });
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 1.0, 0.0), dissolved]], None);
        check_full_range(
            &format!("full-range/dissolve {name}"),
            &tl,
            &[m.bars.clone(), a.clone()],
            &[1.1, 1.3],
            &opts,
            resized,
            true,
        );
        // Not fading, the picture is drawn: the range is converted at the decode in a still.
        let tl = timeline(vec![vec![clip(a, 0.0, 1.5, 0.0)]], None);
        check_with(
            &format!("full-range/plain {name}"),
            &tl,
            std::slice::from_ref(a),
            &[0.5],
            STRICT,
            &opts,
        );
    }
    // An enlargement of the MJPEG clip, fading in.
    let mut fading = clip(&m.fullrange, 0.0, 1.5, 0.0);
    fading.fade_in = 0.5;
    let tl = timeline(vec![vec![fading]], Some(Delivery::new(960, 540, Fit::Contain)));
    check_full_range(
        "full-range/fade-in enlarged",
        &tl,
        std::slice::from_ref(&m.fullrange),
        &[0.1, 0.25, 1.0],
        &opts,
        true,
        true,
    );
    // A 10-bit file tagged full range probes as `yuv420p10le` like the limited one and 9.0.2 fades its raw values
    // (flat max 16 / 15 / 11 against a bound of 8): the name cannot tell the two apart, so under FFmpeg 9 every
    // deeper-than-8-bit picture is refused inside a fade, the limited one included. FFmpeg 6 draws both.
    for (name, a) in [("pc-tagged 10-bit", &m.tenbit_pc), ("limited 10-bit", &m.tenbit)] {
        let mut fading = clip(a, 0.0, 1.5, 0.0);
        fading.fade_in = 0.5;
        fading.fade_out = 0.5;
        let tl = timeline(vec![vec![fading]], None);
        check_full_range(
            &format!("full-range/fade-in-out {name}"),
            &tl,
            std::slice::from_ref(a),
            &[0.0, 0.1, 0.25, 1.1, 1.45],
            &opts,
            false,
            true,
        );
    }
}

/// Every kind of transition and a clip's own fades, in a still, strictly against FFmpeg's still:
/// a frame just inside each transition and one two thirds through (the outgoing clip on its tail,
/// the incoming one on its ramp, dipped, travelling), the incoming clip covering the frame and
/// smaller than it (so that the outgoing clip is seen around it).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn every_kind_of_transition_and_the_clips_own_fades_are_drawn_in_a_still() {
    let m = media();
    let opts = ExportOptions {
        fps: Some(30.0),
        ..ExportOptions::default()
    };
    let assets = [m.bars.clone(), m.testsrc.clone()];
    for kind in TransitionKind::ALL {
        for (cover, scale) in [("covering", 1.0), ("partial", 0.5)] {
            let mut incoming = clip(&m.testsrc, 0.0, 1.0, 1.0);
            incoming.transform.scale = scale;
            incoming.transition_in = Some(Transition { kind, duration: 0.6 });
            let tl = timeline(vec![vec![clip(&m.bars, 0.0, 1.0, 0.0), incoming]], None);
            // A dip's halves are 0.3 s either side of the cut, the others run 0.6 s after it.
            let times = [0.8, 1.0 + 0.1, 1.0 + 0.4, 1.0 + 0.58];
            // A dip to white on a layer the scaler resizes is FFmpeg's (`scale` converts to RGB itself).
            if kind == TransitionKind::DipToWhite && scale < 1.0 {
                check_refused_with(
                    &format!("transition-kind/{}-{cover}", kind.as_str()),
                    &tl,
                    &assets,
                    times[1],
                    Refusal::Plan("scaler resizes"),
                    &opts,
                );
                continue;
            }
            check_with(
                &format!("transition-kind/{}-{cover}", kind.as_str()),
                &tl,
                &assets,
                &times,
                STRICT,
                &opts,
            );
        }
    }
    // A clip's own fades, alone and over another track, on a letterbox and graded.
    let mut fading = clip(&m.testsrc, 0.0, 1.5, 0.0);
    fading.fade_in = 0.5;
    fading.fade_out = 0.5;
    let tl = timeline(vec![vec![fading.clone()]], None);
    check_with(
        "fade/in-out",
        &tl,
        std::slice::from_ref(&m.testsrc),
        &[0.1, 0.25, 0.4, 1.1, 1.3, 1.45],
        STRICT,
        &opts,
    );
    let mut small = fading;
    small.transform.scale = 0.5;
    small.color.contrast = 1.2;
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 1.5, 0.0)], vec![small]], None);
    check_with(
        "fade/small-graded-over-another",
        &tl,
        &assets,
        &[0.1, 0.25, 1.3],
        STRICT,
        &opts,
    );
    let mut tall = clip(&m.portrait, 0.0, 1.5, 0.0);
    tall.fade_in = 0.5;
    let tl = timeline(vec![vec![tall]], Some(Delivery::new(640, 360, Fit::Contain)));
    check_with(
        "fade/letterboxed",
        &tl,
        std::slice::from_ref(&m.portrait),
        &[0.1, 0.3],
        STRICT,
        &opts,
    );
}

/// One transition cut at 30 fps — an outgoing clip for a second, then `testsrc2` from 1.0 s —
/// through `kind` over `secs`, the incoming clip `scale` of the frame (1 covers it, 0.5 leaves a
/// border).
///
/// FFmpeg's still draws a frame inside a transition as the export does (the outgoing clip on its
/// tail under the incoming one's ramp, the dips, the slide and push travel), and so does the
/// compositor: a still plan holds the fades still changing the picture, the travel and the tail
/// layer. So every frame listed — before the transition, inside it, the frame on which only the
/// outgoing clip's tail is left, and after it — is **drawn** by the plan and held strictly to
/// FFmpeg's still, and the still it stands for is held to the export's own frame (>= 35 dB: the
/// still's frame *pick* is `-ss`, not the `fps` filter's, so moving footage can differ from the
/// file by a source frame). Both FFmpegs. The frame-by-frame comparison with the export itself is
/// `tests/motion_parity.rs`.
///
/// A dissolve of 0.61 s is chosen so that a frame with *only* the tail left exists at 30 fps:
/// the fade counts 18 frames (it is over at frame 48) and the window covers 18.3 of them.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn frames_inside_a_transition_are_drawn_as_ffmpegs_still_draws_them() {
    let m = media();
    // The export runs at 30 fps whatever the outgoing clip's own rate is (it would take the first clip's).
    let opts = ExportOptions {
        fps: Some(30.0),
        ..ExportOptions::default()
    };
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    // Per kind: the length and the frames to look at, the frame on which only the outgoing clip's
    // tail is left among them.
    type Frames = &'static [u64];
    let motion: Frames = &[15, 29, 30, 37, 44, 45, 46, 55];
    let cases: [(&str, TransitionKind, f64, Frames, Option<u64>); 4] = [
        (
            "dissolve",
            TransitionKind::Crossfade,
            0.61,
            &[15, 29, 30, 38, 47, 48, 49, 55],
            Some(48),
        ),
        ("slide", TransitionKind::SlideLeft, 0.5, motion, None),
        ("push", TransitionKind::PushUp, 0.5, motion, None),
        (
            "dip",
            TransitionKind::DipToBlack,
            0.8,
            &[15, 18, 19, 29, 30, 41, 42, 43, 55],
            None,
        ),
    ];
    for (name, kind, secs, frames, tail_only) in cases {
        let assets = [m.bars.clone(), m.testsrc.clone()];
        for (cover, scale) in [("covering", 1.0), ("partial", 0.5)] {
            let mut incoming = clip(&m.testsrc, 0.0, 1.0, 1.0);
            incoming.transform.scale = scale;
            incoming.transition_in = Some(Transition { kind, duration: secs });
            let tl = timeline(vec![vec![clip(&m.bars, 0.0, 1.0, 0.0), incoming]], None);
            // What plays: the whole cut exported losslessly, every frame read back.
            let file = scratch.join(format!("transition-{name}-{cover}.mkv"));
            let lossless = ExportOptions {
                container: Container::Mkv,
                video_codec: Some("libx264".into()),
                rate_control: RateControl::Lossless,
                ..opts.clone()
            };
            kerf_core::render_with(&tl, &assets, &file, &lossless).expect("export");
            let played = ffmpeg(&["-i", file.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
            let _ = std::fs::remove_file(&file);
            for &k in frames {
                let case = format!("transition/{name}-{cover}");
                let t = k as f64 / 30.0;
                let plan = RenderPlan::at(&tl, &assets, &opts, t, policy()).expect("plan");
                let size = plan.size(u32::MAX);
                let (w, h) = (size.0 as usize, size.1 as usize);
                assert_eq!(plan.frame, k as i64);
                let why = reasons(&plan, size);
                assert!(why.is_empty(), "{case}: frame {k}: {why:?}");
                if scale < 1.0 && tail_only == Some(k) {
                    assert!(
                        plan.layers.iter().any(|l| l.fx.tail)
                            && plan
                                .layers
                                .iter()
                                .all(|l| l.fx.in_progress(plan.frame, plan.canvas.fps).is_none()),
                        "{case}: frame {k}: only the tail is left"
                    );
                }
                // The still the plan stands for, against the frame that plays.
                let png = scratch.join(format!("transition-{name}-{cover}-{k}.png"));
                export_still(&tl, &assets, &opts, t, &png, ImageFormat::Png, 0).expect("FFmpeg still");
                let still = ffmpeg(&["-i", png.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
                let _ = std::fs::remove_file(&png);
                let export = &played[k as usize * w * h * 3..(k as usize + 1) * w * h * 3];
                let (apart, _) = compare(export, &still, w, h, EDGE_STEP);
                eprintln!("{case:<28} frame {k:<3} still vs export {:>5.1} dB", apart.psnr_all);
                assert!(
                    apart.psnr_all >= 35.0,
                    "{case}: frame {k}: the FFmpeg still draws the transition differently from the export ({:.1} dB)",
                    apart.psnr_all
                );
                check_with(&case, &tl, &assets, &[t], STRICT, &opts);
            }
        }
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_clip_past_the_end_of_its_footage_draws_nothing_like_ffmpeg() {
    let m = media();
    // A 2 s source in a 5 s clip: the source time clamps to the end, `-ss` there
    // finds no frame, and FFmpeg's own still leaves the layer out (the still
    // below it shows). The GPU path does the same.
    let tl = timeline(
        vec![
            vec![clip(&m.still_long, 0.0, 5.0, 0.0)],
            vec![clip(&m.testsrc, 0.0, 5.0, 0.0)],
        ],
        None,
    );
    let assets = [m.still_long.clone(), m.testsrc.clone()];
    for t in [1.99, 3.0] {
        let plan = RenderPlan::at(&tl, &assets, &ExportOptions::default(), t, policy()).expect("plan");
        let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
        assert!(frames[0].is_some(), "the still has its frame at {t}");
        assert!(
            frames[1].is_none(),
            "the clip's source has no frame at {t}: that is the case under test"
        );
    }
    check("clamped-end/nothing-drawn", &tl, &assets, &[1.99, 3.0], STRICT);
}

/// A stack whose layers carry different YCbCr matrices (or an RGB picture in a stack
/// that is not BT.601). Under a fixed-matrix FFmpeg nothing converts them and the
/// compositor draws it like any other; under one that negotiates the matrix across
/// the overlay chain, FFmpeg converts every layer into the bottom layer's with an
/// arithmetic the compositor does not reproduce (measured: 15-30 levels off), so the
/// plan refuses it — and the case holds the refusal, with FFmpeg rendering it.
fn check_mixed(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64], reason: &'static str) {
    match policy() {
        CompositeColorPolicy::FixedBt601 => check(case, tl, assets, times, STRICT),
        CompositeColorPolicy::BottomLayerTag => {
            check_refused(&format!("{case} (negotiated)"), tl, assets, times[0], Refusal::Plan(reason));
        }
        CompositeColorPolicy::Unknown => unreachable!("policy() never answers Unknown"),
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_composite_follows_the_matrix_ffmpeg_negotiates() {
    let m = media();
    eprintln!("composite colour policy of this FFmpeg: {:?}", policy());
    // One tagged clip, and stacks whose layers all carry the same tag: the composite
    // is converted with that tag under a negotiating FFmpeg and as BT.601 under a
    // fixed one, and in both cases the compositor does what FFmpeg does.
    for (name, a) in [
        ("matrix/bt709-bars", &m.bars709),
        ("matrix/bt709-testsrc2", &m.testsrc709),
        ("matrix/bt709-gradient", &m.gradient709),
        ("matrix/bt2020-bars", &m.bars2020),
        ("matrix/bt2020-testsrc2", &m.testsrc2020),
    ] {
        let tl = timeline(vec![vec![clip(a, 0.0, 2.0, 0.0)]], None);
        check(name, &tl, std::slice::from_ref(a), &[0.5], STRICT);
    }
    let pip = |a: &Asset| {
        let mut c = clip(a, 0.0, 2.0, 0.0);
        c.transform = Transform {
            scale: 0.5,
            pos_x: 0.2,
            pos_y: -0.1,
            ..Transform::default()
        };
        c
    };
    let tl = timeline(
        vec![vec![clip(&m.gradient709, 0.0, 2.0, 0.0)], vec![pip(&m.testsrc709)]],
        None,
    );
    check(
        "matrix/bt709-layered",
        &tl,
        &[m.gradient709.clone(), m.testsrc709.clone()],
        &[0.5],
        STRICT,
    );
    let tl = timeline(
        vec![vec![clip(&m.gradient2020, 0.0, 2.0, 0.0)], vec![pip(&m.testsrc2020)]],
        None,
    );
    check(
        "matrix/bt2020-layered",
        &tl,
        &[m.gradient2020.clone(), m.testsrc2020.clone()],
        &[0.5],
        STRICT,
    );
    // Mixed tags, any order: refused under negotiation.
    for (name, base, top) in [
        ("matrix/mixed-709-over-untagged", &m.gradient, &m.testsrc709),
        ("matrix/mixed-untagged-over-709", &m.gradient709, &m.testsrc),
        ("matrix/mixed-709-over-2020", &m.gradient2020, &m.testsrc709),
        ("matrix/mixed-2020-over-709", &m.gradient709, &m.testsrc2020),
    ] {
        let tl = timeline(vec![vec![clip(base, 0.0, 2.0, 0.0)], vec![pip(top)]], None);
        check_mixed(name, &tl, &[base.clone(), top.clone()], &[0.5], "different YCbCr matrices");
    }
    // An RGB picture (a PNG, at its own size: it is under the pip, since resizing an
    // RGB picture is refused) among untagged layers is nothing special; among tagged
    // ones FFmpeg converts it with the negotiated matrix.
    let tl = timeline(vec![vec![clip(&m.png640, 0.0, 2.0, 0.0)], vec![pip(&m.gradient)]], None);
    check(
        "matrix/rgb-png-under-untagged",
        &tl,
        &[m.png640.clone(), m.gradient.clone()],
        &[0.5],
        STRICT,
    );
    let tl = timeline(vec![vec![clip(&m.png640, 0.0, 2.0, 0.0)], vec![pip(&m.gradient709)]], None);
    check_mixed(
        "matrix/rgb-png-under-709",
        &tl,
        &[m.png640.clone(), m.gradient709.clone()],
        &[0.5],
        "RGB picture",
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn translucent_layers_take_the_round_trip_with_the_right_matrices() {
    let m = media();
    // Out of YCbCr with the layer's own matrix, back with the composite's: a
    // translucent BT.709 layer over BT.709 ones is consistent under either policy.
    let fade = |a: &Asset, op: f64| {
        let mut c = clip(a, 0.0, 2.0, 0.0);
        c.transform.opacity = op;
        c
    };
    for (name, a, op) in [
        ("translucent/bt709-bars-0.5-alone", &m.bars709, 0.5),
        ("translucent/bt709-testsrc2-0.65-alone", &m.testsrc709, 0.65),
        ("translucent/bt2020-bars-0.6-alone", &m.bars2020, 0.6),
    ] {
        let tl = timeline(vec![vec![fade(a, op)]], None);
        check(name, &tl, std::slice::from_ref(a), &[0.5], STRICT);
    }
    let tl = timeline(
        vec![vec![clip(&m.bars709, 0.0, 2.0, 0.0)], vec![fade(&m.testsrc709, 0.5)]],
        None,
    );
    check(
        "translucent/bt709-over-bt709",
        &tl,
        &[m.bars709.clone(), m.testsrc709.clone()],
        &[0.5],
        STRICT,
    );
    let tl = timeline(
        vec![vec![clip(&m.bars2020, 0.0, 2.0, 0.0)], vec![fade(&m.gradient2020, 0.4)]],
        None,
    );
    check(
        "translucent/bt2020-over-bt2020",
        &tl,
        &[m.bars2020.clone(), m.gradient2020.clone()],
        &[0.5],
        STRICT,
    );
    // Translucent untagged over a BT.709 base, and the other way round: mixed.
    let tl = timeline(vec![vec![clip(&m.bars709, 0.0, 2.0, 0.0)], vec![fade(&m.testsrc, 0.5)]], None);
    check_mixed(
        "translucent/untagged-over-bt709",
        &tl,
        &[m.bars709.clone(), m.testsrc.clone()],
        &[0.5],
        "different YCbCr matrices",
    );
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![fade(&m.testsrc709, 0.5)]], None);
    check_mixed(
        "translucent/bt709-over-untagged",
        &tl,
        &[m.bars.clone(), m.testsrc709.clone()],
        &[0.5],
        "different YCbCr matrices",
    );
    // Formats that are not 8-bit 4:2:0 take the same round trip (FFmpeg converts
    // them to yuva420p first; so does the decode).
    for (name, a) in [
        ("translucent/yuv444p", &m.yuv444),
        ("translucent/yuv422p", &m.yuv422),
        ("translucent/yuv420p10le", &m.tenbit),
        ("translucent/gray", &m.gray),
        ("translucent/full-range-mjpeg", &m.fullrange),
        ("translucent/jpeg-still", &m.jpeg),
        ("translucent/rgb-png-still", &m.png640),
    ] {
        let dur = a.duration.min(2.0);
        let mut c = clip(a, 0.0, dur, 0.0);
        c.transform.opacity = 0.6;
        let tl = timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)], vec![c]], None);
        check(name, &tl, &[m.testsrc.clone(), a.clone()], &[0.5], STRICT);
    }
}

/// Where a layer's picture lands depends on which grid each filter rounds to, and
/// they are not the same grid. The first `crop` runs on the picture as decoded, so it
/// rounds to its *native* chroma grid: even for 4:2:0, even columns only for 4:2:2,
/// nothing for 4:4:4, gray or RGB. The chain converts to 4:2:0 in its *last* `scale`:
/// with one `scale` (an identity transform, or one that does not resize) the Cover
/// crop and `pad` that follow are on the 4:2:0 grid whatever the source was, and
/// with the transform's own `scale` after a Cover crop that crop is still on the
/// native grid. A layer placed on the 4:2:0 grid throughout is a whole pixel off for
/// every other layout with an odd crop (measured 20 dB over the whole frame on 4:4:4),
/// one placed on the native grid throughout is a pixel off at every odd letterbox and
/// Cover offset (20 to 30 dB: the first version of this fix did exactly that), and a
/// Cover offset followed by a resize needs the native grid again (a gray layer 35
/// levels off, found by fuzzing).
///
/// The compositor's planes are 4:2:0, so it can place a layer exactly only where the
/// two grids agree; luma is always exact, but the chroma of a 4:2:2 / 4:4:4 / RGB
/// picture positioned between two 4:2:0 chroma samples is a pixel off (30 to 38 dB).
/// Those are **refused** (FFmpeg draws them); gray, which has no chroma to misplace,
/// is drawn exactly. Every case is chosen so that the 4:2:0 and the native geometry
/// *differ* or are *equal* as stated, so a regression to one shared rounding cannot
/// pass by landing on the same pixels.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn crop_rounds_to_the_native_chroma_grid_and_pad_and_cover_to_the_420_one() {
    let m = media();
    // (name, asset, native chroma grid, has chroma finer than 4:2:0, gray).
    let layouts: [(&str, &Asset, Subsampling, bool, bool); 6] = [
        ("yuv420p", &m.testsrc, Subsampling::YUV420, false, false),
        ("yuv422p", &m.yuv422, Subsampling { log2_w: 1, log2_h: 0 }, true, false),
        ("yuv444p", &m.yuv444, Subsampling::NONE, true, false),
        ("gray", &m.gray, Subsampling::NONE, false, true),
        ("bgr0", &m.bgr0, Subsampling::NONE, true, false),
        ("rgb24-png", &m.png640, Subsampling::NONE, true, false),
    ];
    // A crop of the 640x360 picture: (left, top, right, bottom) in pixels.
    let cropped = |l: f64, t: f64, r: f64, b: f64| Transform {
        crop_left: l / 640.0,
        crop_top: t / 360.0,
        crop_right: r / 640.0,
        crop_bottom: b / 360.0,
        ..Transform::default()
    };
    let small = (320, 180);
    // Cases with a crop: (name, frame, fit, transform, x odd, y odd).
    type CropCase = (&'static str, (u32, u32), Fit, Transform, bool, bool);
    // The transform's own `scale` after a Cover crop is a second `scale` in the chain,
    // and the picture is converted to 4:2:0 by the last one: the Cover crop before it
    // still rounds to the native grid.
    let rescaled = Transform {
        scale: 0.8,
        ..Transform::default()
    };
    let crops: [CropCase; 8] = [
        ("odd-crop", small, Fit::Contain, cropped(11.0, 7.0, 6.0, 6.0), true, true),
        ("odd-x-crop", small, Fit::Contain, cropped(11.0, 6.0, 0.0, 0.0), true, false),
        ("odd-y-crop", small, Fit::Contain, cropped(10.0, 7.0, 0.0, 0.0), false, true),
        ("even-crop", small, Fit::Contain, cropped(64.0, 18.0, 0.0, 0.0), false, false),
        (
            "odd-crop-cover",
            (90, 162),
            Fit::Cover,
            cropped(11.0, 7.0, 0.0, 0.0),
            true,
            true,
        ),
        ("cover-odd-x-then-scale", (90, 162), Fit::Cover, rescaled, true, false),
        ("cover-odd-y-then-scale", (202, 100), Fit::Cover, rescaled, false, true),
        (
            "odd-crop-behind-a-pip",
            small,
            Fit::Contain,
            cropped(11.0, 7.0, 6.0, 6.0),
            true,
            true,
        ),
    ];
    // Cases with no crop, where the grid after the first `scale` decides: the Cover
    // overhang and the letterbox gap are odd, and land on the 4:2:0 grid for every
    // source. (The two letterbox frames that leave the picture at its own size are the
    // only ones drawn for a picture that is not 4:2:0 or gray: the rest resize it.)
    let uncropped: [(&str, (u32, u32), Fit); 5] = [
        ("cover-odd-x", (90, 162), Fit::Cover),
        ("cover-odd-y", (202, 100), Fit::Cover),
        ("pad-odd-y", (640, 366), Fit::Contain),
        ("pad-odd-x", (646, 360), Fit::Contain),
        ("letterbox-9x16", (360, 640), Fit::Contain),
    ];
    for (layout, asset, native, finer, gray) in layouts {
        let assets = std::slice::from_ref(asset);
        let dur = asset.duration.min(2.0);
        let src = (asset.streams[0].width.unwrap(), asset.streams[0].height.unwrap());
        let geometry =
            |frame: (u32, u32), fit, tf: &Transform, sub| LayerGeometry::resolve(src, frame, fit, tf, sub).expect("geometry");
        for (shape, frame, fit, tf, odd_x, odd_y) in &crops {
            let case = format!("grid/{layout}-{shape}");
            let mut c = clip(asset, 0.0, dur, 0.0);
            c.transform = *tf;
            let tl = if *shape == "odd-crop-behind-a-pip" {
                // The same crop on the top layer of a stack: the layer below is
                // untouched, and the crop must land on the top one alone.
                let mut top = c;
                top.transform.scale = 0.9;
                timeline(
                    vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)], vec![top]],
                    Some(Delivery::new(frame.0, frame.1, *fit)),
                )
            } else {
                timeline(vec![vec![c]], Some(Delivery::new(frame.0, frame.1, *fit)))
            };
            let assets: Vec<Asset> = if *shape == "odd-crop-behind-a-pip" {
                vec![m.testsrc.clone(), asset.clone()]
            } else {
                assets.to_vec()
            };
            // Is this geometry rounded differently on the native grid than on 4:2:0's?
            let differs_here = geometry(*frame, *fit, tf, native) != geometry(*frame, *fit, tf, Subsampling::YUV420);
            // ...which it is exactly when the crop is odd along an axis the layout
            // does not subsample (the "pip" case scales the picture afterwards, so only
            // the crop itself is compared).
            let expect_differs = (*odd_x && native.log2_w == 0) || (*odd_y && native.log2_h == 0);
            if *shape != "odd-crop-behind-a-pip" {
                assert_eq!(
                    differs_here, expect_differs,
                    "{case}: the case does not exercise what it says"
                );
            }
            // A picture that is not 4:2:0 or gray is FFmpeg's whenever it is resized (and
            // every crop frame here resizes it), and also — said first — when its crop
            // lands between two 4:2:0 chroma samples: luma is exact and chroma is not.
            if finer && !gray {
                let why = if differs_here { "4:2:0 chroma samples" } else { "scales a" };
                check_refused(&case, &tl, &assets, 0.5, Refusal::Plan(why));
            } else {
                check(&case, &tl, &assets, &[0.5], STRICT);
            }
        }
        for (shape, frame, fit) in &uncropped {
            let case = format!("grid/{layout}-{shape}");
            let tl = timeline(
                vec![vec![clip(asset, 0.0, dur, 0.0)]],
                Some(Delivery::new(frame.0, frame.1, *fit)),
            );
            let tf = Transform::default();
            assert_eq!(
                geometry(*frame, *fit, &tf, native),
                geometry(*frame, *fit, &tf, Subsampling::YUV420),
                "{case}: only a crop sees the native grid"
            );
            // Only the letterbox gaps that leave the picture at its own size are drawn
            // for a picture that is not 4:2:0 or gray; the Cover frames and the 9:16
            // letterbox resize it.
            let resized = geometry(*frame, *fit, &tf, native)
                .stages
                .iter()
                .any(|s| s.scaled != (s.src.w, s.src.h));
            if finer && !gray && resized {
                check_refused(&case, &tl, assets, 0.5, Refusal::Plan("scales a"));
            } else {
                check(&case, &tl, assets, &[0.5], STRICT);
            }
        }
    }
}

/// When the probe cannot tell which FFmpeg this is, the plan draws only what FFmpeg 6
/// and 9 both do the same way — a stack that is BT.601 throughout — and refuses
/// every other matrix. Forced here on whatever FFmpeg the harness runs against, so
/// the untagged cases hold on both and the tagged ones are asserted refused (with
/// FFmpeg rendering them).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn an_unmeasured_policy_draws_only_what_every_ffmpeg_agrees_on() {
    let m = media();
    with_policy(CompositeColorPolicy::Unknown, || {
        // BT.601-class: drawn, and equal to FFmpeg's own still.
        let single = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)]], None);
        check(
            "unknown/untagged-bars",
            &single,
            std::slice::from_ref(&m.bars),
            &[0.5],
            STRICT,
        );
        let mut top = clip(&m.testsrc, 0.0, 2.0, 0.0);
        top.transform = Transform {
            scale: 0.5,
            opacity: 0.6,
            ..Transform::default()
        };
        let layered = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![top]], None);
        check(
            "unknown/untagged-translucent-pip",
            &layered,
            &[m.bars.clone(), m.testsrc.clone()],
            &[0.5],
            STRICT,
        );
        // BT.709 and BT.2020: the two FFmpegs disagree, so the frame is FFmpeg's.
        for (name, a) in [("bt709", &m.bars709), ("bt2020", &m.bars2020)] {
            let tl = timeline(vec![vec![clip(a, 0.0, 2.0, 0.0)]], None);
            check_refused(
                &format!("unknown/{name}-bars"),
                &tl,
                std::slice::from_ref(a),
                0.5,
                Refusal::Plan("could not be measured"),
            );
        }
        // ...in a stack of its own, on top of a BT.601 one, or under it.
        for (name, layers) in [
            ("709-over-untagged", [&m.bars, &m.testsrc709]),
            ("untagged-over-709", [&m.testsrc709, &m.bars]),
        ] {
            let tl = timeline(layers.iter().map(|a| vec![clip(a, 0.0, 2.0, 0.0)]).collect(), None);
            let assets: Vec<Asset> = layers.iter().map(|a| (*a).clone()).collect();
            let reasons = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 0.5, policy())
                .expect("plan")
                .unsupported_reasons()
                .join("; ");
            assert!(!reasons.is_empty(), "unknown/{name}: the plan should refuse");
        }
    });
}

/// `eq` on a full-range (`yuvj420p`) picture: FFmpeg 9 grades its raw full-range values
/// and converts the range afterwards, while the decode behind the compositor converts to
/// limited range first. The two read 34 to 42 dB apart on FFmpeg 9.0.2 for every knob
/// (brightness 36.7, contrast max 18, saturation 38.0, gamma 34.2, temperature 41.8),
/// and agree on FFmpeg 6.1 — so the plan does not grade full range on the GPU on any
/// FFmpeg, and an ungraded full-range clip (no `eq`) is still drawn and compared.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn grading_a_full_range_picture_is_refused_and_ungraded_it_is_drawn() {
    let m = media();
    assert_eq!(m.fullrange.streams[0].pix_fmt.as_deref(), Some("yuvj420p"));
    let graded = |a: &Asset, color: Color| {
        let mut c = clip(a, 0.0, a.duration.min(2.0), 0.0);
        c.color = color;
        timeline(vec![vec![c]], None)
    };
    for (name, color) in [
        (
            "brightness",
            Color {
                brightness: 0.1,
                ..Color::default()
            },
        ),
        (
            "contrast",
            Color {
                contrast: 1.3,
                ..Color::default()
            },
        ),
        (
            "saturation",
            Color {
                saturation: 1.5,
                ..Color::default()
            },
        ),
        (
            "gamma",
            Color {
                gamma: 1.4,
                ..Color::default()
            },
        ),
        (
            "temperature",
            Color {
                temperature: 0.5,
                ..Color::default()
            },
        ),
    ] {
        check_refused(
            &format!("refused/graded-full-range-{name}"),
            &graded(&m.fullrange, color),
            std::slice::from_ref(&m.fullrange),
            0.5,
            Refusal::Plan("full-range"),
        );
    }
    check(
        "full-range/ungraded",
        &graded(&m.fullrange, Color::default()),
        std::slice::from_ref(&m.fullrange),
        &[0.5],
        STRICT,
    );
    // The same limited-range picture, graded, is drawn and compared.
    check(
        "full-range/limited-range-control-graded",
        &graded(
            &m.testsrc,
            Color {
                gamma: 1.4,
                ..Color::default()
            },
        ),
        std::slice::from_ref(&m.testsrc),
        &[0.5],
        STRICT,
    );
    // A graded layer in a stack with a full-range one: where the range is negotiated
    // along the overlay chain (FFmpeg 9) it is 40 to 44 dB off, so that is FFmpeg's
    // there; FFmpeg 6 reads it exactly and it is compared strictly.
    // (A PNG is RGB, which cannot be resized: it goes in translucent, at its own size.)
    let shifted = Transform {
        scale: 0.5,
        pos_x: 0.2,
        pos_y: -0.1,
        ..Transform::default()
    };
    let faded = Transform {
        opacity: 0.6,
        ..Transform::default()
    };
    for (name, bottom, top, transform) in [
        ("limited-over-full", &m.fullrange, &m.testsrc, shifted),
        ("translucent-png-over-full", &m.fullrange, &m.png640, faded),
    ] {
        let mut pip = clip(top, 0.0, 2.0, 0.0);
        pip.transform = transform;
        pip.color = Color {
            gamma: 1.4,
            ..Color::default()
        };
        let tl = timeline(vec![vec![clip(bottom, 0.0, 2.0, 0.0)], vec![pip]], None);
        check_mixed(
            &format!("range/graded-{name}"),
            &tl,
            &[bottom.clone(), top.clone()],
            &[0.5],
            "stack with a full-range picture",
        );
    }
    // A picture that never recorded its format is not known to be limited range.
    let mut old = m.testsrc.clone();
    for s in &mut old.streams {
        s.pix_fmt = None;
    }
    check_refused(
        "refused/graded-asset-without-a-recorded-format",
        &graded(
            &old,
            Color {
                gamma: 1.4,
                ..Color::default()
            },
        ),
        std::slice::from_ref(&old),
        0.5,
        Refusal::Plan("never recorded"),
    );
}

/// FFmpeg scales a picture in the format it has; the compositor in the 8-bit 4:2:0 a
/// decode reduces it to. Enlarging a 4:4:4 picture was 27 levels off, 4:2:2 15, RGB
/// video 51, an RGB PNG 69 — and a *shrink* as mild as 1.05-1.5x still reads flat max
/// 8-9 (4:2:2 at 0.9x is over the limit) and up to 32 levels on edges, because the
/// chroma kernel differs most near a ratio of 1. No band of ratios was measured
/// strictly inside the limits on both FFmpegs for busy chroma, so none is claimed:
/// every resize of a picture that is not 4:2:0 (8/10-bit) or gray is FFmpeg's, and a
/// picture left at its size is drawn.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn resizing_a_picture_ffmpeg_scales_in_another_format_is_refused() {
    let m = media();
    let scaled = |a: &Asset, scale: f64, fit: Option<Delivery>| {
        let dur = a.duration.min(2.0);
        let mut c = clip(a, 0.0, dur, 0.0);
        c.transform.scale = scale;
        timeline(vec![vec![c]], fit)
    };
    for (name, a) in [
        ("yuv444p", &m.yuv444),
        ("yuv422p", &m.yuv422),
        ("bgr0", &m.bgr0),
        ("rgb24-png", &m.still),
    ] {
        let assets = std::slice::from_ref(a);
        for scale in [1.5, 2.0] {
            check_refused(
                &format!("enlarge/{name}-x{scale}"),
                &scaled(a, scale, None),
                assets,
                0.5,
                Refusal::Plan("scales a"),
            );
        }
        check_refused(
            &format!("enlarge/{name}-fit-to-960x540"),
            &scaled(a, 1.0, Some(Delivery::new(960, 540, Fit::Contain))),
            assets,
            0.5,
            Refusal::Plan("scales a"),
        );
        // Shrinks, the mild ones that read worst included.
        for scale in [0.95, 0.9, 0.75, 0.5] {
            check_refused(
                &format!("shrink/{name}-x{scale}"),
                &scaled(a, scale, None),
                assets,
                0.5,
                Refusal::Plan("scales a"),
            );
        }
        for (w, h) in [(576u32, 324u32), (320, 180)] {
            check_refused(
                &format!("shrink/{name}-fit-to-{w}x{h}"),
                &scaled(a, 1.0, Some(Delivery::new(w, h, Fit::Contain))),
                assets,
                0.5,
                Refusal::Plan("scales a"),
            );
        }
        // ...and a picture left at its size is drawn, held to the strict limits.
        check(&format!("same-size/{name}"), &scaled(a, 1.0, None), assets, &[0.5], STRICT);
    }
    // 4:2:0 and gray are what the compositor works in: resizing them is fine in both
    // directions — including 10 bit (FFmpeg scales it at 10 bits and dithers once at the
    // end).
    for (name, a) in [
        ("yuv420p10le", &m.tenbit),
        ("gray", &m.gray),
        ("yuv420p", &m.testsrc),
        ("yuvj420p-jpeg", &m.jpeg),
    ] {
        let assets = std::slice::from_ref(a);
        check(
            &format!("enlarge-ok/{name}-x1.5"),
            &scaled(a, 1.5, None),
            assets,
            &[0.5],
            STRICT,
        );
        check(
            &format!("enlarge-ok/{name}-fit-to-960x540"),
            &scaled(a, 1.0, Some(Delivery::new(960, 540, Fit::Contain))),
            assets,
            &[0.5],
            STRICT,
        );
    }
    for (name, a) in [("yuv420p", &m.testsrc), ("gray", &m.gray)] {
        check(
            &format!("shrink-ok/{name}-fit-to-576x324"),
            &scaled(a, 1.0, Some(Delivery::new(576, 324, Fit::Contain))),
            std::slice::from_ref(a),
            &[0.5],
            STRICT,
        );
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn an_asset_that_never_recorded_its_pixel_format_is_assumed_nothing() {
    let m = media();
    // A project saved before the pixel format and colour tags were probed: its
    // `color_space: None` could mean untagged or "not read yet".
    let mut old = m.testsrc709.clone();
    for s in &mut old.streams {
        s.pix_fmt = None;
        s.color_space = None;
    }
    let assets = std::slice::from_ref(&old);
    // Opaque and unscaled: drawn (the decode looks for alpha itself).
    let tl = timeline(vec![vec![clip(&old, 0.0, 2.0, 0.0)]], None);
    if policy() == CompositeColorPolicy::FixedBt601 {
        check("old-asset/opaque", &tl, assets, &[0.5], STRICT);
    } else {
        // ...unless the composite's matrix depends on its tag.
        check_refused(
            "old-asset/opaque (negotiated)",
            &tl,
            assets,
            0.5,
            Refusal::Plan("matrix is unknown"),
        );
    }
    // Translucent: BT.709 footage must not be round-tripped as BT.601.
    let mut c = clip(&old, 0.0, 2.0, 0.0);
    c.transform.opacity = 0.6;
    let tl = timeline(vec![vec![c]], None);
    check_refused("old-asset/translucent", &tl, assets, 0.5, Refusal::Plan("probed before"));
    // Enlarged: not known to be 4:2:0.
    let mut c = clip(&old, 0.0, 2.0, 0.0);
    c.transform.scale = 1.5;
    let tl = timeline(vec![vec![c]], None);
    check_refused("old-asset/enlarged", &tl, assets, 0.5, Refusal::Plan("not recorded"));
}

// ---- text ---------------------------------------------------------------------

/// Text is edge everywhere — a glyph is a few pixels wide, and the edge band (two pixels around every
/// strong step of the reference) would excuse every pixel of it — so a text case is not judged by
/// the band. It is held to **the same picture without the text**: the whole-image error may be at
/// most that of the control (which the suite's own bounds judge), so whatever the composite and the
/// YUV -> RGB conversion cost everywhere is not charged to the text. A control with no colour in it
/// has none of the conversion's bias (up to [`CONVERSION_BIAS`] levels on any colour: swscale
/// truncates, the shader rounds) to show, so that is its floor. A frame with **no layer** is blended in
/// RGB, with no conversion at all, and is held to the control exactly (`tests/text.rs` holds the blend
/// to FFmpeg's `drawtext` sample for sample).
///
/// **The allowance over the control is nothing**, and a case that needs some says how much and why
/// ([`check_text_extra`]); measured on both FFmpegs, three do: two glyph-edge pixels of the lower third
/// one level over at 0.1 s (YUV -> RGB error near the gamut edge), the 12 px text (two levels), and the
/// full-range pictures on 9.0 (a known model difference).
const CONVERSION_BIAS: i32 = 3;

fn check_text(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64]) {
    check_text_extra(case, tl, assets, times, 0, "");
}

/// [`check_text`] with `extra` levels over the control, for the reason `why` (which a case with no
/// allowance does not need).
fn check_text_extra(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64], extra: i32, why: &str) {
    assert!(extra == 0 || !why.is_empty(), "{case}: an allowance says why");
    let mut bare = tl.clone();
    bare.overlays.clear();
    for &t in times {
        let opts = ExportOptions::default();
        let control = check_with(&format!("{case} (no text)"), &bare, assets, &[t], STRICT, &opts);
        let in_rgb = RenderPlan::at(tl, assets, &opts, t, policy())
            .expect("plan")
            .layers
            .is_empty();
        let limits = Limits {
            flat_share: 0.0,
            max_all: control.first().map(|worst| {
                if in_rgb {
                    *worst
                } else {
                    (*worst).max(CONVERSION_BIAS) + extra
                }
            }),
            ..STRICT
        };
        check_with(case, tl, assets, &[t], limits, &opts);
    }
}

fn overlay(text: &str, start: f64, end: f64) -> kerf_core::TextOverlay {
    kerf_core::TextOverlay::new(text, start, end)
}

fn with_text(mut tl: Timeline, overlays: Vec<kerf_core::TextOverlay>) -> Timeline {
    tl.overlays = overlays;
    tl
}

/// An overlay in one of the app's own looks (`style-presets.ts`, `CaptionStyle`): position, size,
/// colour, box and weight, with the opacity fade the preset writes as keyframes.
fn preset(text: &str, (x, y): (f64, f64), size: f64, bold: bool, bg: Option<&str>, fade: f64) -> kerf_core::TextOverlay {
    let mut o = overlay(text, 0.0, 2.0);
    (o.pos_x, o.pos_y, o.size, o.bold, o.bg) = (x, y, size, bold, bg.map(String::from));
    if fade > 0.0 {
        let key = |time, opacity| kerf_core::TextKeyframe {
            time,
            pos_x: x,
            pos_y: y,
            opacity,
        };
        o.keyframes = vec![key(0.0, 0.0), key(fade, 1.0), key(2.0 - fade, 1.0), key(2.0, 0.0)];
    }
    o
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_apps_title_and_caption_styles_are_drawn_as_ffmpeg_draws_them() {
    let m = media();
    let one = std::slice::from_ref(&m.testsrc);
    let base = || vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]];
    // Title / Lower third / Caption (the presets), both caption looks (`Lines` and `WordPunch`, which
    // is bold in the default font's own bold face), each at times through its fade.
    let styles: [(&str, kerf_core::TextOverlay, &[f64]); 5] = [
        (
            "text/title",
            preset("Title", (0.5, 0.4), 0.085, true, None, 0.5),
            &[0.0, 0.25, 0.5, 1.0, 1.7, 1.99],
        ),
        (
            "text/lower-third",
            preset("Name \u{2014} Role", (0.24, 0.86), 0.042, true, Some("black@0.55"), 0.35),
            &[0.1, 0.35, 1.0, 1.8],
        ),
        (
            "text/caption",
            preset("Caption", (0.5, 0.88), 0.05, false, Some("black@0.5"), 0.0),
            &[0.5],
        ),
        (
            "text/captions-lines",
            preset("A few words held", (0.5, 0.88), 0.05, false, Some("black@0.5"), 0.0),
            &[0.5],
        ),
        (
            "text/captions-word-punch",
            preset("WORD", (0.5, 0.72), 0.11, true, Some("black@0.5"), 0.0),
            &[0.5],
        ),
    ];
    for (name, o, times) in styles {
        let tl = with_text(timeline(base(), None), vec![o.clone()]);
        if name == "text/lower-third" {
            check_text_extra(
                name,
                &tl,
                one,
                &times[..1],
                1,
                "two glyph-edge pixels of low coverage over saturated green are one level over the control at 0.1 s: \
                 the YUV -> RGB conversion's error near the gamut edge, not the blend (`tests/text.rs` holds \
                 the blend's arithmetic to drawtext's, sample for sample)",
            );
            check_text(name, &tl, one, &times[1..]);
        } else {
            check_text(name, &tl, one, times);
        }
        // The same styles in the 9:16 frame the captions are made for (the font is a fraction of the
        // frame's height, so the text is a different size in pixels).
        let tall = with_text(timeline(base(), Some(Delivery::new(360, 640, Fit::Cover))), vec![o]);
        check_text(&format!("{name}-9x16"), &tall, one, &times[..1]);
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn text_positions_sizes_colours_and_boxes_are_drawn_as_ffmpeg_draws_them() {
    let m = media();
    let one = std::slice::from_ref(&m.testsrc);
    let tl =
        |overlays: Vec<kerf_core::TextOverlay>| with_text(timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None), overlays);
    // Sizes from 12 px (the smallest the plan lets through) to a fifth of the frame, at fractional positions
    // (quarter-pixel glyph phases).
    for size in [0.034, 0.06, 0.12, 0.2] {
        let mut o = overlay("Hello, World", 0.0, 2.0);
        (o.size, o.pos_x, o.pos_y) = (size, 0.4137, 0.5821);
        let extra = if size < 0.04 { 2 } else { 0 };
        check_text_extra(
            &format!("text/size-{size}"),
            &tl(vec![o]),
            one,
            &[0.5],
            extra,
            "12 px is the smallest text the plan draws: its glyph bitmaps share the most pixels, a level or two of \
             chroma there, times the 1.77 of the blue channel",
        );
    }
    // At the frame's edges, half off it, and wholly outside (drawn as nothing).
    for (name, pos) in [
        ("left", (0.0, 0.5)),
        ("right", (1.0, 0.5)),
        ("top", (0.5, 0.0)),
        ("bottom", (0.5, 1.0)),
        ("corner", (1.0, 1.0)),
        ("outside", (2.5, 0.5)),
    ] {
        for bg in [None, Some("black@0.5")] {
            let mut o = overlay("Edge of the frame", 0.0, 2.0);
            (o.pos_x, o.pos_y, o.size) = (pos.0, pos.1, 0.08);
            o.bg = bg.map(String::from);
            check_text(
                &format!("text/edge-{name}{}", if bg.is_some() { "-boxed" } else { "" }),
                &tl(vec![o]),
                one,
                &[0.5],
            );
        }
    }
    // Colours: a name, a hex, translucent, with a translucent box, and an invalid one (drawn as the
    // default, white, as the graph does).
    for (name, color, bg) in [
        ("yellow", "yellow", None),
        ("hex", "#ff6a00", Some("black@0.6")),
        ("translucent", "white@0.5", Some("0x203040")),
        ("translucent-box", "#ffcc00", Some("#00000080")),
        ("invalid", "not a colour!", Some("bad:colour")),
    ] {
        let mut o = overlay("Coloured text", 0.0, 2.0);
        (o.color, o.bg, o.pos_y) = (color.into(), bg.map(String::from), 0.3);
        check_text(&format!("text/colour-{name}"), &tl(vec![o]), one, &[0.5]);
    }
    // A keyframed overlay: position and opacity moving, sampled at times between the frame grid's.
    let mut moving = overlay("Sliding in", 0.0, 2.0);
    moving.size = 0.07;
    moving.keyframes = vec![
        kerf_core::TextKeyframe {
            time: 0.0,
            pos_x: 0.2,
            pos_y: 0.3,
            opacity: 0.1,
        },
        kerf_core::TextKeyframe {
            time: 1.5,
            pos_x: 0.7,
            pos_y: 0.6,
            opacity: 1.0,
        },
    ];
    check_text("text/keyframed", &tl(vec![moving]), one, &[0.0, 0.37, 0.8, 1.2345, 1.9]);
    // Several at once, overlapping: drawn in order, the later on the earlier.
    let (mut a, mut b) = (overlay("Back", 0.0, 2.0), overlay("Front, over it", 0.5, 2.0));
    (a.size, a.bg, a.pos_y) = (0.2, Some("black@0.4".into()), 0.5);
    (b.size, b.color, b.bg, b.pos_y) = (0.08, "yellow".into(), Some("white@0.5".into()), 0.5);
    check_text("text/overlapping", &tl(vec![a, b]), one, &[0.2, 1.0]);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn text_over_every_kind_of_picture_is_drawn_as_ffmpeg_draws_it() {
    let m = media();
    let title = |text: &str| {
        let mut o = overlay(text, 0.0, 2.0);
        (o.size, o.pos_y, o.bg, o.color) = (0.09, 0.5, Some("black@0.5".into()), "yellow@0.9".into());
        o
    };
    // The composite's matrix is what FFmpeg converts the text colour with: BT.601 under 6.1 whatever
    // the picture is tagged, the tag under 9.0. (A mixed stack is refused by the plan, as ever.)
    for (name, a) in [
        ("untagged", &m.bars),
        ("bt709", &m.bars709),
        ("bt2020", &m.bars2020),
        ("gradient-709", &m.gradient709),
    ] {
        let tl = with_text(
            timeline(vec![vec![clip(a, 0.0, 2.0, 0.0)]], None),
            vec![title("Over the bars")],
        );
        check_text(&format!("text/over-{name}"), &tl, std::slice::from_ref(a), &[0.5]);
    }
    for (name, a) in [("full-range-mjpeg", &m.fullrange), ("full-range-jpeg-still", &m.jpeg)] {
        let tl = with_text(
            timeline(vec![vec![clip(a, 0.0, 2.0, 0.0)]], None),
            vec![title("Over a full-range picture")],
        );
        let negotiates = matches!(policy(), CompositeColorPolicy::BottomLayerTag);
        check_text_extra(
            &format!("text/over-{name}"),
            &tl,
            std::slice::from_ref(a),
            &[0.5],
            if negotiates { 2 } else { 0 },
            "a known model difference, not rounding: 9.0 draws the text in full range and converts the range last, \
             the compositor blends it in the limited-range composite",
        );
    }
    // Over a layer whose source has no frame there (a clip longer than its footage): nothing is drawn
    // for it, but the overlay chain is in the graph, so the text is blended as YUV, not as the RGB of a
    // frame with no layer at all.
    let mut late = title("Past the end of the footage");
    late.end = 5.0;
    let tl = with_text(timeline(vec![vec![clip(&m.testsrc, 0.0, 5.0, 0.0)]], None), vec![late]);
    assert!(kerf_gpu::decode_layers(
        &RenderPlan::at(
            &tl,
            std::slice::from_ref(&m.testsrc),
            &ExportOptions::default(),
            3.0,
            policy()
        )
        .expect("plan")
        .layers
    )
    .expect("decode")[0]
        .is_none());
    check_text("text/over-an-empty-layer", &tl, std::slice::from_ref(&m.testsrc), &[3.0]);
    // Over a graded layer, a translucent one and a letterbox (the bars are part of the picture).
    let mut graded = clip(&m.testsrc, 0.0, 2.0, 0.0);
    graded.color = Color {
        contrast: 1.3,
        saturation: 0.6,
        brightness: 0.05,
        ..Color::default()
    };
    let tl = with_text(timeline(vec![vec![graded]], None), vec![title("Graded")]);
    check_text("text/over-graded", &tl, std::slice::from_ref(&m.testsrc), &[0.5]);
    let mut half = clip(&m.bars, 0.0, 2.0, 0.0);
    half.transform.opacity = 0.5;
    let tl = with_text(
        timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![half]], None),
        vec![title("Translucent")],
    );
    check_text("text/over-translucent", &tl, &[m.gradient.clone(), m.bars.clone()], &[0.5]);
    let tl = with_text(
        timeline(
            vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
            Some(Delivery::new(360, 640, Fit::Contain)),
        ),
        vec![title("Letterboxed")],
    );
    check_text("text/over-letterbox-9x16", &tl, std::slice::from_ref(&m.testsrc), &[0.5]);
    // 9:16 from a landscape source, and a frame with an odd chroma grid at the box's edge.
    let mut odd = title("Odd");
    (odd.pos_x, odd.size) = (0.4999, 0.14);
    let tl = with_text(
        timeline(
            vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
            Some(Delivery::new(202, 100, Fit::Cover)),
        ),
        vec![odd],
    );
    check_text("text/odd-canvas-202x100", &tl, std::slice::from_ref(&m.testsrc), &[0.5]);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn text_on_a_frame_with_no_layer_is_drawn_in_rgb_as_ffmpeg_draws_it() {
    let m = media();
    // Past the clip: FFmpeg hands the black base straight to `drawtext` and the PNG without ever
    // converting it to YUV, so the text is blended on RGB (and the colour is the one written).
    let tl = |o: kerf_core::TextOverlay| with_text(timeline(vec![vec![clip(&m.testsrc, 0.0, 1.0, 0.0)]], None), vec![o]);
    for (name, color, bg) in [
        ("white", "white", None),
        ("yellow-boxed", "yellow@0.9", Some("black@0.5")),
        ("hex-on-a-solid-box", "#33aaff", Some("#ff00aa")),
    ] {
        let mut o = overlay("Title card, no picture", 1.2, 2.0);
        (o.size, o.pos_y, o.color, o.bg) = (0.09, 0.5, color.into(), bg.map(String::from));
        check_text(
            &format!("text/blank-{name}"),
            &tl(o),
            std::slice::from_ref(&m.testsrc),
            &[1.5],
        );
    }
    let mut fading = preset("Fading in", (0.5, 0.5), 0.1, true, Some("black@0.5"), 0.5);
    (fading.start, fading.end) = (1.2, 3.2);
    check_text(
        "text/blank-fading",
        &tl(fading),
        std::slice::from_ref(&m.testsrc),
        &[1.4, 2.2],
    );
    // There are no YUV planes to hand back for such a frame: its text would be missing from them.
    let plan = RenderPlan::at(
        &tl(overlay("Title card", 1.2, 2.0)),
        std::slice::from_ref(&m.testsrc),
        &ExportOptions::default(),
        1.5,
        policy(),
    )
    .expect("plan");
    assert!(plan.layers.is_empty() && !plan.overlays.is_empty());
    let size = plan.size(plan.canvas.width);
    let err = compositor()
        .composite_yuv(&plan, &[], size)
        .expect_err("no YUV for text in RGB");
    assert!(matches!(err, kerf_gpu::GpuError::Unsupported(_)), "{err:?}");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn text_that_needs_escaping_is_drawn_as_the_user_typed_it() {
    let m = media();
    let one = std::slice::from_ref(&m.testsrc);
    // Each of these used to draw something else in the still itself (an apostrophe swallowed the options
    // after it, a colon failed the render on FFmpeg 9 and cut the text on 6.1, a backslash vanished): the
    // text is what was typed, in the still and on the GPU alike.
    for (n, text) in [
        "It's here",
        "Note: this works",
        "50% off, 100%",
        "back\\slash",
        "say \"hi\", ok; [x] {y}",
        "  padded  ",
        "Café ☕ 世界",
        "Привет, Łódź",
        "%{pts}",
        "",
    ]
    .into_iter()
    .enumerate()
    {
        let mut o = overlay(text, 0.0, 2.0);
        (o.size, o.pos_y, o.bg) = (0.08, 0.4, Some("black@0.5".into()));
        let tl = with_text(timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None), vec![o]);
        check_text(&format!("text/escapes-{n}"), &tl, one, &[0.5]);
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn text_in_an_installed_font_is_drawn_as_ffmpeg_draws_it() {
    let m = media();
    let one = std::slice::from_ref(&m.testsrc);
    // Whatever families this machine has: a face with a real bold is drawn bold, the one with none is
    // refused (the border `drawtext` thickens it with is blended glyph by glyph, which a mask cannot
    // be), and a family that is not installed is the shipped default.
    let installed = kerf_core::list_system_fonts();
    let mut drawn = 0;
    for family in [
        "DejaVu Sans",
        "Liberation Sans",
        "Noto Sans",
        "Arial",
        "Helvetica",
        "Segoe UI",
        "Inter",
    ] {
        if !installed.iter().any(|f| f == family) {
            continue;
        }
        let regular = kerf_core::fonts::resolve_font_file(family, false).is_some();
        let has_bold = kerf_core::fonts::resolve_font_file(family, true).is_some_and(|(_, bold)| bold);
        if !regular {
            continue;
        }
        let mut o = overlay("Named font, Ay Te", 0.0, 2.0);
        (o.font, o.size, o.pos_y, o.bold) = (Some(family.into()), 0.08, 0.5, has_bold);
        let tl = with_text(timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None), vec![o]);
        check_text(&format!("text/font-{}", family.replace(' ', "-")), &tl, one, &[0.5]);
        drawn += 1;
    }
    // On CI the runners have at least one of these; a machine without any says so rather than
    // passing as if the case had run.
    assert!(
        drawn > 0 || std::env::var_os("CI").is_none(),
        "CI has none of the listed system fonts installed, so no installed-font case ran"
    );
    if drawn == 0 {
        eprintln!("SKIPPED the installed-font cases: none of the listed families is installed here");
    } else {
        eprintln!("installed families drawn: {drawn}");
    }
    let mut o = overlay("Not installed", 0.0, 2.0);
    (o.font, o.size, o.bold) = (Some("No Such Family 123".into()), 0.08, true);
    let tl = with_text(timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None), vec![o]);
    check_text("text/font-not-installed", &tl, one, &[0.5]);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_text_pass_is_the_reference_blend_plane_by_plane() {
    // The shader against its scalar reference on the planes themselves (no YUV -> RGB in between), over
    // pictures with texture: the composite with and without the overlays, the reference blend applied to
    // the second. `tests/text.rs` holds the reference to `drawtext`, so the two together hold the shader to it.
    let m = media();
    let mut title = overlay("Plane by plane, Ay Te 50%", 0.0, 2.0);
    (title.size, title.pos_x, title.pos_y, title.bg) = (0.07, 0.4137, 0.5821, Some("black@0.55".into()));
    let mut caption = overlay("Odd origin", 0.0, 2.0);
    (caption.size, caption.pos_x, caption.pos_y) = (0.09, 0.0031, 0.9733);
    caption.color = "yellow@0.8".into();
    let mut solid = overlay("Solid", 0.0, 2.0);
    (solid.size, solid.pos_x, solid.pos_y, solid.bg) = (0.12, 0.9, 0.12, Some("0x30a0c0".into()));
    let mut fading = overlay("Fading", 0.0, 2.0);
    (fading.size, fading.pos_y, fading.bg) = (0.1, 0.3, Some("white@0.5".into()));
    fading.keyframes = vec![
        kerf_core::TextKeyframe {
            time: 0.0,
            pos_x: 0.5,
            pos_y: 0.3,
            opacity: 0.0,
        },
        kerf_core::TextKeyframe {
            time: 1.0,
            pos_x: 0.5,
            pos_y: 0.3,
            opacity: 1.0,
        },
    ];
    for (name, asset) in [
        ("testsrc2", &m.testsrc),
        ("bt709 bars", &m.bars709),
        ("gradient", &m.gradient),
    ] {
        let tl = with_text(
            timeline(vec![vec![clip(asset, 0.0, 2.0, 0.0)]], None),
            vec![title.clone(), caption.clone(), solid.clone(), fading.clone()],
        );
        for t in [0.2, 0.6, 1.4] {
            let plan = RenderPlan::at(&tl, std::slice::from_ref(asset), &ExportOptions::default(), t, policy()).expect("plan");
            let size = plan.size(u32::MAX);
            assert!(reasons(&plan, size).is_empty(), "{:?}", reasons(&plan, size));
            let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
            let mut bare = plan.clone();
            bare.overlays.clear();
            let base = compositor().composite_yuv(&bare, &frames, size).expect("composite");
            let with = compositor().composite_yuv(&plan, &frames, size).expect("composite with text");
            let (mut y, mut u, mut v) = (base.y.clone(), base.u.clone(), base.v.clone());
            for text in &plan.overlays {
                let graph = kerf_core::text_raster_graph(text, size);
                let raster = kerf_gpu::text::rasterize(&graph).expect("rasterise");
                let blends = kerf_gpu::text::blends(text, &raster, plan.canvas.matrix, false).expect("blends");
                let mut pic = kerf_gpu::text::Planes {
                    w: size.0 as usize,
                    h: size.1 as usize,
                    subsampled: true,
                    planes: [&mut y, &mut u, &mut v],
                };
                kerf_gpu::text::blend_all(&mut pic, &raster, &blends);
            }
            assert_ne!(y, base.y, "{name} @ {t}: the text drew nothing");
            for (plane, got, want) in [("Y", &with.y, &y), ("U", &with.u, &u), ("V", &with.v, &v)] {
                let wrong = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
                assert_eq!(
                    wrong, 0,
                    "{name} @ {t}: {wrong} samples of the {plane} plane differ from the reference blend"
                );
            }
            eprintln!(
                "text pass == reference blend: {name} @ {t} ({}x{}, {} overlays)",
                size.0,
                size.1,
                plan.overlays.len()
            );
        }
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn text_the_gpu_cannot_reproduce_is_refused() {
    let m = media();
    let tl = with_text(
        timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None),
        vec![overlay("Bold, in a font with no bold face", 0.0, 2.0)],
    );
    let assets = std::slice::from_ref(&m.testsrc);
    let mut plan = RenderPlan::at(&tl, assets, &ExportOptions::default(), 0.5, policy()).expect("plan");
    let size = plan.size(u32::MAX);
    assert!(reasons(&plan, size).is_empty(), "the control is drawn");
    // A bold request the face cannot meet (the plan marks it): `drawtext` thickens it with a border.
    plan.overlays[0].synthetic_bold = true;
    let why = reasons(&plan, size).join("; ");
    assert!(why.contains("no bold face"), "{why}");
    let err = compositor().render_plan(&plan, size).expect_err("the render should refuse");
    assert!(
        matches!(&err, kerf_gpu::GpuError::Unsupported(w) if w.contains("no bold face")),
        "{err}"
    );
    plan.overlays[0].synthetic_bold = false;
    // Text under 12 px is all shared pixels: a mask cannot stand for the bitmaps `drawtext` blends in turn.
    plan.overlays[0].size = 0.03;
    let why = reasons(&plan, size).join("; ");
    assert!(why.contains("11 px font"), "{why}");
    let err = compositor().render_plan(&plan, size).expect_err("the render should refuse");
    assert!(
        matches!(&err, kerf_gpu::GpuError::Unsupported(w) if w.contains("11 px font")),
        "{err}"
    );
    plan.overlays[0].size = 0.034;
    assert!(reasons(&plan, size).is_empty(), "12 px and up is drawn");
    // A font file the plan cannot name (the default could not be written): FFmpeg's own default font.
    plan.overlays[0].font_file = None;
    let why = reasons(&plan, size).join("; ");
    assert!(why.contains("no font file"), "{why}");
    eprintln!("refused/text: border, small text, no font file");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn text_in_a_colour_bitmap_font_is_not_drawn_in_colour_by_anyone() {
    // `drawtext` loads glyphs without FT_LOAD_COLOR and has no BGRA path: a colour bitmap font (CBDT,
    // sbix) is a hard error in FFmpeg 6.1.1 (which then segfaults) and 9.0.2 alike, at the one
    // size the font's strike has and at every other ("Could not set font size"). So there is no colour
    // glyph for the compositor to draw as a silhouette; its rasteriser runs the same `drawtext` and the
    // frame is an error, never a picture with the wrong glyphs. (A COLR font is drawn as its monochrome
    // outlines by `drawtext` itself, which the compositor then blends exactly like any other face.)
    let Some((path, _)) = kerf_core::fonts::resolve_font_file("Noto Color Emoji", false) else {
        eprintln!("SKIPPED the colour-font case: no colour bitmap font is installed here");
        return;
    };
    let m = media();
    let assets = std::slice::from_ref(&m.testsrc);
    for size in [0.3028, 0.1] {
        let mut o = overlay("\u{1f3ac}", 0.0, 2.0);
        (o.font, o.size) = (Some("Noto Color Emoji".into()), size);
        let tl = with_text(timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None), vec![o]);
        let png = std::env::temp_dir().join(format!("kerf-colour-font-{}-{size}.png", std::process::id()));
        let still = export_still(&tl, assets, &ExportOptions::default(), 0.5, &png, ImageFormat::Png, 0);
        let _ = std::fs::remove_file(&png);
        assert!(
            still.is_err(),
            "{}: FFmpeg drew a colour bitmap font after all",
            path.display()
        );
        let plan = RenderPlan::at(&tl, assets, &ExportOptions::default(), 0.5, policy()).expect("plan");
        let size = plan.size(u32::MAX);
        if reasons(&plan, size).is_empty() {
            let drawn = compositor().render_plan(&plan, size);
            assert!(drawn.is_err(), "the compositor drew a frame FFmpeg cannot");
        }
    }
}

// ---- the scaler, plane by plane --------------------------------------------------

/// The picture scaled by FFmpeg's own `scale` (default flags: bicubic) to the
/// canvas, as the three raw planes — no RGB conversion in between, which is what
/// separates the scaler's error from the converter's.
fn ffmpeg_scaled_planes(layer: &kerf_core::PlanLayer, w: u32, h: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let seek = format!("{}", layer.source_time);
    let scale = format!("scale={w}:{h}");
    let mut args = vec!["-ss", &seek, "-i", &layer.path];
    if layer.is_image {
        args = vec!["-i", &layer.path];
    }
    args.extend([
        "-frames:v",
        "1",
        "-vf",
        &scale,
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "pipe:1",
    ]);
    let raw = ffmpeg(&args);
    let (luma, chroma) = ((w * h) as usize, (w / 2 * (h / 2)) as usize);
    assert_eq!(raw.len(), luma + 2 * chroma);
    (
        raw[..luma].to_vec(),
        raw[luma..luma + chroma].to_vec(),
        raw[luma + chroma..].to_vec(),
    )
}

/// Every plane of the GPU's scale of a picture to a canvas of its own shape (so
/// nothing is padded or cropped) against `ffmpeg -vf scale`. This is the committed
/// evidence for "the shader's bicubic is swscale's to within a level up to about
/// 4:1, and to 2-5 levels on the extreme downscales" — measured on the planes
/// themselves, on smooth footage (`SCALER_SMOOTH_MEAN`) and on noise and a
/// checkerboard, where every sample depends on the exact kernel
/// (`SCALER_BUSY_MEAN`).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_scaler_matches_ffmpegs_scale_plane_by_plane() {
    let m = media();
    let opts = ExportOptions::default();
    type Case<'a> = (&'a str, &'a Asset, f64, &'a [(u32, u32)], f64);
    let cases: [Case; 6] = [
        (
            "testsrc2",
            &m.testsrc,
            0.5,
            &[(480, 270), (320, 180), (960, 540), (1920, 1080), (64, 36), (32, 18)],
            SCALER_SMOOTH_MEAN,
        ),
        ("bars", &m.bars, 0.5, &[(480, 270), (960, 540), (64, 36)], SCALER_SMOOTH_MEAN),
        (
            "gradient",
            &m.gradient,
            0.5,
            &[(480, 270), (960, 540), (64, 36)],
            SCALER_SMOOTH_MEAN,
        ),
        (
            "noise",
            &m.noise,
            0.1,
            &[(480, 270), (384, 216), (960, 540), (64, 36)],
            SCALER_BUSY_MEAN,
        ),
        (
            "noise-hd",
            &m.noise_hd,
            0.05,
            &[(640, 360), (320, 180), (128, 72), (96, 54), (64, 36), (32, 18)],
            SCALER_BUSY_MEAN,
        ),
        (
            "checker",
            &m.checker,
            0.1,
            &[(480, 270), (384, 216), (960, 540)],
            SCALER_BUSY_MEAN,
        ),
    ];
    let mut failures = Vec::new();
    for (name, asset, t, sizes, mean_limit) in cases {
        for &(w, h) in sizes {
            let tl = timeline(
                vec![vec![clip(asset, 0.0, asset.duration, 0.0)]],
                Some(Delivery::new(w, h, Fit::Contain)),
            );
            let plan = RenderPlan::at(&tl, std::slice::from_ref(asset), &opts, t, policy()).expect("plan");
            let why = reasons(&plan, (w, h));
            assert!(why.is_empty(), "{why:?}");
            let gpu_planes = {
                let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
                compositor().composite_yuv(&plan, &frames, (w, h)).expect("composite")
            };
            let (ry, ru, rv) = ffmpeg_scaled_planes(&plan.layers[0], w, h);
            for (plane, got, want) in [
                ("Y", &gpu_planes.y, &ry),
                ("U", &gpu_planes.u, &ru),
                ("V", &gpu_planes.v, &rv),
            ] {
                assert_eq!(got.len(), want.len(), "{name} {w}x{h} {plane}");
                let (mut max, mut sum) = (0i32, 0f64);
                for (a, b) in got.iter().zip(want.iter()) {
                    let d = (i32::from(*a) - i32::from(*b)).abs();
                    max = max.max(d);
                    sum += f64::from(d);
                }
                let mean = sum / got.len() as f64;
                if std::env::var_os("KERF_SCALER_DEBUG").is_some() && max > 2 {
                    let pw = if plane == "Y" { w as usize } else { w as usize / 2 };
                    let mut worst: Vec<(i32, usize, usize)> = got
                        .iter()
                        .zip(want.iter())
                        .enumerate()
                        .map(|(i, (a, b))| ((i32::from(*a) - i32::from(*b)).abs(), i % pw, i / pw))
                        .filter(|(d, _, _)| *d > 2)
                        .collect();
                    worst.sort_unstable_by(|a, b| b.cmp(a));
                    worst.truncate(12);
                    eprintln!("  worst {name} {w}x{h} {plane}: {worst:?}");
                }
                let line = format!("scaler/{name:<9} -> {w:>4}x{h:<4} {plane}: max {max}  mean {mean:.3}");
                eprintln!("{line}");
                report()
                    .lock()
                    .unwrap()
                    .insert(format!("scaler {name} {w:05}x{h:05} {plane}"), line);
                // A shrink of 8:1 or more reduces each output sample from dozens of
                // inputs, and swscale's x86 vertical scaler rounds each of them
                // slightly differently from the C arithmetic this follows.
                let ratio = plan.layers[0].stream.width / w;
                let (max_limit, mean_limit) = if ratio >= 32 {
                    (SCALER_MAX_STEEP, mean_limit.max(SCALER_STEEP_MEAN))
                } else if ratio >= 8 {
                    (SCALER_MAX_EXTREME, mean_limit.max(SCALER_EXTREME_MEAN))
                } else {
                    (SCALER_MAX, mean_limit)
                };
                if max > max_limit || mean > mean_limit {
                    failures.push(format!(
                        "{name} -> {w}x{h} {plane}: max {max} (<= {max_limit}), mean {mean:.3} (<= {mean_limit})"
                    ));
                }
            }
        }
    }
    // Past the steepest shrink measured (`MAX_SHRINK`, 40:1) the plan declines and
    // FFmpeg draws it.
    let tl = timeline(
        vec![vec![clip(&m.noise_hd, 0.0, m.noise_hd.duration, 0.0)]],
        Some(Delivery::new(30, 16, Fit::Contain)),
    );
    check_refused(
        "shrink/noise-hd-past-40:1",
        &tl,
        std::slice::from_ref(&m.noise_hd),
        0.05,
        Refusal::Plan("steeper"),
    );
    write_report();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ---- a moving zoom last, masks and effects, in a still -----------------------------------------------

fn zoom_keys(from: f64, to: f64, pos: (f64, f64)) -> Vec<Keyframe> {
    let key = |time: f64, scale: f64, pos: (f64, f64)| Keyframe {
        easing: Default::default(),
        time,
        scale,
        pos_x: pos.0,
        pos_y: pos.1,
        rotation: 0.0,
        opacity: 1.0,
    };
    vec![key(0.0, from, (0.0, 0.0)), key(1.5, to, pos)]
}

fn a_mask(shape: kerf_core::MaskShape, feather: f64, inverted: bool) -> kerf_core::Mask {
    kerf_core::Mask {
        shape,
        x: 0.5,
        y: 0.5,
        width: 0.6,
        height: 0.6,
        feather,
        inverted,
    }
}

/// The still runs a moving zoom as the last stage of the clip's chain, like the export, so what is in
/// front of it (a grade, a mask, an effect, the clip's fades) acts at the fit size and is magnified.
/// Held to FFmpeg's still, at several moments of the zoom.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_moving_zoom_is_the_last_stage_of_the_still_too() {
    use kerf_core::VideoEffect as E;
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let times = [0.5, 1.0, 1.25, 1.9];
    let zooming = |edit: &dyn Fn(&mut Clip)| {
        let mut z = clip(&m.testsrc, 0.0, 2.0, 0.5);
        z.keyframes = zoom_keys(1.0, 1.6, (0.0, 0.0));
        edit(&mut z);
        timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]], None)
    };
    check("zoom/plain", &zooming(&|_| {}), &assets, &times, STRICT);
    check(
        "zoom/out with position",
        &zooming(&|c| c.keyframes = zoom_keys(1.0, 0.6, (0.2, -0.1))),
        &assets,
        &times,
        STRICT,
    );
    check(
        "zoom+grade",
        &zooming(&|c| {
            c.color = Color {
                contrast: 1.3,
                saturation: 1.2,
                ..Color::default()
            }
        }),
        &assets,
        &times,
        STRICT,
    );
    check(
        "zoom+mask",
        &zooming(&|c| c.mask = Some(a_mask(kerf_core::MaskShape::Ellipse, 0.2, false))),
        &assets,
        &times,
        STRICT,
    );
    check(
        "zoom+mask rect inverted",
        &zooming(&|c| c.mask = Some(a_mask(kerf_core::MaskShape::Rect, 0.1, true))),
        &assets,
        &times,
        STRICT,
    );
    for (name, effect) in [
        ("blur", E::Blur { sigma: 5.0 }),
        ("sharpen", E::Sharpen { amount: 1.5 }),
        ("grayscale", E::Grayscale),
        ("invert", E::Invert),
    ] {
        check(
            &format!("zoom+{name}"),
            &zooming(&|c| c.effects = vec![effect.clone()]),
            &assets,
            &times,
            STRICT,
        );
    }
    // The tutorial's clip: a feathered mask over a blurred picture, in a Cover frame, zooming in.
    let mut z = clip(&m.testsrc, 0.0, 2.0, 0.0);
    z.keyframes = zoom_keys(1.0, 1.25, (-0.05, 0.0));
    z.mask = Some(a_mask(kerf_core::MaskShape::Rect, 0.1, false));
    z.effects = vec![E::Blur { sigma: 6.0 }];
    let tl = timeline(
        vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]],
        Some(Delivery::new(960, 540, Fit::Cover)),
    );
    check("zoom+mask+blur cover", &tl, &assets, &times, STRICT);
    // A fade-in running at the moment of the still: faded at the fit size, then zoomed.
    check(
        "zoom+fade-in",
        &zooming(&|c| c.fade_in = 1.0),
        &assets,
        &[0.6, 0.9, 1.2],
        STRICT,
    );
    // A dissolve into a zooming, masked clip.
    let out = clip(&m.bars, 0.0, 2.0, 0.0);
    let mut inc = clip(&m.testsrc, 0.0, 2.0, 1.0);
    inc.keyframes = zoom_keys(1.0, 1.4, (0.0, 0.0));
    inc.mask = Some(a_mask(kerf_core::MaskShape::Ellipse, 0.2, false));
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.6,
    });
    let tl = timeline(vec![vec![out, inc]], None);
    check("zoom/dissolve+mask", &tl, &assets, &[1.1, 1.3, 1.5], STRICT);
}

/// A mask in a still: the alpha `geq` writes, blended by `overlay` (luma per sample; chroma over the
/// average of its block's alphas).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn masks_cut_a_layer_to_a_shape() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let masked = |edit: &dyn Fn(&mut Clip), format: Option<Delivery>| {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        edit(&mut c);
        timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], format)
    };
    for shape in [kerf_core::MaskShape::Rect, kerf_core::MaskShape::Ellipse] {
        for (feather, inverted) in [(0.0, false), (0.15, false), (0.6, false), (0.1, true)] {
            check(
                &format!("mask/{shape:?}-{feather}{}", if inverted { "-inverted" } else { "" }),
                &masked(&|c| c.mask = Some(a_mask(shape, feather, inverted)), None),
                &assets,
                &[0.5, 1.2],
                STRICT,
            );
        }
    }
    // Smaller, moved and odd-sized layers (211x118: the last chroma row and column have fewer neighbours).
    for (name, scale, pos) in [
        ("pip", 0.5, (0.2, -0.15)),
        ("odd", 0.33, (0.1, 0.1)),
        ("off the frame", 0.8, (0.45, 0.4)),
    ] {
        check(
            &format!("mask/{name}"),
            &masked(
                &|c| {
                    c.transform.scale = scale;
                    c.transform.pos_x = pos.0;
                    c.transform.pos_y = pos.1;
                    c.mask = Some(a_mask(kerf_core::MaskShape::Ellipse, 0.2, false));
                },
                None,
            ),
            &assets,
            &[0.5],
            STRICT,
        );
    }
    // A letterboxed picture: the mask is cut from the whole padded frame, bars included.
    check(
        "mask/letterboxed 9:16",
        &masked(
            &|c| c.mask = Some(a_mask(kerf_core::MaskShape::Ellipse, 0.2, false)),
            Some(Delivery::new(360, 640, Fit::Contain)),
        ),
        &assets,
        &[0.5],
        STRICT,
    );
    // A cropped clip Cover-fitted into a vertical frame, the clip's fades running.
    let tl = masked(
        &|c| {
            c.transform.crop_left = 0.2;
            c.transform.crop_right = 0.2;
            c.mask = Some(a_mask(kerf_core::MaskShape::Rect, 0.12, false));
            c.fade_in = 1.0;
        },
        Some(Delivery::new(360, 640, Fit::Cover)),
    );
    check("mask/9:16 cover crop fade-in", &tl, &assets, &[0.2, 0.4, 1.0], STRICT);
    // Over a busy picture, a mask is a mask: noise under a feathered ellipse.
    let mut c = clip(&m.noise, 0.0, 0.2, 0.0);
    c.mask = Some(a_mask(kerf_core::MaskShape::Ellipse, 0.3, false));
    let tl = timeline(vec![vec![clip(&m.black, 0.0, 2.0, 0.0)], vec![c]], None);
    check("mask/over noise", &tl, &[m.black.clone(), m.noise.clone()], &[0.1], BUSY);
    // The mask is the alpha plane alone, whatever the picture it cuts: the plain source's own figures.
    for (label, asset) in [
        ("10-bit", &m.tenbit),
        ("4:4:4", &m.yuv444),
        ("full-range", &m.fullrange),
        ("gray", &m.gray),
        ("rgb-png", &m.png640),
    ] {
        let mut c = clip(asset, 0.0, asset.duration.min(2.0), 0.0);
        c.mask = Some(a_mask(kerf_core::MaskShape::Ellipse, 0.2, false));
        check(
            &format!("mask/on-{label}"),
            &timeline(vec![vec![c]], None),
            std::slice::from_ref(asset),
            &[0.5],
            STRICT,
        );
    }
}

/// The effects in a still, against FFmpeg's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn effects_are_drawn_as_their_filters_draw_them() {
    use kerf_core::VideoEffect as E;
    let m = media();
    for (name, effects) in [
        ("blur-0.5", vec![E::Blur { sigma: 0.5 }]),
        ("blur-3", vec![E::Blur { sigma: 3.0 }]),
        ("blur-12", vec![E::Blur { sigma: 12.0 }]),
        ("sharpen-1", vec![E::Sharpen { amount: 1.0 }]),
        ("sharpen-2.5", vec![E::Sharpen { amount: 2.5 }]),
        ("sharpen-neg", vec![E::Sharpen { amount: -1.5 }]),
        ("grayscale", vec![E::Grayscale]),
        ("invert", vec![E::Invert]),
        (
            "chain",
            vec![E::Sharpen { amount: 1.0 }, E::Grayscale, E::Blur { sigma: 2.0 }, E::Invert],
        ),
    ] {
        for (asset, label) in [(&m.testsrc, "testsrc2"), (&m.bars, "bars")] {
            let mut c = clip(asset, 0.0, 2.0, 0.0);
            c.effects = effects.clone();
            let tl = timeline(vec![vec![c]], None);
            check(
                &format!("effect/{name}-{label}"),
                &tl,
                std::slice::from_ref(asset),
                &[0.5, 1.5],
                STRICT,
            );
        }
    }
    // Under a grade, scaled, odd-sized, in a Cover frame, translucent and rotated: the effect comes
    // ahead of the opacity and the rotation.
    for (name, edit) in [
        (
            "graded",
            Box::new(|c: &mut Clip| {
                c.color.contrast = 1.25;
                c.color.saturation = 1.3;
            }) as Box<dyn Fn(&mut Clip)>,
        ),
        ("scaled", Box::new(|c: &mut Clip| c.transform.scale = 0.6)),
        ("odd", Box::new(|c: &mut Clip| c.transform.scale = 0.33)),
        (
            "translucent",
            Box::new(|c: &mut Clip| {
                c.transform.scale = 0.6;
                c.transform.opacity = 0.7;
            }),
        ),
        (
            "rotated",
            Box::new(|c: &mut Clip| {
                c.transform.scale = 0.6;
                c.transform.rotation = 15.0;
            }),
        ),
    ] {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        edit(&mut c);
        // (A sharpen after a scale is drawn to half strength: past it the scaler's level is amplified.)
        c.effects = vec![E::Blur { sigma: 3.0 }, E::Sharpen { amount: 0.5 }];
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None);
        let limits = if name == "rotated" { ROTATED } else { STRICT };
        check(
            &format!("effect/{name}"),
            &tl,
            &[m.bars.clone(), m.testsrc.clone()],
            &[0.5],
            limits,
        );
    }
    // A sharpen whose gain on a scale's or a blur's level passes 2 is FFmpeg's: past half strength after a scale
    // or a blur, or two of half strength.
    for (name, effects) in [
        ("sharpen-1.5", vec![E::Sharpen { amount: 1.5 }]),
        ("sharpen-5", vec![E::Sharpen { amount: 5.0 }]),
        (
            "blur-then-sharpen-1.5",
            vec![E::Blur { sigma: 3.0 }, E::Sharpen { amount: 1.5 }],
        ),
        (
            "sharpen-0.5-twice",
            vec![E::Sharpen { amount: 0.5 }, E::Sharpen { amount: 0.5 }],
        ),
        (
            "sharpen-0.2-twice",
            vec![E::Sharpen { amount: 0.2 }, E::Sharpen { amount: 0.2 }],
        ),
    ] {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.transform.scale = 0.6;
        c.effects = effects;
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None);
        check_refused(
            &format!("refused/effect-{name}-after-a-scale"),
            &tl,
            &[m.bars.clone(), m.testsrc.clone()],
            0.5,
            Refusal::Plan("sharpen after a scale or a blur"),
        );
    }
    // ...and after a blur with nothing scaled.
    for (name, effects) in [
        (
            "blur-then-sharpen-1",
            vec![E::Blur { sigma: 3.0 }, E::Sharpen { amount: 1.0 }],
        ),
        (
            "blur-then-sharpen-5",
            vec![E::Blur { sigma: 3.0 }, E::Sharpen { amount: 5.0 }],
        ),
    ] {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.effects = effects;
        check_refused(
            &format!("refused/effect-{name}"),
            &timeline(vec![vec![c]], None),
            std::slice::from_ref(&m.testsrc),
            0.5,
            Refusal::Plan("sharpen after a scale or a blur"),
        );
    }
    // A table on a letterbox: the bars are part of the frame `pad` hands the effect, so they go through it.
    for (name, effect) in [("invert", E::Invert), ("grayscale", E::Grayscale)] {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.effects = vec![effect];
        let tl = timeline(
            vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]],
            Some(Delivery::new(360, 640, Fit::Contain)),
        );
        check(
            &format!("effect/{name}-on-a-letterbox"),
            &tl,
            &[m.bars.clone(), m.testsrc.clone()],
            &[0.5],
            STRICT,
        );
    }
    // Pictures the decode converts to 4:2:0 first (10-bit, 4:2:2, 4:4:4, RGB, gray): FFmpeg runs the effect on
    // the picture as it is and converts afterwards, so the figures are the plain source's own (the decode's
    // rounding, `source/` above), not more. Not an invert of a gray picture, whose chroma FFmpeg leaves at 128.
    for (label, asset) in [
        ("10-bit", &m.tenbit),
        ("4:4:4", &m.yuv444),
        ("4:2:2", &m.yuv422),
        ("bgr0", &m.bgr0),
        ("rgb-png", &m.png640),
        ("gray", &m.gray),
    ] {
        for (name, effect) in [
            ("grayscale", E::Grayscale),
            ("invert", E::Invert),
            ("blur", E::Blur { sigma: 3.0 }),
            ("sharpen", E::Sharpen { amount: 1.0 }),
        ] {
            if label == "gray" && name == "invert" {
                continue;
            }
            let mut c = clip(asset, 0.0, asset.duration.min(2.0), 0.0);
            c.effects = vec![effect];
            let tl = timeline(vec![vec![c]], None);
            // A deeper-than-8-bit picture may be `pc`-tagged, which FFmpeg 9 keeps the raw values of (as a
            // `yuvj` one): where the range is negotiated the effects but grayscale are refused.
            if label == "10-bit" && name != "grayscale" && policy() != CompositeColorPolicy::FixedBt601 {
                check_refused(
                    &format!("refused/effect-{name}-on-{label}"),
                    &tl,
                    std::slice::from_ref(asset),
                    0.5,
                    Refusal::Plan("full-range"),
                );
                continue;
            }
            check(
                &format!("effect/{name}-on-{label}"),
                &tl,
                std::slice::from_ref(asset),
                &[0.5],
                STRICT,
            );
        }
    }
    // A full-range picture reaches the filter with its raw values and is converted afterwards: grayscale (chroma
    // to 128) is the same either way and is drawn; an invert is up to 8 levels off, a sharpen's overshoot 26 and a
    // blur 4 on 9.0.2, and an invert of a gray picture leaves FFmpeg's chroma at 128: refused.
    let on = |asset: &Asset, effect: E| {
        let mut c = clip(asset, 0.0, asset.duration.min(2.0), 0.0);
        c.effects = vec![effect];
        timeline(vec![vec![c]], None)
    };
    check(
        "effect/grayscale-on-full-range",
        &on(&m.fullrange, E::Grayscale),
        std::slice::from_ref(&m.fullrange),
        &[0.5],
        STRICT,
    );
    for (name, effect) in [
        ("invert", E::Invert),
        ("blur", E::Blur { sigma: 3.0 }),
        ("sharpen", E::Sharpen { amount: 1.0 }),
    ] {
        check_refused(
            &format!("refused/effect-{name}-on-full-range"),
            &on(&m.fullrange, effect.clone()),
            std::slice::from_ref(&m.fullrange),
            0.5,
            Refusal::Plan("full-range"),
        );
        check_refused(
            &format!("refused/effect-{name}-on-a-jpeg"),
            &on(&m.jpeg, effect),
            std::slice::from_ref(&m.jpeg),
            0.0,
            Refusal::Plan("full-range"),
        );
    }
    check_refused(
        "refused/effect-invert-on-gray",
        &on(&m.gray, E::Invert),
        std::slice::from_ref(&m.gray),
        0.5,
        Refusal::Plan("gray picture"),
    );
    // Noise in all three planes under every effect: every sample matters to its neighbours.
    for (name, effects) in [
        ("blur", vec![E::Blur { sigma: 2.5 }]),
        ("sharpen", vec![E::Sharpen { amount: 1.5 }]),
        ("invert", vec![E::Invert]),
        // A sharpen of half strength after a blur: the blur's level times a gain of two.
        (
            "blur-then-sharpen-0.5",
            vec![E::Blur { sigma: 2.5 }, E::Sharpen { amount: 0.5 }],
        ),
    ] {
        let mut c = clip(&m.noise, 0.0, 0.2, 0.0);
        c.effects = effects;
        let tl = timeline(vec![vec![c]], None);
        check(
            &format!("effect/noise-{name}"),
            &tl,
            std::slice::from_ref(&m.noise),
            &[0.1],
            BUSY,
        );
    }
}

/// Each effect's shader against its filter, **plane by plane**: the picture at its own size, so
/// nothing is scaled and no colour is converted, and what is compared is the filter's arithmetic. The
/// sharpen, the grayscale and the invert are integer arithmetic and equal to the byte on every
/// sample; the blur is `gblur`'s float recursion in the portable order (see `effects.rs`).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_effect_filters_match_ffmpeg_plane_by_plane() {
    use kerf_core::VideoEffect as E;
    let m = media();
    let opts = ExportOptions::default();
    // (name, the effect, FFmpeg's filter for it, whether it is exact to the byte)
    let cases: Vec<(&str, E, String, bool)> = vec![
        ("invert", E::Invert, "negate".into(), true),
        ("grayscale", E::Grayscale, "hue=s=0".into(), true),
        (
            "sharpen 1",
            E::Sharpen { amount: 1.0 },
            "unsharp=luma_msize_x=5:luma_msize_y=5:luma_amount=1".into(),
            true,
        ),
        (
            "sharpen 2.5",
            E::Sharpen { amount: 2.5 },
            "unsharp=luma_msize_x=5:luma_msize_y=5:luma_amount=2.5".into(),
            true,
        ),
        (
            "sharpen 0.7",
            E::Sharpen { amount: 0.7 },
            "unsharp=luma_msize_x=5:luma_msize_y=5:luma_amount=0.7".into(),
            true,
        ),
        (
            "sharpen -2",
            E::Sharpen { amount: -2.0 },
            "unsharp=luma_msize_x=5:luma_msize_y=5:luma_amount=-2".into(),
            true,
        ),
        ("blur 0.5", E::Blur { sigma: 0.5 }, "gblur=sigma=0.5".into(), false),
        ("blur 2", E::Blur { sigma: 2.0 }, "gblur=sigma=2".into(), false),
        ("blur 6", E::Blur { sigma: 6.0 }, "gblur=sigma=6".into(), false),
        ("blur 6.3", E::Blur { sigma: 6.3 }, "gblur=sigma=6.3".into(), false),
        ("blur 25", E::Blur { sigma: 25.0 }, "gblur=sigma=25".into(), false),
    ];
    let mut failures = Vec::new();
    for (asset, label, t) in [
        (&m.noise, "noise", 0.1),
        (&m.testsrc, "testsrc2", 0.5),
        (&m.bars, "bars", 0.5),
    ] {
        for (name, effect, filter, exact) in &cases {
            let mut c = clip(asset, 0.0, asset.duration, 0.0);
            c.effects = vec![effect.clone()];
            let tl = timeline(vec![vec![c]], None);
            let plan = RenderPlan::at(&tl, std::slice::from_ref(asset), &opts, t, policy()).expect("plan");
            let size = plan.size(u32::MAX);
            let why = reasons(&plan, size);
            assert!(why.is_empty(), "{why:?}");
            let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
            let got = compositor().composite_yuv(&plan, &frames, size).expect("composite");
            let seek = format!("{}", plan.layers[0].source_time);
            let raw = ffmpeg(&[
                "-ss",
                &seek,
                "-i",
                &plan.layers[0].path,
                "-frames:v",
                "1",
                "-vf",
                filter,
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "pipe:1",
            ]);
            let (w, h) = (size.0 as usize, size.1 as usize);
            let (luma, chroma) = (w * h, (w / 2) * (h / 2));
            assert_eq!(raw.len(), luma + 2 * chroma);
            for (plane, a, b) in [
                ("Y", &got.y[..], &raw[..luma]),
                ("U", &got.u[..], &raw[luma..luma + chroma]),
                ("V", &got.v[..], &raw[luma + chroma..]),
            ] {
                let (mut max, mut differing) = (0i32, 0usize);
                for (x, y) in a.iter().zip(b) {
                    let d = (i32::from(*x) - i32::from(*y)).abs();
                    max = max.max(d);
                    differing += usize::from(d != 0);
                }
                let line = format!(
                    "effect-planes/{label:<8} {name:<12} {plane}: max {max}  differing {differing} of {}",
                    a.len()
                );
                eprintln!("{line}");
                report()
                    .lock()
                    .unwrap()
                    .insert(format!("effect-planes {label} {name} {plane}"), line);
                // The blur may sit a level off on the samples whose value is on a half (see `effects.rs`):
                // a handful, never more than a level and never more than a few in ten thousand.
                let (max_limit, share) = if *exact { (0, 0.0) } else { (1, 2e-4) };
                if max > max_limit || differing as f64 > share * a.len() as f64 {
                    failures.push(format!(
                        "{label} {name} {plane}: max {max} (<= {max_limit}), differing {differing} of {}",
                        a.len()
                    ));
                }
            }
        }
    }
    write_report();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ---- the FFmpeg `eq` port, byte for byte --------------------------------------

/// `eq` applied by FFmpeg to a 256-level ramp in each plane equals the table the
/// compositor builds, entry for entry — the one place "close" is not good
/// enough, because the tables are the same arithmetic.
#[test]
#[ignore = "needs ffmpeg"]
fn the_eq_tables_match_ffmpegs_eq_filter() {
    // A 256x2 picture whose luma ramps 0..255 left to right; chroma planes
    // (128x1) ramp 0..254 in steps of two, so both halves of U / V are probed
    // at even values, and then again shifted by one for the odd values.
    for (name, c) in [
        (
            "all four knobs",
            Color {
                brightness: 0.08,
                contrast: 1.2,
                saturation: 1.4,
                gamma: 1.25,
                temperature: 0.0,
            },
        ),
        (
            "contrast+saturation",
            Color {
                contrast: 1.3,
                saturation: 0.6,
                ..Color::default()
            },
        ),
        (
            "warm",
            Color {
                temperature: 0.6,
                ..Color::default()
            },
        ),
        (
            "cool+brightness",
            Color {
                temperature: -0.8,
                brightness: -0.05,
                ..Color::default()
            },
        ),
        (
            "gamma only",
            Color {
                gamma: 0.7,
                ..Color::default()
            },
        ),
    ] {
        let tables = kerf_gpu::eq::luts(&c).expect("not the identity");
        // Build the eq= argument the way kerf-core does.
        let mut f = format!(
            "eq=brightness={}:contrast={}:saturation={}:gamma={}",
            c.brightness, c.contrast, c.saturation, c.gamma
        );
        if let Some((r, b)) = c.temperature_gammas() {
            f.push_str(&format!(":gamma_r={r}:gamma_b={b}"));
        }
        for odd in [0usize, 1] {
            // 256 wide: Y = x; U = V = x/2*2 + odd over the 128-wide chroma plane.
            let mut raw = Vec::new();
            for _ in 0..2 {
                raw.extend(0..=255u8);
            }
            for _ in 0..2 {
                // two chroma planes, each 128x1
                raw.extend((0..128usize).map(|x| (2 * x + odd) as u8));
            }
            let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
            let input = dir.join(format!("eq-ramp-{}.yuv", std::process::id()));
            std::fs::write(&input, &raw).unwrap();
            let out = ffmpeg(&[
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "-s",
                "256x2",
                "-i",
                input.to_str().unwrap(),
                "-vf",
                &f,
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "pipe:1",
            ]);
            let _ = std::fs::remove_file(&input);
            assert_eq!(out.len(), raw.len());
            for x in 0..256usize {
                assert_eq!(out[x], tables[0][x], "{name}: Y[{x}]");
            }
            for x in 0..128usize {
                let v = 2 * x + odd;
                assert_eq!(out[512 + x], tables[1][v], "{name}: U[{v}]");
                assert_eq!(out[512 + 128 + x], tables[2][v], "{name}: V[{v}]");
            }
        }
    }
}

// ---- the BT.709 label ------------------------------------------------------------

/// Footage as it arrives: the **same pictures** encoded with a known matrix and labelled in each
/// of the ways a camera or an encoder labels them, in the two sizes where the convention for an
/// untagged picture differs (HD is read as BT.709, SD as BT.601). The pictures are made from RGB,
/// so the file's real colours are known.
struct HdMedia {
    files: BTreeMap<&'static str, Asset>,
}

impl HdMedia {
    fn get(&self, name: &str) -> &Asset {
        self.files.get(name).unwrap_or_else(|| panic!("no media {name}"))
    }
}

/// The smooth, saturated picture (`grad/…`) and the busy test card (`detail/…`).
const GRADIENT: &str = "gradients=size=1280x720:rate=10:duration=1:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:x0=0:y0=0:x1=1280:y1=720:speed=0.00001";
const GRADIENT_SD: &str = "gradients=size=720x576:rate=10:duration=1:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:x0=0:y0=0:x1=720:y1=576:speed=0.00001";
const DETAIL: &str = "testsrc2=size=1280x720:rate=10:duration=1";
const DETAIL_SD: &str = "testsrc2=size=720x576:rate=10:duration=1";

fn hd_media() -> &'static HdMedia {
    static M: OnceLock<HdMedia> = OnceLock::new();
    M.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-media-hd");
        std::fs::create_dir_all(&dir).unwrap();
        let mut files = BTreeMap::new();
        let none = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
        let bt709 = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709";
        let bt601 = "setparams=colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m";
        let bt2020 = "setparams=colorspace=bt2020nc:color_primaries=bt2020:color_trc=bt709";
        // (matrix the values are encoded with, how the file labels itself)
        let encode = |files: &mut BTreeMap<&'static str, Asset>, name: &'static str, source: &str, matrix: &str, label: &str| {
            let p = dir.join(format!("{}.mp4", name.replace('/', "-")));
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                &format!("{source},format=gbrp,scale=out_color_matrix={matrix}:out_range=tv,format=yuv420p,{label}"),
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-qp",
                "0",
                "-g",
                "10",
                "-pix_fmt",
                "yuv420p",
                p.to_str().unwrap(),
            ]);
            files.insert(name, probed(&p));
        };
        for (picture, hd, sd) in [("grad", GRADIENT, GRADIENT_SD), ("detail", DETAIL, DETAIL_SD)] {
            let n = |hd_or_sd: &str, label: &str| -> &'static str {
                Box::leak(format!("{picture}/{hd_or_sd}-{label}").into_boxed_str())
            };
            encode(&mut files, n("hd", "untagged"), hd, "bt709", none);
            encode(&mut files, n("hd", "709"), hd, "bt709", bt709);
            encode(&mut files, n("hd", "601"), hd, "bt601", bt601);
            encode(&mut files, n("hd", "2020"), hd, "bt2020", bt2020);
            encode(&mut files, n("sd", "untagged"), sd, "bt601", none);
            encode(&mut files, n("sd", "601"), sd, "bt601", bt601);
        }
        for (name, source) in [("grad/rgb", GRADIENT), ("detail/rgb", DETAIL)] {
            let p = dir.join(format!("{}.mkv", name.replace('/', "-")));
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                source,
                "-c:v",
                "ffv1",
                "-pix_fmt",
                "bgr0",
                p.to_str().unwrap(),
            ]);
            files.insert(name, probed(&p));
        }
        // A logo the size of the canvas: RGB, so it names nothing and is never labelled.
        let logo = dir.join("logo.png");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "color=c=0xf0a020:s=640x360:d=1,drawbox=x=40:y=40:w=240:h=160:color=0x2060e0:t=fill,format=rgb24",
            "-frames:v",
            "1",
            logo.to_str().unwrap(),
        ]);
        files.insert("logo", probed(&logo));
        let hlg = dir.join("hlg.mp4");
        let made = Command::new(kerf_core::ffmpeg_path())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!(
                "{DETAIL},format=yuv420p,setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv,\
                 zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt2020:t=arib-std-b67:m=bt2020nc:r=tv,format=yuv420p10le"
            ))
            .args([
                "-c:v",
                "libx265",
                "-x265-params",
                "log-level=error",
                "-color_primaries",
                "bt2020",
                "-color_trc",
                "arib-std-b67",
                "-colorspace",
                "bt2020nc",
            ])
            .arg(&hlg)
            .stdin(Stdio::null())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if made {
            files.insert("detail/hlg", probed(&hlg));
        }
        HdMedia { files }
    })
}

/// Preview proxies generated for a test and removed when it ends (the cache is the user's: the
/// engine's own tests do the same with `ProxyGuard`). A proxy's place is a function of its source,
/// so two tests that proxy one file would delete each other's: the tests that hold proxies run one
/// at a time.
struct Proxies {
    made: Vec<PathBuf>,
    _one_at_a_time: std::sync::MutexGuard<'static, ()>,
}

impl Proxies {
    fn new() -> Self {
        static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
        Proxies {
            made: Vec::new(),
            _one_at_a_time: ONE_AT_A_TIME.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        }
    }

    /// `a` as the preview decodes it: its proxy, made by the product's own `generate_proxy`, and
    /// **the stream the proxy's sidecar describes** (what `ProxyMedia` hands the plan).
    fn of(&mut self, a: &Asset) -> Asset {
        let src = Path::new(&a.path);
        let proxy = kerf_core::generate_proxy(src, kerf_core::proxy_width(a.projection()))
            .unwrap_or_else(|e| panic!("proxy of {}: {e}", a.path));
        self.made.push(proxy.clone());
        let media = kerf_core::MediaResolver::resolve(&kerf_core::ProxyMedia, a);
        assert!(
            media.proxy && Path::new(&media.path) == proxy,
            "{}: the preview does not resolve to the proxy that was just made",
            a.path
        );
        let mut proxied = a.clone();
        proxied.path = media.path;
        let video = media.video.expect("the proxy has a video stream");
        *proxied
            .streams
            .iter_mut()
            .find(|s| s.kind == StreamKind::Video)
            .expect("a video stream") = video;
        proxied
    }
}

impl Drop for Proxies {
    fn drop(&mut self) {
        for proxy in &self.made {
            let _ = std::fs::remove_file(proxy);
            let _ = std::fs::remove_file(proxy.with_extension("json"));
        }
    }
}

/// A picture-in-picture clip: the top layer of a stack.
fn pip_clip(a: &Asset, opacity: f64) -> Clip {
    let mut c = clip(a, 0.0, 1.0, 0.0);
    c.transform = Transform {
        scale: 0.5,
        pos_x: 0.2,
        pos_y: -0.1,
        opacity,
        ..Transform::default()
    };
    c
}

/// The delivery shape of these cases: half of 1280x720, so each render is cheap and the HD layers
/// are shrunk 2:1 (the scaler's best-measured regime).
fn hd_canvas() -> Option<Delivery> {
    Some(Delivery::new(640, 360, Fit::Contain))
}

/// `base` full frame with `top` a picture-in-picture over it, `top` at `opacity`.
fn hd_stack(base: &Asset, top: &Asset, opacity: f64) -> (Timeline, Vec<Asset>) {
    let tl = timeline(
        vec![vec![clip(base, 0.0, 1.0, 0.0)], vec![pip_clip(top, opacity)]],
        hd_canvas(),
    );
    (tl, vec![base.clone(), top.clone()])
}

/// Which layers of the plan of `tl` at `t` the export graph labels BT.709, bottom first.
fn labelled_layers(tl: &Timeline, assets: &[Asset], t: f64) -> Vec<bool> {
    RenderPlan::at(tl, assets, &ExportOptions::default(), t, policy())
        .expect("plan")
        .layers
        .iter()
        .map(|l| l.stream.label_bt709)
        .collect()
}

/// The matrix the plan draws the composite with, and the one of each layer.
fn plan_matrices(tl: &Timeline, assets: &[Asset]) -> String {
    let plan = RenderPlan::at(tl, assets, &ExportOptions::default(), 0.5, policy()).expect("plan");
    format!(
        "composite {:?}, layers {:?}",
        plan.canvas.matrix,
        plan.layers.iter().map(|l| l.stream.matrix()).collect::<Vec<_>>()
    )
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn untagged_hd_is_labelled_bt709_beside_footage_that_names_a_matrix_and_stacks_with_it() {
    let h = hd_media();
    eprintln!("composite colour policy of this FFmpeg: {:?}", policy());
    let untagged = h.get("grad/hd-untagged");
    let phone = h.get("detail/hd-709");
    assert_eq!(untagged.streams[0].color_space, None, "the footage says nothing");

    // Alone, or among footage that names nothing, it is read as BT.601 as it always was: not labelled,
    // and drawn exactly as before.
    let tl = timeline(vec![vec![clip(untagged, 0.0, 1.0, 0.0)]], hd_canvas());
    assert_eq!(labelled_layers(&tl, std::slice::from_ref(untagged), 0.5), [false]);
    check("label/untagged-hd-alone", &tl, std::slice::from_ref(untagged), &[0.5], STRICT);
    let mut fade = clip(untagged, 0.0, 1.0, 0.0);
    fade.transform.opacity = 0.5;
    let tl = timeline(vec![vec![fade]], hd_canvas());
    assert_eq!(labelled_layers(&tl, std::slice::from_ref(untagged), 0.5), [false]);
    check(
        "label/untagged-hd-alone-translucent",
        &tl,
        std::slice::from_ref(untagged),
        &[0.5],
        STRICT,
    );
    let (tl, assets) = hd_stack(h.get("detail/hd-untagged"), untagged, 1.0);
    assert_eq!(labelled_layers(&tl, &assets, 0.5), [false, false]);
    check("label/untagged-hd-over-untagged-hd", &tl, &assets, &[0.5], STRICT);

    // Beside a BT.709 clip it is labelled, both are one matrix, and the stack is drawn under either
    // policy: it was a mixed stack the plan refused on a negotiating FFmpeg.
    for (name, base, top, opacity, want) in [
        ("label/untagged-hd-over-709", phone, untagged, 1.0, [false, true]),
        ("label/709-over-untagged-hd", untagged, phone, 1.0, [true, false]),
        ("label/untagged-hd-translucent-over-709", phone, untagged, 0.5, [false, true]),
        ("label/709-translucent-over-untagged-hd", untagged, phone, 0.5, [true, false]),
    ] {
        let (tl, assets) = hd_stack(base, top, opacity);
        assert_eq!(labelled_layers(&tl, &assets, 0.5), want, "{name}");
        eprintln!("{name}: {}", plan_matrices(&tl, &assets));
        check(name, &tl, &assets, &[0.5], STRICT);
    }
    // The graph's decision, not the frame's: a BT.709 clip on another track that is not on screen yet
    // still makes the cut one the export negotiates, and a still of a frame without it does not.
    let late = timeline(
        vec![vec![clip(untagged, 0.0, 1.0, 0.0)], vec![clip(phone, 0.0, 1.0, 1.0)]],
        hd_canvas(),
    );
    let assets = [untagged.clone(), phone.clone()];
    assert_eq!(
        labelled_layers(&late, &assets, 0.5),
        [false],
        "the still holds the layers on screen"
    );
    check("label/untagged-hd-before-709-still", &late, &assets, &[0.5], STRICT);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn footage_that_is_not_untagged_hd_is_left_as_it_is_and_the_other_matrices_stay_mixed() {
    let h = hd_media();
    eprintln!("composite colour policy of this FFmpeg: {:?}", policy());
    let untagged = h.get("grad/hd-untagged");
    let phone = h.get("detail/hd-709");

    // SD footage is BT.601 by the convention, and nothing is labelled beside it: untagged SD with
    // untagged HD is one matrix (BT.601 throughout), drawn.
    for (name, base, top) in [
        (
            "label/sd-untagged-under-untagged-hd",
            "detail/sd-untagged",
            "grad/hd-untagged",
        ),
        (
            "label/untagged-hd-under-sd-untagged",
            "grad/hd-untagged",
            "detail/sd-untagged",
        ),
    ] {
        let (tl, assets) = hd_stack(h.get(base), h.get(top), 1.0);
        assert_eq!(labelled_layers(&tl, &assets, 0.5), [false, false], "{name}");
        check(name, &tl, &assets, &[0.5], STRICT);
    }
    // …and against a BT.709 clip it is the other matrix, as it was: refused where FFmpeg negotiates.
    for (name, base, top) in [
        ("label/sd-untagged-over-709", phone, h.get("detail/sd-untagged")),
        ("label/709-over-sd-untagged", h.get("detail/sd-untagged"), phone),
    ] {
        let (tl, assets) = hd_stack(base, top, 1.0);
        assert!(labelled_layers(&tl, &assets, 0.5).iter().all(|l| !l), "{name}");
        check_mixed(name, &tl, &assets, &[0.5], "different YCbCr matrices");
    }
    // Untagged HD beside footage tagged BT.601 is labelled BT.709 (a stated matrix is a matrix): the
    // stack is mixed now, where FFmpeg read both alike before. It is drawn where the composite is fixed.
    for (name, base, top) in [
        ("label/untagged-hd-over-sd-601", h.get("detail/sd-601"), untagged),
        ("label/untagged-hd-over-hd-601", h.get("detail/hd-601"), untagged),
        ("label/hd-2020-over-untagged-hd", untagged, h.get("detail/hd-2020")),
    ] {
        let (tl, assets) = hd_stack(base, top, 1.0);
        assert!(labelled_layers(&tl, &assets, 0.5).iter().any(|l| *l), "{name}");
        check_mixed(name, &tl, &assets, &[0.5], "different YCbCr matrices");
    }
    // A PNG logo is RGB: never labelled, and it names no YCbCr matrix. At the canvas's own size (an RGB
    // picture is not resized) it is BT.601 with untagged HD, under it or translucent over it: drawn. In a
    // stack that holds a BT.709 clip the HD is labelled, and the logo is an RGB picture in a stack that
    // is not BT.601: refused where the matrix is negotiated.
    let logo = h.get("logo");
    assert_eq!((logo.streams[0].width, logo.streams[0].height), (Some(640), Some(360)));
    let tl = timeline(
        vec![vec![clip(logo, 0.0, 1.0, 0.0)], vec![pip_clip(untagged, 1.0)]],
        hd_canvas(),
    );
    let assets = [logo.clone(), untagged.clone()];
    assert_eq!(labelled_layers(&tl, &assets, 0.5), [false, false]);
    check("label/png-logo-under-untagged-hd", &tl, &assets, &[0.5], STRICT);
    let mut veil = clip(logo, 0.0, 1.0, 0.0);
    veil.transform.opacity = 0.5;
    let tl = timeline(vec![vec![clip(untagged, 0.0, 1.0, 0.0)], vec![veil]], hd_canvas());
    let assets = [untagged.clone(), logo.clone()];
    assert_eq!(labelled_layers(&tl, &assets, 0.5), [false, false]);
    check("label/png-logo-translucent-over-untagged-hd", &tl, &assets, &[0.5], STRICT);
    let tl = timeline(
        vec![
            vec![clip(logo, 0.0, 1.0, 0.0)],
            vec![pip_clip(untagged, 1.0)],
            vec![{
                let mut c = pip_clip(phone, 1.0);
                c.transform.pos_x = -0.25;
                c
            }],
        ],
        hd_canvas(),
    );
    let assets = [logo.clone(), untagged.clone(), phone.clone()];
    assert_eq!(labelled_layers(&tl, &assets, 0.5), [false, true, false]);
    check_mixed(
        "label/png-logo-under-untagged-hd-and-709",
        &tl,
        &assets,
        &[0.5],
        "RGB picture",
    );
}

/// A photo and a webcam clip are tagged `bt470bg` by their decoders and say nothing of the file: a JPEG, a
/// WebP and MJPEG video (probed on both FFmpegs) are no matrix beside untagged HD. The stack is read as BT.601
/// throughout, as it always was, and drawn exactly where the plan drew it before the label.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_decoders_default_tag_labels_nothing_beside_untagged_hd() {
    let h = hd_media();
    eprintln!("composite colour policy of this FFmpeg: {:?}", policy());
    let untagged = h.get("grad/hd-untagged");
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-media-hd");
    let source = "testsrc2=size=640x360:rate=10:duration=1";
    let mut made = 0;
    for (name, encoder, still) in [
        ("photo.jpg", &["-pix_fmt", "yuvj420p"][..], true),
        ("photo.webp", &["-c:v", "libwebp", "-pix_fmt", "yuv420p"][..], true),
        (
            "webcam.mkv",
            &["-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", "yuvj420p"][..],
            false,
        ),
    ] {
        let path = dir.join(name);
        let ok = Command::new(kerf_core::ffmpeg_path())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", source])
            .args(if still { &["-frames:v", "1"][..] } else { &[][..] })
            .args(encoder)
            .arg(&path)
            .stdin(Stdio::null())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("skipped {name}: this FFmpeg cannot make it");
            continue;
        }
        let file = probed(&path);
        let tag = file.streams[0].color_space.clone();
        eprintln!(
            "{name}: probed {} {:?} tag {tag:?}",
            file.streams[0].codec, file.streams[0].pix_fmt
        );
        // Under it (the canvas's own size: nothing is resized), and in a stack of two HD clips.
        let mut base = clip(&file, 0.0, 1.0, 0.0);
        base.source_out = 1.0;
        let tl = timeline(vec![vec![base], vec![pip_clip(untagged, 1.0)]], hd_canvas());
        let assets = [file.clone(), untagged.clone()];
        assert_eq!(
            labelled_layers(&tl, &assets, 0.5),
            [false, false],
            "{name}: its tag is not a matrix"
        );
        check(&format!("label/{name}-under-untagged-hd"), &tl, &assets, &[0.5], STRICT);
        made += 1;
    }
    assert!(made >= 2, "only {made} of the three files could be made");
}

/// FFmpeg's h264 decoder calls every full-range stream `yuvj420p`, and what OBS, a drone or a phone writes with
/// `-color_range pc -colorspace bt709` probes `h264 yuvj420p bt709`: the file's own word, a matrix. `yuvj` is not the
/// decoder, so the untagged HD beside such a clip is labelled BT.709 like any other, and the stack is one matrix.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_full_range_clip_with_a_stated_matrix_labels_the_untagged_hd_beside_it() {
    let h = hd_media();
    eprintln!("composite colour policy of this FFmpeg: {:?}", policy());
    let untagged = h.get("grad/hd-untagged");
    let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("parity-media-hd")
        .join("obs-full-range.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &format!("{DETAIL},format=yuv420p"),
        "-c:v",
        "libx264",
        "-qp",
        "0",
        "-g",
        "10",
        "-pix_fmt",
        "yuv420p",
        "-color_range",
        "pc",
        "-colorspace",
        "bt709",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
        path.to_str().unwrap(),
    ]);
    let obs = probed(&path);
    eprintln!(
        "obs-full-range.mp4: probed {} {:?} tag {:?}",
        obs.streams[0].codec, obs.streams[0].pix_fmt, obs.streams[0].color_space
    );
    assert_eq!(
        obs.streams[0].pix_fmt.as_deref(),
        Some("yuvj420p"),
        "FFmpeg calls a full-range stream yuvj420p"
    );
    assert_eq!(obs.streams[0].color_space.as_deref(), Some("bt709"));
    for (name, base, top) in [
        ("label/untagged-hd-over-full-range-709", &obs, untagged),
        ("label/full-range-709-over-untagged-hd", untagged, &obs),
    ] {
        let (tl, assets) = hd_stack(base, top, 1.0);
        let labels = labelled_layers(&tl, &assets, 0.5);
        assert_eq!(
            labels.iter().filter(|l| **l).count(),
            1,
            "{name}: the untagged HD clip is labelled, {labels:?}"
        );
        assert!(labels[usize::from(top.id == untagged.id)], "{name}");
        eprintln!("{name}: {}", plan_matrices(&tl, &assets));
        check(name, &tl, &assets, &[0.5], STRICT);
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_previews_proxies_stack_as_their_originals_do() {
    let h = hd_media();
    let mut proxies = Proxies::new();
    eprintln!("composite colour policy of this FFmpeg: {:?}", policy());
    // A proxy keeps what its original says: nothing is added to the tags of untagged HD, so a proxy
    // is the picture the original is, and the preview is decided from the same facts as the export.
    let untagged = proxies.of(h.get("grad/hd-untagged"));
    let phone = proxies.of(h.get("detail/hd-709"));
    assert_eq!(
        untagged.streams[0].color_space, None,
        "the untagged HD proxy is as untagged as its source"
    );
    assert_eq!(phone.streams[0].color_space.as_deref(), Some("bt709"));
    for (name, base, top, opacity) in [
        ("proxy/untagged-hd-over-709", &phone, &untagged, 1.0),
        ("proxy/709-over-untagged-hd", &untagged, &phone, 1.0),
        ("proxy/untagged-hd-translucent-over-709", &phone, &untagged, 0.5),
    ] {
        let (tl, assets) = hd_stack(base, top, opacity);
        assert!(labelled_layers(&tl, &assets, 0.5).iter().any(|l| *l), "{name}");
        check(name, &tl, &assets, &[0.5], STRICT);
    }
    // A tone-mapped HLG proxy is tagged BT.709 by the tone-map: it names a matrix beside untagged HD.
    match h.files.get("detail/hlg") {
        Some(hlg) => {
            let tone_mapped = proxies.of(hlg);
            assert_eq!(
                tone_mapped.streams[0].color_space.as_deref(),
                Some("bt709"),
                "the tone-map tags its picture"
            );
            for (name, base, top) in [
                ("proxy/hlg-tone-mapped-over-untagged-hd", &untagged, &tone_mapped),
                ("proxy/untagged-hd-over-hlg-tone-mapped", &tone_mapped, &untagged),
            ] {
                let (tl, assets) = hd_stack(base, top, 1.0);
                assert!(labelled_layers(&tl, &assets, 0.5).iter().any(|l| *l), "{name}");
                check(name, &tl, &assets, &[0.5], STRICT);
            }
        }
        None => eprintln!("skipped: this FFmpeg cannot make the HLG test clip"),
    }
    // An SD proxy is as it was, and beside a BT.709 proxy is refused where the matrix is negotiated.
    let sd = proxies.of(h.get("detail/sd-untagged"));
    let (tl, assets) = hd_stack(&phone, &sd, 1.0);
    check_mixed("proxy/sd-untagged-over-709", &tl, &assets, &[0.5], "different YCbCr matrices");
    let (tl, assets) = hd_stack(&untagged, &sd, 1.0);
    check("proxy/sd-untagged-over-untagged-hd", &tl, &assets, &[0.5], STRICT);
}

// ---- recorded numbers -----------------------------------------------------------
//
// Final limits (the constants at the top; no case relaxes the flat-region ones):
//   flat region  PSNR >= 40 dB, max per-channel error <= 8/255, outside a 2 px
//                band around every >24-level reference step (>12 for a case with a
//                rotated layer), and at least 50% of the frame in the flat region
//   whole image  PSNR >= 40 dB (>= 30 dB for a case with a rotated layer)
//   busy source  (noise, checkerboard) no band: whole image PSNR >= 40 dB, max <= 8
//   scaler       every plane within 1 level of `ffmpeg -vf scale` up to ~4:1; within
//                3 levels (mean <= 0.6) on 8:1 to 20:1 downscales, within 5 (mean
//                <= 1.0) from 32:1 to the 40:1 the plan stops at
//
// One run on Mesa lavapipe (llvmpipe, LLVM 20.1.2, Vulkan) against FFmpeg 6.1.1
// (policy `FixedBt601`). The last column is the same case against the pinned
// FFmpeg 9.0.2 build (what the Windows and macOS bundles ship; policy
// `BottomLayerTag`): `=` is the same figures (within 0.15 dB, same max), otherwise
// "flat PSNR / flat max" there, and `refused` is a stack the plan declines on that
// FFmpeg (mixed matrices, an RGB picture in a non-BT.601 stack, an asset that
// never recorded its pixel format), asserted refused rather than compared. Both
// runs pass every case, strictly (no `KERF_PARITY_EXPLORE`).
// The `text/` rows are the cases with text, judged against the same picture without it
// (whole-image max at most the control's, floor 3; the lower third at 0.1 s +1, the 12 px text and the
// full-range pictures on 9.0 +2, each with its reason at the case); the `(no text)` controls are in the
// report, not here. The `blank-*` rows are frames with no layer (drawn in RGB: exact).
// The smallest flat share of any non-busy case is 56.9% (`grid/yuv420p-cover-odd-y-then-
// scale`, a 202x100 frame, band 43.1%; the other `grid/*-cover-odd-y` cases 59.4%); the
// largest band among the cases judged strictly is that one. Every run rewrites `target/parity/report.txt` (these columns plus the
// decode / composite time of the GPU path and the scaler lines).
//
// case                                             t    canvas | flat PSNR flat max |  all PSNR  all max |  band 9.0.2
// busy/checker-to-384x216                        0.1   384x216 |    48.2 dB        2 |    48.2 dB        2 |  0.0%    =
// busy/checker-to-480x270                        0.1   480x270 |    49.4 dB        2 |    49.4 dB        2 |  0.0%    =
// busy/checker-to-960x540                        0.1   960x540 |    99.0 dB        0 |    48.8 dB        2 | 100.0%    =
// busy/noise-scaled-0.7                          0.1   640x360 |    46.5 dB        3 |    45.9 dB        5 | 50.8%    =
// busy/noise-to-384x216                          0.1   384x216 |    99.0 dB        0 |    45.8 dB        5 | 100.0%    =
// busy/noise-to-480x270                          0.1   480x270 |    99.0 dB        0 |    45.8 dB        5 | 100.0%    =
// busy/noise-to-960x540                          0.1   960x540 |    99.0 dB        0 |    46.3 dB        5 | 100.0%    =
// clamped-end/nothing-drawn                     1.99   640x360 |    46.6 dB        3 |    46.6 dB        3 |  0.0%    =
// clamped-end/nothing-drawn                        3   640x360 |    46.6 dB        3 |    46.6 dB        3 |  0.0%    =
// colour/brightness+contrast+saturation+gamma     0.5   640x360 |    48.8 dB        3 |    48.6 dB        3 | 15.4%    =
// colour/contrast+saturation-only                0.5   640x360 |    47.9 dB        3 |    47.9 dB        3 |  9.9%    =
// colour/cool+brightness                         0.5   640x360 |    47.6 dB        3 |    47.6 dB        3 | 15.5%    =
// colour/on-a-transformed-clip                   0.5   640x360 |    48.9 dB        6 |    48.9 dB        6 | 15.4%    =
// colour/warm                                    0.5   640x360 |    45.6 dB        3 |    45.6 dB        3 |  0.0%    =
// contain/9x16                                   0.5   360x640 |    55.0 dB        4 |    53.6 dB        5 | 10.6%    =
// contain/9x16                                   1.2   360x640 |    55.0 dB        4 |    53.6 dB        5 | 10.6%    =
// contain/downscale-320x180                      0.5   320x180 |    48.9 dB        4 |    48.4 dB        5 | 29.4%    =
// contain/portrait-in-16x9                       0.7   640x360 |    55.1 dB        3 |    53.5 dB        5 | 11.6%    =
// contain/upscale-960x540                        0.5   960x540 |    48.8 dB        5 |    48.5 dB        5 | 14.3%    =
// cover/9x16                                     0.5   360x640 |    46.4 dB        5 |    46.5 dB        5 |  7.4%    =
// cover/9x16                                     1.2   360x640 |    46.3 dB        4 |    46.4 dB        5 |  6.5%    =
// cover/portrait-in-16x9                         0.7   640x360 |    49.0 dB        5 |    48.8 dB        5 | 11.5%    =
// enlarge-ok/gray-fit-to-960x540                 0.5   960x540 |    51.2 dB        2 |    51.0 dB        2 | 10.4%    =
// enlarge-ok/gray-x1.5                           0.5   640x360 |    51.2 dB        2 |    50.9 dB        2 | 13.2%    =
// enlarge-ok/yuv420p-fit-to-960x540              0.5   960x540 |    48.8 dB        5 |    48.5 dB        5 | 14.3%    =
// enlarge-ok/yuv420p-x1.5                        0.5   640x360 |    47.7 dB        5 |    47.5 dB        5 | 17.7%    =
// enlarge-ok/yuv420p10le-fit-to-960x540          0.5   960x540 |    47.8 dB        5 |    47.4 dB        5 | 14.3%    =
// enlarge-ok/yuv420p10le-x1.5                    0.5   640x360 |    46.7 dB        5 |    46.4 dB        5 | 17.8%    =
// enlarge-ok/yuvj420p-jpeg-fit-to-960x540        0.5   960x540 |    99.0 dB        0 |    99.0 dB        0 |  0.0%    =
// enlarge-ok/yuvj420p-jpeg-x1.5                  0.5   480x270 |    99.0 dB        0 |    99.0 dB        0 |  0.0%    =
// full-range/limited-range-control-graded        0.5   640x360 |    48.3 dB        3 |    48.2 dB        3 | 15.6%    =
// full-range/ungraded                            0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 15.6% 52.8 / 3
// gap                                            1.5   640x360 |    99.0 dB        0 |    99.0 dB        0 |  0.0%    =
// geometry/crop-then-cover                       0.5   360x640 |    48.2 dB        5 |    48.0 dB        5 | 13.0%    =
// geometry/fully-off-canvas                      0.5   640x360 |    46.3 dB        3 |    46.3 dB        3 |  0.0%    =
// geometry/odd-rotated-box                       0.5   640x360 |    46.4 dB        3 |    36.9 dB      254 |  7.1%    =
// geometry/odd-sized-layer                       0.5   640x360 |    46.3 dB        3 |    46.4 dB        5 |  5.5%    =
// geometry/zoom-off-canvas                       0.5   640x360 |    48.1 dB        4 |    48.0 dB        5 |  9.5%    =
// grid/bgr0-pad-odd-x                            0.5   646x360 |    48.4 dB        3 |    48.3 dB        3 | 17.2%    =
// grid/bgr0-pad-odd-y                            0.5   640x366 |    48.4 dB        3 |    48.4 dB        3 | 18.2%    =
// grid/gray-cover-odd-x                          0.5    90x162 |    47.6 dB        4 |    47.4 dB        4 | 18.1%    =
// grid/gray-cover-odd-x-then-scale               0.5    90x162 |    53.2 dB        2 |    52.3 dB        2 | 28.5%    =
// grid/gray-cover-odd-y                          0.5   202x100 |    49.0 dB        4 |    48.6 dB        4 | 31.5%    =
// grid/gray-cover-odd-y-then-scale               0.5   202x100 |    54.0 dB        2 |    52.3 dB        2 | 36.2%    =
// grid/gray-even-crop                            0.5   320x180 |    51.9 dB        3 |    51.3 dB        4 | 24.8%    =
// grid/gray-letterbox-9x16                       0.5   360x640 |    57.2 dB        2 |    56.1 dB        2 |  8.4%    =
// grid/gray-odd-crop                             0.5   320x180 |    51.3 dB        2 |    51.0 dB        2 | 25.4%    =
// grid/gray-odd-crop-behind-a-pip                0.5   320x180 |    51.2 dB        3 |    50.6 dB        5 | 31.1%    =
// grid/gray-odd-crop-cover                       0.5    90x162 |    51.1 dB        3 |    50.7 dB        4 | 18.8%    =
// grid/gray-odd-x-crop                           0.5   320x180 |    51.2 dB        3 |    50.8 dB        4 | 23.1%    =
// grid/gray-odd-y-crop                           0.5   320x180 |    50.4 dB        4 |    50.1 dB        4 | 25.0%    =
// grid/gray-pad-odd-x                            0.5   646x360 |    51.4 dB        1 |    51.3 dB        1 | 15.1%    =
// grid/gray-pad-odd-y                            0.5   640x366 |    51.4 dB        1 |    51.3 dB        1 | 16.2%    =
// grid/rgb24-png-pad-odd-x                       0.5   646x360 |    48.3 dB        3 |    48.3 dB        3 | 15.9%    =
// grid/rgb24-png-pad-odd-y                       0.5   640x366 |    48.4 dB        3 |    48.3 dB        3 | 16.9%    =
// grid/yuv420p-cover-odd-x                       0.5    90x162 |    46.4 dB        3 |    46.5 dB        4 | 23.4%    =
// grid/yuv420p-cover-odd-x-then-scale            0.5    90x162 |    48.6 dB        3 |    48.7 dB        4 | 31.5%    =
// grid/yuv420p-cover-odd-y                       0.5   202x100 |    49.0 dB        3 |    48.6 dB        5 | 40.6%    =
// grid/yuv420p-cover-odd-y-then-scale            0.5   202x100 |    52.0 dB        3 |    50.3 dB        4 | 43.1%    =
// grid/yuv420p-even-crop                         0.5   320x180 |    48.7 dB        4 |    48.5 dB        5 | 32.3%    =
// grid/yuv420p-letterbox-9x16                    0.5   360x640 |    55.0 dB        4 |    53.6 dB        5 | 10.6%    =
// grid/yuv420p-odd-crop                          0.5   320x180 |    48.8 dB        3 |    48.5 dB        5 | 32.2%    =
// grid/yuv420p-odd-crop-behind-a-pip             0.5   320x180 |    49.1 dB        5 |    48.4 dB        5 | 30.0%    =
// grid/yuv420p-odd-crop-cover                    0.5    90x162 |    46.5 dB        3 |    46.8 dB        4 | 22.4%    =
// grid/yuv420p-odd-x-crop                        0.5   320x180 |    48.8 dB        4 |    48.5 dB        5 | 31.1%    =
// grid/yuv420p-odd-y-crop                        0.5   320x180 |    48.8 dB        4 |    48.3 dB        5 | 31.8%    =
// grid/yuv420p-pad-odd-x                         0.5   646x360 |    48.8 dB        3 |    48.8 dB        3 | 17.1%    =
// grid/yuv420p-pad-odd-y                         0.5   640x366 |    48.9 dB        3 |    48.8 dB        3 | 18.2%    =
// grid/yuv422p-pad-odd-x                         0.5   646x360 |    48.9 dB        3 |    48.8 dB        3 | 17.7%    =
// grid/yuv422p-pad-odd-y                         0.5   640x366 |    48.9 dB        3 |    48.8 dB        3 | 18.8%    =
// grid/yuv444p-pad-odd-x                         0.5   646x360 |    48.9 dB        3 |    48.7 dB        3 | 19.4%    =
// grid/yuv444p-pad-odd-y                         0.5   640x366 |    48.9 dB        3 |    48.8 dB        3 | 20.4%    =
// letterbox/graded-over-gradient                 0.5   640x360 |    65.6 dB        3 |    59.5 dB        6 | 10.9%    =
// letterbox/graded-single-clip                   0.5   640x360 |    54.8 dB        3 |    53.0 dB        5 | 12.1%    =
// letterbox/landscape-over-portrait-9x16         0.5   360x640 |    55.8 dB        3 |    54.4 dB        5 |  8.3%    =
// letterbox/odd-fit-over-gradient                0.5   360x640 |    55.0 dB        4 |    53.6 dB        5 | 10.6%    =
// letterbox/portrait-over-landscape              0.5   640x360 |    55.2 dB        3 |    53.5 dB        5 | 12.0%    =
// matrix/bt2020-bars                             0.5   640x360 |    48.8 dB        3 |    48.8 dB        3 |  7.8% 47.9 / 2
// matrix/bt2020-layered                          0.5   640x360 |    46.7 dB        4 |    46.8 dB        5 |  9.7% 46.2 / 4
// matrix/bt2020-testsrc2                         0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.7%    =
// matrix/bt709-bars                              0.5   640x360 |    49.8 dB        2 |    49.7 dB        2 | 10.4% 49.9 / 3
// matrix/bt709-gradient                          0.5   640x360 |    46.3 dB        3 |    46.3 dB        3 |  0.0%    =
// matrix/bt709-layered                           0.5   640x360 |    46.7 dB        4 |    46.8 dB        5 |  9.7% 46.9 / 4
// matrix/bt709-testsrc2                          0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.7%    =
// matrix/mixed-2020-over-709                     0.5   640x360 |    46.7 dB        4 |    46.8 dB        5 |  9.7% refused
// matrix/mixed-709-over-2020                     0.5   640x360 |    46.7 dB        4 |    46.8 dB        5 |  9.7% refused
// matrix/mixed-709-over-untagged                 0.5   640x360 |    46.7 dB        4 |    46.8 dB        5 |  9.7% refused
// matrix/mixed-untagged-over-709                 0.5   640x360 |    46.7 dB        4 |    46.8 dB        5 |  9.7% refused
// matrix/rgb-png-under-709                       0.5   640x360 |    47.1 dB        5 |    47.1 dB        5 | 12.8% refused
// matrix/rgb-png-under-untagged                  0.5   640x360 |    47.1 dB        5 |    47.1 dB        5 | 12.8%    =
// old-asset/opaque                               0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.7% refused
// opacity/0.3+scale+rotate                       0.5   640x360 |    46.6 dB        5 |    43.5 dB       66 | 14.2%    =
// opacity/0.5                                      0   640x360 |    43.4 dB        4 |    43.5 dB        5 | 21.8%    =
// opacity/0.5                                      1   640x360 |    43.2 dB        5 |    43.3 dB        5 | 22.4%    =
// opacity/bars-0.5-over-testsrc2                 0.5   640x360 |    44.5 dB        5 |    44.5 dB        5 | 22.9%    =
// opacity/bars-0.65-alone                        0.5   640x360 |    45.0 dB        5 |    44.9 dB        5 |  9.8%    =
// opacity/bt2020-bars-0.6                        0.5   640x360 |    47.9 dB        4 |    47.8 dB        4 |  7.8% 46.2 / 4
// opacity/keyframed-fade+grade                   0.5   640x360 |    46.5 dB        5 |    46.4 dB        5 | 14.8%    =
// opacity/odd-source-cropped-0.7                 0.5   640x360 |    43.7 dB        6 |    43.8 dB        6 | 28.7%    =
// opacity/testsrc2-0.65+brightness0.2            0.5   640x360 |    42.1 dB        5 |    42.1 dB        5 | 14.9%    =
// opacity/testsrc2-0.65+saturation1.9            0.5   640x360 |    45.6 dB        5 |    45.3 dB        5 | 14.9%    =
// pip/cover-then-scale                           0.5   360x640 |    45.7 dB        5 |    45.8 dB        5 |  6.3%    =
// pip/odd-361x203-in-722x640                     0.5   722x640 |    48.4 dB        5 |    48.3 dB        5 |  7.7%    =
// pip/scaled+offset                                0   640x360 |    49.4 dB        3 |    49.3 dB        5 | 15.9%    =
// pip/scaled+offset                                1   640x360 |    49.4 dB        5 |    49.3 dB        5 | 16.0%    =
// range/graded-limited-over-full                 0.5   640x360 |    48.5 dB        4 |    48.3 dB        5 | 21.1% refused
// range/graded-translucent-png-over-full         0.5   640x360 |    44.8 dB        5 |    44.8 dB        5 | 18.6% refused
// same-size/bgr0                                 0.5   640x360 |    48.4 dB        3 |    48.3 dB        3 | 15.7%    =
// same-size/rgb24-png                            0.5   480x270 |    48.4 dB        3 |    48.2 dB        3 | 17.7%    =
// same-size/yuv422p                              0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 16.3%    =
// same-size/yuv444p                              0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 17.9%    =
// shrink-ok/gray-fit-to-576x324                  0.5   576x324 |    51.4 dB        2 |    51.1 dB        2 | 15.2%    =
// shrink-ok/yuv420p-fit-to-576x324               0.5   576x324 |    48.9 dB        4 |    48.7 dB        5 | 18.9%    =
// single/gradient                                  0   640x360 |    46.3 dB        3 |    46.3 dB        3 |  0.0%    =
// single/gradient                                  1   640x360 |    46.3 dB        3 |    46.3 dB        3 |  0.0%    =
// single/smptehdbars                               0   640x360 |    49.8 dB        2 |    49.7 dB        2 | 10.4%    =
// single/smptehdbars                               1   640x360 |    49.8 dB        2 |    49.7 dB        2 | 10.4%    =
// single/testsrc2                                  0   640x360 |    48.8 dB        3 |    48.7 dB        3 | 14.3%    =
// single/testsrc2                                0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.7%    =
// single/testsrc2                             1.2345   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.8%    =
// source/10-bit                                  0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.7%    =
// source/4:4:4                                   0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 17.9%    =
// source/full-range-jpeg                         0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 15.6% 52.8 / 3
// source/odd-641x361                             0.5   640x360 |    48.8 dB        4 |    48.5 dB        5 | 19.0%    =
// source/rotated-by-metadata                     0.5   360x640 |    48.9 dB        3 |    48.8 dB        3 | 15.7%    =
// still/alone                                      0   480x270 |    48.4 dB        3 |    48.2 dB        3 | 17.7%    =
// still/alone                                    2.5   480x270 |    48.4 dB        3 |    48.2 dB        3 | 17.7%    =
// still/jpeg                                       1   480x270 |    48.8 dB        3 |    48.6 dB        3 | 17.7% 52.7 / 3
// still/pip-over-video                             1   640x360 |    46.4 dB        5 |    45.6 dB       17 |  9.4% 48.0 / 5
// still/png-under-video-pip                        1   640x360 |    47.6 dB        5 |    47.6 dB        5 | 13.5%    =
// text/blank-fading                              1.4   640x360 |    99.0 dB        0 |    99.0 dB        0 |  1.9%    =
// text/blank-fading                              2.2   640x360 |    99.0 dB        0 |    99.0 dB        0 |  1.9%    =
// text/blank-hex-on-a-solid-box                  1.5   640x360 |    99.0 dB        0 |    99.0 dB        0 |  4.8%    =
// text/blank-white                               1.5   640x360 |    99.0 dB        0 |    99.0 dB        0 |  2.9%    =
// text/blank-yellow-boxed                        1.5   640x360 |    99.0 dB        0 |    99.0 dB        0 |  2.8%    =
// text/caption                                   0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 16.9%    =
// text/caption-9x16                              0.5   360x640 |    46.3 dB        5 |    46.4 dB        5 |  9.7%    =
// text/captions-lines                            0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 17.9%    =
// text/captions-lines-9x16                       0.5   360x640 |    46.3 dB        5 |    46.4 dB        5 | 11.4%    =
// text/captions-word-punch                       0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 18.4%    =
// text/captions-word-punch-9x16                  0.5   360x640 |    46.3 dB        5 |    46.4 dB        5 | 12.6%    =
// text/colour-hex                                0.5   640x360 |    49.0 dB        3 |    48.8 dB        3 | 17.8%    =
// text/colour-invalid                            0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 16.8%    =
// text/colour-translucent                        0.5   640x360 |    49.0 dB        3 |    48.8 dB        3 | 17.8%    =
// text/colour-translucent-box                    0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 17.8%    =
// text/colour-yellow                             0.5   640x360 |    48.8 dB        3 |    48.8 dB        3 | 16.2%    =
// text/edge-bottom                               0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 16.8%    =
// text/edge-bottom-boxed                         0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 17.8%    =
// text/edge-corner                               0.5   640x360 |    48.8 dB        3 |    48.8 dB        3 | 16.3%    =
// text/edge-corner-boxed                         0.5   640x360 |    48.8 dB        3 |    48.7 dB        3 | 16.7%    =
// text/edge-left                                 0.5   640x360 |    48.8 dB        3 |    48.7 dB        3 | 16.8%    =
// text/edge-left-boxed                           0.5   640x360 |    48.8 dB        3 |    48.7 dB        3 | 17.7%    =
// text/edge-outside                              0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.7%    =
// text/edge-outside-boxed                        0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.7%    =
// text/edge-right                                0.5   640x360 |    48.8 dB        3 |    48.8 dB        3 | 16.8%    =
// text/edge-right-boxed                          0.5   640x360 |    48.7 dB        3 |    48.6 dB        3 | 17.7%    =
// text/edge-top                                  0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 16.7%    =
// text/edge-top-boxed                            0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 17.6%    =
// text/escapes-0                                 0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 17.5%    =
// text/escapes-1                                 0.5   640x360 |    48.9 dB        3 |    48.6 dB        3 | 19.0%    =
// text/escapes-2                                 0.5   640x360 |    48.9 dB        3 |    48.6 dB        3 | 18.7%    =
// text/escapes-3                                 0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 18.4%    =
// text/escapes-4                                 0.5   640x360 |    48.8 dB        3 |    48.6 dB        3 | 19.4%    =
// text/escapes-5                                 0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 17.8%    =
// text/escapes-6                                 0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 18.2%    =
// text/escapes-7                                 0.5   640x360 |    48.9 dB        3 |    48.6 dB        3 | 18.8%    =
// text/escapes-8                                 0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 17.5%    =
// text/escapes-9                                 0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.7%    =
// text/font-DejaVu-Sans                          0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 18.0%    =
// text/font-Inter                                0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 17.6%    =
// text/font-Liberation-Sans                      0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 17.6%    =
// text/font-not-installed                        0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 17.5%    =
// text/keyframed                                   0   640x360 |    48.8 dB        3 |    48.7 dB        3 | 14.3%    =
// text/keyframed                                0.37   640x360 |    48.9 dB        3 |    48.8 dB        3 | 16.3%    =
// text/keyframed                                 0.8   640x360 |    48.9 dB        3 |    48.8 dB        3 | 16.0%    =
// text/keyframed                              1.2345   640x360 |    48.8 dB        3 |    48.7 dB        3 | 16.8%    =
// text/keyframed                                 1.9   640x360 |    48.8 dB        3 |    48.7 dB        3 | 16.0%    =
// text/lower-third                               0.1   640x360 |    48.8 dB        3 |    48.7 dB        4 | 16.0%    =
// text/lower-third                              0.35   640x360 |    48.8 dB        3 |    48.7 dB        3 | 17.0%    =
// text/lower-third                                 1   640x360 |    48.8 dB        3 |    48.7 dB        3 | 16.5%    =
// text/lower-third                               1.8   640x360 |    48.8 dB        3 |    48.7 dB        3 | 16.3%    =
// text/lower-third-9x16                          0.1   360x640 |    46.4 dB        4 |    46.4 dB        5 |  9.7%    =
// text/odd-canvas-202x100                        0.5   202x100 |    49.1 dB        3 |    48.3 dB        5 | 45.5%    =
// text/over-an-empty-layer                         3   640x360 |    99.0 dB        0 |    63.3 dB        3 |  3.8%    =
// text/over-bt2020                               0.5   640x360 |    49.0 dB        3 |    48.9 dB        3 | 11.1% 47.9 / 2
// text/over-bt709                                0.5   640x360 |    49.8 dB        3 |    49.5 dB        3 | 13.6% 50.0 / 3
// text/over-full-range-jpeg-still                0.5   480x270 |    48.8 dB        3 |    48.3 dB        3 | 24.8% 52.7 / 3
// text/over-full-range-mjpeg                     0.5   640x360 |    48.8 dB        3 |    48.5 dB        3 | 20.7% 52.9 / 3
// text/over-graded                               0.5   640x360 |    48.1 dB        3 |    47.9 dB        3 | 17.0%    =
// text/over-gradient-709                         0.5   640x360 |    46.3 dB        3 |    46.3 dB        3 |  3.5% 46.5 / 3
// text/over-letterbox-9x16                       0.5   360x640 |    55.3 dB        3 |    52.8 dB        5 | 15.6%    =
// text/over-translucent                          0.5   640x360 |    43.8 dB        5 |    43.9 dB        5 | 12.6%    =
// text/over-untagged                             0.5   640x360 |    49.8 dB        3 |    49.5 dB        3 | 13.6%    =
// text/overlapping                               0.2   640x360 |    49.2 dB        3 |    49.1 dB        3 | 19.3%    =
// text/overlapping                                 1   640x360 |    49.1 dB        3 |    48.9 dB        3 | 20.4%    =
// text/size-0.034                                0.5   640x360 |    48.9 dB        3 |    48.8 dB        5 | 16.1%    =
// text/size-0.06                                 0.5   640x360 |    49.0 dB        3 |    48.8 dB        3 | 16.6%    =
// text/size-0.12                                 0.5   640x360 |    49.0 dB        3 |    48.8 dB        3 | 18.3%    =
// text/size-0.2                                  0.5   640x360 |    48.9 dB        3 |    48.9 dB        3 | 20.8%    =
// text/title                                       0   640x360 |    48.8 dB        3 |    48.7 dB        3 | 14.3%    =
// text/title                                    0.25   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.9%    =
// text/title                                     0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 16.4%    =
// text/title                                       1   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.9%    =
// text/title                                     1.7   640x360 |    48.9 dB        3 |    48.7 dB        3 | 16.0%    =
// text/title                                    1.99   640x360 |    74.4 dB        1 |    74.4 dB        1 |  0.0%    =
// text/title-9x16                                  0   360x640 |    46.3 dB        4 |    46.4 dB        5 |  7.1%    =
// time/keyframes                                 0.5   640x360 |    48.8 dB        3 |    48.7 dB        3 | 14.3%    =
// time/keyframes                                1.25   640x360 |    50.3 dB        4 |    49.9 dB        5 | 18.4%    =
// time/keyframes                                   2   640x360 |    56.2 dB        4 |    54.3 dB        5 |  9.5%    =
// time/keyframes+rotation+opacity                0.5   640x360 |    50.6 dB        4 |    50.1 dB        5 | 16.3%    =
// time/keyframes+rotation+opacity               1.25   640x360 |    48.8 dB        5 |    35.4 dB      206 | 19.6%    =
// time/keyframes+rotation+opacity                  2   640x360 |    56.6 dB        5 |    39.3 dB      132 | 11.1%    =
// time/reversed                                  0.3   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.3%    =
// time/reversed                                  1.1   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.1%    =
// time/speed-2x-offset                             1   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.3%    =
// time/speed-2x-offset                           1.4   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.2%    =
// time/speed-2x-offset                           1.7   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.4%    =
// tracks/late-top-clip                           0.5   640x360 |    46.3 dB        3 |    46.3 dB        3 |  0.0%    =
// tracks/late-top-clip                           1.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.7%    =
// transform/crop-only                            0.5   640x360 |    47.8 dB        4 |    47.5 dB        5 | 19.3%    =
// transform/rotate-90                            0.5   640x360 |    47.2 dB        3 |    47.3 dB        3 |  8.2%    =
// transform/scale+rotate+crop                    0.5   640x360 |    47.0 dB        6 |    33.3 dB      255 | 17.1%    =
// transform/scale+rotate+crop                    1.5   640x360 |    46.9 dB        6 |    33.4 dB      255 | 17.1%    =
// translucent/bt2020-bars-0.6-alone              0.5   640x360 |    47.9 dB        4 |    47.8 dB        4 |  7.8% 46.2 / 4
// translucent/bt2020-over-bt2020                 0.5   640x360 |    44.0 dB        5 |    44.1 dB        5 |  7.8% 42.4 / 5
// translucent/bt709-bars-0.5-alone               0.5   640x360 |    50.0 dB        3 |    49.7 dB        4 |  9.7% 50.5 / 3
// translucent/bt709-over-bt709                   0.5   640x360 |    45.3 dB        5 |    45.2 dB        5 | 22.8% 44.0 / 5
// translucent/bt709-over-untagged                0.5   640x360 |    45.3 dB        5 |    45.2 dB        5 | 22.8% refused
// translucent/bt709-testsrc2-0.65-alone          0.5   640x360 |    43.1 dB        5 |    42.9 dB        5 | 15.3% 40.9 / 5
// translucent/full-range-mjpeg                   0.5   640x360 |    48.2 dB        5 |    47.7 dB        5 | 15.6% 48.5 / 6
// translucent/gray                               0.5   640x360 |    43.8 dB        5 |    43.8 dB        5 | 15.2%    =
// translucent/jpeg-still                         0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.7%    =
// translucent/rgb-png-still                      0.5   640x360 |    49.6 dB        5 |    48.8 dB        6 | 18.7%    =
// translucent/untagged-over-bt709                0.5   640x360 |    43.4 dB        4 |    43.4 dB        5 | 22.8% refused
// translucent/yuv420p10le                        0.5   640x360 |    48.3 dB        5 |    47.7 dB        5 | 15.6%    =
// translucent/yuv422p                            0.5   640x360 |    48.4 dB        5 |    47.8 dB        5 | 16.2%    =
// translucent/yuv444p                            0.5   640x360 |    48.4 dB        5 |    47.6 dB        5 | 16.4%    =
// unknown/untagged-bars                          0.5   640x360 |    49.8 dB        2 |    49.7 dB        2 | 10.4%    =
// unknown/untagged-translucent-pip               0.5   640x360 |    49.0 dB        4 |    48.2 dB        5 | 17.9%    =
// scaler (plane by plane vs `ffmpeg -vf scale`; worst plane per source, FFmpeg 6.1.1;
// every source is at most 1 level off up to ~4:1, the 2-5 are the 8:1 to 40:1 shrinks)
//   bars       max 2  worst mean 0.493
//   checker    max 1  worst mean 0.572
//   gradient   max 2  worst mean 0.191
//   noise      max 1  worst mean 0.250
//   noise-hd   max 5  worst mean 0.792   (1280x720 down to 32x18, 40:1)
//   testsrc2   max 3  worst mean 0.521   (down to 32x18, 20:1)
// ... FFmpeg 9.0.2: the same maxima, worst means 0.493 / 0.572 / 0.191 / 0.227 / 0.833 / 0.521.
