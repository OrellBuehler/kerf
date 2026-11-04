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
//! **No tolerance.** A frame the plan lets the compositor draw is FFmpeg's still **to the sample**, and a figure that is not
//! zero is a failure, whatever its size ([`check`]):
//!
//! 1. **the composite**: the still's own `yuv444p` planes (`timeline_frame_yuv_cancellable`: the composite's samples with the one
//!    conversion the graph ends in, 4:2:0 to 4:4:4) against the compositor's `yuv420p` composite (`composite_yuv`) put through the
//!    same conversion by FFmpeg: 0 differing samples in Y, U and V;
//! 2. **the PNG**: the compositor's composite taken to `rgb24` by FFmpeg itself (the conversion the still's PNG took) is the PNG
//!    `export_still` wrote, every channel of every pixel;
//! 3. **the display**: the RGBA that `render_plan` reads back (what the preview shows) is that same conversion of the composite, in
//!    swscale's own integers (`ff_yuv_420_rgb24_ssse3`'s lanes, `shaders/composite.wgsl`), wherever that kernel is what converts: an
//!    even size on a machine whose probe says its conversions are the model's. A frame with no layer under text has no YUV
//!    composite (FFmpeg blends `drawtext` over the black base in RGB), and there the RGB is held to the PNG directly.
//!
//! There is no PSNR, no edge band and no allowance a case can name. What the compositor does not draw exactly is **refused by the
//! plan** (`check_refused`: the plan says no, with a reason, and FFmpeg still renders the frame) and the harness asserts the
//! refusal; a case that must differ (`drawn_anyway`) is held to differ by at least one sample, which is what makes a refusal
//! needed. The scaler, `eq`, the blend, the translucent round trip, the blur and the placement are each held to FFmpeg's own
//! filter on planes (`the_scaler_matches_ffmpegs_scale_plane_by_plane`, `the_eq_tables_match_ffmpegs_eq_filter`,
//! `the_effect_filters_match_ffmpeg_plane_by_plane`, `tests/blur_model.rs`, `roundtrip_probe.rs`), every sample.
//!
//! The runtime self-test (`kerf_gpu::selftest`) is a different thing: a screen run once on a user's machine, in RGB, against that
//! machine's own FFmpeg, to catch a GPU or a build that draws grossly something else. Its limits are its own (40 dB, 8 levels) and
//! it makes no parity claim: parity is what this file and `motion_parity.rs` hold.
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
//! # What is refused, and why (nothing here is drawn within a bound)
//!
//! * **Opacity below 1** is reproduced, not approximated: FFmpeg takes the layer
//!   through RGB (`colorchannelmixer` has no YUV mode) and `roundtrip.rs` /
//!   `roundtrip.wgsl` follow it in integer arithmetic. Out of YUV is exact on
//!   random pictures (the layer's own matrix); luma back is exact; chroma back runs the
//!   x86 `yuv2yuvX` vertical kernel as swscale does (every row but the last), exact on every
//!   plane where `roundtrip_probe` measured this machine's kernels (otherwise the layer is
//!   refused: `TranslucentApproximate`). The way out uses the layer's own matrix and the way back the
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
//!   and is placed exactly (it is refused once it is resized, below).
//! * **Colour correction and full range.** The decode converts a full-range
//!   (`yuvj420p`) picture to limited range first; FFmpeg 9 grades the raw full-range
//!   values and converts last (34 to 44 dB apart, every knob; FFmpeg 6 agrees). A
//!   graded `yuvj` picture is refused, as is a graded layer in a stack with one under
//!   a negotiating policy (`grading_a_full_range_picture_is_refused_and_ungraded_it_is_drawn`).
//! * **Resizing a picture that is not 8-bit 4:2:0** (4:2:2, 4:4:4, RGB, 10 and 12-bit,
//!   gray, or a format never recorded) is refused, in both directions. FFmpeg scales
//!   in the format the picture has, the decode reduces it to 8-bit 4:2:0 first, and the
//!   chroma is then interpolated from different samples: enlarging was 15 to 69 levels
//!   off, and a *shrink* as mild as 1.05x to 1.5x reads a flat max of 8 to 9 levels
//!   and up to 32 on edges, the kernels differing most near a ratio of 1. No band of
//!   ratios was measured exact on both FFmpegs for busy chroma, so none is claimed
//!   (`resizing_a_picture_ffmpeg_scales_in_another_format_is_refused`). A picture left
//!   at its size is drawn for every format.
//! * **A moving zoom or position is the last stage of FFmpeg's chain** (the still and the export alike), so a
//!   grade, effects, a mask and the clip's fades act on the picture at its fit size, and the picture is then
//!   placed at sub-pixel positions by `pad`, `perspective` and `crop`, which the compositor reproduces
//!   (`kerf_core::perspective`): exact, 0 differing samples (`check_exact`, `a_moving_zoom_is_placed_as_the_still_places_it`
//!   and `tests/motion_parity.rs`, `tests/placement.rs`). A rotation and an opacity moving over a zoom that holds
//!   are drawn (`time/keyframes+rotation+opacity`); ahead of a placement they are refused (`ZoomBehind`).
//! * **Masks and effects** (`masks_cut_a_layer_to_a_shape`, `effects_are_drawn_as_their_filters_draw_them`,
//!   `the_effect_filters_match_ffmpeg_plane_by_plane`): a mask's alpha is `geq`'s, blended per sample (luma)
//!   and over each 2x2 block's average (chroma) like `overlay`; grayscale, invert, sharpen and vignette are
//!   exact; a blur is `gblur`'s float recursion with every step a fused multiply-add, as the AVX2 / AVX-512 kernels run it, exact where
//!   `blur_probe` measured this FFmpeg's kernel (otherwise there is no blur in the caps). A mask on a letterbox is cut from
//!   the whole padded frame, as `geq` is. A chroma key is `chromakey`'s alpha to the sample
//!   (`the_chroma_key_alpha_is_the_filters_plane_by_plane`, `chroma_keys_are_drawn_as_the_filter_draws_them`), and refused
//!   after a scale, on another format, under a mask or turned. A mask on a rotated or translucent layer, a
//!   blur, a sharpen, a vignette or a chroma key on a letterbox, a vignette on a picture that is not `yuv420p`, an
//!   invert / blur / sharpen on a full-range picture (FFmpeg runs it on the raw values and converts the range
//!   afterwards: an invert 8 levels off, a sharpen's overshoot 26) and an invert of a gray picture (its chroma
//!   stays 128 in FFmpeg) are refused. On a 10-bit, 4:2:2, 4:4:4, RGB or gray picture the other effects and masks
//!   are drawn at the plain source's own figures.
//! * **`geq` on FFmpeg 6.1 copies a plane's last column and row from the ones before them** (its bilinear
//!   read clamps to `w - 2` / `h - 2`; 9.0 copies them), so a masked picture's right-most column and bottom
//!   row differ between the two FFmpegs (up to 224 levels of chroma at the bottom row of a frame whose picture
//!   changes there). `kerf_core::geq_edge()` measures which the build in use does and the compositor draws
//!   it, so the mask cases hold on both (`a_mask_hands_the_planes_on_as_the_geq_that_clamps_its_read_does`).
//! * **A dip to white ahead of a moving zoom or position** is refused (`ZoomBehind`): the fade's RGB round
//!   trip is not held to FFmpeg's with a placement behind it (the zoom's `scale` takes the RGB frame the fade
//!   leaves and converts it in the same pass: up to 14 levels off a draw that scales the converted picture on
//!   6.1.1, 3 on 9.0.2).
//! * **A shrink steeper than 63:1** (`kerf_core::MAX_SHRINK`) is refused: past it swscale
//!   cascades a second scaler, which the model does not follow (it has no table there).
//! * **A rotated edge** is a fixed-point stair-step in FFmpeg and a float sample
//!   here (Y up to 117 and UV up to 169 levels off along it): no turn is drawn but a
//!   quarter, half or three quarters of one, which lands on whole samples (every other
//!   angle, and a rotation a key moves, is refused: `Unsupported::Rotation`,
//!   `a_turned_neutral_layer_has_no_colour_fringe_and_any_other_turn_is_refused`).
//! * **swscale's YUV -> RGB** is the x86 kernel's integers (`pmulhw` by 13-bit lanes), which the display conversion
//!   follows to the level (it used to round in `float`, up to 3 levels from the PNG).
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

mod common;

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

// ---- thresholds: none. Every drawn frame is the export's / the still's to the sample. -------

// ---- environment ------------------------------------------------------------

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

thread_local! {
    /// A case that draws with every blend mode in the caps, whatever this FFmpeg's probe said ([`blend_check`]).
    static FORCED_BLENDS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    static FORCING: OnceLock<Compositor> = OnceLock::new();
    if FORCED_BLENDS.with(std::cell::Cell::get) {
        return FORCING.get_or_init(|| {
            let _ = kerf_gpu::grade::kernels();
            Compositor::new(gpu())
                .expect("a compositor")
                .with_blend_kinds(kerf_gpu::blend::all_modes())
        });
    }
    C.get_or_init(|| {
        // The grade is in the caps once this FFmpeg's conversions are known to be the model's (the app's `prepare` asks
        // ahead of a frame): the harness measures them once, up front, and its grade cases hold the answer to `Match`.
        let _ = kerf_gpu::grade::kernels();
        let _ = kerf_gpu::scaler::kernels();
        let _ = kerf_gpu::convert::kernels();
        // ... and how its `geq` reads a plane's edge, which a mask and a keyed opacity need (a render only reads it).
        let _ = kerf_core::geq_edge();
        // ... and whether this FFmpeg converts a 4:4:4 / 4:2:2 picture in a graph as the decode does (a placed one).
        let _ = kerf_gpu::conversion::conversion();
        // ... and whether this FFmpeg places a picture as the model does (the moving zoom and position are in the caps only then).
        let _ = kerf_gpu::placement_probe::placement();
        // ... and whether it takes a translucent layer through RGB, and blurs, as the model does (a layer below full opacity and a blur are in the caps only then).
        let _ = kerf_gpu::roundtrip_probe::translucency();
        let _ = kerf_gpu::blur_probe::blur();
        let _ = kerf_gpu::display_probe::display();
        // ... and so is the layout of its `drawtext`, which decides whether text whose bitmaps share samples is drawn.
        let _ = kerf_gpu::text::model();
        // ...and the blend chain's, which the blend modes are in the caps by.
        let _ = kerf_gpu::blend::kernels();
        Compositor::new(gpu()).expect("a compositor")
    })
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
    /// The test pattern tagged as an NTSC (`smpte170m`) and a PAL (`bt470bg`) file is: BT.601's coefficients under a tag.
    testsrc170m: Asset,
    bars470bg: Asset,
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
        let smpte170m = "setparams=colorspace=smpte170m:color_primaries=smpte170m:color_trc=smpte170m";
        let bt470bg = "setparams=colorspace=bt470bg:color_primaries=bt470bg:color_trc=bt470bg";
        let bars = video("bars.mp4", &format!("smptehdbars=size=640x360:rate=30:duration=2,{untagged}"));
        let bars709 = video("bars709.mp4", &format!("smptehdbars=size=640x360:rate=30:duration=2,{bt709}"));
        let testsrc709 = video("testsrc709.mp4", &format!("testsrc2=size=640x360:rate=30:duration=2,{bt709}"));
        let testsrc2020 = video("testsrc2020.mp4", &format!("testsrc2=size=640x360:rate=30:duration=2,{bt2020}"));
        let testsrc170m = video("testsrc170m.mp4", &format!("testsrc2=size=640x360:rate=30:duration=2,{smpte170m}"));
        let bars470bg = video("bars470bg.mp4", &format!("smptehdbars=size=640x360:rate=30:duration=2,{bt470bg}"));
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
        // (FFmpeg before 6 has no `-display_rotation`: its way to say it is the `rotate` tag.)
        let modern = Command::new(kerf_core::ffmpeg_path())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-display_rotation", "90", "-i"])
            .arg(&landscape)
            .args(["-c", "copy"])
            .arg(&rotated_path)
            .stdin(Stdio::null())
            .output()
            .expect("run ffmpeg")
            .status
            .success();
        if !modern {
            ffmpeg(&[
                "-i",
                landscape.to_str().unwrap(),
                "-c",
                "copy",
                "-metadata:s:v:0",
                "rotate=90",
                rotated_path.to_str().unwrap(),
            ]);
        }
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
            testsrc170m: probed(&testsrc170m),
            bars470bg: probed(&bars470bg),
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
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format,
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

fn clip(a: &Asset, source_in: f64, source_out: f64, start: f64) -> Clip {
    Clip::new(a.id, source_in, source_out, start)
}

// ---- comparison ---------------------------------------------------------------

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

/// The matrix name FFmpeg's `scale` takes for the plan's composite.
fn matrix_name(plan: &RenderPlan) -> &'static str {
    match plan.canvas.matrix {
        kerf_core::YuvMatrix::Bt601 => "bt601",
        kerf_core::YuvMatrix::Bt709 => "bt709",
        kerf_core::YuvMatrix::Bt2020 => "bt2020nc",
    }
}

/// The compositor's `yuv420p` planes put through one FFmpeg graph (`filter`, `-pix_fmt` `out_pix`): what the still does to its own
/// composite, so that any difference is in the composite and nowhere else.
fn ffmpeg_over_planes(planes: &kerf_gpu::YuvFrame, (w, h): (usize, usize), filter: &str, out_pix: &str) -> Vec<u8> {
    use std::io::Write;
    let mut raw = planes.y.clone();
    raw.extend_from_slice(&planes.u);
    raw.extend_from_slice(&planes.v);
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
        ])
        .arg(format!("{w}x{h}"))
        .args(["-i", "pipe:0", "-vf", filter])
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", out_pix, "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&raw);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// How many samples differ, and the largest difference.
fn differ(a: &[u8], b: &[u8]) -> (usize, i32) {
    assert_eq!(a.len(), b.len(), "the pictures are not the same size");
    let mut n = 0;
    let mut max = 0;
    for (x, y) in a.iter().zip(b) {
        let d = (i32::from(*x) - i32::from(*y)).abs();
        n += usize::from(d != 0);
        max = max.max(d);
    }
    (n, max)
}

/// Render `tl` at each of `times` through FFmpeg (`export_still`, and the still's own `yuv444p` planes) and through the GPU, and hold
/// them to **no tolerance**: a frame the plan lets the compositor draw is FFmpeg's to the sample.
///
/// 1. **The composite**: the still's `yuv444p` planes against the compositor's `yuv420p` composite put through the same conversion
///    (`timeline_frame_yuv_cancellable`), 0 differing samples in Y, U and V.
/// 2. **The PNG**: the compositor's composite taken to `rgb24` by FFmpeg itself, the conversion the still's PNG took, equals the PNG.
/// 3. **The display**: the RGBA `render_plan` reads back equals the same conversion of the composite, wherever swscale's unscaled
///    `yuv420p -> rgb24` kernel is what converts (even sizes, the probe says this FFmpeg's is the model: `grade::kernels()`); an odd
///    size takes swscale's generic path, which the display conversion does not follow, and is held to 1 and 2 alone.
///
/// A figure that is not zero is a failure, whatever its size. `KERF_PARITY_EXPLORE=1` prints every figure and fails nothing.
fn check(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64]) {
    check_with(case, tl, assets, times, &ExportOptions::default());
}

/// [`check`] for a render with options (the frame rate the plan counts frames in, say). Returns, per time, how many samples of the
/// still's planes, its PNG and the display differ (always `[0, 0, 0]` unless `KERF_PARITY_EXPLORE` is set).
fn check_with(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64], opts: &ExportOptions) -> Vec<[usize; 3]> {
    let out_dir = target_dir().join("parity");
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    // A file-name-safe form of the case name (`pip/scaled+offset`).
    let slug: String = case
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let mut failures = Vec::new();
    let mut figures = Vec::new();
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
        let (_, _, want444) =
            kerf_core::timeline_frame_yuv_cancellable(tl, assets, opts, t, size.0, &|| false).expect("the FFmpeg still's planes");

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
        // A frame with no layer under text has no YUV composite: FFmpeg hands the black base to `drawtext` and on to the PNG as RGB,
        // never converting it, and so does the compositor. There the RGB the compositor reads back is the PNG, to the sample.
        let in_rgb = plan.layers.is_empty() && !plan.overlays.is_empty();
        let (planes_n, planes_max, got_rgb, display) = if in_rgb {
            (0, 0, gpu_rgb.clone(), false)
        } else {
            let drawn = compositor()
                .composite_yuv(&plan, &oneshot, size)
                .unwrap_or_else(|e| panic!("{case} @ {t}: the GPU's planes: {e}"));
            let got444 = ffmpeg_over_planes(&drawn, (w, h), "format=yuv444p", "yuv444p");
            let (planes_n, planes_max) = differ(&got444, &want444);
            let got_rgb = ffmpeg_over_planes(
                &drawn,
                (w, h),
                &format!("scale=in_color_matrix={}:in_range=tv", matrix_name(&plan)),
                "rgb24",
            );
            let display = w % 2 == 0 && h % 2 == 0 && kerf_gpu::grade::measured() == kerf_gpu::grade::Kernels::Match;
            (planes_n, planes_max, got_rgb, display)
        };
        let (png_n, png_max) = differ(&got_rgb, &reference);
        let (display_n, display_max) = if display { differ(&gpu_rgb, &got_rgb) } else { (0, 0) };
        figures.push([planes_n, png_n, display_n]);
        let line = format!(
            "{case:<28} t={t:<7} {w}x{h} layers={} differing samples: planes {planes_n} (max {planes_max}), png {png_n} (max {png_max}), display {display_n} (max {display_max}{}) (decode {:.0} ms, composite {:.0} ms)",
            plan.layers.len(),
            if display { "" } else { ", not judged" },
            timings.decode.as_secs_f64() * 1e3,
            timings.composite.as_secs_f64() * 1e3,
        );
        eprintln!("{line}");
        report().lock().unwrap().insert(format!("{case} {t:08.4}"), line);

        let ok = planes_n == 0 && png_n == 0 && display_n == 0;
        if !ok || std::env::var_os("KERF_PARITY_KEEP").is_some() {
            std::fs::create_dir_all(&out_dir).unwrap();
            let stem = format!("{slug}-{t}");
            write_png(&out_dir.join(format!("{stem}-ref.png")), &reference, w, h);
            write_png(&out_dir.join(format!("{stem}-gpu.png")), &got_rgb, w, h);
            // Diff: |ref - gpu| x 8.
            let diff: Vec<u8> = (0..w * h)
                .flat_map(|i| {
                    let d = |c: usize| (i32::from(reference[i * 3 + c]) - i32::from(got_rgb[i * 3 + c])).unsigned_abs();
                    let mag = (d(0).max(d(1)).max(d(2)) * 8).min(255) as u8;
                    [mag, mag, mag]
                })
                .collect();
            write_png(&out_dir.join(format!("{stem}-diff.png")), &diff, w, h);
            if !ok {
                failures.push(format!(
                    "{case} @ {t}s: differing samples: planes {planes_n} (max {planes_max}), png {png_n} (max {png_max}), display {display_n} (max {display_max}) — images in {}",
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
        return figures;
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    figures
}

/// [`check`] for a case that may hold a full-range picture in a **still**: at each time, a frame with one on screen is drawn and exact under
/// FFmpeg 6 (it converts the range at the decode, as the compositor does) and refused by the plan under a policy that negotiates the colour
/// (FFmpeg 9 keeps the picture's range through the graph and converts last: the still's planes are full-range ones, up to 17 levels from
/// the compositor's limited-range composite, and the PNG's RGB 3 to 4 levels from its own), FFmpeg rendering the still; a frame without one
/// is drawn and exact under both.
fn check_range(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64]) {
    for &t in times {
        let plan = RenderPlan::at(tl, assets, &ExportOptions::default(), t, policy()).expect("plan");
        if policy() != CompositeColorPolicy::FixedBt601 && plan.layers.iter().any(|l| l.stream.maybe_full_range()) {
            check_refused(case, tl, assets, t, Refusal::Plan("a full-range picture in a still"));
        } else {
            check(case, tl, assets, &[t]);
        }
    }
}

/// [`check`] under its older name, from when it held the planes apart from the RGB comparison.
fn check_exact(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64]) {
    check(case, tl, assets, times);
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
    check("single/testsrc2", &tl, std::slice::from_ref(&m.testsrc), &[0.0, 0.5, 1.2345]);
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)]], None);
    check("single/smptehdbars", &tl, std::slice::from_ref(&m.bars), &[0.0, 1.0]);
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)]], None);
    check("single/gradient", &tl, std::slice::from_ref(&m.gradient), &[0.0, 1.0]);
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
        rotation: 90.0,
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
    check("gap", &tl, std::slice::from_ref(&m.testsrc), &[1.5]);
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
    check("contain/9x16", &contain, assets, &[0.5, 1.2]);
    let cover = timeline(
        vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(360, 640, Fit::Cover)),
    );
    check("cover/9x16", &cover, assets, &[0.5, 1.2]);
    // A portrait source into a landscape frame, both ways round.
    let pa = std::slice::from_ref(&m.portrait);
    let tl = timeline(
        vec![vec![clip(&m.portrait, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(640, 360, Fit::Contain)),
    );
    check("contain/portrait-in-16x9", &tl, pa, &[0.7]);
    let tl = timeline(
        vec![vec![clip(&m.portrait, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(640, 360, Fit::Cover)),
    );
    check("cover/portrait-in-16x9", &tl, pa, &[0.7]);
    // Upscaling: the 640x360 source into a larger frame.
    let up = timeline(
        vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(960, 540, Fit::Contain)),
    );
    check("contain/upscale-960x540", &up, assets, &[0.5]);
    let down = timeline(
        vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(320, 180, Fit::Contain)),
    );
    check("contain/downscale-320x180", &down, assets, &[0.5]);
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
    check("pip/scaled+offset", &tl, &[m.bars.clone(), m.testsrc.clone()], &[0.0, 1.0]);

    // The later track is on top even when it starts later; before it starts
    // only the base shows.
    let late = clip(&m.testsrc, 0.0, 1.0, 1.0);
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![late]], None);
    check(
        "tracks/late-top-clip",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        &[0.5, 1.5],
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
        vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![pip.clone()]],
        Some(Delivery::new(360, 640, Fit::Contain)),
    );
    check(
        "pip/contain-then-scale",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        &[0.5],
    );
    // Under Cover the same picture-in-picture is the cover of the frame times 0.6, in one resample, cut by
    // the canvas: the plan refuses it (`FoldedFit`) and FFmpeg draws it.
    let tl = timeline(
        vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![pip]],
        Some(Delivery::new(360, 640, Fit::Cover)),
    );
    check_refused(
        "pip/cover-then-scale",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        0.5,
        Refusal::Plan("one resample"),
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
    // (A turn that is not a quarter is FFmpeg's: its edge is a fixed-point sample, Y up to 117 and UV up to 169 off a float one.)
    for t in [0.5, 1.5] {
        check_refused(
            "transform/scale+rotate+crop",
            &tl,
            &[m.gradient.clone(), m.testsrc.clone()],
            t,
            Refusal::Plan("a rotation"),
        );
    }

    let mut rot = clip(&m.bars, 0.0, 2.0, 0.0);
    rot.transform.rotation = 90.0;
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![rot]], None);
    check("transform/rotate-90", &tl, &[m.gradient.clone(), m.bars.clone()], &[0.5]);

    let mut crop = clip(&m.testsrc, 0.0, 2.0, 0.0);
    crop.transform = Transform {
        crop_left: 0.25,
        crop_right: 0.1,
        crop_top: 0.2,
        crop_bottom: 0.15,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![crop]], None);
    check("transform/crop-only", &tl, std::slice::from_ref(&m.testsrc), &[0.5]);
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
    check_refused(
        "geometry/odd-rotated-box",
        &tl,
        &[m.gradient.clone(), m.portrait.clone()],
        0.5,
        Refusal::Plan("a rotation"),
    );
    // Moved so far that the layer starts left of the canvas and above it.
    let tl = timeline(
        vec![
            vec![base.clone()],
            vec![over(
                Transform {
                    pos_x: -0.3,
                    pos_y: -0.2,
                    ..Transform::default()
                },
                &m.testsrc,
            )],
        ],
        None,
    );
    check(
        "geometry/pan-off-canvas",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        &[0.5],
    );
    // Zoomed in past the frame and panned: FFmpeg takes the picture from the source in one resample
    // (to the fit of the frame times the zoom), where the compositor fits it to the frame and scales
    // that, so the plan refuses it (`FoldedFit`) and FFmpeg draws the frame.
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
    check_refused(
        "geometry/zoom-off-canvas",
        &tl,
        &[m.gradient.clone(), m.testsrc.clone()],
        0.5,
        Refusal::Plan("one resample"),
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
    check("geometry/crop-then-cover", &tl, std::slice::from_ref(&m.testsrc), &[0.5]);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_turned_neutral_layer_has_no_colour_fringe_and_any_other_turn_is_refused() {
    let m = media();
    // Mid-grey turned over black: every pixel of the result is a grey. FFmpeg's `rotate` leaves chroma outside the picture at zero (a deep
    // green) under an alpha that is not quite zero there, and a few of its own pixels show it; a quarter turn has no such edge, and any other
    // turn is refused (the compositor's float edge is not the filter's fixed-point one), by the plan and by the compositor itself.
    for rotation in [17.0, 33.0, 45.0, -25.0, 5.0] {
        let mut c = clip(&m.grey, 0.0, 2.0, 0.0);
        c.transform.rotation = rotation;
        c.transform.scale = 0.7;
        let tl = timeline(vec![vec![clip(&m.black, 0.0, 2.0, 0.0)], vec![c]], None);
        let assets = [m.black.clone(), m.grey.clone()];
        check_refused(
            &format!("fringe/grey-rotated-{rotation}"),
            &tl,
            &assets,
            0.5,
            Refusal::Plan("a rotation"),
        );
        let plan = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 0.5, policy()).expect("plan");
        let err = compositor()
            .render_plan(&plan, plan.size(u32::MAX))
            .expect_err("the compositor refuses a turn it does not draw");
        assert!(
            matches!(&err, kerf_gpu::GpuError::Unsupported(why) if why.contains("a rotation that is not a quarter")),
            "rotation {rotation}: {err}"
        );
    }
    for rotation in [90.0, 180.0, 270.0, -90.0] {
        let mut c = clip(&m.grey, 0.0, 2.0, 0.0);
        c.transform.rotation = rotation;
        let tl = timeline(vec![vec![clip(&m.black, 0.0, 2.0, 0.0)], vec![c]], None);
        let assets = [m.black.clone(), m.grey.clone()];
        let plan = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 0.5, policy()).expect("plan");
        let size = plan.size(u32::MAX);
        let (frame, _) = compositor().render_plan(&plan, size).expect("GPU render");
        let gpu = rgba_to_rgb(&frame.data)
            .chunks(3)
            .map(|p| p.iter().max().unwrap() - p.iter().min().unwrap())
            .max()
            .unwrap_or(0);
        eprintln!("fringe/grey-turned-{rotation:<5} worst channel spread: GPU {gpu}");
        assert!(
            gpu == 0,
            "rotation {rotation}: the GPU frame has a colour fringe (spread {gpu})"
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
    check("opacity/0.5", &tl, &[m.bars.clone(), m.testsrc.clone()], &[0.0, 1.0]);

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
    );

    let mut top = clip(&m.gradient, 0.0, 2.0, 0.0);
    top.transform = Transform {
        opacity: 0.3,
        scale: 0.6,
        rotation: -25.0,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![top]], None);
    check_refused(
        "opacity/0.3+scale+rotate",
        &tl,
        &[m.bars.clone(), m.gradient.clone()],
        0.5,
        Refusal::Plan("a rotation"),
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
        check(&format!("opacity/{name}"), &tl, std::slice::from_ref(a), &[0.5]);
    }

    // A BT.2020-tagged picture (saturated bars) at 0.6: out of YUV with its own
    // matrix, back as BT.601.
    let mut c = clip(&m.bars2020, 0.0, 2.0, 0.0);
    c.transform.opacity = 0.6;
    let tl = timeline(vec![vec![c]], None);
    check("opacity/bt2020-bars-0.6", &tl, std::slice::from_ref(&m.bars2020), &[0.5]);

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
    check("opacity/keyframed-fade+grade", &tl, std::slice::from_ref(&m.testsrc), &[0.5]);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn colour_adjustments() {
    let m = media();
    let case = |name: &str, color: Color, asset: &Asset| {
        let mut c = clip(asset, 0.0, 2.0, 0.0);
        c.color = color;
        let tl = timeline(vec![vec![c]], None);
        check(name, &tl, std::slice::from_ref(asset), &[0.5]);
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
    );
}

