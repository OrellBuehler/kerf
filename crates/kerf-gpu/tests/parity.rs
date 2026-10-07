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
//! * **Enlarging a picture that is not 4:2:0** (4:2:2, 4:4:4, RGB, 12-bit) is
//!   refused: FFmpeg scales in the format the picture has, the decode reduces it
//!   to 8-bit 4:2:0 first, and the two disagree by 15 to 69 levels on an
//!   enlargement (`enlarging_a_picture_ffmpeg_scales_in_another_format_is_refused`).
//!   Shrinking and 1:1 agree for every format.
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
    export_still, Asset, Clip, Color, CompositeColorPolicy, Delivery, ExportOptions, Fit, ImageFormat, Keyframe, Project,
    RenderPlan, StreamKind, Timeline, Track, Transform,
};
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
/// between 70% and 100% flat (the busiest, a shrink to 320x180, 70.1%); a case
/// below this is [`BUSY`] and says so.
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

/// How this FFmpeg picks the composite's matrix (probed from the real graph, once).
fn policy() -> CompositeColorPolicy {
    kerf_core::composite_color_policy()
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
    let opts = ExportOptions::default();
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
        let plan = RenderPlan::at(tl, assets, &opts, t, policy()).expect("plan");
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
        assert_eq!(reference.len(), w * h * 3, "{case} @ {t}: the reference is not {w}x{h}");

        let (frame, timings) = compositor().render_plan(&plan, size).expect("GPU render");
        assert_eq!((frame.width as usize, frame.height as usize), (w, h));
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
    // On top of video, scaled (a 640x360 PNG: a smaller one fitted into this frame
    // would be an enlargement of an RGB picture, which is refused).
    let mut pic = clip(&m.png640, 0.0, 5.0, 0.0);
    pic.transform = Transform {
        scale: 0.5,
        pos_x: -0.2,
        pos_y: -0.2,
        ..Transform::default()
    };
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![pic]], None);
    check(
        "still/pip-over-video",
        &tl,
        &[m.gradient.clone(), m.png640.clone()],
        &[1.0],
        STRICT,
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

    // Keyframed transform: sampled at the same clip time by both renderers.
    let mut kf = clip(&m.testsrc, 0.0, 2.0, 0.5);
    kf.keyframes = vec![
        Keyframe {
            time: 0.0,
            scale: 1.0,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
        },
        Keyframe {
            time: 1.5,
            scale: 0.5,
            pos_x: 0.2,
            pos_y: -0.1,
            rotation: 10.0,
            opacity: 0.6,
        },
    ];
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![kf]], None);
    check(
        "time/keyframes",
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
    // An RGB picture (a PNG) among untagged layers is nothing special; among tagged
    // ones FFmpeg converts it with the negotiated matrix.
    let tl = timeline(vec![vec![clip(&m.gradient, 0.0, 2.0, 0.0)], vec![pip(&m.png640)]], None);
    check(
        "matrix/rgb-png-over-untagged",
        &tl,
        &[m.gradient.clone(), m.png640.clone()],
        &[0.5],
        STRICT,
    );
    let tl = timeline(vec![vec![clip(&m.gradient709, 0.0, 2.0, 0.0)], vec![pip(&m.png640)]], None);
    check_mixed(
        "matrix/rgb-png-over-709",
        &tl,
        &[m.gradient709.clone(), m.png640.clone()],
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

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn enlarging_a_picture_ffmpeg_scales_in_another_format_is_refused() {
    let m = media();
    // FFmpeg scales the picture in the format it has; the compositor in the 8-bit
    // 4:2:0 a decode reduces it to. Shrinking agrees. Enlarging a 4:4:4 picture was
    // 27 levels off, 4:2:2 15, RGB video 51, an RGB PNG 69.
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
                Refusal::Plan("enlarges"),
            );
        }
        check_refused(
            &format!("enlarge/{name}-fit-to-960x540"),
            &scaled(a, 1.0, Some(Delivery::new(960, 540, Fit::Contain))),
            assets,
            0.5,
            Refusal::Plan("enlarges"),
        );
        // ...and what is not an enlargement is drawn, held to the strict limits.
        check(&format!("shrink/{name}-x0.5"), &scaled(a, 0.5, None), assets, &[0.5], STRICT);
        check(
            &format!("shrink/{name}-fit-to-320x180"),
            &scaled(a, 1.0, Some(Delivery::new(320, 180, Fit::Contain))),
            assets,
            &[0.5],
            STRICT,
        );
        check(&format!("same-size/{name}"), &scaled(a, 1.0, None), assets, &[0.5], STRICT);
    }
    // 4:2:0 and gray are what the compositor works in: enlarging them is fine —
    // including 10 bit (FFmpeg scales it at 10 bits and dithers once at the end).
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
// The smallest flat share of any non-busy case is 70.1% (the `shrink/` cases to
// 320x180, band 29.9%); the largest band among the cases judged strictly is that
// one. Every run rewrites `target/parity/report.txt` (these columns plus the
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
// gap                                            1.5   640x360 |    99.0 dB        0 |    99.0 dB        0 |  0.0%    =
// geometry/crop-then-cover                       0.5   360x640 |    48.2 dB        5 |    48.0 dB        5 | 13.0%    =
// geometry/fully-off-canvas                      0.5   640x360 |    46.3 dB        3 |    46.3 dB        3 |  0.0%    =
// geometry/odd-rotated-box                       0.5   640x360 |    46.4 dB        3 |    36.9 dB      254 |  7.1%    =
// geometry/odd-sized-layer                       0.5   640x360 |    46.3 dB        3 |    46.4 dB        5 |  5.5%    =
// geometry/zoom-off-canvas                       0.5   640x360 |    48.1 dB        4 |    48.0 dB        5 |  9.5%    =
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
// matrix/rgb-png-over-709                        0.5   640x360 |    46.5 dB        5 |    46.4 dB        5 |  9.2% refused
// matrix/rgb-png-over-untagged                   0.5   640x360 |    46.5 dB        5 |    46.4 dB        5 |  9.2%    =
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
// same-size/bgr0                                 0.5   640x360 |    48.4 dB        3 |    48.3 dB        3 | 15.7%    =
// same-size/rgb24-png                            0.5   480x270 |    48.4 dB        3 |    48.2 dB        3 | 17.7%    =
// same-size/yuv422p                              0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 16.3%    =
// same-size/yuv444p                              0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 17.9%    =
// shrink/bgr0-fit-to-320x180                     0.5   320x180 |    46.9 dB        4 |    46.3 dB        5 | 29.9%    =
// shrink/bgr0-x0.5                               0.5   640x360 |    54.2 dB        4 |    52.4 dB        5 |  9.8%    =
// shrink/rgb24-png-fit-to-320x180                0.5   320x180 |    46.6 dB        5 |    46.4 dB        6 | 27.8%    =
// shrink/rgb24-png-x0.5                          0.5   480x270 |    54.3 dB        4 |    52.3 dB        5 | 11.7%    =
// shrink/yuv422p-fit-to-320x180                  0.5   320x180 |    48.9 dB        4 |    48.1 dB        5 | 29.4%    =
// shrink/yuv422p-x0.5                            0.5   640x360 |    56.2 dB        4 |    54.1 dB        5 |  9.7%    =
// shrink/yuv444p-fit-to-320x180                  0.5   320x180 |    48.9 dB        3 |    48.0 dB        6 | 29.9%    =
// shrink/yuv444p-x0.5                            0.5   640x360 |    56.3 dB        3 |    54.0 dB        6 |  9.8%    =
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
// still/pip-over-video                             1   640x360 |    46.5 dB        5 |    46.4 dB        5 |  9.2%    =
// time/keyframes                                 0.5   640x360 |    48.8 dB        3 |    48.7 dB        3 | 14.5%    =
// time/keyframes                                1.25   640x360 |    48.2 dB        5 |    34.3 dB      217 | 25.9%    =
// time/keyframes                                   2   640x360 |    56.6 dB        5 |    39.3 dB      132 | 11.1%    =
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
// scaler (plane by plane vs `ffmpeg -vf scale`; worst plane per source, FFmpeg 6.1.1;
// every source is at most 1 level off up to ~4:1, the 2-5 are the 8:1 to 40:1 shrinks)
//   bars       max 2  worst mean 0.493
//   checker    max 1  worst mean 0.572
//   gradient   max 2  worst mean 0.191
//   noise      max 1  worst mean 0.250
//   noise-hd   max 5  worst mean 0.792   (1280x720 down to 32x18, 40:1)
//   testsrc2   max 3  worst mean 0.521   (down to 32x18, 20:1)
// ... FFmpeg 9.0.2: the same maxima, worst means 0.493 / 0.572 / 0.191 / 0.227 / 0.833 / 0.521.
