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
//! * **A moving zoom with a rotation, a grade or a fade of opacity in front of it** is
//!   refused (`refused/moving-zoom-behind-*`): FFmpeg, the still and the export alike, runs a
//!   moving zoom as the *last* stage of the clip's chain, so those act on the picture at its
//!   fit size and are magnified with it, an order the compositor does not have. The same
//!   keys with nothing in front of the zoom are drawn (`time/keyframes`, a zoom and a
//!   position moving), as are a rotation and an opacity moving over a zoom that holds
//!   (`time/keyframes+rotation+opacity`).
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

// ---- the check ----------------------------------------------------------------

fn report() -> &'static Mutex<BTreeMap<String, String>> {
    static R: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Render `tl` at each of `times` through both renderers and hold them to
/// `limits`. Returns the failures (empty when every time passes) after printing
/// a line per time.
fn check(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64], limits: Limits) {
    check_with(case, tl, assets, times, limits, &ExportOptions::default());
}

/// [`check`] for a render with options (the frame rate the plan counts frames in, say).
fn check_with(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64], limits: Limits, opts: &ExportOptions) {
    let out_dir = target_dir().join("parity");
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    // A file-name-safe form of the case name (`pip/scaled+offset`).
    let slug: String = case
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let mut failures = Vec::new();
    for &t in times {
        let plan = RenderPlan::at(tl, assets, opts, t, policy()).expect("plan");
        let size = plan.size(u32::MAX);
        let why = plan.unsupported_reasons_at(size);
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
        let shared = frame_source()
            .frames(&plan.layers, kerf_gpu::Hint::Scrub)
            .unwrap_or_else(|e| panic!("{case} @ {t}: the frame source: {e}"));
        let oneshot = kerf_gpu::decode_layers(&plan.layers).expect("decode");
        for (n, (a, b)) in shared.iter().zip(&oneshot).enumerate() {
            assert!(
                a.as_deref() == b.as_ref(),
                "{case} @ {t}: layer {n}: the frame source's frame is not the one-shot decode's"
            );
        }
        let gpu_rgb = rgba_to_rgb(&frame.data);

        let (m, band) = compare(&reference, &gpu_rgb, w, h, limits.edge_step);
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
        return;
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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
    let opts = ExportOptions::default();
    let plan = RenderPlan::at(tl, assets, &opts, t, policy()).expect("plan");
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    let png = scratch.join(format!("refused-{}.png", case.replace('/', "_")));
    export_still(tl, assets, &opts, t, &png, ImageFormat::Png, 0).expect("the FFmpeg fallback renders it");
    let _ = std::fs::remove_file(&png);
    match expect {
        Refusal::Plan(reason) => {
            let reasons = plan.unsupported_reasons_at(plan.size(u32::MAX));
            assert!(!reasons.is_empty(), "{case}: the plan should refuse");
            let reasons = reasons.join("; ");
            assert!(reasons.contains(reason), "{case}: {reasons:?} does not say {reason:?}");
        }
        Refusal::Render(reason) => {
            let why = plan.unsupported_reasons_at(plan.size(u32::MAX));
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
    // A moving zoom with a rotation, a grade or a fade of opacity in front of it: FFmpeg (the
    // still and the export alike) runs the zoom last, so those act on the picture at its fit
    // size and are magnified with it, an order the compositor does not have. The same keys
    // at an instant where all of them are neutral are drawn (`time/keyframes`).
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
    let mut graded = clip(&m.testsrc, 0.0, 2.0, 0.5);
    graded.keyframes = vec![key(0.0, 1.0, 0.0, 1.0), key(1.5, 0.5, 0.0, 1.0)];
    graded.color.contrast = 1.3;
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![graded]], None);
    check_refused(
        "refused/moving-zoom-behind-a-grade",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        1.25,
        Refusal::Plan("moving zoom"),
    );
}

/// One transition cut at 30 fps — an outgoing clip for a second, then `testsrc2` from 1.0 s —
/// through `kind` over `secs`, the incoming clip `scale` of the frame (1 covers it, 0.5 leaves a
/// border).
///
/// What FFmpeg's still draws is the frame each *clip's own span* contributes: no fades, and no
/// clip past its end. What the **export** draws is what plays, and a transition differs from the
/// still in more places than where it is visibly mid-way: the outgoing clip's `enable` window
/// runs on after the ramp (or the travel) is over, and it is drawn around an incoming clip that
/// does not cover it. So the contract tested here is the one that matters — for every frame
/// listed, either the plan refuses it, or the still it draws like is the export's frame (and the
/// GPU matches that still strictly) — plus the expectations written out per frame: before the
/// transition and after it frames are drawn, inside it (and on the frame after the ramp where
/// only the tail is left) they are refused, and the second half of a dip, which has neither a
/// tail nor a fade left, is drawn. Both FFmpegs.
///
/// A dissolve of 0.61 s is chosen so that a frame with *only* the tail left exists at 30 fps:
/// the fade counts 18 frames (it is over at frame 48) and the window covers 18.3 of them. (A
/// slide or a push cannot part from the still there: its travel ends when its window does, and
/// an equal-rate outgoing clip has no frame at the very end; those frames are refused anyway.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn frames_inside_a_transition_are_refused_and_the_ones_around_it_are_drawn() {
    let m = media();
    // The export runs at 30 fps whatever the outgoing clip's own rate is (it would take the first clip's).
    let opts = ExportOptions {
        fps: Some(30.0),
        ..ExportOptions::default()
    };
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    // Per kind: the length, the frames to look at as `(frame, refused)`, and the frame on which
    // only the outgoing clip's tail is left.
    type Frames = &'static [(u64, bool)];
    let motion: Frames = &[
        (15, false),
        (29, false),
        (30, true),
        (37, true),
        (44, true),
        (45, true),
        (46, false),
        (55, false),
    ];
    let cases: [(&str, TransitionKind, f64, Frames, Option<u64>); 4] = [
        (
            "dissolve",
            TransitionKind::Crossfade,
            0.61,
            &[
                (15, false),
                (29, false),
                (30, true),
                (38, true),
                (47, true),
                (48, true),
                (49, false),
                (55, false),
            ],
            Some(48),
        ),
        ("slide", TransitionKind::SlideLeft, 0.5, motion, None),
        ("push", TransitionKind::PushUp, 0.5, motion, None),
        (
            "dip",
            TransitionKind::DipToBlack,
            0.8,
            &[
                (15, false),
                (18, false),
                (19, true),
                (29, true),
                (30, true),
                (41, true),
                (42, false),
                (43, false),
                (55, false),
            ],
            None,
        ),
    ];
    let mut parted = 0;
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
            for &(k, refused) in frames {
                let case = format!("transition/{name}-{cover}");
                let t = k as f64 / 30.0;
                let plan = RenderPlan::at(&tl, &assets, &opts, t, policy()).expect("plan");
                let size = plan.size(u32::MAX);
                let (w, h) = (size.0 as usize, size.1 as usize);
                assert_eq!(plan.frame, k as i64);
                let why = plan.unsupported_reasons_at(size);
                assert_eq!(!why.is_empty(), refused, "{case}: frame {k}: {why:?}");
                // The still the plan stands for, against the frame that plays.
                let png = scratch.join(format!("transition-{name}-{cover}-{k}.png"));
                export_still(&tl, &assets, &opts, t, &png, ImageFormat::Png, 0).expect("FFmpeg still");
                let still = ffmpeg(&["-i", png.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
                let _ = std::fs::remove_file(&png);
                let export = &played[k as usize * w * h * 3..(k as usize + 1) * w * h * 3];
                let (apart, _) = compare(export, &still, w, h, EDGE_STEP);
                eprintln!(
                    "{case:<28} frame {k:<3} {} still vs export {:>5.1} dB",
                    if refused { "refused" } else { "drawn  " },
                    apart.psnr_all
                );
                if refused {
                    // Only the tail is left on this frame; a border around the incoming clip shows it.
                    if scale < 1.0 && tail_only == Some(k) {
                        assert!(
                            apart.psnr_all < 25.0,
                            "{case}: frame {k} is refused for a tail the export draws, yet the still agrees ({:.1} dB)",
                            apart.psnr_all
                        );
                        assert!(
                            why.iter().any(|r| r.contains("past its end")) && !why.iter().any(|r| r.contains("inside")),
                            "{case}: frame {k}: {why:?}"
                        );
                        parted += 1;
                    }
                } else {
                    assert!(
                        apart.psnr_all >= 35.0,
                        "{case}: frame {k} is drawn like the still, but the export draws it differently ({:.1} dB)",
                        apart.psnr_all
                    );
                    check_with(&case, &tl, &assets, &[t], STRICT, &opts);
                }
            }
        }
    }
    assert_eq!(
        parted, 1,
        "the dissolve parts from the export on the frame only its tail is left on"
    );
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
            let why = plan.unsupported_reasons_at((w, h));
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