/// The scaler used to be a level from swscale's and `eq` maps two values a level apart to values its tables' steepest step apart
/// (`eq::table_steps`), so on a layer the compositor scales a grade whose tables step past two was FFmpeg's (`GradeSteep`). Where the
/// scaler is measured to be swscale's to the level (`caps.exact_scaler`, both FFmpegs of CI), there is no level to multiply: the
/// tables are exact at any step, drawn on the scaled layer to the level in a still as in an export frame (`motion_parity.rs`). Contrast
/// and saturation past 2, a gamma past about 1.2, a black point parked above 0 under a gamma above 1, the far ends of every range;
/// on a picture-in-picture, a letterboxed layer and a Cover fit. (Refused on a compositor whose scaler is not measured exact: the plan's
/// unit tests, `a_grade_whose_tables_amplify_the_scalers_level_past_two_is_refused_on_a_layer_it_resizes`.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn eq_tables_of_any_step_are_drawn_on_a_layer_the_scaler_resizes() {
    let m = media();
    assert!(kerf_gpu::scaler::kernels().exact(), "this FFmpeg's scaler is not the model");
    let grade = |brightness: f64, contrast: f64, saturation: f64, gamma: f64, temperature: f64| Color {
        brightness,
        contrast,
        saturation,
        gamma,
        temperature,
    };
    let shifted = Transform {
        scale: 0.6,
        pos_x: 0.1,
        pos_y: -0.1,
        ..Transform::default()
    };
    let pip = |color: Color| {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.transform = shifted;
        c.color = color;
        timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None)
    };
    let under = [m.bars.clone(), m.testsrc.clone()];
    let steep = [
        ("contrast 3", grade(0.0, 3.0, 1.0, 1.0, 0.0)),
        ("saturation 3", grade(0.0, 1.0, 3.0, 1.0, 0.0)),
        ("gamma 1.4", grade(0.0, 1.0, 1.0, 1.4, 0.0)),
        ("gamma 2", grade(0.0, 1.0, 1.0, 2.0, 0.0)),
        ("brightness -0.3 and gamma 1.5", grade(-0.3, 1.0, 1.0, 1.5, 0.0)),
        ("contrast 1.8 and gamma 2", grade(0.0, 1.8, 1.0, 2.0, 0.0)),
        ("the far end of every range", grade(0.3, 1.8, 2.0, 2.0, 1.0)),
        // (Gentle ones, which were drawn before.)
        ("contrast 2", grade(0.0, 2.0, 1.0, 1.0, 0.0)),
        ("gamma 1.2", grade(0.0, 1.0, 1.0, 1.2, 0.0)),
    ];
    for (name, color) in steep {
        check(&format!("colour/pip, {name}"), &pip(color), &under, &[0.5]);
    }
    // A letterboxed layer (the portrait picture shrunk into a landscape frame, the bars in the picture the `eq` sees) and a
    // layer scaled by the fit alone.
    let boxed = |color: Color| {
        let mut c = clip(&m.portrait, 0.0, 2.0, 0.0);
        c.color = color;
        timeline(vec![vec![c]], Some(Delivery::new(640, 360, Fit::Contain)))
    };
    for (name, color) in steep {
        check(
            &format!("colour/letterbox, {name}"),
            &boxed(color),
            std::slice::from_ref(&m.portrait),
            &[0.5],
        );
    }
    for (name, color) in [
        ("contrast 1.8 and gamma 2", grade(0.0, 1.8, 1.0, 2.0, 0.0)),
        ("saturation 3", grade(0.0, 1.0, 3.0, 1.0, 0.0)),
    ] {
        let mut cover = clip(&m.testsrc, 0.0, 2.0, 0.0);
        cover.color = color;
        check(
            &format!("colour/cover fit, {name}"),
            &timeline(vec![vec![cover]], Some(Delivery::new(360, 640, Fit::Cover))),
            std::slice::from_ref(&m.testsrc),
            &[0.5],
        );
    }
    // Busy pictures, where every table entry is looked at.
    let mut noisy = clip(&m.noise, 0.0, 0.2, 0.0);
    noisy.color = grade(0.0, 1.8, 2.0, 2.0, 0.5);
    check(
        "colour/noise, enlarged, the far end of every range",
        &timeline(vec![vec![noisy]], Some(Delivery::new(960, 540, Fit::Contain))),
        std::slice::from_ref(&m.noise),
        &[0.1],
    );
    // A layer the compositor does not resize: the tables are exact at any step.
    let mut alone = clip(&m.testsrc, 0.0, 2.0, 0.0);
    alone.color = grade(0.3, 1.8, 2.0, 2.0, 1.0);
    check(
        "colour/the far end of every range at the picture's size",
        &timeline(vec![vec![alone]], None),
        std::slice::from_ref(&m.testsrc),
        &[0.5],
    );
}

/// A clip's grade on a layer the scaler resizes: the picture goes `yuv420p -> rgb24` through the x86 kernel, the grade, and back; with the
/// scaler exact (`eq_tables_of_any_step_are_drawn_on_a_layer_the_scaler_resizes`) the picture it converts is FFmpeg's to the level, so the
/// whole is — **when an `eq` sits between the fit `scale` and the grade**. Without one the format negotiation lets the fit `scale` output
/// RGB itself (the filters between pass any format along, the grade's RGB-only filters ask for it), scaling and converting in one pass from
/// chroma the compositor has already scaled: refused, with FFmpeg rendering it. A picture-in-picture, an enlargement, a shrink, a Cover fit.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_grade_on_a_layer_the_scaler_resizes_is_the_still_where_an_eq_precedes_it() {
    let m = media();
    assert!(kerf_gpu::scaler::kernels().exact(), "this FFmpeg's scaler is not the model");
    let all = grades();
    let eq = Color {
        contrast: 1.08,
        saturation: 1.15,
        temperature: 0.1,
        ..Color::default()
    };
    let pip = |asset: &Asset, grade: &kerf_core::Grade, color: Color, scale: f64| {
        let mut c = clip(asset, 0.0, 2.0, 0.0);
        c.transform.scale = scale;
        c.grade = grade.clone();
        c.color = color;
        (
            timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None),
            vec![m.bars.clone(), asset.clone()],
        )
    };
    for (name, grade) in &all {
        let (tl, assets) = pip(&m.testsrc, grade, eq, 0.6);
        check(&format!("grade/pip with eq, {name}"), &tl, &assets, &[0.5]);
    }
    let heavy = &all[5].1;
    // The shapes: half size, enlarged, shrunk, a Cover crop, a tagged picture (through 4:4:4 on the way back).
    for (name, delivery) in [
        ("enlarged to 960x540", Delivery::new(960, 540, Fit::Contain)),
        ("shrunk to 320x180", Delivery::new(320, 180, Fit::Contain)),
        ("cover into 360x640", Delivery::new(360, 640, Fit::Cover)),
        ("cover into 640x360", Delivery::new(640, 360, Fit::Cover)),
    ] {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.grade = heavy.clone();
        c.color = eq;
        check(
            &format!("grade/{name} with eq"),
            &timeline(vec![vec![c]], Some(delivery)),
            std::slice::from_ref(&m.testsrc),
            &[0.5],
        );
    }
    for (source, asset) in [
        ("testsrc709", &m.testsrc709),
        ("testsrc170m", &m.testsrc170m),
        ("bars470bg", &m.bars470bg),
    ] {
        let (tl, assets) = pip(asset, heavy, eq, 0.6);
        // (A stack of one matrix under FFmpeg 9: the bars beside it must be of its matrix.)
        if policy() == CompositeColorPolicy::FixedBt601 || source == "testsrc170m" || source == "bars470bg" {
            check(&format!("grade/pip with eq, {source}"), &tl, &assets, &[0.5]);
        }
    }
    // Busy: noise enlarged.
    let mut c = clip(&m.noise, 0.0, 0.2, 0.0);
    c.grade = heavy.clone();
    c.color = eq;
    check(
        "grade/noise enlarged with eq",
        &timeline(vec![vec![c]], Some(Delivery::new(960, 540, Fit::Contain))),
        std::slice::from_ref(&m.noise),
        &[0.1],
    );
    // Without an `eq` the fit scale converts to RGB itself: refused.
    for (name, scale, delivery) in [
        ("pip", 0.6, None),
        ("enlarged", 1.0, Some(Delivery::new(960, 540, Fit::Contain))),
    ] {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.transform.scale = scale;
        c.grade = heavy.clone();
        check_refused(
            &format!("grade/{name}, no eq"),
            &timeline(vec![vec![c]], delivery),
            std::slice::from_ref(&m.testsrc),
            0.5,
            Refusal::Plan("no `eq` ahead of the grade"),
        );
    }
    // A letterbox is still a matte the grade would see.
    let mut c = clip(&m.portrait, 0.0, 2.0, 0.0);
    c.grade = heavy.clone();
    c.color = eq;
    check_refused(
        "grade/letterbox with eq",
        &timeline(vec![vec![c]], Some(Delivery::new(640, 360, Fit::Contain))),
        std::slice::from_ref(&m.portrait),
        0.5,
        Refusal::Plan("letterbox"),
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn still_images() {
    let m = media();
    // The PNG alone: the frame is the picture's own shape.
    let tl = timeline(vec![vec![clip(&m.still, 0.0, 5.0, 0.0)]], None);
    check("still/alone", &tl, std::slice::from_ref(&m.still), &[0.0, 2.5]);
    // A JPEG is full-range YUV, which has to be brought to the limited range the
    // composite works in (FFmpeg does it in the graph; the decode does it here).
    let tl = timeline(vec![vec![clip(&m.jpeg, 0.0, 5.0, 0.0)]], None);
    check_range("still/jpeg", &tl, std::slice::from_ref(&m.jpeg), &[1.0]);
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
    check_refused(
        "still/pip-over-video",
        &tl,
        &[m.gradient.clone(), m.jpeg.clone()],
        1.0,
        Refusal::Plan("full-range picture in a still that the scaler resizes"),
    );
    let mut video_pip = clip(&m.gradient, 0.0, 2.0, 0.0);
    video_pip.transform = pip;
    let tl = timeline(vec![vec![clip(&m.png640, 0.0, 5.0, 0.0)], vec![video_pip]], None);
    check(
        "still/png-under-video-pip",
        &tl,
        &[m.png640.clone(), m.gradient.clone()],
        &[1.0],
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
    );

    let mut rev = clip(&m.testsrc, 0.0, 2.0, 0.0);
    rev.speed = -1.0;
    let tl = timeline(vec![vec![rev]], None);
    check("time/reversed", &tl, std::slice::from_ref(&m.testsrc), &[0.3, 1.1]);

    // Keyframed transform: a zoom and a position that move are placed by FFmpeg's `perspective`, which the compositor
    // reproduces (no tolerance).
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
    check_placed("time/keyframes", &tl, &[m.bars.clone(), m.testsrc.clone()], &[0.5, 1.25, 2.0]);
    // The rotation and the opacity keyed, over a zoom and a position that hold still: both are
    // sampled at the clip's time and the order is the one the GPU has.
    let mut kf = clip(&m.testsrc, 0.0, 2.0, 0.5);
    kf.keyframes = vec![key(0.0, 0.5, (0.2, -0.1), 0.0, 1.0), key(1.5, 0.5, (0.2, -0.1), 10.0, 0.6)];
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![kf]], None);
    // (At the clip's first frame the keys hold no rotation and full opacity: drawn. Past it the picture is turned by a number that is not a
    // quarter: FFmpeg's.)
    check(
        "time/keyframes+rotation+opacity",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        &[0.5],
    );
    for t in [1.25, 2.0] {
        check_refused(
            "time/keyframes+rotation+opacity",
            &tl,
            &[m.bars.clone(), m.testsrc.clone()],
            t,
            Refusal::Plan("a rotation"),
        );
    }
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
        check_range(name, &tl, std::slice::from_ref(a), &[t]);
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
    );
    // The bars are part of the frame `eq` sees: a brightened clip has brightened
    // bars (Y 16 -> 16 + 0.15 * 219, U / V through the chroma tables).
    let mut graded = clip(&m.portrait, 0.0, 2.0, 0.0);
    graded.color.brightness = 0.15;
    let tl = timeline(vec![vec![graded]], Some(Delivery::new(640, 360, Fit::Contain)));
    check("letterbox/graded-single-clip", &tl, std::slice::from_ref(&m.portrait), &[0.5]);
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
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn busy_sources_are_held_to_the_scaler_not_to_a_flat_region() {
    let m = media();
    // Noise and a 1-px checkerboard, scaled by ratios that are not whole numbers
    // (down 0.75 and 0.6, up 1.5): every output pixel depends on the exact kernel
    // (taps, phase, stretch on shrink). Every sample of these pictures is the evidence, as it is of every case.
    for (name, a) in [("noise", &m.noise), ("checker", &m.checker)] {
        for (w, h) in [(480u32, 270u32), (384, 216), (960, 540)] {
            let tl = timeline(vec![vec![clip(a, 0.0, 0.2, 0.0)]], Some(Delivery::new(w, h, Fit::Contain)));
            check(&format!("busy/{name}-to-{w}x{h}"), &tl, std::slice::from_ref(a), &[0.1]);
        }
    }
    // The transform's own scale after the fit scale: two kernels in cascade.
    let mut pip = clip(&m.noise, 0.0, 0.2, 0.0);
    pip.transform = Transform {
        scale: 0.7,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![pip]], None);
    check("busy/noise-scaled-0.7", &tl, &[m.gradient.clone(), m.noise.clone()], &[0.1]);
}

/// A moving zoom or position: FFmpeg's still places the picture at sub-pixel positions (a `perspective`, as
/// the export does), which the compositor reproduces (`kerf_core::perspective`, `placement.wgsl`): exact.
fn check_placed(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64]) {
    check_exact(case, tl, assets, times);
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

/// A chroma key on a layer the compositor scales is drawn, as `chromakey` draws it, to the sample: its alpha is a distance in the chroma
/// over `blend`, and the chroma it reads is what the scaler leaves, which is swscale's to the sample where the scaler is measured (the plan
/// refuses the key after a scale only where it is not: `caps.exact_scaler`). Several scales, keys from a hard one to a wide one, noise and
/// a test card, and the key at every position among the other effects (the green-screen archetype puts it first). (`motion_parity.rs` holds the same in an export.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_chroma_key_after_a_scale_is_drawn_as_chromakey_draws_it() {
    let m = media();
    use kerf_core::VideoEffect as E;
    // (The noise is 0.2 s long: the clips are as long as the foreground is, and the frame is looked at in the middle of it. A frame at 0.5 s
    // would hold only the bars for it, and every noise case would be a plain frame of bars.)
    let over_in = |fg: &Asset, delivery: Option<Delivery>, edit: &dyn Fn(&mut Clip)| {
        let dur = fg.duration.min(2.0);
        let mut c = clip(fg, 0.0, dur, 0.0);
        edit(&mut c);
        (
            timeline(vec![vec![clip(&m.bars, 0.0, dur, 0.0)], vec![c]], delivery),
            vec![m.bars.clone(), fg.clone()],
            dur / 2.0,
        )
    };
    let over = |fg: &Asset, edit: &dyn Fn(&mut Clip)| over_in(fg, None, edit);
    let key_check = |case: &str, (tl, assets, t): (Timeline, Vec<Asset>, f64)| {
        let plan = RenderPlan::at(&tl, &assets, &ExportOptions::default(), t, policy()).expect("plan");
        assert_eq!(
            plan.layers.len(),
            2,
            "{case}: the keyed layer is on screen at {t} s, over the bars"
        );
        check(case, &tl, &assets, &[t]);
    };
    for scale in [0.6, 0.4985, 0.9] {
        for (similarity, blend) in [(0.15, 0.1), (0.1, 0.0), (0.5, 0.5), (0.00001, 0.0), (1.0, 1.0)] {
            for fg in [&m.testsrc, &m.noise] {
                for color in ["green", "0x808080"] {
                    let case = over(fg, &|c| {
                        c.transform.scale = scale;
                        c.effects = vec![a_key(color, similarity, blend)];
                    });
                    key_check(
                        &format!("key-scale/{} {color} scale {scale} sim {similarity} blend {blend}", fg.name),
                        case,
                    );
                }
            }
        }
    }
    for (name, effects) in [
        ("blur", vec![E::Blur { sigma: 2.0 }]),
        ("sharpen", vec![E::Sharpen { amount: 1.5 }]),
        ("invert", vec![E::Invert]),
        ("grayscale", vec![E::Grayscale]),
        ("vignette", vec![E::Vignette]),
        ("blur, sharpen", vec![E::Blur { sigma: 3.0 }, E::Sharpen { amount: 0.5 }]),
        (
            "sharpen, invert, grayscale",
            vec![E::Sharpen { amount: 2.0 }, E::Invert, E::Grayscale],
        ),
        ("blur, invert, vignette", vec![E::Blur { sigma: 1.5 }, E::Invert, E::Vignette]),
    ] {
        for fg in [&m.testsrc, &m.noise] {
            for scale in [0.6, 0.4985] {
                for at in 0..=effects.len() {
                    let case = over(fg, &|c| {
                        c.transform.scale = scale;
                        let mut list = effects.clone();
                        list.insert(at, a_key("0x808080", 0.25, 0.2));
                        c.effects = list;
                    });
                    key_check(&format!("key-scale/{name}, the key at {at}, {} scale {scale}", fg.name), case);
                }
            }
        }
    }
    let case = over(&m.testsrc, &|c| {
        c.transform.scale = 0.6;
        c.color.contrast = 1.2;
        c.color.saturation = 1.3;
        c.effects = vec![a_key("green", 0.15, 0.1)];
    });
    key_check("key-scale/with an eq ahead", case);
    // The green-screen shape: scale 0.9, the key first, a grade (an `eq`) on the clip, and each effect after it.
    for (name, after) in [
        ("alone", vec![]),
        ("blur", vec![E::Blur { sigma: 1.5 }]),
        ("vignette", vec![E::Vignette]),
        ("sharpen", vec![E::Sharpen { amount: 0.8 }]),
    ] {
        let case = over(&m.testsrc, &|c| {
            c.transform.scale = 0.9;
            c.color.contrast = 1.15;
            c.color.saturation = 1.2;
            c.effects = [a_key("0x00b140", 0.25, 0.1)].into_iter().chain(after.clone()).collect();
        });
        key_check(&format!("key-scale/green screen, {name}"), case);
    }
    let cover = Delivery::new(960, 540, Fit::Cover);
    // Enlargements: the picture drawn bigger than it is (a 640x360 clip into a 960x540 frame, 1.5x, into 1280x720, 2x, under Cover and under
    // Contain, and shrunk back a little after the fit: 1.35x), every key position among the other effects, noise and a test card. (A zoom
    // in on top of a fit is the plan's own refusal, "a Cover clip that zooms" and "a zoom in under Contain", and is not drawn.)
    for (frame, scale) in [
        (Delivery::new(960, 540, Fit::Cover), 1.0),
        (Delivery::new(1280, 720, Fit::Cover), 1.0),
        (Delivery::new(960, 540, Fit::Contain), 1.0),
        (Delivery::new(960, 540, Fit::Contain), 0.9),
    ] {
        for fg in [&m.testsrc, &m.noise] {
            for (name, effects) in [
                ("alone", vec![]),
                ("blur", vec![E::Blur { sigma: 2.0 }]),
                ("sharpen", vec![E::Sharpen { amount: 1.5 }]),
                ("invert, grayscale", vec![E::Invert, E::Grayscale]),
            ] {
                for at in 0..=effects.len() {
                    let case = over_in(fg, Some(frame), &|c| {
                        c.transform.scale = scale;
                        let mut list = effects.clone();
                        list.insert(at, a_key("0x808080", 0.25, 0.2));
                        c.effects = list;
                    });
                    key_check(
                        &format!(
                            "key-scale/enlarged to {}x{} {:?}, {name}, the key at {at}, {} scale {scale}",
                            frame.width, frame.height, frame.fit, fg.name
                        ),
                        case,
                    );
                }
            }
        }
    }
}

/// The display probe (`display_probe`) runs FFmpeg's `yuv420p -> rgb24` as a bare `-pix_fmt rgb24` (an untagged composite) and through
/// `scale=in_color_matrix=<m>:in_range=tv` (the matrix a tag names); the engine's own still writes no conversion text, its PNG is made by the
/// scaler FFmpeg inserts for `-pix_fmt rgb24`. This ties the two: the compositor's composite of a small still, put through the probe's text
/// (`conversion_filter`), is the still's PNG to the sample, for an untagged composite (both texts) and for a tagged one (the matrix's text),
/// and so is the bare form for BT.601 whatever the build negotiates. (The RGB pipe is the same conversion as the PNG's: it is `rgb24` by the
/// same inserted scaler; kerf-core's own rendered tests hold it.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_display_probes_text_is_the_conversion_a_stills_png_runs() {
    use kerf_gpu::display_probe::conversion_filter;
    let m = media();
    let opts = ExportOptions::default();
    let cases: Vec<(&str, Timeline, Vec<Asset>, f64)> = vec![
        (
            "untagged noise",
            timeline(
                vec![vec![clip(&m.noise, 0.0, 0.2, 0.0)]],
                Some(Delivery::new(64, 36, Fit::Contain)),
            ),
            vec![m.noise.clone()],
            0.1,
        ),
        (
            "untagged test card",
            timeline(
                vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]],
                Some(Delivery::new(98, 54, Fit::Contain)),
            ),
            vec![m.testsrc.clone()],
            0.5,
        ),
        (
            "bt709-tagged test card",
            timeline(
                vec![vec![clip(&m.testsrc709, 0.0, 2.0, 0.0)]],
                Some(Delivery::new(64, 36, Fit::Contain)),
            ),
            vec![m.testsrc709.clone()],
            0.5,
        ),
        (
            "bt2020-tagged test card",
            timeline(
                vec![vec![clip(&m.testsrc2020, 0.0, 2.0, 0.0)]],
                Some(Delivery::new(64, 36, Fit::Contain)),
            ),
            vec![m.testsrc2020.clone()],
            0.5,
        ),
    ];
    let mut tied = 0;
    for (case, tl, assets, t) in cases {
        let plan = RenderPlan::at(&tl, &assets, &opts, t, policy()).expect("plan");
        let size = plan.size(u32::MAX);
        let why = reasons(&plan, size);
        assert!(why.is_empty(), "{case}: {why:?}");
        let (w, h) = (size.0 as usize, size.1 as usize);
        let png = target_dir()
            .join("parity-ref")
            .join(format!("display-tie-{}.png", case.replace(' ', "_")));
        std::fs::create_dir_all(png.parent().unwrap()).unwrap();
        export_still(&tl, &assets, &opts, t, &png, ImageFormat::Png, 0).expect("FFmpeg still");
        let reference = ffmpeg(&["-i", png.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
        let _ = std::fs::remove_file(&png);
        let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
        let planes = compositor().composite_yuv(&plan, &frames, size).expect("the planes");
        let matrix = plan.canvas.matrix;
        // The bare text is the untagged composite's (BT.601); the matrix's text is any tag's.
        let texts: Vec<(&str, String)> = std::iter::once(("scale", conversion_filter(matrix, true).unwrap()))
            .chain((matrix == kerf_core::YuvMatrix::Bt601).then(|| ("bare", "null".to_string())))
            .collect();
        for (how, filter) in texts {
            let got = ffmpeg_over_planes(&planes, (w, h), &filter, "rgb24");
            let (n, max) = differ(&got, &reference);
            assert_eq!(
                (n, max),
                (0, 0),
                "{case}: the probe's {how} text ({filter}) is not the still's PNG: {n} samples differ, max {max}"
            );
            tied += 1;
        }
    }
    assert!(tied >= 5, "{tied} ties");
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
    let exif_assets = std::slice::from_ref(&m.exif);
    match policy() {
        CompositeColorPolicy::FixedBt601 => {
            check_refused(
                "refused/exif-orientation",
                &tl,
                exif_assets,
                1.0,
                Refusal::Render("probed as"),
            );
        }
        CompositeColorPolicy::BottomLayerTag => {
            check_refused(
                "refused/exif-orientation (negotiated: a full-range picture)",
                &tl,
                exif_assets,
                1.0,
                Refusal::Plan("full-range picture in a still"),
            );
            let plan = RenderPlan::at(&tl, exif_assets, &ExportOptions::default(), 1.0, policy()).expect("plan");
            let err = kerf_gpu::decode_layer(&plan.layers[0]).expect_err("the decode should find the rotated picture");
            assert!(
                matches!(&err, kerf_gpu::GpuError::Unsupported(why) if why.contains("probed as")),
                "{err}"
            );
        }
        CompositeColorPolicy::Unknown => unreachable!("policy() never answers Unknown"),
    }

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
    // What the compositor does not draw of an effect or a mask: a chroma key on chroma that is not FFmpeg's to the
    // level (after a scale or a blur: its alpha moves by up to 1/blend levels a level of chroma), on another pixel
    // format or under a mask, a mask on a turned layer or one below full opacity, a blur, a sharpen or a vignette on
    // the padded frame of a letterbox, a vignette on a picture that is not yuv420p, a value the filters refuse.
    let effect = |e: kerf_core::VideoEffect| {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.effects = vec![e];
        timeline(vec![vec![c]], None)
    };
    let mut vignetted = clip(&m.yuv444, 0.0, 2.0, 0.0);
    vignetted.effects = vec![kerf_core::VideoEffect::Vignette];
    check_refused(
        "refused/vignette-on-a-444-picture",
        &timeline(vec![vec![vignetted]], None),
        std::slice::from_ref(&m.yuv444),
        0.5,
        Refusal::Plan("yuv420p"),
    );
    let keyed = |asset: &Asset, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(asset, 0.0, 2.0, 0.0);
        c.effects = vec![a_key("green", 0.15, 0.1)];
        edit(&mut c);
        timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None)
    };
    for (name, asset, edit, reason) in [
        (
            "on-a-444-picture",
            &m.yuv444,
            Box::new(|_: &mut Clip| {}) as Box<dyn Fn(&mut Clip)>,
            "yuv420p",
        ),
        (
            "under-a-mask",
            &m.testsrc,
            Box::new(|c: &mut Clip| c.mask = Some(a_mask(kerf_core::MaskShape::Rect, 0.1, false))),
            "over a chroma key",
        ),
        (
            "on-a-rotated-layer",
            &m.testsrc,
            Box::new(|c: &mut Clip| c.transform.rotation = 10.0),
            "chroma key on a rotated layer",
        ),
    ] {
        check_refused(
            &format!("refused/chroma-key-{name}"),
            &keyed(asset, &*edit),
            &[m.bars.clone(), asset.clone()],
            0.5,
            Refusal::Plan(reason),
        );
    }
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
    let mut vignetted = clip(&m.testsrc, 0.0, 2.0, 0.0);
    vignetted.effects = vec![kerf_core::VideoEffect::Vignette];
    check_refused(
        "refused/vignette-on-a-letterbox",
        &timeline(vec![vec![vignetted]], Some(Delivery::new(360, 640, Fit::Contain))),
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
            check_with(case, tl, assets, &[t], opts);
        } else {
            // Which reason, by variant: FFmpeg 6 only refuses what the scaler resizes, FFmpeg 9 any picture
            // under a running fade (the two messages share their words).
            let why = plan.reasons(&compositor().caps(), plan.size(u32::MAX));
            let want = |u: &Unsupported| match policy() {
                CompositeColorPolicy::FixedBt601 => matches!(u, Unsupported::FullRangeResized(..)),
                _ => matches!(u, Unsupported::FullRangeNegotiated(_) | Unsupported::FullRangeStill(_)),
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
/// pictures it was measured on (8-bit limited-range 4:2:0, untagged or BT.709), to the sample. The pictures it is not
/// drawn for were all measured off (levels of Y / UV: RGB 11 / 12, 10-bit 9 / 16, 4:4:4 16 / 17, 4:2:2 9 / 13) — and so
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
        // Not fading, the picture is drawn under FFmpeg 6 (the range is converted at the decode in a still) and refused under FFmpeg 9.
        let tl = timeline(vec![vec![clip(a, 0.0, 1.5, 0.0)]], None);
        check_range(&format!("full-range/plain {name}"), &tl, std::slice::from_ref(a), &[0.5]);
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
    // (a flat max of 16 / 15 / 11 levels off the compositor's): the name cannot tell the two apart, and a limited file may say
    // nothing of its range, so under FFmpeg 9 every deeper-than-8-bit picture is refused inside a fade, the limited
    // one included (the flag now settles the full-range one where it is recorded). FFmpeg 6 draws both.
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
        &opts,
    );
    let mut small = fading;
    small.transform.scale = 0.5;
    small.color.contrast = 1.2;
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 1.5, 0.0)], vec![small]], None);
    check_with("fade/small-graded-over-another", &tl, &assets, &[0.1, 0.25, 1.3], &opts);
    let mut tall = clip(&m.portrait, 0.0, 1.5, 0.0);
    tall.fade_in = 0.5;
    let tl = timeline(vec![vec![tall]], Some(Delivery::new(640, 360, Fit::Contain)));
    check_with("fade/letterboxed", &tl, std::slice::from_ref(&m.portrait), &[0.1, 0.3], &opts);
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
                let (apart, apart_max) = differ(export, &still);
                eprintln!("{case:<28} frame {k:<3} still vs export: {apart} samples differ, max {apart_max}");
                assert_eq!(
                    apart, 0,
                    "{case}: frame {k}: the FFmpeg still draws the transition differently from the export (max {apart_max})"
                );
                check_with(&case, &tl, &assets, &[t], &opts);
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
    check("clamped-end/nothing-drawn", &tl, &assets, &[1.99, 3.0]);
}

/// A stack whose layers carry different YCbCr matrices (or an RGB picture in a stack that is not BT.601) that the compositor
/// cannot take into the bottom layer's matrix. Under a fixed-matrix FFmpeg nothing converts them and the compositor draws
/// it like any other; under one that negotiates the matrix across the overlay chain, FFmpeg converts every layer into
/// the bottom layer's by swscale's two-step conversion through RGB, which the compositor reproduces for a plain opaque
/// layer (`check_converted`) and not for the rest (a translucent or graded layer, an RGB picture: the case holds the
/// refusal, with FFmpeg rendering it).
fn check_mixed(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64], reason: &'static str) {
    match policy() {
        CompositeColorPolicy::FixedBt601 => check(case, tl, assets, times),
        CompositeColorPolicy::BottomLayerTag => {
            check_refused(&format!("{case} (negotiated)"), tl, assets, times[0], Refusal::Plan(reason));
        }
        CompositeColorPolicy::Unknown => unreachable!("policy() never answers Unknown"),
    }
}

/// A stack whose layers carry different YCbCr matrices that the compositor draws: under a fixed-matrix FFmpeg as every
/// stack, under one that negotiates the matrix across the overlay chain each layer of another matrix than the bottom's
/// is converted into it (`kerf_gpu::convert`), and the composite is exactly the still's.
fn check_converted(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64]) {
    check_converted_with(case, tl, assets, times);
}

fn check_converted_with(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64]) {
    match policy() {
        CompositeColorPolicy::FixedBt601 => check(case, tl, assets, times),
        CompositeColorPolicy::BottomLayerTag => check(&format!("{case} (converted)"), tl, assets, times),
        CompositeColorPolicy::Unknown => unreachable!("policy() never answers Unknown"),
    }
}

/// A layer whose matrix is not the bottom layer's is converted into it by FFmpeg 9 (swscale's two-step conversion through RGB,
/// `kerf_gpu::convert`), and drawn exactly where the picture the chain made is not resized by the chain's last `scale`: every
/// pair of BT.601 / BT.709 / BT.2020 in either order, dissolving in and out, on busy pictures, at several times; a layer a `scale`
/// resizes (a picture-in-picture, a shrink, an enlargement, a Cover crop, a letterbox) is refused in a still (`check_resized`). The
/// stills hold the stack on screen as the graph (the bottom layer's tag is the composite's). What the conversion does not
/// cover is refused, each with FFmpeg rendering it (`check_mixed`): translucent, graded, an `eq` ahead of it, a fade to black running
/// on it, an RGB picture, a picture of odd size. Under a fixed-matrix FFmpeg none of it converts and all of it is drawn as it is.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn layers_of_another_matrix_are_converted_into_the_bottom_ones() {
    let m = media();
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
    // Every pair of matrices of the untagged (BT.601), BT.709 and BT.2020 pictures, both ways round; whole frame over whole frame
    // (the top covers the bottom: the frame is the top layer as converted), then a picture-in-picture (refused in a still).
    let matrices: [(&str, &Asset, &Asset); 3] = [
        ("601", &m.testsrc, &m.gradient),
        ("709", &m.testsrc709, &m.gradient709),
        ("2020", &m.testsrc2020, &m.gradient2020),
    ];
    for (bn, bottom, _) in &matrices {
        for (tn, top, _) in &matrices {
            if bn == tn {
                continue;
            }
            let assets = [(*bottom).clone(), (*top).clone()];
            let tl = timeline(vec![vec![clip(bottom, 0.0, 2.0, 0.0)], vec![clip(top, 0.0, 2.0, 0.0)]], None);
            check_converted(&format!("convert/{tn} over {bn}, whole frame"), &tl, &assets, &[0.5, 1.5]);
            let tl = timeline(vec![vec![clip(bottom, 0.0, 2.0, 0.0)], vec![pip(top)]], None);
            check_resized(&format!("convert/{tn} over {bn}, pip"), &tl, &assets, 0.5);
        }
    }
    // BT.601's coefficients under a tag (`smpte170m`) are the same matrix as no tag: nothing to convert in either order, and a
    // BT.709 clip over it is converted into it.
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![pip(&m.testsrc170m)]], None);
    check_converted(
        "convert/170m over untagged (one matrix)",
        &tl,
        &[m.bars.clone(), m.testsrc170m.clone()],
        &[0.5],
    );
    let tl = timeline(
        vec![
            vec![clip(&m.testsrc170m, 0.0, 2.0, 0.0)],
            vec![clip(&m.testsrc709, 0.0, 2.0, 0.0)],
        ],
        None,
    );
    check_converted(
        "convert/709 over 170m",
        &tl,
        &[m.testsrc170m.clone(), m.testsrc709.clone()],
        &[0.5],
    );
    // Three layers, two of them converted, one of them of the bottom's matrix.
    let tl = timeline(
        vec![
            vec![clip(&m.gradient709, 0.0, 2.0, 0.0)],
            vec![pip(&m.testsrc)],
            vec![{
                let mut c = pip(&m.bars709);
                c.transform.pos_x = -0.25;
                c
            }],
            vec![{
                let mut c = pip(&m.testsrc2020);
                c.transform.pos_x = -0.1;
                c.transform.pos_y = 0.3;
                c.transform.scale = 0.4;
                c
            }],
        ],
        None,
    );
    check_resized(
        "convert/three over a bottom, two converted",
        &tl,
        &[
            m.gradient709.clone(),
            m.testsrc.clone(),
            m.bars709.clone(),
            m.testsrc2020.clone(),
        ],
        0.5,
    );
    // The canvas's size and the picture's: shrunk, enlarged, letterboxed, Cover: refused in a still, the last scale resizes.
    for (name, delivery) in [
        ("shrunk", Delivery::new(320, 180, Fit::Contain)),
        ("enlarged", Delivery::new(960, 540, Fit::Contain)),
        ("cover", Delivery::new(360, 640, Fit::Cover)),
    ] {
        let tl = timeline(
            vec![
                vec![clip(&m.gradient709, 0.0, 2.0, 0.0)],
                vec![clip(&m.testsrc, 0.0, 2.0, 0.0)],
            ],
            Some(delivery),
        );
        check_resized(
            &format!("convert/untagged over 709, {name}"),
            &tl,
            &[m.gradient709.clone(), m.testsrc.clone()],
            0.5,
        );
    }
    // Letterboxed: a landscape picture on a portrait canvas, bars above and below it.
    let tl = timeline(
        vec![
            vec![clip(&m.gradient709, 0.0, 2.0, 0.0)],
            vec![clip(&m.testsrc, 0.0, 2.0, 0.0)],
        ],
        Some(Delivery::new(320, 640, Fit::Contain)),
    );
    check_resized(
        "convert/untagged letterboxed over 709",
        &tl,
        &[m.gradient709.clone(), m.testsrc.clone()],
        0.5,
    );
    // Busy: per-pixel noise (every Y, U, V and the clipped RGB), and the 1-px checkerboard, converted from a tag to another.
    let tl = timeline(
        vec![vec![clip(&m.gradient709, 0.0, 0.2, 0.0)], vec![clip(&m.noise, 0.0, 0.2, 0.0)]],
        None,
    );
    check_converted_with(
        "convert/noise over 709",
        &tl,
        &[m.gradient709.clone(), m.noise.clone()],
        &[0.1],
    );
    let tl = timeline(
        vec![
            vec![clip(&m.gradient2020, 0.0, 0.2, 0.0)],
            vec![clip(&m.checker, 0.0, 0.2, 0.0)],
        ],
        None,
    );
    check_converted_with(
        "convert/checkerboard over 2020",
        &tl,
        &[m.gradient2020.clone(), m.checker.clone()],
        &[0.1],
    );
    // A dissolve between clips of two matrices, at the start, the middle and the end of the ramp and after it (the dissolving
    // clip is converted ahead of its alpha plane, which the ramp scales).
    for (name, from, to) in [
        ("709 into untagged", &m.testsrc709, &m.testsrc),
        ("untagged into 709", &m.testsrc, &m.testsrc709),
        ("2020 into 709", &m.testsrc2020, &m.testsrc709),
    ] {
        for kind in [
            TransitionKind::Crossfade,
            TransitionKind::SlideLeft,
            TransitionKind::DipToBlack,
        ] {
            let mut incoming = clip(to, 0.0, 1.5, 1.0);
            incoming.transition_in = Some(Transition { kind, duration: 0.6 });
            let tl = timeline(vec![vec![clip(from, 0.0, 1.5, 0.0), incoming]], None);
            let assets = [from.clone(), to.clone()];
            let case = format!("convert/{name} through {kind:?}");
            // A dip to black fades the incoming picture to black before the conversion: refused while it runs.
            if kind == TransitionKind::DipToBlack && policy() == CompositeColorPolicy::BottomLayerTag {
                check_refused(&case, &tl, &assets, 1.1, Refusal::Plan("fade to black running"));
                check_converted(&case, &tl, &assets, &[0.5, 2.0]);
            } else {
                check_converted(&case, &tl, &assets, &[0.5, 1.1, 1.3, 1.5, 2.0]);
            }
        }
    }
    // Refused: what the conversion does not cover (FFmpeg draws the frame; under a fixed matrix the layers are drawn as they are).
    let over = |top: Asset, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(&top, 0.0, 2.0, 0.0);
        edit(&mut c);
        (
            timeline(vec![vec![clip(&m.bars709, 0.0, 2.0, 0.0)], vec![c]], None),
            vec![m.bars709.clone(), top],
        )
    };
    let (tl, assets) = over(m.testsrc.clone(), &|c| c.transform.opacity = 0.6);
    check_mixed("convert/translucent", &tl, &assets, &[0.5], "opacity below 1");
    let (tl, assets) = over(m.testsrc.clone(), &|c| {
        c.color = Color {
            contrast: 1.2,
            ..Color::default()
        }
    });
    check_mixed("convert/colour corrected", &tl, &assets, &[0.5], "colour correction");
    let (tl, assets) = over(m.testsrc.clone(), &|c| {
        c.effects = vec![kerf_core::VideoEffect::Blur { sigma: 2.0 }];
    });
    check_mixed("convert/effect", &tl, &assets, &[0.5], "video effect");
    // A pointwise effect is a table the scale applies ahead of the conversion, in an export (`motion_parity.rs`); a still's chain has no
    // converter of its own after it, and its composite is not the conversion of the effect's picture: refused there.
    for (name, effect) in [
        ("invert", kerf_core::VideoEffect::Invert),
        ("grayscale", kerf_core::VideoEffect::Grayscale),
    ] {
        let (tl, assets) = over(m.testsrc.clone(), &|c| c.effects = vec![effect.clone()]);
        check_mixed(
            &format!("convert/{name} on the converted layer"),
            &tl,
            &assets,
            &[0.5],
            "video effect",
        );
    }
    // (A still's `scale` takes a 4:2:2 or 4:4:4 picture to 4:2:0 and into the composite's matrix in one pass: refused there; the
    // export's conversion is held in `motion_parity.rs`.)
    for (name, picture) in [("4:4:4", &m.yuv444), ("4:2:2", &m.yuv422)] {
        let (tl, assets) = over(picture.clone(), &|_| {});
        check_mixed(&format!("convert/{name} over 709"), &tl, &assets, &[0.5], "in a still");
    }
    let (tl, assets) = over(m.testsrc.clone(), &|c| c.transform.rotation = 10.0);
    // (A turn that is not a quarter is no layer's to draw under any policy: refused before the conversion is asked.)
    check_refused("convert/rotated", &tl, &assets, 0.5, Refusal::Plan("rotation"));
    let (tl, assets) = over(m.testsrc.clone(), &|c| {
        c.mask = Some(a_mask(kerf_core::MaskShape::Ellipse, 0.1, false));
    });
    check_mixed("convert/masked", &tl, &assets, &[0.5], "mask");
    let (tl, assets) = over(m.odd.clone(), &|_| {});
    check_mixed("convert/odd size", &tl, &assets, &[0.5], "odd size");
    let (tl, assets) = over(m.png640.clone(), &|_| {});
    check_mixed("convert/rgb png over 709", &tl, &assets, &[0.5], "RGB picture");
    // A fade to black running on the converted clip: FFmpeg fades the picture first. At the clip's start (running), then after.
    let mut faded = clip(&m.testsrc, 0.0, 2.0, 0.0);
    faded.fade_in = 0.6;
    let tl = timeline(vec![vec![clip(&m.bars709, 0.0, 2.0, 0.0)], vec![faded]], None);
    let assets = [m.bars709.clone(), m.testsrc.clone()];
    if policy() == CompositeColorPolicy::BottomLayerTag {
        check_refused(
            "convert/fade-in running",
            &tl,
            &assets,
            0.2,
            Refusal::Plan("fade to black running"),
        );
    }
    check_converted("convert/fade-in finished", &tl, &assets, &[1.0]);
}

/// A layer of another matrix than the bottom's whose last `scale` resizes it, in a still: the graph has no `format` ahead of
/// the overlay, so the `scale` converts the matrix itself, in the same pass as the resize (swscale scales and converts to
/// `bgr24` at the destination size, then `ff_rgb24toyv12`), which is not the conversion of the picture the resize made
/// (measured: 47398 of 230400 luma samples differ on a half-size picture-in-picture). The case holds the refusal, with
/// FFmpeg rendering it; under a fixed-matrix FFmpeg the layers are drawn as they are. (An export's chain ends in `format`,
/// and its layers are converted by a `scale` of their own after it: `motion_parity.rs`.)
fn check_resized(case: &str, tl: &Timeline, assets: &[Asset], t: f64) {
    match policy() {
        CompositeColorPolicy::FixedBt601 => check(case, tl, assets, &[t]),
        CompositeColorPolicy::BottomLayerTag => {
            check_refused(
                &format!("{case} (negotiated)"),
                tl,
                assets,
                t,
                Refusal::Plan("last scale resizes it"),
            );
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
        check(name, &tl, std::slice::from_ref(a), &[0.5]);
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
    );
    // Mixed tags, any order: a picture-in-picture is scaled by the chain's last `scale`, which converts the matrix in the same pass: refused in a still.
    for (name, base, top) in [
        ("matrix/mixed-709-over-untagged", &m.gradient, &m.testsrc709),
        ("matrix/mixed-untagged-over-709", &m.gradient709, &m.testsrc),
        ("matrix/mixed-709-over-2020", &m.gradient2020, &m.testsrc709),
        ("matrix/mixed-2020-over-709", &m.gradient709, &m.testsrc2020),
    ] {
        let tl = timeline(vec![vec![clip(base, 0.0, 2.0, 0.0)], vec![pip(top)]], None);
        check_resized(name, &tl, &[base.clone(), top.clone()], 0.5);
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
        check(name, &tl, std::slice::from_ref(a), &[0.5]);
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
    );
    // Translucent untagged over a BT.709 base, and the other way round: mixed.
    let tl = timeline(vec![vec![clip(&m.bars709, 0.0, 2.0, 0.0)], vec![fade(&m.testsrc, 0.5)]], None);
    check_mixed(
        "translucent/untagged-over-bt709",
        &tl,
        &[m.bars709.clone(), m.testsrc.clone()],
        &[0.5],
        "other than the composite's",
    );
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![fade(&m.testsrc709, 0.5)]], None);
    check_mixed(
        "translucent/bt709-over-untagged",
        &tl,
        &[m.bars.clone(), m.testsrc709.clone()],
        &[0.5],
        "other than the composite's",
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
        check_range(name, &tl, &[m.testsrc.clone(), a.clone()], &[0.5]);
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
/// Those are **refused** (FFmpeg draws them); gray, which has no chroma to misplace, is
/// placed exactly but is refused as soon as it is resized (the exact scaler is 8-bit 4:2:0's). Every case is chosen so that the 4:2:0 and the native geometry
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
    // only ones drawn for a picture that is not 8-bit 4:2:0: the rest resize it.)
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
            // A Cover clip that zooms is scaled to cover the frame times the zoom in one resample and cut
            // by the canvas (no crop to the frame first): FFmpeg's, whatever the picture.
            if *fit == Fit::Cover && tf.scale != 1.0 {
                check_refused(&case, &tl, &assets, 0.5, Refusal::Plan("one resample"));
                continue;
            }
            // A picture that is not 8-bit 4:2:0 (gray included) is FFmpeg's whenever it is resized (and
            // every crop frame here resizes it), and also — said first — when its crop
            // lands between two 4:2:0 chroma samples: luma is exact and chroma is not.
            if finer || gray {
                let why = if finer && differs_here {
                    "4:2:0 chroma samples"
                } else {
                    "scales a"
                };
                check_refused(&case, &tl, &assets, 0.5, Refusal::Plan(why));
            } else {
                check(&case, &tl, &assets, &[0.5]);
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
            // for a picture that is not 8-bit 4:2:0; the Cover frames and the 9:16
            // letterbox resize it.
            let resized = geometry(*frame, *fit, &tf, native)
                .stages
                .iter()
                .any(|s| s.scaled != (s.src.w, s.src.h));
            if (finer || gray) && resized {
                check_refused(&case, &tl, assets, 0.5, Refusal::Plan("scales a"));
            } else {
                check(&case, &tl, assets, &[0.5]);
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
        check("unknown/untagged-bars", &single, std::slice::from_ref(&m.bars), &[0.5]);
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
    check_range(
        "full-range/ungraded",
        &graded(&m.fullrange, Color::default()),
        std::slice::from_ref(&m.fullrange),
        &[0.5],
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
        // (A gamma of 1.2: the limited-range pip is scaled, and a steeper one is the plan's own refusal, below.)
        pip.color = Color {
            gamma: 1.2,
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
/// video 51, an RGB PNG 69 — and a *shrink* as mild as 1.05-1.5x still reads a flat max
/// of 8-9 levels and up to 32 on edges, because the chroma kernel differs most near a
/// ratio of 1. No band of ratios was measured exact on both FFmpegs for busy chroma,
/// so none is claimed:
/// every resize of a picture that is not 8-bit 4:2:0 is FFmpeg's (10-bit 4:2:0 scaled and dithered at 10
/// bits is off by one in Y, U and V on 6.1.1; gray scaled in `gray` is 10 levels off over half the plane on
/// 9.0.2), and a picture left at its size is drawn.
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
        // ...and a picture left at its size is drawn, held to zero differing samples.
        check(&format!("same-size/{name}"), &scaled(a, 1.0, None), assets, &[0.5]);
    }
    // 8-bit 4:2:0 is what the compositor works in: resizing it is fine in both directions. 10-bit 4:2:0 and
    // gray are the decode's 8-bit 4:2:0 only at their own size.
    for (name, a) in [("yuv420p10le", &m.tenbit), ("gray", &m.gray)] {
        let assets = std::slice::from_ref(a);
        check_refused(
            &format!("refused/{name}-x1.5"),
            &scaled(a, 1.5, None),
            assets,
            0.5,
            Refusal::Plan("scales a"),
        );
        check_refused(
            &format!("refused/{name}-fit-to-960x540"),
            &scaled(a, 1.0, Some(Delivery::new(960, 540, Fit::Contain))),
            assets,
            0.5,
            Refusal::Plan("scales a"),
        );
        check_refused(
            &format!("refused/{name}-fit-to-576x324"),
            &scaled(a, 1.0, Some(Delivery::new(576, 324, Fit::Contain))),
            assets,
            0.5,
            Refusal::Plan("scales a"),
        );
        check(&format!("same-size/{name}"), &scaled(a, 1.0, None), assets, &[0.5]);
    }
    for (name, a) in [("yuv420p", &m.testsrc), ("yuvj420p-jpeg", &m.jpeg)] {
        let assets = std::slice::from_ref(a);
        // A zoom in of footage under Contain: FFmpeg takes it from the source in one resample, where
        // the compositor fits and then scales, so the plan refuses it (`FoldedFit`) and FFmpeg draws
        // it. A still's `duration` is 0, so its clip here has no length and the frame is empty: there
        // is nothing to refuse.
        if a.is_image() {
            check(&format!("enlarge-ok/{name}-x1.5"), &scaled(a, 1.5, None), assets, &[0.5]);
        } else {
            check_refused(
                &format!("enlarge-ok/{name}-x1.5"),
                &scaled(a, 1.5, None),
                assets,
                0.5,
                Refusal::Plan("one resample"),
            );
        }
        check(
            &format!("enlarge-ok/{name}-fit-to-960x540"),
            &scaled(a, 1.0, Some(Delivery::new(960, 540, Fit::Contain))),
            assets,
            &[0.5],
        );
    }
    check(
        "shrink-ok/yuv420p-fit-to-576x324",
        &scaled(&m.testsrc, 1.0, Some(Delivery::new(576, 324, Fit::Contain))),
        std::slice::from_ref(&m.testsrc),
        &[0.5],
    );
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
        check("old-asset/opaque", &tl, assets, &[0.5]);
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

/// A text case is held to the still like every other: the composite's planes are `drawtext`'s over the layers, sample for sample
/// (`tests/text.rs` holds the blend to `drawtext`'s own). A frame with **no layer** is blended in RGB with no conversion at all
/// (FFmpeg hands the black base to `drawtext` and on to the PNG as RGB), so there the compositor's RGB is the PNG, to the sample.
fn check_text(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64]) {
    check(case, tl, assets, times);
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
        check_text(name, &tl, one, times);
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
        check_text(&format!("text/size-{size}"), &tl(vec![o]), one, &[0.5]);
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
        if policy() == CompositeColorPolicy::FixedBt601 {
            check_text(&format!("text/over-{name}"), &tl, std::slice::from_ref(a), &[0.5]);
        } else {
            // (FFmpeg 9 keeps the picture's range through the graph and blends the text in it: the plan refuses the picture.)
            check_refused(
                &format!("text/over-{name}"),
                &tl,
                std::slice::from_ref(a),
                0.5,
                Refusal::Plan("a full-range picture in a still"),
            );
        }
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

/// A family installed here with no bold face (a bold request in it is `drawtext` thickened with a border).
fn regular_only_family() -> Option<String> {
    kerf_core::list_system_fonts()
        .into_iter()
        .find(|family| kerf_core::fonts::resolve_font_file(family, true).is_some_and(|(_, bold)| !bold))
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn small_print_is_drawn_as_ffmpeg_draws_it() {
    // Under 12 px every glyph bitmap shares samples (and 2x2 chroma blocks) with its neighbours, and
    // `drawtext` blends them in turn: the compositor blends the same bitmaps in the same order, so the
    // picture is `drawtext`'s to the sample (`tests/text.rs` and the plane-level case) and the whole
    // frame is the control's within the usual conversion bias.
    let m = media();
    let one = std::slice::from_ref(&m.testsrc);
    for px in [4.0, 7.0, 9.0, 11.0, 12.0] {
        let mut o = overlay("*Results vary. Terms apply. @kerf.studio", 0.0, 2.0);
        (o.size, o.pos_x, o.pos_y) = (px / 360.0, 0.37, 0.94);
        o.color = "white@0.7".into();
        let tl = with_text(timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None), vec![o]);
        let plan = RenderPlan::at(&tl, one, &ExportOptions::default(), 0.5, policy()).expect("plan");
        assert!(reasons(&plan, plan.size(u32::MAX)).is_empty(), "{px} px is drawn");
        check_text(&format!("text/small-{px}px"), &tl, one, &[0.5]);
        let mut boxed = overlay("Cmd + K", 0.0, 2.0);
        (boxed.size, boxed.pos_x, boxed.pos_y, boxed.bg) = (px / 360.0, 0.5, 0.1, Some("black@0.5".into()));
        let tl = with_text(timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None), vec![boxed]);
        check_text(&format!("text/small-{px}px-boxed"), &tl, one, &[0.5]);
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn bold_in_a_face_with_no_bold_is_drawn_as_ffmpeg_draws_it() {
    let Some(family) = regular_only_family() else {
        eprintln!("SKIPPED the synthetic-bold cases: every installed family has a bold face (the plane-level case still runs)");
        return;
    };
    let m = media();
    let one = std::slice::from_ref(&m.testsrc);
    for (name, size, pos, bg, fade) in [
        ("caption", 0.07, (0.5, 0.82), None, 0.0),
        ("boxed", 0.06, (0.5, 0.82), Some("black@0.5"), 0.0),
        ("title", 0.14, (0.4137, 0.4821), None, 0.0),
        ("small", 0.04, (0.51, 0.3), None, 0.0),
        ("fading", 0.07, (0.5, 0.6), Some("white@0.4"), 0.5),
    ] {
        let mut o = preset("Bold, thickened with a border: fi ffi Ay", pos, size, true, bg, fade);
        o.font = Some(family.clone());
        let tl = with_text(timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None), vec![o]);
        let plan = RenderPlan::at(&tl, one, &ExportOptions::default(), 0.7, policy()).expect("plan");
        assert!(plan.overlays[0].synthetic_bold, "{family} has no bold face");
        assert!(
            reasons(&plan, plan.size(u32::MAX)).is_empty(),
            "{name}: {:?}",
            reasons(&plan, plan.size(u32::MAX))
        );
        check_text(&format!("text/bold-border-{name}"), &tl, one, &[0.1, 0.7, 1.4]);
    }
    eprintln!("synthetic bold drawn in {family}");
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
    // The two the plan used to refuse, whose bitmaps share samples and chroma blocks: a thickened bold
    // (named below, and given the border the plan would give a face with no bold) and print under 12 px.
    let mut bold = overlay("Bold without a bold face, fi ffi", 0.0, 2.0);
    (bold.size, bold.pos_x, bold.pos_y, bold.bg) = (0.08, 0.5, 0.78, Some("black@0.4".into()));
    let mut print = overlay("*Results vary. Terms apply. @kerf.studio", 0.0, 2.0);
    (print.size, print.pos_x, print.pos_y) = (0.027, 0.31, 0.95);
    print.color = "white@0.7".into();
    for (name, asset) in [
        ("testsrc2", &m.testsrc),
        ("bt709 bars", &m.bars709),
        ("gradient", &m.gradient),
    ] {
        let tl = with_text(
            timeline(vec![vec![clip(asset, 0.0, 2.0, 0.0)]], None),
            vec![
                title.clone(),
                caption.clone(),
                solid.clone(),
                fading.clone(),
                bold.clone(),
                print.clone(),
            ],
        );
        for t in [0.2, 0.6, 1.4] {
            let mut plan =
                RenderPlan::at(&tl, std::slice::from_ref(asset), &ExportOptions::default(), t, policy()).expect("plan");
            for text in &mut plan.overlays {
                text.synthetic_bold = text.text.starts_with("Bold");
            }
            let size = plan.size(u32::MAX);
            assert!(reasons(&plan, size).is_empty(), "{:?}", reasons(&plan, size));
            let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
            let mut bare = plan.clone();
            bare.overlays.clear();
            let base = compositor().composite_yuv(&bare, &frames, size).expect("composite");
            let with = compositor().composite_yuv(&plan, &frames, size).expect("composite with text");
            let (mut y, mut u, mut v) = (base.y.clone(), base.u.clone(), base.v.clone());
            for text in &plan.overlays {
                let raster = kerf_gpu::text::raster(text, size).expect("rasterise");
                assert!(raster.exact, "{name} @ {t}: {:?} was blended as a sum", text.text);
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
    // Bold in a face with no bold (the plan marks it) and print under 12 px were refused for as long as text
    // was one mask of the sum of the glyphs' bitmaps; `drawtext` blends them in turn, and so does the
    // compositor now, on a build whose `drawtext` it can take apart (`caps.text`).
    assert!(compositor().caps().text, "this FFmpeg's drawtext is one the compositor knows");
    plan.overlays[0].synthetic_bold = true;
    assert!(reasons(&plan, size).is_empty(), "{:?}", reasons(&plan, size));
    compositor().render_plan(&plan, size).expect("a bold with a border is drawn");
    plan.overlays[0].synthetic_bold = false;
    plan.overlays[0].size = 0.03;
    assert!(reasons(&plan, size).is_empty(), "{:?}", reasons(&plan, size));
    compositor().render_plan(&plan, size).expect("an 11 px text is drawn");
    plan.overlays[0].size = 0.034;
    // What the plan can tell cannot be taken apart is FFmpeg's: a tab, and a text of over 160 characters.
    let text = plan.overlays[0].text.clone();
    plan.overlays[0].text = "a\ttab".into();
    let why = reasons(&plan, size).join("; ");
    assert!(why.contains("a tab in the text"), "{why}");
    plan.overlays[0].text = "word ".repeat(40);
    let why = reasons(&plan, size).join("; ");
    assert!(why.contains("200 characters of text"), "{why}");
    plan.overlays[0].text = text;
    assert!(reasons(&plan, size).is_empty(), "the control is drawn again");
    // A font file the plan cannot name (the default could not be written): FFmpeg's own default font.
    plan.overlays[0].font_file = None;
    let why = reasons(&plan, size).join("; ");
    assert!(why.contains("no font file"), "{why}");
    eprintln!("refused/text: a tab, a long text, no font file; border and small text drawn");
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
    // A complex graph, as the product's: an unflagged `scale` there runs the filter's own default algorithm (bilinear
    // before FFmpeg 5.0), where `-vf` is handed the command line's bicubic.
    let scale = format!("[0:v]scale={w}:{h}[o]");
    let mut args = vec!["-ss", &seek, "-i", &layer.path];
    if layer.is_image {
        args = vec!["-i", &layer.path];
    }
    args.extend([
        "-frames:v",
        "1",
        "-filter_complex",
        &scale,
        "-map",
        "[o]",
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
/// nothing is padded or cropped) against `ffmpeg -vf scale`: 0 differing samples in Y, U and V, at every size, on smooth footage and
/// on noise and a checkerboard, where every sample depends on the exact kernel.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_scaler_matches_ffmpegs_scale_plane_by_plane() {
    let m = media();
    let opts = ExportOptions::default();
    type Case<'a> = (&'a str, &'a Asset, f64, &'a [(u32, u32)]);
    let cases: [Case; 6] = [
        (
            "testsrc2",
            &m.testsrc,
            0.5,
            &[(480, 270), (320, 180), (960, 540), (1920, 1080), (64, 36), (32, 18)],
        ),
        ("bars", &m.bars, 0.5, &[(480, 270), (960, 540), (64, 36)]),
        ("gradient", &m.gradient, 0.5, &[(480, 270), (960, 540), (64, 36)]),
        ("noise", &m.noise, 0.1, &[(480, 270), (384, 216), (960, 540), (64, 36)]),
        (
            "noise-hd",
            &m.noise_hd,
            0.05,
            &[(640, 360), (320, 180), (128, 72), (96, 54), (64, 36), (32, 18)],
        ),
        ("checker", &m.checker, 0.1, &[(480, 270), (384, 216), (960, 540)]),
    ];
    let mut failures = Vec::new();
    let exact = kerf_gpu::scaler::kernels().exact();
    assert!(
        exact,
        "this FFmpeg's scaler is not the model: {:?}",
        kerf_gpu::scaler::kernels()
    );
    for (name, asset, t, sizes) in cases {
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
                if std::env::var_os("KERF_SCALER_DEBUG").is_some() && max > 0 {
                    let pw = if plane == "Y" { w as usize } else { w as usize / 2 };
                    let mut worst: Vec<(i32, usize, usize)> = got
                        .iter()
                        .zip(want.iter())
                        .enumerate()
                        .map(|(i, (a, b))| ((i32::from(*a) - i32::from(*b)).abs(), i % pw, i / pw))
                        .filter(|(d, _, _)| *d > 0)
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
                if max > 0 {
                    failures.push(format!("{name} -> {w}x{h} {plane}: max {max}, mean {mean:.3}"));
                }
            }
        }
    }
    // Past the steepest shrink the model is held to (`MAX_SHRINK`, 63:1: swscale needs a second scaler from 64:1) the plan declines and
    // FFmpeg draws it. (1280x720 into 20x11 is 64:1.)
    let tl = timeline(
        vec![vec![clip(&m.noise_hd, 0.0, m.noise_hd.duration, 0.0)]],
        Some(Delivery::new(20, 12, Fit::Contain)),
    );
    check_refused(
        "shrink/noise-hd-past-63:1",
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

/// The still runs a moving zoom as the last stage of the clip's chain, like the export: the placement's
/// `perspective`, after what is in front of it (a grade, a mask, an effect, the clip's fades). The compositor
/// draws that order and reproduces the placement, so every case is exact (`check_exact`: the planes, taken to
/// RGB by FFmpeg, are the still's PNG to the sample), a blur included: it is `gblur`'s fused-multiply-add recursion where
/// `blur_probe` says this FFmpeg runs that, and the plan refuses it otherwise.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_moving_zoom_is_placed_as_the_still_places_it() {
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
    // The harness itself: a layer drawn at its own size is the PNG exactly.
    check_exact(
        "control/plain",
        &timeline(vec![vec![clip(&m.testsrc, 0.0, 2.0, 0.0)]], None),
        std::slice::from_ref(&m.testsrc),
        &[0.5, 1.2],
    );
    check_placed("zoom/plain", &zooming(&|_| {}), &assets, &times);
    check_placed(
        "zoom/out with position",
        &zooming(&|c| c.keyframes = zoom_keys(1.0, 0.6, (0.2, -0.1))),
        &assets,
        &times,
    );
    check_placed(
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
    );
    check_placed(
        "zoom+mask",
        &zooming(&|c| c.mask = Some(a_mask(kerf_core::MaskShape::Ellipse, 0.2, false))),
        &assets,
        &times,
    );
    check_placed(
        "zoom+mask rect inverted",
        &zooming(&|c| c.mask = Some(a_mask(kerf_core::MaskShape::Rect, 0.1, true))),
        &assets,
        &times,
    );
    for (name, effect) in [
        ("sharpen", E::Sharpen { amount: 1.5 }),
        ("grayscale", E::Grayscale),
        ("invert", E::Invert),
    ] {
        check_placed(
            &format!("zoom+{name}"),
            &zooming(&|c| c.effects = vec![effect.clone()]),
            &assets,
            &times,
        );
    }
    check(
        "zoom+blur",
        &zooming(&|c| c.effects = vec![E::Blur { sigma: 5.0 }]),
        &assets,
        &times,
    );
    // The tutorial's clip: a feathered mask over a blurred picture, in a Cover frame, zooming in.
    let mut z = clip(&m.testsrc, 0.0, 2.0, 0.0);
    z.keyframes = zoom_keys(1.0, 1.25, (-0.05, 0.0));
    z.mask = Some(a_mask(kerf_core::MaskShape::Rect, 0.1, false));
    z.effects = vec![E::Blur { sigma: 6.0 }];
    let tl = timeline(
        vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]],
        Some(Delivery::new(960, 540, Fit::Cover)),
    );
    check("zoom+mask+blur cover", &tl, &assets, &times);
    // A fade-in running at the moment of the still: faded at the fit size, then zoomed.
    check_placed("zoom+fade-in", &zooming(&|c| c.fade_in = 1.0), &assets, &[0.6, 0.9, 1.2]);
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
    check_placed("zoom/dissolve+mask", &tl, &assets, &[1.1, 1.3, 1.5]);
}

/// Pictures of other shapes than the frame, at the size the fit leaves them (no resize, so no scaler): a 4:3 clip in a
/// 16:9 frame (480x360) and a picture of an odd width (203x360, cut with `crop ... exact=1`: `testsrc2` makes even sides).
fn shapes() -> &'static (Asset, Asset) {
    static S: OnceLock<(Asset, Asset)> = OnceLock::new();
    S.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-media-shapes");
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, source: &str, crop: &str| -> Asset {
            let p = dir.join(name);
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size={source}:rate=30:duration=2,{crop},setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown"),
                "-c:v",
                "ffv1",
                "-pix_fmt",
                "yuv420p",
                p.to_str().unwrap(),
            ]);
            probed(&p)
        };
        (
            make("pillar.mkv", "480x360", "null"),
            make("odd-width.mkv", "204x360", "crop=w=203:h=360:x=0:y=0:exact=1"),
        )
    })
}

/// A moving zoom and position over a picture of another shape, and one of another format or colour tag, in a still:
/// the placement is the same chain, and its pad, fringe and conversion are held exactly (`check_exact`). A 4:4:4 or 4:2:2
/// picture is refused (`PlacedFormat`, `tests/motion_parity.rs`: swscale's in-graph conversion is not the decode's on 4.4.2),
/// an odd height too (`pad` keeps or loses its last row by how the graph allocated the frame).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_placed_picture_of_another_shape_or_format_is_placed_as_the_still_places_it() {
    let m = media();
    let (pillar, odd_width) = shapes();
    // (Not 0.5: the clip starts there, at the identity pose, and a letterboxed picture at the identity pose is padded to the
    // whole frame by the still before the placement, which is refused: see below.)
    let times = [0.8, 1.2, 1.75];
    let over = |under: &Asset, asset: &Asset, from: f64, to: f64, pos: (f64, f64)| {
        let mut z = clip(asset, 0.0, 2.0, 0.5);
        z.keyframes = zoom_keys(from, to, pos);
        (
            timeline(vec![vec![clip(under, 0.0, 2.0, 0.0)], vec![z]], None),
            [under.clone(), asset.clone()],
        )
    };
    for (name, asset) in [("pillarboxed 480x360", pillar), ("odd width 203x360", odd_width)] {
        for (case, from, to, pos) in [
            ("zoom in", 1.0, 1.5, (0.0, 0.0)),
            ("zoom out", 1.0, 0.6, (0.1, -0.05)),
            ("pan", 1.0, 1.001, (-0.3, 0.2)),
        ] {
            let (tl, assets) = over(&m.bars, asset, from, to, pos);
            check_placed(&format!("shape/{name}/{case}"), &tl, &assets, &times);
        }
        let (tl, assets) = over(&m.bars, asset, 1.0, 1.5, (0.0, 0.0));
        check_refused(
            &format!("shape/{name}/at the identity pose"),
            &tl,
            &assets,
            0.5,
            Refusal::Plan("a letterboxed picture at the identity pose"),
        );
    }
    // The format, the range and the matrix tag: drawn where the plan draws them (what the composite colour policy
    // refuses for the cut is refused), refused as `PlacedFormat` for another layout.
    let times = [0.5, 1.0, 1.75];
    let cases: [(&str, &Asset, &Asset); 9] = [
        ("8-bit", &m.testsrc, &m.bars),
        ("10-bit", &m.tenbit, &m.bars),
        ("10-bit full range", &m.tenbit_pc, &m.bars),
        ("gray", &m.gray, &m.bars),
        ("full-range yuvj420p", &m.fullrange, &m.bars),
        ("bt709 over bt709", &m.testsrc709, &m.bars709),
        ("smpte170m", &m.testsrc170m, &m.bars),
        ("bt470bg", &m.testsrc, &m.bars470bg),
        ("bt2020 over bt2020", &m.testsrc2020, &m.bars2020),
    ];
    for (name, asset, under) in cases {
        let (tl, assets) = over(under, asset, 1.0, 1.6, (0.0, 0.0));
        let plan = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 1.0, policy()).expect("plan");
        let why = reasons(&plan, plan.size(u32::MAX));
        let case = format!("format/{name}");
        if why.is_empty() {
            check_placed(&case, &tl, &assets, &times);
        } else {
            let policy_says = [
                "holds a full-range picture",
                "different YCbCr matrices",
                "matrices",
                "full-range",
                "BT.2020",
            ];
            assert!(
                why.iter().all(|w| policy_says.iter().any(|p| w.contains(p))),
                "{case}: refused for something else: {why:?}"
            );
            eprintln!("{case}: refused for the composite colour policy of this FFmpeg: {why:?}");
        }
    }
    // 4:4:4 and 4:2:2: drawn where this FFmpeg converts them in a graph as the decode does (the probe), refused where it does not.
    for (name, asset) in [("yuv444p", &m.yuv444), ("yuv422p", &m.yuv422)] {
        let (tl, assets) = over(&m.bars, asset, 1.0, 1.6, (0.0, 0.0));
        if compositor().caps().placed_chroma {
            check_placed(&format!("format/{name}"), &tl, &assets, &times);
        } else {
            check_refused(
                &format!("format/{name}"),
                &tl,
                &assets,
                1.0,
                Refusal::Plan("a moving zoom or position on a"),
            );
        }
    }
    // A slide into a zooming clip, a position that moves alone, the travel of a push.
    for (name, kind) in [("slide", TransitionKind::SlideLeft), ("push", TransitionKind::PushRight)] {
        let out = clip(&m.bars, 0.0, 2.0, 0.0);
        let mut inc = clip(&m.testsrc, 0.0, 2.0, 1.0);
        inc.keyframes = zoom_keys(1.0, 1.4, (0.0, 0.0));
        inc.transition_in = Some(Transition { kind, duration: 0.6 });
        let tl = timeline(vec![vec![out, inc]], None);
        check_placed(
            &format!("transition/{name}"),
            &tl,
            &[m.bars.clone(), m.testsrc.clone()],
            &[1.1, 1.3, 1.5],
        );
    }
    let (mut moving, assets) = over(&m.bars, &m.testsrc, 1.0, 1.0, (0.0, 0.0));
    moving.tracks[1].clips[0].keyframes = vec![
        Keyframe {
            easing: Default::default(),
            time: 0.0,
            scale: 1.0,
            pos_x: -0.3,
            pos_y: 0.1,
            rotation: 0.0,
            opacity: 1.0,
        },
        Keyframe {
            easing: Default::default(),
            time: 1.5,
            scale: 1.0,
            pos_x: 0.25,
            pos_y: -0.15,
            rotation: 0.0,
            opacity: 1.0,
        },
    ];
    check_placed("position/over bars", &moving, &assets, &times);
}

/// A live breath, drift or shake in a still: the pose the plan carries at the moment is what the still's graph prints, and
/// the placement is the same pass.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn live_breath_drift_and_shake_are_placed_as_the_still_places_them() {
    use kerf_core::Behavior as B;
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let breathe = B::Breathe {
        amount: 0.15,
        speed: 1.3,
        phase: 0.2,
    };
    let drift = B::Drift {
        speed: 0.2,
        angle: 30.0,
        phase: 0.1,
    };
    let shake = B::Shake {
        amount: 0.03,
        speed: 2.5,
        seed: 7,
        phase: 0.0,
    };
    for (name, behaviors) in [
        ("breathe", vec![breathe]),
        ("drift", vec![drift]),
        ("shake", vec![shake]),
        ("all three", vec![breathe, drift, shake]),
    ] {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.5);
        c.behaviors = behaviors;
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None);
        check_placed(&format!("behavior/{name}"), &tl, &assets, &[0.6, 1.0, 1.37, 1.9]);
    }
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
    check("mask/9:16 cover crop fade-in", &tl, &assets, &[0.2, 0.4, 1.0]);
    // Over a busy picture, a mask is a mask: noise under a feathered ellipse.
    let mut c = clip(&m.noise, 0.0, 0.2, 0.0);
    c.mask = Some(a_mask(kerf_core::MaskShape::Ellipse, 0.3, false));
    let tl = timeline(vec![vec![clip(&m.black, 0.0, 2.0, 0.0)], vec![c]], None);
    check("mask/over noise", &tl, &[m.black.clone(), m.noise.clone()], &[0.1]);
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
        check_range(
            &format!("mask/on-{label}"),
            &timeline(vec![vec![c]], None),
            std::slice::from_ref(asset),
            &[0.5],
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
        ("vignette", vec![E::Vignette]),
        (
            "chain",
            vec![E::Sharpen { amount: 1.0 }, E::Grayscale, E::Blur { sigma: 2.0 }, E::Invert],
        ),
        ("vignette-chain", vec![E::Invert, E::Vignette, E::Blur { sigma: 2.0 }]),
        ("sharpen-blur", vec![E::Sharpen { amount: 2.5 }, E::Blur { sigma: 3.0 }]),
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
        let assets = [m.bars.clone(), m.testsrc.clone()];
        if name == "rotated" {
            check_refused(&format!("effect/{name}"), &tl, &assets, 0.5, Refusal::Plan("a rotation"));
        } else {
            check(&format!("effect/{name}"), &tl, &assets, &[0.5]);
        }
    }
    // A vignette's map is the picture's at the size the filter sees it: scaled, odd-sized, in a Cover frame.
    for (name, edit, format) in [
        (
            "scaled",
            Box::new(|c: &mut Clip| c.transform.scale = 0.6) as Box<dyn Fn(&mut Clip)>,
            None,
        ),
        ("odd", Box::new(|c: &mut Clip| c.transform.scale = 0.33), None),
        ("graded", Box::new(|c: &mut Clip| c.color.contrast = 1.25), None),
        (
            "cover",
            Box::new(|_: &mut Clip| {}),
            Some(Delivery::new(960, 540, Fit::Cover)),
        ),
    ] {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        edit(&mut c);
        c.effects = vec![E::Vignette];
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], format);
        check(
            &format!("effect/vignette-{name}"),
            &tl,
            &[m.bars.clone(), m.testsrc.clone()],
            &[0.5],
        );
    }
    // A sharpen after a scale or a blur is drawn at any amount, chained: the scaler is swscale's and the blur `gblur`'s to the sample,
    // so the filter's gain has no difference to amplify.
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
        check(
            &format!("effect/{name}-after-a-scale"),
            &tl,
            &[m.bars.clone(), m.testsrc.clone()],
            &[0.5],
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
        check(
            &format!("effect/{name}"),
            &timeline(vec![vec![c]], None),
            std::slice::from_ref(&m.testsrc),
            &[0.5],
        );
    }
    // A key after a blur is drawn too: the blur is `gblur`'s to the sample, so the chroma the key reads is FFmpeg's.
    let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
    c.effects = vec![E::Blur { sigma: 2.0 }, a_key("green", 0.15, 0.1)];
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None);
    check(
        "effect/chroma-key-after-a-blur",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        &[0.5],
    );
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
            check(&format!("effect/{name}-on-{label}"), &tl, std::slice::from_ref(asset), &[0.5]);
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
    check_range(
        "effect/grayscale-on-full-range",
        &on(&m.fullrange, E::Grayscale),
        std::slice::from_ref(&m.fullrange),
        &[0.5],
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
        check(&format!("effect/noise-{name}"), &tl, std::slice::from_ref(&m.noise), &[0.1]);
    }
}

fn a_key(color: &str, similarity: f64, blend: f64) -> kerf_core::VideoEffect {
    kerf_core::VideoEffect::ChromaKey {
        color: color.into(),
        similarity,
        blend,
    }
}

/// A chroma key in a still: the keyed layer over the bars, against FFmpeg's still.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn chroma_keys_are_drawn_as_the_filter_draws_them() {
    let m = media();
    for (name, effects) in [
        ("green", vec![a_key("green", 0.15, 0.1)]),
        ("green-wide", vec![a_key("green", 0.3, 0.3)]),
        ("hard", vec![a_key("0x2050a0", 0.2, 0.0)]),
        ("magenta", vec![a_key("magenta", 0.25, 0.05)]),
        (
            "after-tables",
            vec![
                kerf_core::VideoEffect::Invert,
                a_key("magenta", 0.25, 0.05),
                kerf_core::VideoEffect::Sharpen { amount: 1.0 },
            ],
        ),
        ("twice", vec![a_key("green", 0.1, 0.1), a_key("0x2050a0", 0.2, 0.2)]),
    ] {
        for (asset, label) in [(&m.testsrc, "testsrc2"), (&m.gradient, "gradient")] {
            let mut c = clip(asset, 0.0, 2.0, 0.0);
            c.effects = effects.clone();
            let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None);
            check(
                &format!("chroma-key/{name}-{label}"),
                &tl,
                &[m.bars.clone(), asset.clone()],
                &[0.5, 1.5],
            );
        }
    }
    // Graded first (`eq` runs ahead of the key), and in a frame the picture is the canvas of.
    let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
    c.color.saturation = 1.4;
    c.effects = vec![a_key("green", 0.2, 0.15)];
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None);
    check("chroma-key/graded", &tl, &[m.bars.clone(), m.testsrc.clone()], &[0.5]);
    // Cut without a scale: a Cover crop of a 640x360 picture into 640x300, and a crop on top of it (the
    // cropped 640x324 covers the frame as it is); and a vignette ahead of the key (exact, so the key is).
    let cover = Some(Delivery::new(640, 300, Fit::Cover));
    for (name, edit, format) in [
        ("cover", Box::new(|_: &mut Clip| {}) as Box<dyn Fn(&mut Clip)>, cover),
        ("cropped-cover", Box::new(|c: &mut Clip| c.transform.crop_top = 0.1), cover),
        (
            "after-a-vignette",
            Box::new(|c: &mut Clip| c.effects.insert(0, kerf_core::VideoEffect::Vignette)),
            None,
        ),
    ] {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.effects = vec![a_key("green", 0.2, 0.15)];
        edit(&mut c);
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], format);
        check(
            &format!("chroma-key/{name}"),
            &tl,
            &[m.bars.clone(), m.testsrc.clone()],
            &[0.5],
        );
    }
    // Every sample its own colour: the window's edges and every grade of alpha.
    let mut c = clip(&m.noise, 0.0, m.noise.duration, 0.0);
    c.effects = vec![a_key("0x808080", 0.05, 0.3)];
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None);
    check("chroma-key/noise", &tl, &[m.bars.clone(), m.noise.clone()], &[0.1]);
}

/// The key's alpha plane against `chromakey`'s, sample for sample, at FFmpeg's own thread count (the
/// compositor's default is what an export on this machine runs).
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_chroma_key_alpha_is_the_filters_plane_by_plane() {
    use kerf_gpu::effects::{chroma_key_uv, ChromaKeyOptions};
    let m = media();
    let opts = ExportOptions::default();
    let mut failures = Vec::new();
    for (asset, label, t) in [
        (&m.noise, "noise", 0.1),
        (&m.testsrc, "testsrc2", 0.5),
        (&m.gradient, "gradient", 0.5),
    ] {
        for (color, similarity, blend) in [
            ("green", 0.15, 0.1),
            ("0x808080", 0.05, 0.3),
            ("0x2050a0", 0.2, 0.0),
            ("magenta", 0.01, 1.0),
        ] {
            let mut c = clip(asset, 0.0, asset.duration, 0.0);
            c.effects = vec![a_key(color, similarity, blend)];
            let tl = timeline(vec![vec![c]], None);
            let plan = RenderPlan::at(&tl, std::slice::from_ref(asset), &opts, t, policy()).expect("plan");
            let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
            let frame = frames[0].as_ref().expect("a frame");
            let key = ChromaKeyOptions {
                uv: chroma_key_uv(kerf_core::color_rgb(color).expect("colour")),
                similarity: similarity as f32,
                blend: blend as f32,
            };
            let got = compositor().chroma_key_alpha(frame, &key).expect("alpha");
            let filter = format!("format=yuva420p,chromakey={color}:{similarity}:{blend}");
            let seek = format!("{}", plan.layers[0].source_time);
            let threads = kerf_core::cpu_graph_threads().to_string();
            let raw = ffmpeg(&[
                "-filter_threads",
                &threads,
                "-ss",
                &seek,
                "-i",
                &plan.layers[0].path,
                "-frames:v",
                "1",
                "-vf",
                &filter,
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuva420p",
                "pipe:1",
            ]);
            let (w, h) = (frame.width as usize, frame.height as usize);
            let chroma = w.div_ceil(2) * h.div_ceil(2);
            let want = &raw[w * h + 2 * chroma..];
            assert_eq!(want.len(), w * h);
            let differing = got.iter().zip(want).filter(|(a, b)| a != b).count();
            let line = format!(
                "chroma-key-planes/{label:<8} {color:<9} {similarity} {blend}: differing {differing} of {}",
                w * h
            );
            eprintln!("{line}");
            report()
                .lock()
                .unwrap()
                .insert(format!("chroma-key-planes {label} {color} {similarity} {blend}"), line);
            if differing > 0 {
                let at: Vec<(usize, usize)> = got
                    .iter()
                    .zip(want)
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .map(|(i, _)| (i % w, i / w))
                    .take(8)
                    .collect();
                failures.push(format!("{label} {color} {similarity} {blend}: {differing} differ, at {at:?}"));
            }
        }
    }
    write_report();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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
    // (name, the effect, FFmpeg's filter for it)
    let cases: Vec<(&str, E, String)> = vec![
        ("invert", E::Invert, "negate".into()),
        ("grayscale", E::Grayscale, "hue=s=0".into()),
        ("vignette", E::Vignette, "vignette".into()),
        (
            "sharpen 1",
            E::Sharpen { amount: 1.0 },
            "unsharp=luma_msize_x=5:luma_msize_y=5:luma_amount=1".into(),
        ),
        (
            "sharpen 2.5",
            E::Sharpen { amount: 2.5 },
            "unsharp=luma_msize_x=5:luma_msize_y=5:luma_amount=2.5".into(),
        ),
        (
            "sharpen 0.7",
            E::Sharpen { amount: 0.7 },
            "unsharp=luma_msize_x=5:luma_msize_y=5:luma_amount=0.7".into(),
        ),
        (
            "sharpen -2",
            E::Sharpen { amount: -2.0 },
            "unsharp=luma_msize_x=5:luma_msize_y=5:luma_amount=-2".into(),
        ),
        ("blur 0.5", E::Blur { sigma: 0.5 }, "gblur=sigma=0.5".into()),
        ("blur 2", E::Blur { sigma: 2.0 }, "gblur=sigma=2".into()),
        ("blur 6", E::Blur { sigma: 6.0 }, "gblur=sigma=6".into()),
        ("blur 6.3", E::Blur { sigma: 6.3 }, "gblur=sigma=6.3".into()),
        ("blur 25", E::Blur { sigma: 25.0 }, "gblur=sigma=25".into()),
        ("blur 100", E::Blur { sigma: 100.0 }, "gblur=sigma=100".into()),
    ];
    let mut failures = Vec::new();
    for (asset, label, t) in [
        (&m.noise, "noise", 0.1),
        (&m.testsrc, "testsrc2", 0.5),
        (&m.bars, "bars", 0.5),
    ] {
        for (name, effect, filter) in &cases {
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
                if differing != 0 {
                    failures.push(format!(
                        "{label} {name} {plane}: max {max}, differing {differing} of {}",
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
    check("label/untagged-hd-alone", &tl, std::slice::from_ref(untagged), &[0.5]);
    let mut fade = clip(untagged, 0.0, 1.0, 0.0);
    fade.transform.opacity = 0.5;
    let tl = timeline(vec![vec![fade]], hd_canvas());
    assert_eq!(labelled_layers(&tl, std::slice::from_ref(untagged), 0.5), [false]);
    check(
        "label/untagged-hd-alone-translucent",
        &tl,
        std::slice::from_ref(untagged),
        &[0.5],
    );
    let (tl, assets) = hd_stack(h.get("detail/hd-untagged"), untagged, 1.0);
    assert_eq!(labelled_layers(&tl, &assets, 0.5), [false, false]);
    check("label/untagged-hd-over-untagged-hd", &tl, &assets, &[0.5]);

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
        check(name, &tl, &assets, &[0.5]);
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
    check("label/untagged-hd-before-709-still", &late, &assets, &[0.5]);
}

/// The dip to white takes the clip through `rgb24` and back, out of YUV with the clip's matrix, so a label
/// that came after the fit `scale` (the filter FFmpeg has convert the picture for the fade) was read as BT.601
/// there and BT.709 on the way back: the still of a labelled HD layer mid-dip differs from the layer tagged
/// in the file by 8 to 15 levels. The label is the first filter of the chain; the layer is drawn as BT.709.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_labelled_hd_layer_in_a_dip_to_white_is_drawn_as_the_layer_tagged_in_the_file() {
    let h = hd_media();
    let (untagged, tagged) = (h.get("grad/hd-untagged"), h.get("grad/hd-709"));
    let phone = h.get("detail/hd-709");
    let canvas = Some(Delivery::new(1280, 720, Fit::Contain));
    let dipping = |a: &Asset| {
        let mut incoming = clip(a, 0.0, 1.0, 1.0);
        incoming.transition_in = Some(Transition {
            kind: TransitionKind::DipToWhite,
            duration: 0.6,
        });
        timeline(
            vec![vec![clip(phone, 0.0, 1.0, 0.6)], vec![clip(phone, 0.0, 1.0, 0.0), incoming]],
            canvas,
        )
    };
    let (tl, assets) = (dipping(untagged), [untagged.clone(), phone.clone()]);
    assert_eq!(
        labelled_layers(&tl, &assets, 1.15),
        [false, true],
        "the BT.709 clip is on screen with it"
    );
    check("label/dip-white-labelled-hd-still", &tl, &assets, &[1.05, 1.15, 1.25]);
    let (twin, assets) = (dipping(tagged), [tagged.clone(), phone.clone()]);
    assert_eq!(
        labelled_layers(&twin, &assets, 1.15),
        [false, false],
        "the tagged file needs no label"
    );
    check("label/dip-white-tagged-hd-still", &twin, &assets, &[1.05, 1.15, 1.25]);
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
        check(name, &tl, &assets, &[0.5]);
    }
    // …and against a BT.709 clip it is the other matrix, as it was: refused where FFmpeg negotiates.
    for (name, base, top) in [
        ("label/sd-untagged-over-709", phone, h.get("detail/sd-untagged")),
        ("label/709-over-sd-untagged", h.get("detail/sd-untagged"), phone),
    ] {
        let (tl, assets) = hd_stack(base, top, 1.0);
        assert!(labelled_layers(&tl, &assets, 0.5).iter().all(|l| !l), "{name}");
        check_resized(name, &tl, &assets, 0.5);
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
        check_resized(name, &tl, &assets, 0.5);
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
    check("label/png-logo-under-untagged-hd", &tl, &assets, &[0.5]);
    let mut veil = clip(logo, 0.0, 1.0, 0.0);
    veil.transform.opacity = 0.5;
    let tl = timeline(vec![vec![clip(untagged, 0.0, 1.0, 0.0)], vec![veil]], hd_canvas());
    let assets = [untagged.clone(), logo.clone()];
    assert_eq!(labelled_layers(&tl, &assets, 0.5), [false, false]);
    check("label/png-logo-translucent-over-untagged-hd", &tl, &assets, &[0.5]);
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
        check_range(&format!("label/{name}-under-untagged-hd"), &tl, &assets, &[0.5]);
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
        // (The 720p clips are shrunk into the 360p canvas, and swscale converts a full-range picture's range inside the resize:
        // the plan refuses the one it flags full range in a still, on either FFmpeg.)
        check_refused(
            name,
            &tl,
            &assets,
            0.5,
            Refusal::Plan("full-range picture in a still that the scaler resizes"),
        );
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
        check(name, &tl, &assets, &[0.5]);
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
                check(name, &tl, &assets, &[0.5]);
            }
        }
        None => eprintln!("skipped: this FFmpeg cannot make the HLG test clip"),
    }
    // An SD proxy is as it was, and beside a BT.709 proxy is refused where the matrix is negotiated.
    let sd = proxies.of(h.get("detail/sd-untagged"));
    let (tl, assets) = hd_stack(&phone, &sd, 1.0);
    check_resized("proxy/sd-untagged-over-709", &tl, &assets, 0.5);
    let (tl, assets) = hd_stack(&untagged, &sd, 1.0);
    check("proxy/sd-untagged-over-untagged-hd", &tl, &assets, &[0.5]);
}

// ---- recorded numbers -----------------------------------------------------------
//
// There is no table of figures and no limit to keep one against: **every drawn case is held to 0 differing samples** (the still's
// `yuv444p` planes, its PNG, and the display's RGB), on FFmpeg 6.1.1 (policy `FixedBt601`) and on the pinned 9.0.2 build (what the
// Windows and macOS bundles ship; policy `BottomLayerTag`), strictly (no `KERF_PARITY_EXPLORE`), on Mesa lavapipe. A stack the plan
// declines on one of them (mixed matrices, an RGB picture in a non-BT.601 stack, an asset that never recorded its pixel format, a
// full-range picture in a still where the range is negotiated, a resize of a picture the scaler does not resize exactly) is asserted
// refused rather than compared, and `drawn_anyway` shows each refusal that must be needed to differ. Every run rewrites
// `target/parity/report.txt` (the differing-sample counts and the decode / composite time of the GPU path); the figures in it are
// all zero for a case that is drawn.
//

// ---- a yuv420p flagged full range -----------------------------------------------------------

/// The pictures of `a_yuv420p_flagged_full_range_is_refused_where_ffmpeg_reads_its_raw_samples`: the same
/// pattern in lossless FFV1 (which keeps the format `yuv420p` whatever the flag; H.264 would call a full-range
/// stream `yuvj420p`), flagged full range, flagged limited and not flagged.
struct RangeMedia {
    /// The test pattern, and a smooth gradient (so a key's alpha moves everywhere), flagged `-color_range pc`.
    pc: Asset,
    pc_gradient: Asset,
    /// The pattern and the gradient flagged `tv` and not flagged at all.
    tv: Asset,
    unflagged: Asset,
    tv_gradient: Asset,
    unflagged_gradient: Asset,
}

fn range_media() -> &'static RangeMedia {
    static R: OnceLock<RangeMedia> = OnceLock::new();
    R.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-media-range");
        std::fs::create_dir_all(&dir).unwrap();
        let testsrc = "testsrc2=size=640x360:rate=30:duration=2";
        let gradients = "gradients=size=640x360:rate=30:duration=2:c0=0xd03020:c1=0x2040e0:c2=0x30c060:nb_colors=3:seed=11:x0=0:y0=0:x1=640:y1=360:speed=0.00001,format=yuv420p";
        let make = |name: &str, lavfi: &str, flag: Option<&str>| -> Asset {
            let p = dir.join(name);
            let mut args = vec!["-f", "lavfi", "-i", lavfi, "-c:v", "ffv1", "-pix_fmt", "yuv420p"];
            if let Some(flag) = flag {
                args.extend(["-color_range", flag]);
            }
            args.push(p.to_str().unwrap());
            ffmpeg(&args);
            let a = probed(&p);
            assert_eq!(a.streams[0].pix_fmt.as_deref(), Some("yuv420p"), "{name}: the format says nothing of the range");
            // (An unflagged FFV1 file reads `tv` from FFmpeg 9's muxer and nothing from 6.1.1's: neither is full range.)
            match flag {
                Some(flag) => assert_eq!(a.streams[0].color_range.as_deref(), Some(flag), "{name}: the probe records the flag"),
                None => assert_ne!(a.streams[0].color_range.as_deref(), Some("pc"), "{name}"),
            }
            a
        };
        RangeMedia {
            pc: make("pc.mkv", testsrc, Some("pc")),
            pc_gradient: make("pc-gradient.mkv", gradients, Some("pc")),
            tv: make("tv.mkv", testsrc, Some("tv")),
            unflagged: make("unflagged.mkv", testsrc, None),
            tv_gradient: make("tv-gradient.mkv", gradients, Some("tv")),
            unflagged_gradient: make("unflagged-gradient.mkv", gradients, None),
        }
    })
}

/// How many samples of the compositor's frame of `tl` at `t` differ from FFmpeg's still (its `yuv444p` planes against the composite's, put
/// through the same conversion), **whatever the plan says**: the frame the plan refuses, drawn anyway, to show the refusal is needed
/// and not just cautious. Returns the count and the largest difference.
fn drawn_anyway(tl: &Timeline, assets: &[Asset], t: f64) -> (usize, i32) {
    still_planes_differ(tl, assets, t)
}

/// `yuv420p` is the format every overlay takes, so a `yuv420p` picture flagged full range (a VP9, HEVC or AV1
/// file; ffprobe names it `yuvj420p` only where the decoder says so) reaches FFmpeg's filters **raw**, on 6.1.1 and
/// on 9.0.2, where the compositor's decode has converted it to limited range. Anything that reads the values
/// disagrees: a vignette (10 to 13 levels off), a fade (9 to 14), `eq`, and on 9.0.2 the chroma key (19 on the gradient, 73 on
/// the test pattern). A plain
/// layer is the same picture, so it is drawn (on 9.0.2 an export is refused as a cut, `FullRangeCut`, as a
/// `yuvj` clip's is). The same file flagged `tv`, or not at all, is drawn as before, and so is an asset stored
/// before the flag was recorded (`color_range` `None`), which is what the plan knows of it.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_yuv420p_flagged_full_range_is_refused_where_ffmpeg_reads_its_raw_samples() {
    use kerf_core::VideoEffect as E;
    let m = media();
    let r = range_media();
    let fixed = policy() == CompositeColorPolicy::FixedBt601;
    eprintln!("composite colour policy of this FFmpeg: {:?}", policy());
    let one = |a: &Asset, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(a, 0.0, 2.0, 0.0);
        edit(&mut c);
        timeline(vec![vec![c]], None)
    };
    let over_bars = |a: &Asset, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(a, 0.0, 2.0, 0.0);
        edit(&mut c);
        (
            timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None),
            vec![m.bars.clone(), a.clone()],
        )
    };

    // What FFmpeg's graph reads raw: refused by the plan (and FFmpeg renders the frame), one reason each.
    for (name, effects, why) in [
        ("vignette", vec![E::Vignette], "vignette"),
        ("invert", vec![E::Invert], "full-range"),
        ("blur", vec![E::Blur { sigma: 3.0 }], "full-range"),
        ("sharpen", vec![E::Sharpen { amount: 1.0 }], "full-range"),
    ] {
        check_refused(
            &format!("range/pc-{name}"),
            &one(&r.pc, &|c| c.effects = effects.clone()),
            std::slice::from_ref(&r.pc),
            0.5,
            Refusal::Plan(why),
        );
    }
    let (tl, assets) = over_bars(&r.pc_gradient, &|c| c.effects = vec![a_key("green", 0.3, 0.3)]);
    check_refused("range/pc-chroma-key", &tl, &assets, 0.5, Refusal::Plan("chroma key"));
    check_refused(
        "range/pc-graded",
        &one(&r.pc, &|c| c.color.gamma = 1.4),
        std::slice::from_ref(&r.pc),
        0.5,
        Refusal::Plan("colour correction"),
    );
    // A fade running on it, under either FFmpeg; the same fade over is the plain picture.
    let fading = |a: &Asset| one(a, &|c| c.fade_in = 1.0);
    check_refused(
        "range/pc-fade-running",
        &fading(&r.pc),
        std::slice::from_ref(&r.pc),
        0.5,
        Refusal::Plan("full-range picture inside a fade"),
    );
    check_range("range/pc-fade-over", &fading(&r.pc), std::slice::from_ref(&r.pc), &[1.5]);

    // The same picture in the same graphs, flagged limited or not flagged: drawn, exactly as before.
    for (label, a) in [("tv", &r.tv), ("unflagged", &r.unflagged)] {
        for (name, effects) in [
            ("vignette", vec![E::Vignette]),
            ("invert", vec![E::Invert]),
            ("blur", vec![E::Blur { sigma: 3.0 }]),
        ] {
            check(
                &format!("range/{label}-{name}"),
                &one(a, &|c| c.effects = effects.clone()),
                std::slice::from_ref(a),
                &[0.5],
            );
        }
        check(
            &format!("range/{label}-graded"),
            &one(a, &|c| c.color.gamma = 1.4),
            std::slice::from_ref(a),
            &[0.5],
        );
        check(
            &format!("range/{label}-fade-running"),
            &fading(a),
            std::slice::from_ref(a),
            &[0.5],
        );
    }
    for (label, a) in [("tv", &r.tv_gradient), ("unflagged", &r.unflagged_gradient)] {
        let (tl, assets) = over_bars(a, &|c| c.effects = vec![a_key("green", 0.3, 0.3)]);
        check(&format!("range/{label}-chroma-key"), &tl, &assets, &[0.5]);
    }

    // A plain picture agrees with FFmpeg's: alone, and after a limited one on the same track; shrunk over bars or enlarged
    // it does not (swscale converts the range inside the resize), and the plan refuses it in a still. (Under FFmpeg 9 an *export*
    // of the cut is refused as a whole.)
    let pc = std::slice::from_ref(&r.pc);
    check_range("range/pc-plain", &one(&r.pc, &|_| {}), pc, &[0.5]);
    let (tl, assets) = over_bars(&r.pc, &|c| c.transform.scale = 0.5);
    check_refused(
        "range/pc-scaled-over-bars",
        &tl,
        &assets,
        0.5,
        Refusal::Plan("full-range picture in a still that the scaler resizes"),
    );
    let enlarged = timeline(
        vec![vec![clip(&r.pc, 0.0, 2.0, 0.0)]],
        Some(Delivery::new(960, 540, Fit::Contain)),
    );
    check_refused(
        "range/pc-enlarged",
        &enlarged,
        pc,
        0.5,
        Refusal::Plan("full-range picture in a still that the scaler resizes"),
    );
    let after = timeline(vec![vec![clip(&m.testsrc, 0.0, 1.0, 0.0), clip(&r.pc, 0.0, 1.0, 1.0)]], None);
    check_range(
        "range/pc-after-limited",
        &after,
        &[m.testsrc.clone(), r.pc.clone()],
        &[0.5, 1.5],
    );

    // The refusal is needed: plan the very same frames as a file stored before the flag was recorded would be
    // (`None`), draw them anyway, and the compositor differs from the still.
    let unrecorded = |a: &Asset| {
        let mut a = a.clone();
        a.streams[0].color_range = None;
        a
    };
    let off = |what: &str, (differing, max): (usize, i32)| {
        eprintln!("{what}: drawn anyway: {differing} samples differ, max {max}");
        assert!(
            differing > 0,
            "{what}: the compositor agrees with FFmpeg's still: the refusal is no longer needed"
        );
    };
    let old = unrecorded(&r.pc);
    off(
        "vignette",
        drawn_anyway(
            &one(&old, &|c| c.effects = vec![E::Vignette]),
            std::slice::from_ref(&old),
            0.5,
        ),
    );
    off("fade", drawn_anyway(&fading(&old), std::slice::from_ref(&old), 0.5));
    if !fixed {
        // (FFmpeg 6 keys this gradient as the compositor does; FFmpeg 9 is 19 levels off on it.)
        let old = unrecorded(&r.pc_gradient);
        let (tl, assets) = over_bars(&old, &|c| c.effects = vec![a_key("green", 0.3, 0.3)]);
        off("chroma key", drawn_anyway(&tl, &assets, 0.5));
        off(
            "graded",
            drawn_anyway(&one(&unrecorded(&r.pc), &|c| c.color.gamma = 1.4), &[unrecorded(&r.pc)], 0.5),
        );
    }
}

// ---- the grade: LUT, curves and wheels ---------------------------------------------------------

/// A `.cube` of `size`^3 rows, written to the target directory; `f` maps a lattice point in `0..=1` to its row.
fn grade_cube(name: &str, size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> String {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-grade-luts");
    std::fs::create_dir_all(&dir).unwrap();
    let n = (size - 1) as f32;
    let mut data = Vec::with_capacity(size * size * size);
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                data.push(f([r as f32 / n, g as f32 / n, b as f32 / n]));
            }
        }
    }
    let table = kerf_core::color_lut::Lut3d {
        title: String::new(),
        size,
        data,
    };
    let path = dir.join(format!("{name}.cube"));
    common::write_fixture(&path, &table.to_cube(1.0));
    path.to_string_lossy().into_owned()
}

fn grade_points(p: &[(f64, f64)]) -> Vec<kerf_core::CurvePoint> {
    p.iter().map(|&(x, y)| kerf_core::CurvePoint { x, y }).collect()
}

fn grade_puck(x: f64, y: f64, lightness: f64) -> kerf_core::Wheel {
    kerf_core::Wheel { x, y, lightness }
}

/// The grades of this section: each part alone, then the three together.
fn grades() -> Vec<(&'static str, kerf_core::Grade)> {
    use kerf_core::{ClipLut, Curves, Grade, Wheels};
    let look = |c: [f32; 3]| {
        [
            0.92 * c[0] + 0.08 * c[1] * c[2] + 0.03,
            0.05 * c[0] * c[0] + 0.9 * c[1] + 0.04 * c[2],
            (0.1 + 0.8 * c[2] + 0.15 * c[0] * c[1]).min(1.0),
        ]
    };
    let wheels = Wheels {
        shadows: grade_puck(0.4, -0.2, 0.1),
        midtones: grade_puck(-0.3, 0.5, -0.05),
        highlights: grade_puck(0.1, 0.7, 0.2),
    };
    let curves = Curves {
        master: grade_points(&[(0.0, 0.03), (0.25, 0.15), (0.75, 0.85), (1.0, 0.98)]),
        red: grade_points(&[(0.0, 0.0), (0.5, 0.58), (1.0, 1.0)]),
        green: Vec::new(),
        blue: grade_points(&[(0.0, 0.1), (1.0, 0.8)]),
    };
    let lut = |name: &str, size: usize, intensity: f64| ClipLut {
        path: grade_cube(name, size, look),
        intensity,
    };
    vec![
        (
            "wheels",
            Grade {
                wheels: Some(wheels),
                ..Grade::default()
            },
        ),
        (
            "curves",
            Grade {
                curves: Some(curves.clone()),
                ..Grade::default()
            },
        ),
        (
            "curves invert",
            Grade {
                curves: Some(Curves {
                    master: grade_points(&[(0.0, 1.0), (1.0, 0.0)]),
                    ..Curves::default()
                }),
                ..Grade::default()
            },
        ),
        (
            "lut",
            Grade {
                lut: Some(lut("look17", 17, 1.0)),
                ..Grade::default()
            },
        ),
        (
            "lut at 40%",
            Grade {
                lut: Some(lut("look33", 33, 0.4)),
                ..Grade::default()
            },
        ),
        (
            "lut, curves and wheels",
            Grade {
                lut: Some(lut("look17", 17, 0.75)),
                curves: Some(curves),
                wheels: Some(wheels),
            },
        ),
    ]
}

/// A graded clip alone, at the footage's own size.
fn graded(asset: &Asset, grade: &kerf_core::Grade, edit: &dyn Fn(&mut Clip)) -> (Timeline, Vec<Asset>) {
    let mut c = clip(asset, 0.0, 2.0, 0.0);
    c.grade = grade.clone();
    edit(&mut c);
    (timeline(vec![vec![c]], None), vec![asset.clone()])
}

/// The grade is FFmpeg's `lut3d`, `lutrgb` and `colorbalance` between a conversion to RGB and one back, and the
/// compositor draws all of it to the level (`tests/grade.rs` holds the arithmetic to the filters on noise,
/// `motion_parity.rs` the export's planes); here it is the still: a graded layer at its own size, alone, over
/// another track, translucent, masked, rotated, through a dissolve, among the stills of a clip tagged BT.709 and of
/// one tagged `smpte170m` / `bt470bg` (BT.601's coefficients through 4:4:4), and beside the layers of another
/// matrix (drawn under FFmpeg 6, refused as mixed matrices under 9, which converts them into the bottom layer's).
/// The planes, the PNG's RGB and the display's are all exact (0 differing samples), the same on both builds.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_grade_is_drawn_as_the_still_draws_it() {
    let m = media();
    for (name, grade) in grades() {
        for (source, asset) in [
            ("testsrc", &m.testsrc),
            ("bars", &m.bars),
            ("gradient", &m.gradient),
            ("testsrc709", &m.testsrc709),
            ("bars709", &m.bars709),
            ("testsrc170m", &m.testsrc170m),
            ("bars470bg", &m.bars470bg),
        ] {
            let (tl, assets) = graded(asset, &grade, &|_| {});
            check(&format!("grade/{name}, {source}"), &tl, &assets, &[0.5]);
        }
    }
    let all = grades();
    let heavy = &all[5].1;
    // Noise: every Y, U, V combination, tables walked in no order.
    let (tl, assets) = graded(&m.noise, heavy, &|c| c.source_out = 0.2);
    check("grade/lut, curves and wheels, noise", &tl, &assets, &[0.1]);
    // After the `eq`, with an opacity, a mask, a rotation.
    let (tl, assets) = graded(&m.testsrc, heavy, &|c| {
        c.color = Color {
            contrast: 1.2,
            saturation: 1.3,
            gamma: 1.1,
            ..Color::default()
        }
    });
    check("grade/after an eq", &tl, &assets, &[0.5]);
    let (tl, assets) = graded(&m.testsrc, heavy, &|c| c.transform.opacity = 0.6);
    check("grade/translucent", &tl, &assets, &[0.5]);
    let (tl, assets) = graded(&m.testsrc, heavy, &|c| {
        c.mask = Some(kerf_core::Mask {
            shape: kerf_core::MaskShape::Ellipse,
            x: 0.5,
            y: 0.5,
            width: 0.6,
            height: 0.6,
            feather: 0.2,
            inverted: false,
        });
    });
    check("grade/masked", &tl, &assets, &[0.5]);
    let (tl, assets) = graded(&m.gradient, heavy, &|c| c.transform.rotation = 20.0);
    check_refused("grade/rotated", &tl, &assets, 0.5, Refusal::Plan("a rotation"));
    // A layer over another: the grade does not cover what is below, and the picture is placed by the overlay.
    let mut upper = clip(&m.testsrc, 0.0, 2.0, 0.0);
    upper.grade = heavy.clone();
    upper.transform.crop_left = 0.25;
    upper.transform.crop_right = 0.25;
    upper.transform.pos_x = 0.2;
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![upper]], None);
    check(
        "grade/cropped and moved over another track",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        &[0.5],
    );
    // A dissolve into a graded clip, and out of one.
    let mut into = clip(&m.testsrc, 0.0, 2.0, 1.0);
    into.grade = heavy.clone();
    into.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.6,
    });
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 1.0, 0.0), into]], None);
    check(
        "grade/dissolving in",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        &[0.95, 1.1, 1.3, 1.7],
    );
    // Beside a layer of another matrix on screen: FFmpeg 6 takes each as it is and the stack is drawn, FFmpeg 9 converts
    // the layers into the bottom one's, which the plan refuses (a frame with the graded clip alone is the graph of that
    // clip alone, as a still builds it: drawn). Beside one of its own matrix, drawn on both.
    let mut over = clip(&m.testsrc, 0.0, 2.0, 0.0);
    over.grade = heavy.clone();
    let tl = timeline(vec![vec![clip(&m.bars709, 0.0, 2.0, 0.0)], vec![over.clone()]], None);
    check_mixed(
        "grade/untagged SD over a BT.709 clip",
        &tl,
        &[m.bars709.clone(), m.testsrc.clone()],
        &[0.5],
        "other than the composite's",
    );
    let tl = timeline(vec![vec![over.clone()], vec![clip(&m.testsrc709, 0.0, 2.0, 0.0)]], None);
    check_mixed(
        "grade/untagged SD under a BT.709 clip",
        &tl,
        &[m.testsrc.clone(), m.testsrc709.clone()],
        &[0.5],
        "other than the composite's",
    );
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![over]], None);
    check(
        "grade/untagged SD over untagged",
        &tl,
        &[m.bars.clone(), m.testsrc.clone()],
        &[0.5],
    );
    // A still's graph is the layers on screen: the graded clip after a BT.709 one in the cut is the graph of that clip
    // alone at its time, drawn on both builds (the export's graph holds both clips, which is the motion plan's refusal).
    let mut after = clip(&m.testsrc, 0.0, 1.0, 1.0);
    after.grade = heavy.clone();
    let tl = timeline(vec![vec![clip(&m.bars709, 0.0, 1.0, 0.0), after]], None);
    check(
        "grade/untagged SD after a BT.709 clip, the graded clip's still",
        &tl,
        &[m.bars709.clone(), m.testsrc.clone()],
        &[1.5],
    );
    let mut over = clip(&m.testsrc170m, 0.0, 2.0, 0.0);
    over.grade = heavy.clone();
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![over]], None);
    check(
        "grade/smpte170m over untagged",
        &tl,
        &[m.bars.clone(), m.testsrc170m.clone()],
        &[0.5],
    );
}

/// What is refused for a graded layer, each with FFmpeg rendering the frame: a layer the compositor resizes
/// (its scaler is a level off swscale's, which the RGB conversions grow to 3 or 4), a letterbox (the bars are
/// graded and their chroma meets the picture's), every format but 8-bit limited-range
/// `yuv420p`, a matrix with no measured round trip, effects behind the grade, a fade running on it and a moving zoom.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn what_the_compositor_does_not_draw_of_a_grade_is_refused() {
    use kerf_core::VideoEffect as E;
    let m = media();
    let all = grades();
    let heavy = &all[5].1;
    let one = |asset: &Asset, edit: &dyn Fn(&mut Clip)| graded(asset, heavy, edit);
    let refuse = |case: &str, (tl, assets): (Timeline, Vec<Asset>), t: f64, why: &'static str| {
        check_refused(&format!("grade/refused {case}"), &tl, &assets, t, Refusal::Plan(why));
    };
    refuse("pip", one(&m.testsrc, &|c| c.transform.scale = 0.6), 0.5, "resizes");
    refuse(
        "enlarged",
        {
            let (mut tl, a) = one(&m.testsrc, &|_| {});
            tl.format = Some(Delivery::new(960, 540, Fit::Contain));
            (tl, a)
        },
        0.5,
        "resizes",
    );
    refuse(
        "letterboxed",
        {
            let (mut tl, a) = one(&m.portrait, &|_| {});
            tl.format = Some(Delivery::new(640, 360, Fit::Contain));
            (tl, a)
        },
        0.5,
        "letterboxed",
    );
    for (name, asset) in [
        ("10-bit", &m.tenbit),
        ("4:4:4", &m.yuv444),
        ("4:2:2", &m.yuv422),
        ("gray", &m.gray),
        ("bgr0", &m.bgr0),
    ] {
        refuse(name, one(asset, &|_| {}), 0.5, "8-bit 4:2:0");
    }
    refuse("full range", one(&m.fullrange, &|_| {}), 0.5, "colour correction");
    refuse("bt2020", one(&m.testsrc2020, &|_| {}), 0.5, "colour matrix");
    let mut unknown = m.testsrc.clone();
    unknown.streams[0].pix_fmt = None;
    refuse("unrecorded format", one(&unknown, &|_| {}), 0.5, "8-bit 4:2:0");
    for (name, effect) in [
        ("blur", E::Blur { sigma: 2.0 }),
        ("sharpen", E::Sharpen { amount: 0.5 }),
        ("grayscale", E::Grayscale),
        ("invert", E::Invert),
        ("vignette", E::Vignette),
    ] {
        refuse(
            &format!("{name} behind it"),
            one(&m.testsrc, &|c| c.effects = vec![effect.clone()]),
            0.5,
            "video effects",
        );
    }
    refuse(
        "fading in",
        one(&m.testsrc, &|c| c.fade_in = 1.0),
        0.3,
        "fade to black or white",
    );
    // A clip's own fade is `fade` to black of the picture whatever its track (not an alpha ramp: a dissolve's is), so it is
    // refused on a layer over another one too.
    let fade_in = |c: &mut Clip| c.fade_in = 1.0;
    let fade_out = |c: &mut Clip| c.fade_out = 1.0;
    for (name, edit, t) in [
        ("fading in over another track", &fade_in as &dyn Fn(&mut Clip), 0.3),
        ("fading out over another track", &fade_out, 1.7),
    ] {
        let (mut tl, mut assets) = one(&m.testsrc, edit);
        tl.tracks.insert(
            0,
            Track {
                clips: vec![clip(&m.bars, 0.0, 2.0, 0.0)],
                ..Track::new(StreamKind::Video, "V0")
            },
        );
        assets.push(m.bars.clone());
        refuse(name, (tl, assets), t, "fade to black or white");
    }
    // (A moving zoom behind a grade is drawn: `grade/moving zoom`, below.)
    // The same clip past its fade is drawn.
    let (tl, assets) = one(&m.testsrc, &|c| c.fade_in = 0.3);
    check("grade/after its fade-in", &tl, &assets, &[1.0]);
    // A moving zoom or position placed behind the grade: the placement reads the 4:2:0 planes the grade hands on.
    let (tl, assets) = one(&m.testsrc, &|c| {
        c.keyframes = zoom_keys(1.0, 1.4, (0.0, 0.0));
    });
    check_exact("grade/moving zoom", &tl, &assets, &[0.8, 1.2, 1.9]);
}

// ---- checker/a: keyed placement in a still, cases the implementer did not write ----------------------------

/// A moving zoom at the instants where the pose is awkward: a key's exact time, a hair either side of it, times that
/// are not round in binary, the hold past the last key. The still samples the pose at `t` and writes it into the
/// `perspective` as text; the plan's pose must be those very numbers.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_stills_at_awkward_instants_are_placed_as_the_still_places_them() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let times = [
        0.5,
        0.500000001,
        0.5 + 1.0 / 3.0,
        0.7,
        0.8999999999999999,
        1.0,
        1.1,
        1.2345678,
        1.4999999,
        1.5,
        1.5000001,
        1.9,
        1.999,
    ];
    let mut z = clip(&m.testsrc, 0.0, 2.0, 0.5);
    z.keyframes = zoom_keys(1.0, 1.8, (0.31, -0.17));
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]], None);
    check_exact("checker/a/zoom and pan over bars", &tl, &assets, &times);
    // Zoom out past the picture (the layer below shows around it), position alone, and the zoom alone.
    for (name, keys) in [
        ("zoom out to 0.15", zoom_keys(1.0, 0.15, (0.0, 0.0))),
        ("zoom out to 0.0005", zoom_keys(1.0, 0.0005, (0.0, 0.0))),
        ("zoom in to 80", zoom_keys(1.0, 80.0, (0.0, 0.0))),
        ("pan alone", zoom_keys(1.0, 1.0, (0.43, 0.29))),
        ("pan far", zoom_keys(1.0, 1.0, (7.5, -3.25))),
    ] {
        let mut z = clip(&m.testsrc, 0.0, 2.0, 0.5);
        z.keyframes = keys;
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]], None);
        let plan = RenderPlan::at(&tl, &assets, &ExportOptions::default(), 1.0, policy()).expect("plan");
        let why = reasons(&plan, plan.size(u32::MAX));
        if why.is_empty() {
            check_exact(&format!("checker/a/{name}"), &tl, &assets, &times);
        } else {
            eprintln!("checker/a/{name}: refused: {why:?}");
            assert!(why.iter().all(|w| w.contains("fixed-point range")), "{name}: {why:?}");
        }
    }
    // Eased segments (the polyline both renderers read).
    for easing in [
        kerf_core::Easing::EaseIn,
        kerf_core::Easing::EaseOut,
        kerf_core::Easing::EaseInOut,
    ] {
        let mut z = clip(&m.testsrc, 0.0, 2.0, 0.5);
        z.keyframes = zoom_keys(1.0, 1.7, (0.2, 0.1));
        z.keyframes[0].easing = easing;
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]], None);
        check_exact(&format!("checker/a/eased {easing:?}"), &tl, &assets, &times);
    }
}

/// Live behaviours in a still, with seeds.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_stills_of_behaviours_with_seeds_are_placed_as_the_still_places_them() {
    use kerf_core::Behavior as B;
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let times = [0.5, 0.73, 1.1, 1.6371];
    for (name, behaviors) in [
        (
            "shake seed 0",
            vec![B::Shake {
                amount: 0.2,
                speed: 6.0,
                seed: 0,
                phase: 0.0,
            }],
        ),
        (
            "shake seed u32::MAX",
            vec![B::Shake {
                amount: 0.25,
                speed: 13.0,
                seed: u32::MAX,
                phase: 0.7,
            }],
        ),
        (
            "shake seed 1234567",
            vec![B::Shake {
                amount: 0.01,
                speed: 0.3,
                seed: 1_234_567,
                phase: -2.0,
            }],
        ),
        (
            "breathe 0.9",
            vec![B::Breathe {
                amount: 0.9,
                speed: 1.7,
                phase: 0.1,
            }],
        ),
        (
            "drift 270",
            vec![B::Drift {
                speed: 0.4,
                angle: 270.0,
                phase: 0.0,
            }],
        ),
        (
            "drift -45",
            vec![B::Drift {
                speed: -2.0,
                angle: -45.0,
                phase: 0.3,
            }],
        ),
        (
            "all three",
            vec![
                B::Breathe {
                    amount: 0.3,
                    speed: 0.8,
                    phase: 0.0,
                },
                B::Drift {
                    speed: 0.3,
                    angle: 100.0,
                    phase: 0.0,
                },
                B::Shake {
                    amount: 0.05,
                    speed: 3.0,
                    seed: 5,
                    phase: 0.0,
                },
            ],
        ),
    ] {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.5);
        c.behaviors = behaviors;
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None);
        check_exact(&format!("checker/a/{name}"), &tl, &assets, &times);
    }
}

/// A static crop ahead of the placement: odd widths and an odd x offset are drawn, an odd height is the plan's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_stills_of_cropped_pictures_are_placed_as_the_still_places_them() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let times = [0.8, 1.3, 1.9];
    for (name, crop, odd_height) in [
        ("crop right 0.0013 (639 wide)", (0.0, 0.0013, 0.0, 0.0), false),
        ("crop left 0.003", (0.003, 0.0, 0.0, 0.0), false),
        ("crop 0.0058 each side", (0.0058, 0.0058, 0.0, 0.0), false),
        ("crop top and bottom 0.1", (0.0, 0.0, 0.1, 0.1), false),
        (
            "crop bottom 0.0042 and right 0.0021 (odd height)",
            (0.0, 0.0021, 0.0, 0.0042),
            true,
        ),
    ] {
        let mut z = clip(&m.testsrc, 0.0, 2.0, 0.5);
        z.keyframes = zoom_keys(1.0, 1.5, (0.1, -0.05));
        z.transform.crop_left = crop.0;
        z.transform.crop_right = crop.1;
        z.transform.crop_top = crop.2;
        z.transform.crop_bottom = crop.3;
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]], None);
        if odd_height {
            check_refused(&format!("checker/a/{name}"), &tl, &assets, 1.0, Refusal::Plan("odd-height"));
        } else {
            check_exact(&format!("checker/a/{name}"), &tl, &assets, &times);
        }
    }
}

/// Both clips of a transition move, every kind of transition, in a still.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_stills_through_transitions_between_moving_clips_are_placed_as_the_still_places_them() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    for kind in [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::SlideLeft,
        TransitionKind::SlideDown,
        TransitionKind::PushRight,
        TransitionKind::PushUp,
    ] {
        let mut out = clip(&m.bars, 0.0, 2.0, 0.0);
        out.keyframes = zoom_keys(1.0, 1.5, (-0.1, 0.1));
        let mut inc = clip(&m.testsrc, 0.0, 2.0, 1.0);
        inc.keyframes = zoom_keys(1.2, 1.7, (0.1, -0.1));
        inc.transition_in = Some(Transition { kind, duration: 0.8 });
        let tl = timeline(vec![vec![out, inc]], None);
        check_exact(
            &format!("checker/a/{kind:?} both moving"),
            &tl,
            &assets,
            &[1.05, 1.2, 1.4, 1.7],
        );
    }
}

/// `check_exact`'s comparison for a frame the compositor may refuse: `Err` is the compositor's refusal, `Ok` the number of
/// differing samples and the largest difference (RGB, after FFmpeg's own conversion of the compositor's planes).
fn exact_if_drawn(tl: &Timeline, assets: &[Asset], t: f64) -> Result<(usize, i32), String> {
    exact_if_drawn_as(tl, assets, assets, t)
}

/// [`exact_if_drawn`] where the plan is made from `told` (what the plan knows of the footage) and the still from `assets`.
fn exact_if_drawn_as(tl: &Timeline, assets: &[Asset], told: &[Asset], t: f64) -> Result<(usize, i32), String> {
    use std::io::Write;
    let opts = ExportOptions::default();
    let plan = RenderPlan::at(tl, told, &opts, t, policy()).expect("plan");
    let size = plan.size(u32::MAX);
    let (w, h) = (size.0 as usize, size.1 as usize);
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    // (A name of its own per call: the tests that draw frames anyway run side by side.)
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ref_png = scratch.join(format!("checker-anyway-{}-{call}-{t}.png", std::process::id()));
    export_still(tl, assets, &opts, t, &ref_png, ImageFormat::Png, 0).expect("FFmpeg still");
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
    let frames = kerf_gpu::decode_layers(&plan.layers).map_err(|e| e.to_string())?;
    let planes = compositor().composite_yuv(&plan, &frames, size).map_err(|e| e.to_string())?;
    let matrix = match plan.canvas.matrix {
        kerf_core::YuvMatrix::Bt601 => "bt601",
        kerf_core::YuvMatrix::Bt709 => "bt709",
        kerf_core::YuvMatrix::Bt2020 => "bt2020nc",
    };
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
        ])
        .arg(format!("{w}x{h}"))
        .args(["-i", "pipe:0", "-vf", &format!("scale=in_color_matrix={matrix}:in_range=tv")])
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let data: Vec<u8> = [&planes.y, &planes.u, &planes.v].into_iter().flatten().copied().collect();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    let differing = out.stdout.iter().zip(&reference).filter(|(a, b)| a != b).count();
    let max = out
        .stdout
        .iter()
        .zip(&reference)
        .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
        .max()
        .unwrap_or(0);
    Ok((differing, max))
}

/// The still's refusals of a placement that are left, drawn anyway (see `checker_a_refusals_drawn_anyway` in `motion_parity.rs`:
/// the scratch patch and `KERF_CHECKER_ANYWAY`): a rotation, an opacity below 1, a letterboxed picture at the identity pose.
/// Prints one line per case; judges nothing.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); the plan-level refusals need KERF_CHECKER_ANYWAY and the scratch patch"]
fn checker_a_stills_refusals_drawn_anyway() {
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let times = [0.8, 1.3, 1.9];
    let moving = |edit: &dyn Fn(&mut Clip)| {
        let mut z = clip(&m.testsrc, 0.0, 2.0, 0.5);
        z.keyframes = zoom_keys(1.0, 1.6, (0.1, -0.05));
        edit(&mut z);
        timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]], None)
    };
    let show = |name: &str, tl: &Timeline, assets: &[Asset]| {
        for t in times {
            match exact_if_drawn(tl, assets, t) {
                Ok((0, _)) => eprintln!("ANYWAY still {name:<44} t={t}: EXACT (the refusal is wider than it has to be)"),
                Ok((n, max)) => eprintln!("ANYWAY still {name:<44} t={t}: not exact, {n} samples differ (max {max})"),
                Err(e) => eprintln!(
                    "ANYWAY still {name:<44} t={t}: still refused: {}",
                    e.chars().take(120).collect::<String>()
                ),
            }
        }
    };
    show("plain zoom (control)", &moving(&|_| {}), &assets);
    // (A grade ahead of the placement is not a refusal any more: `checker_a_a_grade_ahead_of_a_placement_is_the_exports`.)
    show(
        "rotation 8",
        &moving(&|c| c.keyframes.iter_mut().for_each(|k| k.rotation = 8.0)),
        &assets,
    );
    show(
        "opacity 0.7",
        &moving(&|c| c.keyframes.iter_mut().for_each(|k| k.opacity = 0.7)),
        &assets,
    );
    let (pillar, _) = shapes();
    let mut z = clip(pillar, 0.0, 2.0, 0.5);
    z.keyframes = zoom_keys(1.0, 1.5, (0.0, 0.0));
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]], None);
    show(
        "letterboxed 480x360 at the identity pose",
        &tl,
        &[m.bars.clone(), pillar.clone()],
    );
}

/// The refusal of a letterboxed picture at the identity pose (the instant a push-in starts at 1): is the still FFmpeg draws
/// there any other than the one it draws of the same clip with no keys at all (which the compositor draws, exactly)? If it is
/// the same picture, the refusal is wider than it has to be (it costs the one instant). Reports; asserts that the plan refuses.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_the_still_at_the_identity_pose_of_a_letterboxed_picture_is_the_unplaced_still() {
    let m = media();
    let (pillar, odd_width) = shapes();
    for (name, asset) in [("480x360 pillarboxed", pillar), ("203x360 odd width", odd_width)] {
        let assets = [m.bars.clone(), asset.clone()];
        let build = |keys: bool| {
            let mut z = clip(asset, 0.0, 2.0, 0.5);
            if keys {
                z.keyframes = zoom_keys(1.0, 1.5, (0.0, 0.0));
            }
            timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]], None)
        };
        let still = |tl: &Timeline, tag: &str| {
            let png = Path::new(env!("CARGO_TARGET_TMPDIR"))
                .join("parity-ref")
                .join(format!("checker-identity-{tag}.png"));
            std::fs::create_dir_all(png.parent().unwrap()).unwrap();
            export_still(tl, &assets, &ExportOptions::default(), 0.5, &png, ImageFormat::Png, 0).expect("FFmpeg still");
            let rgb = ffmpeg(&["-i", png.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
            let _ = std::fs::remove_file(&png);
            rgb
        };
        let (keyed, plain) = (still(&build(true), "keyed"), still(&build(false), "plain"));
        let differing = keyed.iter().zip(&plain).filter(|(a, b)| a != b).count();
        eprintln!("checker/a/identity pose, {name}: the keyed clip's still and the unkeyed clip's differ in {differing} samples");
        let plan = RenderPlan::at(&build(true), &assets, &ExportOptions::default(), 0.5, policy()).expect("plan");
        let why = reasons(&plan, plan.size(u32::MAX));
        assert!(
            why.iter().any(|w| w.contains("letterboxed picture at the identity pose")),
            "{name}: {why:?}"
        );
        let plan = RenderPlan::at(&build(false), &assets, &ExportOptions::default(), 0.5, policy()).expect("plan");
        assert!(
            reasons(&plan, plan.size(u32::MAX)).is_empty(),
            "{name}: the unkeyed clip is drawn"
        );
    }
}

/// `PlacedFormat` in a still, drawn anyway by telling the plan the footage is plain 8-bit limited-range 4:2:0 (the decode makes
/// that of it either way). Prints one line per case; judges nothing.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_stills_placed_formats_drawn_anyway() {
    let m = media();
    eprintln!("composite colour policy of this FFmpeg: {:?}", policy());
    let as_420 = |a: &Asset| {
        let mut a = a.clone();
        for s in &mut a.streams {
            if s.kind == StreamKind::Video {
                s.pix_fmt = Some("yuv420p".into());
                s.color_range = Some("tv".into());
            }
        }
        a
    };
    // (4:4:4 and 4:2:2 are not here: lifted, and refused only when flagged full range, which the chroma test holds.)
    for (name, asset) in [
        ("full-range yuvj420p", &m.fullrange),
        ("10-bit tagged full range", &m.tenbit_pc),
        ("10-bit", &m.tenbit),
    ] {
        let mut z = clip(asset, 0.0, 2.0, 0.5);
        z.keyframes = zoom_keys(1.0, 1.6, (0.1, 0.0));
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]], None);
        let truth = [m.bars.clone(), asset.clone()];
        let told = [m.bars.clone(), as_420(asset)];
        let real = RenderPlan::at(&tl, &truth, &ExportOptions::default(), 1.0, policy()).expect("plan");
        let why = reasons(&real, real.size(u32::MAX));
        for t in [0.8, 1.3, 1.9] {
            match exact_if_drawn_as(&tl, &truth, &told, t) {
                Ok((0, _)) => eprintln!(
                    "ANYWAY still format {name:<28} t={t}: EXACT (refused really: {})",
                    !why.is_empty()
                ),
                Ok((n, max)) => eprintln!(
                    "ANYWAY still format {name:<28} t={t}: not exact, {n} samples differ (max {max}) (refused really: {})",
                    !why.is_empty()
                ),
                Err(e) => eprintln!(
                    "ANYWAY still format {name:<28} t={t}: still refused: {}",
                    e.chars().take(100).collect::<String>()
                ),
            }
        }
    }
}

/// 4:4:4 and 4:2:2 under a placement in a still, once the probe lets them through: other sizes, tags and range flags than the
/// probe's picture. A frame the plan draws must be the still's to the sample; the rest are listed.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_stills_of_4_4_4_and_4_2_2_under_a_placement_are_the_stills_or_refused() {
    let m = media();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-a-chroma-stills");
    std::fs::create_dir_all(&dir).unwrap();
    eprintln!(
        "composite colour policy: {:?}; placed_chroma: {}",
        policy(),
        compositor().caps().placed_chroma
    );
    let make = |name: &str, w: u32, vf: &str, pix: &str, flags: &[&str]| -> Asset {
        let p = dir.join(name);
        let graph = format!(
            "testsrc2=size={}x360:rate=30:duration=2{vf},setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown",
            w.next_multiple_of(2)
        );
        let mut args = vec!["-f", "lavfi", "-i", graph.as_str(), "-c:v", "ffv1", "-pix_fmt", pix];
        args.extend_from_slice(flags);
        args.push(p.to_str().unwrap());
        ffmpeg(&args);
        probed(&p)
    };
    let odd = ",crop=w=203:h=360:x=0:y=0:exact=1";
    let cases: Vec<(&str, Asset)> = vec![
        ("yuv444p", make("a.mkv", 640, "", "yuv444p", &[])),
        ("yuv422p", make("b.mkv", 640, "", "yuv422p", &[])),
        ("yuv444p odd width", make("c.mkv", 203, odd, "yuv444p", &[])),
        ("yuv422p odd width", make("d.mkv", 203, odd, "yuv422p", &[])),
        ("yuv444p 480x360", make("e.mkv", 480, "", "yuv444p", &[])),
        (
            "yuv444p flagged pc",
            make("f.mkv", 640, "", "yuv444p", &["-color_range", "pc"]),
        ),
        (
            "yuv422p flagged pc",
            make("g.mkv", 640, "", "yuv422p", &["-color_range", "pc"]),
        ),
        (
            "yuv444p flagged tv",
            make("h.mkv", 640, "", "yuv444p", &["-color_range", "tv"]),
        ),
        ("yuvj444p", make("i.mkv", 640, "", "yuvj444p", &[])),
        ("yuv444p10le", make("j.mkv", 640, "", "yuv444p10le", &[])),
    ];
    let mut fails = Vec::new();
    for (name, asset) in cases {
        let mut z = clip(&asset, 0.0, 2.0, 0.5);
        z.keyframes = zoom_keys(1.0, 1.6, (0.1, 0.0));
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]], None);
        let truth = [m.bars.clone(), asset.clone()];
        let real = RenderPlan::at(&tl, &truth, &ExportOptions::default(), 1.0, policy()).expect("plan");
        let why = reasons(&real, real.size(u32::MAX));
        for t in [0.8, 1.3, 1.9] {
            match exact_if_drawn_as(&tl, &truth, &truth, t) {
                Ok((0, _)) => eprintln!(
                    "checker/a/chroma still {name:<22} t={t}: drawn, exact (plan refuses: {})",
                    !why.is_empty()
                ),
                Ok((n, max)) => {
                    eprintln!(
                        "checker/a/chroma still {name:<22} t={t}: drawn, {n} samples differ (max {max}) (plan refuses: {})",
                        !why.is_empty()
                    );
                    fails.push(format!("{name} @ {t}: {n} samples differ (max {max})"));
                }
                Err(e) => eprintln!(
                    "checker/a/chroma still {name:<22} t={t}: refused: {}",
                    e.chars().take(110).collect::<String>()
                ),
            }
        }
    }
    // A picture the project stored before the range flag was recorded (`color_range` None) whose file is in fact full range:
    // the plan is told nothing of the range. A 4:2:0 one is refused where the composite's range is negotiated; a 4:4:4 or 4:2:2
    // one must not be drawn wrongly.
    for (name, pix) in [("yuv444p", "yuv444p"), ("yuv422p", "yuv422p"), ("yuv420p", "yuv420p")] {
        let asset = make(&format!("u{pix}.mkv"), 640, "", pix, &["-color_range", "pc"]);
        let mut told = asset.clone();
        for st in &mut told.streams {
            st.color_range = None;
        }
        let mut z = clip(&asset, 0.0, 2.0, 0.5);
        z.keyframes = zoom_keys(1.0, 1.6, (0.1, 0.0));
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![z]], None);
        let truth = [m.bars.clone(), asset.clone()];
        let told = [m.bars.clone(), told];
        for t in [0.8, 1.3] {
            match exact_if_drawn_as(&tl, &truth, &told, t) {
                Ok((0, _)) => eprintln!("checker/a/chroma still {name} full range, not recorded, t={t}: drawn, exact"),
                Ok((n, max)) => {
                    eprintln!(
                        "checker/a/chroma still {name} full range, not recorded, t={t}: drawn, {n} samples differ (max {max})"
                    );
                    fails.push(format!(
                        "{name} full range, not recorded @ {t}: {n} samples differ (max {max})"
                    ));
                }
                Err(e) => eprintln!(
                    "checker/a/chroma still {name} full range, not recorded, t={t}: refused: {}",
                    e.chars().take(110).collect::<String>()
                ),
            }
        }
    }
    assert!(fails.is_empty(), "drawn and not the still's: {}", fails.join("; "));
}

/// A file written for the range tests: `codec` and `flags` as ffmpeg takes them, the picture `testsrc2` at `size`, `vf` after it.
fn range_file(dir: &Path, name: &str, size: &str, vf: &str, pix: &str, codec: &[&str], flags: &[&str]) -> Asset {
    let p = dir.join(name);
    let graph = format!(
        "testsrc2=size={size}:rate=30:duration=2{vf},setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown"
    );
    let mut args = vec!["-f", "lavfi", "-i", graph.as_str()];
    args.extend_from_slice(codec);
    args.extend_from_slice(&["-pix_fmt", pix]);
    args.extend_from_slice(flags);
    args.push(p.to_str().unwrap());
    ffmpeg(&args);
    probed(&p)
}

/// An asset probed before the range flag was recorded: the same file, `color_range` None.
fn silent_about_range(a: &Asset) -> Asset {
    let mut a = a.clone();
    for s in &mut a.streams {
        s.color_range = None;
    }
    a
}

/// A moving zoom over bars, the picture the asset, at the instants the range tests look at.
fn zoomed_over_bars(asset: &Asset, bars: &Asset) -> Timeline {
    let mut z = clip(asset, 0.0, 2.0, 0.5);
    z.keyframes = zoom_keys(1.0, 1.6, (0.1, 0.0));
    timeline(vec![vec![clip(bars, 0.0, 2.0, 0.0)], vec![z]], None)
}

/// An asset that does not say its range (`color_range` None, probed before the flag was recorded) under a placement, where the
/// composite's range is negotiated (9.0.2): the compositor asks the file. A file that says `pc` is refused, a file that says
/// `tv` or nothing is drawn and is the still, to the sample; where the range is not negotiated (6.1.1) every one of them is
/// drawn. Over 4:2:0, 4:2:2, 4:4:4, gray, 10-bit, H.264 and FFV1.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_stills_an_asset_silent_about_its_range_is_asked_of_its_file() {
    let m = media();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-a-range");
    std::fs::create_dir_all(&dir).unwrap();
    let negotiated = policy() != CompositeColorPolicy::FixedBt601;
    eprintln!("composite colour policy: {:?}", policy());
    let ffv1 = ["-c:v", "ffv1"];
    let x264 = ["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"];
    // (name, file, the file says full range)
    let cases: Vec<(&str, Asset, bool)> = vec![
        (
            "yuv420p pc",
            range_file(&dir, "a420pc.mkv", "640x360", "", "yuv420p", &ffv1, &["-color_range", "pc"]),
            true,
        ),
        (
            "yuv420p tv",
            range_file(&dir, "a420tv.mkv", "640x360", "", "yuv420p", &ffv1, &["-color_range", "tv"]),
            false,
        ),
        (
            "yuv420p untagged",
            range_file(&dir, "a420.mkv", "640x360", "", "yuv420p", &ffv1, &[]),
            false,
        ),
        (
            "yuv422p pc",
            range_file(&dir, "a422pc.mkv", "640x360", "", "yuv422p", &ffv1, &["-color_range", "pc"]),
            true,
        ),
        (
            "yuv422p tv",
            range_file(&dir, "a422tv.mkv", "640x360", "", "yuv422p", &ffv1, &["-color_range", "tv"]),
            false,
        ),
        (
            "yuv444p pc",
            range_file(&dir, "a444pc.mkv", "640x360", "", "yuv444p", &ffv1, &["-color_range", "pc"]),
            true,
        ),
        (
            "yuv444p tv",
            range_file(&dir, "a444tv.mkv", "640x360", "", "yuv444p", &ffv1, &["-color_range", "tv"]),
            false,
        ),
        (
            "yuv444p untagged",
            range_file(&dir, "a444.mkv", "640x360", "", "yuv444p", &ffv1, &[]),
            false,
        ),
        (
            "yuv444p pc, odd width",
            range_file(
                &dir,
                "a444odd.mkv",
                "204x360",
                ",crop=w=203:h=360:x=0:y=0:exact=1",
                "yuv444p",
                &ffv1,
                &["-color_range", "pc"],
            ),
            true,
        ),
        (
            "gray pc",
            range_file(&dir, "agraypc.mkv", "640x360", "", "gray", &ffv1, &["-color_range", "pc"]),
            true,
        ),
        (
            "yuv420p10le pc",
            range_file(
                &dir,
                "a10pc.mkv",
                "640x360",
                "",
                "yuv420p10le",
                &ffv1,
                &["-color_range", "pc"],
            ),
            true,
        ),
        (
            "h264 yuv420p untagged",
            range_file(&dir, "h264.mp4", "640x360", "", "yuv420p", &x264, &[]),
            false,
        ),
        (
            "h264 yuv420p, the stream (VUI) says full range, the container does not",
            range_file(
                &dir,
                "h264vui.mp4",
                "640x360",
                "",
                "yuv420p",
                &x264,
                &["-bsf:v", "h264_metadata=video_full_range_flag=1"],
            ),
            true,
        ),
        (
            "h264 yuvj420p",
            range_file(&dir, "h264j.mp4", "640x360", "", "yuvj420p", &x264, &[]),
            true,
        ),
    ];
    let mut fails = Vec::new();
    for (name, truth_asset, says_pc) in cases {
        let v = truth_asset.streams.iter().find(|s| s.kind == StreamKind::Video).unwrap();
        eprintln!(
            "checker/a/range {name}: probed pix_fmt {:?} color_range {:?}; the source's own flag: {:?}",
            v.pix_fmt,
            v.color_range,
            kerf_core::source_is_full_range(Path::new(&truth_asset.path))
        );
        if kerf_core::source_is_full_range(Path::new(&truth_asset.path)) != Some(says_pc) {
            fails.push(format!(
                "{name}: the render-time probe says {:?}, not {says_pc}",
                kerf_core::source_is_full_range(Path::new(&truth_asset.path))
            ));
        }
        let tl = zoomed_over_bars(&truth_asset, &m.bars);
        let truth = [m.bars.clone(), truth_asset.clone()];
        let told = [m.bars.clone(), silent_about_range(&truth_asset)];
        for t in [0.8, 1.5] {
            match exact_if_drawn_as(&tl, &truth, &told, t) {
                Ok((0, _)) => {
                    eprintln!("   t={t}: drawn, exact");
                    if negotiated && says_pc {
                        fails.push(format!(
                            "{name} @ {t}: a full-range file was drawn (exact, but it must be refused)"
                        ));
                    }
                }
                Ok((n, max)) => {
                    eprintln!("   t={t}: drawn, {n} samples differ (max {max})");
                    fails.push(format!("{name} @ {t}: drawn and {n} samples differ (max {max})"));
                }
                Err(e) => {
                    eprintln!("   t={t}: refused: {}", e.chars().take(120).collect::<String>());
                    if !negotiated || !says_pc {
                        fails.push(format!(
                            "{name} @ {t}: refused where it should be drawn: {}",
                            e.chars().take(160).collect::<String>()
                        ));
                    }
                }
            }
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// The same file when the asset says what the file says: a flagged one is refused by the plan, an unflagged one is drawn. And a
/// file whose container and stream disagree: H.264 whose VUI says limited range under a container that says full (a `colr`
/// atom written by the muxer), and the reverse, with the asset as `probe_asset` made it.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_stills_a_file_whose_stream_and_container_disagree_on_the_range() {
    let m = media();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-a-range-split");
    std::fs::create_dir_all(&dir).unwrap();
    let negotiated = policy() != CompositeColorPolicy::FixedBt601;
    let x264 = ["-c:v", "libx264", "-preset", "ultrafast", "-qp", "0", "-g", "10"];
    // Both say limited / both say full (the encoder writes the VUI and the muxer the container's tag), then the stream's VUI is
    // rewritten on a stream copy (`h264_metadata`) so that the two disagree, either way round.
    let tv = range_file(&dir, "tv.mp4", "640x360", "", "yuv420p", &x264, &["-color_range", "tv"]);
    let pc = range_file(&dir, "pc.mp4", "640x360", "", "yuv420p", &x264, &["-color_range", "pc"]);
    let remux = |src: &Asset, name: &str, flags: &[&str]| -> Asset {
        let out = dir.join(name);
        let mut args = vec!["-i", src.path.as_str(), "-c", "copy"];
        args.extend_from_slice(flags);
        args.push(out.to_str().unwrap());
        ffmpeg(&args);
        probed(&out)
    };
    let cases = [
        ("both limited", tv.clone()),
        ("both full", pc.clone()),
        (
            "container full, VUI limited",
            remux(&pc, "pc-vui-tv.mp4", &["-bsf:v", "h264_metadata=video_full_range_flag=0"]),
        ),
        (
            "container limited, VUI full",
            remux(&tv, "tv-vui-pc.mp4", &["-bsf:v", "h264_metadata=video_full_range_flag=1"]),
        ),
        (
            "container full, VUI limited, mkv",
            remux(&pc, "pc-vui-tv.mkv", &["-bsf:v", "h264_metadata=video_full_range_flag=0"]),
        ),
        (
            "container limited, VUI full, mkv",
            remux(&tv, "tv-vui-pc.mkv", &["-bsf:v", "h264_metadata=video_full_range_flag=1"]),
        ),
    ];
    let mut fails = Vec::new();
    for (name, asset) in cases {
        let v = asset.streams.iter().find(|s| s.kind == StreamKind::Video).unwrap();
        eprintln!(
            "checker/a/range {name}: probed pix_fmt {:?} color_range {:?}; the source's own flag: {:?}",
            v.pix_fmt,
            v.color_range,
            kerf_core::source_is_full_range(Path::new(&asset.path))
        );
        let tl = zoomed_over_bars(&asset, &m.bars);
        let truth = [m.bars.clone(), asset.clone()];
        for (told_name, told) in [
            ("as probed", truth.clone()),
            ("range not recorded", [m.bars.clone(), silent_about_range(&asset)]),
        ] {
            for t in [0.8, 1.5] {
                match exact_if_drawn_as(&tl, &truth, &told, t) {
                    Ok((0, _)) => eprintln!("   {told_name} t={t}: drawn, exact"),
                    Ok((n, max)) => {
                        eprintln!("   {told_name} t={t}: drawn, {n} samples differ (max {max})");
                        if negotiated {
                            fails.push(format!("{name}, {told_name} @ {t}: drawn and {n} samples differ (max {max})"));
                        }
                    }
                    Err(e) => eprintln!("   {told_name} t={t}: refused: {}", e.chars().take(110).collect::<String>()),
                }
            }
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
}

/// A blend case: held to the still where this FFmpeg's chain is the model's (`blend::kernels`); where the probe refused the build
/// (FFmpeg 4.4's `overlay` loses the alpha rows of a clip above the frame's top edge when it runs on several threads) the plan must
/// refuse every time and the case is drawn anyway with the caps forced, to hold the arithmetic itself to the still.
fn blend_check(case: &str, tl: &Timeline, assets: &[Asset], times: &[f64]) {
    if compositor().caps().blend == kerf_core::BlendKinds::ALL {
        return check(case, tl, assets, times);
    }
    for &t in times {
        let plan = RenderPlan::at(tl, assets, &ExportOptions::default(), t, policy()).expect("plan");
        let why = reasons(&plan, plan.size(u32::MAX));
        assert!(
            why.iter().any(|w| w.contains("no compositor pass draws this mode")),
            "{case} @ {t}: a blend the probe refused is refused by the plan: {why:?}"
        );
    }
    FORCED_BLENDS.with(|f| f.set(true));
    check(&format!("{case} (forced)"), tl, assets, times);
    FORCED_BLENDS.with(|f| f.set(false));
}

/// A blend mode is FFmpeg's `blend` in RGB between two conversions (`engine/cli/blend.rs`), drawn in integers (`src/blend.rs`; held to
/// the filters over every value in `tests/blend_kernels.rs` and to the export's planes sample for sample in `tests/motion_parity.rs`).
/// The still's PNG is converted by swscale and the compositor's frame by its shader, which runs swscale's own integers, so these hold the whole
/// frame to zero differing samples, as every still is: every mode over the bars and the test pattern, the noise (every Y, U, V), a clip with its fades running, a mask and a
/// key, a moved one, and on BT.709 / BT.601-tagged footage, where the chain names the composite's matrix.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn blend_modes_are_drawn_as_the_still_draws_them() {
    use kerf_core::BlendMode;
    let m = media();
    let blended = |asset: &Asset, mode: BlendMode, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(asset, 0.0, 2.0, 0.0);
        c.blend_mode = mode;
        edit(&mut c);
        c
    };
    for mode in [
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::HardLight,
        BlendMode::Add,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::Difference,
        BlendMode::Exclusion,
    ] {
        let over = |top: Clip, base: &Asset, with: &Asset| {
            (
                timeline(vec![vec![clip(base, 0.0, 2.0, 0.0)], vec![top]], None),
                [base.clone(), with.clone()],
            )
        };
        let (tl, assets) = over(blended(&m.testsrc, mode, &|_| {}), &m.bars, &m.testsrc);
        blend_check(&format!("blend/{mode:?}"), &tl, &assets, &[0.5, 1.3]);
        let (tl, assets) = over(blended(&m.bars, mode, &|_| {}), &m.testsrc, &m.bars);
        blend_check(&format!("blend/{mode:?} bars over testsrc"), &tl, &assets, &[0.5]);
        let (tl, assets) = over(blended(&m.noise, mode, &|_| {}), &m.gradient, &m.noise);
        blend_check(&format!("blend/{mode:?} noise over a gradient"), &tl, &assets, &[0.1]);
        let (tl, assets) = over(blended(&m.testsrc, mode, &|c| c.fade_in = 1.0), &m.bars, &m.testsrc);
        blend_check(&format!("blend/{mode:?} fading in"), &tl, &assets, &[0.4]);
        let (tl, assets) = over(
            blended(&m.testsrc, mode, &|c| {
                c.mask = Some(a_mask(kerf_core::MaskShape::Ellipse, 0.3, false));
            }),
            &m.bars,
            &m.testsrc,
        );
        blend_check(&format!("blend/{mode:?} masked"), &tl, &assets, &[0.5]);
        let (tl, assets) = over(
            blended(&m.testsrc, mode, &|c| {
                c.transform.pos_x = 0.2;
                c.transform.pos_y = 0.1;
            }),
            &m.bars,
            &m.testsrc,
        );
        blend_check(&format!("blend/{mode:?} moved"), &tl, &assets, &[0.5]);
        // A quarter turn lands on whole samples (`rotate` is exact there): drawn, and refused at any other angle.
        for turn in [90.0, 180.0] {
            let (tl, assets) = over(
                blended(&m.testsrc, mode, &|c| c.transform.rotation = turn),
                &m.bars,
                &m.testsrc,
            );
            blend_check(&format!("blend/{mode:?} turned {turn}"), &tl, &assets, &[0.5]);
        }
        for (name, base, top) in [
            ("bt709", &m.bars709, &m.testsrc709),
            ("smpte170m", &m.testsrc170m, &m.testsrc170m),
            ("bt470bg", &m.bars470bg, &m.testsrc170m),
        ] {
            let (tl, assets) = over(blended(top, mode, &|_| {}), base, top);
            blend_check(&format!("blend/{mode:?} on {name} footage"), &tl, &assets, &[0.5]);
        }
    }
}

/// What the chain is not exact for is refused in a still as in an export frame, and FFmpeg draws it.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn what_a_blend_is_not_exact_for_is_refused_in_a_still() {
    use kerf_core::BlendMode;
    let m = media();
    type Edit = Box<dyn Fn(&mut Clip)>;
    let edits: [(&str, &str, Edit); 3] = [
        ("scaled", "scaler resizes", Box::new(|c| c.transform.scale = 0.6)),
        ("rotated", "rotated layer", Box::new(|c| c.transform.rotation = 12.0)),
        ("translucent", "below full opacity", Box::new(|c| c.transform.opacity = 0.6)),
    ];
    for (name, reason, edit) in &edits {
        let mut c = clip(&m.testsrc, 0.0, 2.0, 0.0);
        c.blend_mode = BlendMode::Multiply;
        edit(&mut c);
        let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None);
        let plan = RenderPlan::at(
            &tl,
            &[m.bars.clone(), m.testsrc.clone()],
            &ExportOptions::default(),
            0.5,
            policy(),
        )
        .expect("plan");
        let why = reasons(&plan, plan.size(u32::MAX));
        assert!(
            why.iter().any(|w| w.contains(reason)),
            "blend/{name}: refused for `{reason}`: {why:?}"
        );
    }
    let mut c = clip(&m.portrait, 0.0, 2.0, 0.0);
    c.blend_mode = BlendMode::Multiply;
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![c]], None);
    let plan = RenderPlan::at(
        &tl,
        &[m.bars.clone(), m.portrait.clone()],
        &ExportOptions::default(),
        0.5,
        policy(),
    )
    .expect("plan");
    let why = reasons(&plan, plan.size(u32::MAX));
    assert!(why.iter().any(|w| w.contains("letterboxed layer")), "{why:?}");
}

// ==== checker/e: blend modes in a still (independent of the implementer's cases) =======================================

/// A still's graph is the layers on screen: the matrix its blend names is the bottom one's **there**, not the cut's. A cut whose layers change tag half
/// way (BT.709 bars and a BT.709 blended clip, then untagged ones) looked at in each half; a wrong matrix is tens of levels in `difference`.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_still_names_the_matrix_of_the_layers_on_screen() {
    use kerf_core::BlendMode;
    let m = media();
    for mode in [
        BlendMode::Difference,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Exclusion,
    ] {
        for (name, first, second, top_first, top_second) in [
            ("709 then untagged", &m.bars709, &m.bars, &m.testsrc709, &m.testsrc),
            ("untagged then 709", &m.bars, &m.bars709, &m.testsrc, &m.testsrc709),
            ("709 then 470bg", &m.bars709, &m.bars470bg, &m.testsrc709, &m.testsrc170m),
            ("470bg then untagged", &m.bars470bg, &m.bars, &m.testsrc170m, &m.testsrc),
        ] {
            let blended = |asset: &Asset, start: f64| {
                let mut c = clip(asset, 0.0, 1.0, start);
                c.blend_mode = mode;
                c
            };
            let tl = timeline(
                vec![
                    vec![clip(first, 0.0, 1.0, 0.0), clip(second, 0.0, 1.0, 1.0)],
                    vec![blended(top_first, 0.0), blended(top_second, 1.0)],
                ],
                None,
            );
            blend_check(
                &format!("checker/e/still {mode:?} {name}"),
                &tl,
                &[first.clone(), second.clone(), top_first.clone(), top_second.clone()],
                &[0.4, 1.4],
            );
        }
    }
}

/// A still of a blended clip with the others a still can hold: a late start (nothing blended yet, then the clip), a clip ending before the
/// cut does, two blended clips, and the clip's alpha from a key and a fade.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_still_of_a_blended_clip_among_others() {
    use kerf_core::BlendMode;
    let m = media();
    let assets = [m.bars.clone(), m.testsrc.clone(), m.noise.clone()];
    let mut late = clip(&m.testsrc, 0.0, 1.0, 0.8);
    late.blend_mode = BlendMode::Screen;
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![late]], None);
    blend_check("checker/e/still late blended clip", &tl, &assets, &[0.3, 0.9, 1.7]);
    let mut a = clip(&m.testsrc, 0.0, 2.0, 0.0);
    a.blend_mode = BlendMode::Overlay;
    let mut b = clip(&m.noise, 0.0, 2.0, 0.0);
    b.blend_mode = BlendMode::Difference;
    b.fade_in = 0.5;
    b.effects = vec![a_key("green", 0.2, 0.3)];
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![a], vec![b]], None);
    blend_check(
        "checker/e/still two blended clips, a key and a fade",
        &tl,
        &assets,
        &[0.1, 0.25, 1.2],
    );
}

// ==== checker/e, second pass: the lifted refusals in a still =========================================================

/// The three refusals the blend lifted (a quarter turn, an RGB picture, a BT.2020 composite) in a still: held to the still wherever the caps say they
/// are drawn (`blend_check` forces the caps where the probe refused the build). A turn that is not a quarter, and a quarter turned by a key (there are no
/// keys in a still: the angle at the instant), are other cases.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_e_a_still_of_the_lifted_blends() {
    use kerf_core::BlendMode;
    let m = media();
    let modes = [
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
    let over = |top: &Asset, base: &Asset, mode: BlendMode, edit: &dyn Fn(&mut Clip)| {
        let mut c = clip(top, 0.0, 2.0, 0.0);
        c.blend_mode = mode;
        edit(&mut c);
        (
            timeline(vec![vec![clip(base, 0.0, 2.0, 0.0)], vec![c]], None),
            [base.clone(), top.clone()],
        )
    };
    let mut n = 0;
    for turn in [90.0, 180.0, 270.0, -90.0, -180.0, 360.0, 450.0, 540.0, -270.0] {
        for (name, top) in [("testsrc", &m.testsrc), ("noise", &m.noise)] {
            let mode = modes[n % 9];
            n += 1;
            let (tl, assets) = over(top, &m.bars, mode, &|c| c.transform.rotation = turn);
            blend_check(
                &format!("checker/e/still {name} turned {turn} {mode:?}"),
                &tl,
                &assets,
                &[0.5],
            );
        }
    }
    for (what, edit) in [
        (
            "moved",
            Box::new(|c: &mut Clip| {
                c.transform.pos_x = 0.2;
                c.transform.pos_y = 0.15;
            }) as Box<dyn Fn(&mut Clip)>,
        ),
        ("fading in", Box::new(|c: &mut Clip| c.fade_in = 1.0)),
    ] {
        let mode = modes[n % 9];
        n += 1;
        let (tl, assets) = over(&m.testsrc, &m.bars, mode, &|c| {
            c.transform.rotation = 90.0;
            edit(c);
        });
        blend_check(&format!("checker/e/still turned 90 {what} {mode:?}"), &tl, &assets, &[0.5]);
    }
    for (name, rgb) in [("png640", &m.png640), ("bgr0", &m.bgr0)] {
        for mode in [
            BlendMode::Multiply,
            BlendMode::Difference,
            BlendMode::Screen,
            BlendMode::Overlay,
        ] {
            let (tl, assets) = over(rgb, &m.bars, mode, &|_| {});
            blend_check(&format!("checker/e/still {name} over bars {mode:?}"), &tl, &assets, &[0.5]);
            let (tl, assets) = over(&m.testsrc, rgb, mode, &|_| {});
            blend_check(&format!("checker/e/still testsrc over {name} {mode:?}"), &tl, &assets, &[0.5]);
        }
    }
    for mode in modes {
        let (tl, assets) = over(&m.testsrc2020, &m.bars2020, mode, &|_| {});
        blend_check(
            &format!("checker/e/still bt2020 testsrc over bars {mode:?}"),
            &tl,
            &assets,
            &[0.5],
        );
        let (tl, assets) = over(&m.testsrc2020, &m.gradient2020, mode, &|_| {});
        blend_check(
            &format!("checker/e/still bt2020 testsrc over a bt2020 gradient {mode:?}"),
            &tl,
            &assets,
            &[0.5],
        );
    }
}

// ---- the blurred fill (`Fit::Blur`) ------------------------------------------------------------------

/// A clip placed by the plain fit whose picture leaves bars sits over a blurred copy of itself (the thumbnail chain of
/// `engine::cli::blur_fill`: `scale` to cover an eighth of the frame with `flags=bilinear`, `crop`, `boxblur=r:2`, `scale`
/// back up, the picture's own fit `overlay`ed on it). The still draws the same chain at the still's canvas. Each drawn case
/// is held to the still **exactly** (`still_planes_differ`: no differing sample in the `yuv444p` planes the still ends in)
/// and, as the other still cases are, through swscale's RGB conversion, also to the sample. The refusals are held with FFmpeg
/// rendering each.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_blurred_fill_is_drawn_as_the_still_draws_it() {
    let m = media();
    let blur = |w, h| Some(Delivery::new(w, h, Fit::Blur));
    let alone = |a: &Asset, w, h| timeline(vec![vec![clip(a, 0.0, 2.0, 0.0)]], blur(w, h));
    let one = |a: &Asset| std::slice::from_ref(a).to_vec();
    // The shapes: portrait in landscape, landscape in portrait, both in a square.
    check(
        "blur/portrait in 640x360",
        &alone(&m.portrait, 640, 360),
        &one(&m.portrait),
        &[0.5, 1.2],
    );
    check(
        "blur/landscape in 360x640",
        &alone(&m.testsrc, 360, 640),
        &one(&m.testsrc),
        &[0.5, 1.2],
    );
    check(
        "blur/portrait in 480x480",
        &alone(&m.portrait, 480, 480),
        &one(&m.portrait),
        &[0.5],
    );
    check(
        "blur/landscape in 480x480",
        &alone(&m.testsrc, 480, 480),
        &one(&m.testsrc),
        &[0.5],
    );
    // Busy pictures and hard edges: the thumbnail's scale and the blur see every sample.
    check(
        "blur/noise in 360x640",
        &alone(&m.noise, 360, 640),
        &one(&m.noise),
        &[0.05, 0.1],
    );
    check("blur/bars in 360x640", &alone(&m.bars, 360, 640), &one(&m.bars), &[0.5]);
    check(
        "blur/gradient in 360x640",
        &alone(&m.gradient, 360, 640),
        &one(&m.gradient),
        &[0.5],
    );
    // The frame sizes: a thumbnail of 2*floor(w/16) by 2*floor(h/16), a radius of a sixteenth of its smaller side.
    for (w, h) in [(646, 358), (358, 646), (320, 180), (240, 136), (96, 64), (1280, 720)] {
        check(
            &format!("blur/portrait in {w}x{h}"),
            &alone(&m.portrait, w, h),
            &one(&m.portrait),
            &[0.5],
        );
    }
    // An odd picture (its cover, fit and chroma planes) and a fit with an odd side.
    check("blur/odd 641x361 in 360x640", &alone(&m.odd, 360, 640), &one(&m.odd), &[0.5]);
    check("blur/odd 641x361 in 640x360", &alone(&m.odd, 640, 360), &one(&m.odd), &[0.5]);
    // A picture the shape of the frame has nothing to fill.
    check("blur/same shape", &alone(&m.testsrc, 640, 360), &one(&m.testsrc), &[0.5]);
    // Colour: `eq` acts on the whole padded frame, the fill included.
    let mut graded = clip(&m.portrait, 0.0, 2.0, 0.0);
    graded.color = Color {
        brightness: 0.1,
        contrast: 1.3,
        saturation: 1.4,
        gamma: 1.1,
        temperature: -0.3,
    };
    check(
        "blur/eq",
        &timeline(vec![vec![graded]], blur(640, 360)),
        &one(&m.portrait),
        &[0.5],
    );
    let mut gray = clip(&m.portrait, 0.0, 2.0, 0.0);
    gray.effects = vec![kerf_core::VideoEffect::Grayscale];
    check(
        "blur/grayscale",
        &timeline(vec![vec![gray]], blur(640, 360)),
        &one(&m.portrait),
        &[0.5],
    );
    // Over another track (the fill is a whole frame: what is below is covered), and under a picture-in-picture.
    let tl = timeline(
        vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![clip(&m.portrait, 0.0, 2.0, 0.0)]],
        blur(640, 360),
    );
    check("blur/over bars", &tl, &[m.bars.clone(), m.portrait.clone()], &[0.5]);
    let mut pip = clip(&m.testsrc, 0.0, 2.0, 0.0);
    pip.transform = Transform {
        scale: 0.4,
        pos_x: 0.3,
        pos_y: -0.25,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![clip(&m.portrait, 0.0, 2.0, 0.0)], vec![pip]], blur(640, 360));
    check("blur/under a pip", &tl, &[m.portrait.clone(), m.testsrc.clone()], &[0.5]);
    // Inside a fade, a dissolve and a dip (the fill is part of the padded frame the filters see) and a slide.
    let mut fading = clip(&m.portrait, 0.0, 2.0, 0.0);
    fading.fade_in = 1.0;
    fading.fade_out = 1.0;
    check(
        "blur/fade in and out",
        &timeline(vec![vec![fading]], blur(640, 360)),
        &one(&m.portrait),
        &[0.3, 1.7],
    );
    for (name, kind) in [
        ("dissolve", TransitionKind::Crossfade),
        ("dip to black", TransitionKind::DipToBlack),
        ("slide left", TransitionKind::SlideLeft),
        ("push up", TransitionKind::PushUp),
    ] {
        let mut inc = clip(&m.portrait, 0.0, 2.0, 1.0);
        inc.transition_in = Some(Transition { kind, duration: 1.0 });
        let tl = timeline(vec![vec![clip(&m.portrait, 0.0, 1.5, 0.0), inc]], blur(640, 360));
        check(&format!("blur/{name}"), &tl, &one(&m.portrait), &[1.3, 1.6]);
    }
    // Refused (FFmpeg renders each): what the compositor does not draw of a fill.
    let refuse = |case: &str, a: &Asset, w, h, why: &'static str| {
        check_refused(case, &alone(a, w, h), &one(a), 0.5, Refusal::Plan(why));
    };
    refuse("blur/refused: 10-bit", &m.tenbit, 360, 640, "not 8-bit `yuv420p`");
    refuse("blur/refused: 4:4:4", &m.yuv444, 360, 640, "not 8-bit `yuv420p`");
    refuse(
        "blur/refused: full-range mjpeg",
        &m.fullrange,
        360,
        640,
        "not 8-bit `yuv420p`",
    );
    refuse("blur/refused: jpeg still", &m.jpeg, 360, 640, "not 8-bit `yuv420p`");
    refuse("blur/refused: still image", &m.still, 360, 640, "not 8-bit `yuv420p`");
    // A frame too small for the thumbnail's box blur (a 4x4 thumbnail has 2-sample chroma lines, which `boxblur` reads past:
    // FFmpeg 9 refuses to configure it at all, FFmpeg 6 reads garbage). The plan says no; there is no still to compare.
    let plan = RenderPlan::at(
        &alone(&m.portrait, 32, 32),
        &one(&m.portrait),
        &ExportOptions::default(),
        0.5,
        policy(),
    )
    .expect("plan");
    let why = reasons(&plan, plan.size(u32::MAX)).join("; ");
    assert!(why.contains("too small for the thumbnail"), "{why}");
    // A blur, a sharpen or a vignette on the padded frame would see the fill as picture; a dip to white whitens it.
    let mut blurred_clip = clip(&m.portrait, 0.0, 2.0, 0.0);
    blurred_clip.effects = vec![kerf_core::VideoEffect::Blur { sigma: 3.0 }];
    check_refused(
        "blur/refused: blur effect on the padded frame",
        &timeline(vec![vec![blurred_clip]], blur(640, 360)),
        &one(&m.portrait),
        0.5,
        Refusal::Plan("a blur"),
    );
    // A clip the fill does not suit is placed with Contain's chain: a zoomed one is drawn as ever.
    let mut small = clip(&m.portrait, 0.0, 2.0, 0.0);
    small.transform.scale = 0.5;
    check(
        "blur/not taken: zoomed out",
        &timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![small]], blur(640, 360)),
        &[m.bars.clone(), m.portrait.clone()],
        &[0.5],
    );
}

/// The still, exact: its `yuv444p` planes (`timeline_frame_yuv_cancellable`: the composite's samples with the one conversion
/// the graph ends in, 4:2:0 to 4:4:4) against the compositor's `yuv420p` composite put through the same conversion in a
/// graph of the same kind. The comparison the RGB one above cannot make: nothing in it truncates differently, so a differing
/// sample is a difference in the composite. Returns how many samples differ and the largest difference.
fn still_planes_differ(tl: &Timeline, assets: &[Asset], t: f64) -> (usize, i32) {
    let opts = ExportOptions::default();
    let plan = RenderPlan::at(tl, assets, &opts, t, policy()).expect("plan");
    let size = plan.size(u32::MAX);
    let (w, h, want) =
        kerf_core::timeline_frame_yuv_cancellable(tl, assets, &opts, t, size.0, &|| false).expect("the FFmpeg still's planes");
    assert_eq!((w, h), size);
    let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
    let drawn = compositor().composite_yuv(&plan, &frames, size).expect("the GPU's planes");
    let mut raw = drawn.y.clone();
    raw.extend_from_slice(&drawn.u);
    raw.extend_from_slice(&drawn.v);
    let dims = format!("{w}x{h}");
    let got = {
        use std::io::Write;
        let mut child = Command::new(kerf_core::ffmpeg_path())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "-s",
                &dims,
                "-i",
                "pipe:0",
            ])
            .args([
                "-filter_complex",
                "[0:v]format=yuv444p[o]",
                "-map",
                "[o]",
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run ffmpeg");
        let mut stdin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&raw);
        });
        let out = child.wait_with_output().expect("ffmpeg output");
        let _ = writer.join();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        out.stdout
    };
    assert_eq!(got.len(), want.len());
    let mut n = 0;
    let mut max = 0;
    for (a, b) in got.iter().zip(&want) {
        let d = (i32::from(*a) - i32::from(*b)).abs();
        n += usize::from(d != 0);
        max = max.max(d);
    }
    (n, max)
}
