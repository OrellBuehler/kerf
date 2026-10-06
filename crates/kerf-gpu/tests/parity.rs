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
//! The two renderers cannot be bit-identical: FFmpeg composites 4:2:0 YUV in its
//! own fixed-point scaler and `rotate`, the GPU the same planes in floating point
//! (its canvas is 4:4:4 with the chroma replicated), and a hard coloured edge is a
//! different pixel in each — chroma is averaged over 2x2 in one's alpha blend and
//! per pixel in the other's, a rotated edge is stepped in one and sits at a
//! fraction of a pixel in the other. Away from edges the two agree to a level or
//! two (the rest is swscale's YUV -> RGB conversion, which truncates and carries a
//! mean bias of about one level that the shader does not copy: it is a property of
//! the platform's SIMD, not of the maths). So a frame is judged in two parts:
//!
//! * the **edge band** — every pixel within [`BAND`] px of a strong gradient in
//!   the *reference* (a step of more than [`EDGE_STEP`] levels in any channel
//!   between neighbours, which is every layer boundary and every hard edge of
//!   the test pattern) — is excluded from the strict check, and
//! * everywhere else (the *flat* region) the GPU must hold **PSNR >= 40 dB** and a
//!   **max per-channel error <= 8/255**.
//!
//! The whole-image PSNR (band included) is held to its own floor
//! ([`PSNR_ALL_MIN`], lower for a case with a rotated layer): it exists to catch a
//! layer in the wrong place, which the band alone would excuse.
//!
//! On failure (or always, with `KERF_PARITY_KEEP=1`) the reference, the GPU frame
//! and an amplified diff are written to `target/parity/`.
//!
//! # Known divergences (all inside the edge band, none hidden by a threshold)
//!
//! * **Opacity below 1.** `colorchannelmixer` only takes RGB, so FFmpeg converts
//!   the layer to RGB and back with swscale's 4:2:0 chroma resampling both ways,
//!   which blurs chroma by up to ~4 px around a hard colour edge. The GPU keeps
//!   the chroma sharp. (It does *not* matter for gamut: the blend itself is in YUV
//!   on both sides.)
//! * **An odd-sized layer.** `overlay` blends the chroma sample of the trailing
//!   half block, so the one pixel column / row past an odd layer carries the
//!   layer's chroma on the base's luma — a stray coloured line the GPU does not
//!   draw (`geometry/odd-sized-layer`: whole-image max 207 at that one column).
//! * **A rotated edge** is a fixed-point stair-step in FFmpeg and a float sample
//!   here; the interior agrees to a level.
//! * **swscale's YUV -> RGB** truncates, a mean bias of about one level (the flat
//!   mean error column below); the shader rounds.

#![allow(clippy::print_stderr)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Utc;
use kerf_core::{
    export_still, Asset, Clip, Color, Delivery, ExportOptions, Fit, ImageFormat, Keyframe, RenderPlan, StreamInfo, StreamKind,
    Timeline, Track, Transform,
};
use kerf_gpu::{Compositor, Gpu, GpuOptions};
use uuid::Uuid;

// ---- thresholds (final values; the recorded numbers are at the bottom) ------

/// Pixels this close to a strong reference gradient are the edge band.
const BAND: usize = 2;
/// A neighbour-to-neighbour step (any channel, in 0..255) that counts as an
/// edge. 12 levels, about 5%: at 24 the edges of a layer at 30% opacity (which
/// shows them at 30% of their contrast) fell below it, and the chroma blur
/// FFmpeg's RGB round trip puts around them was then judged as a mismatch in
/// the flat region — the errors above 8 sat 3-6 px from such an edge, never
/// farther (measured), so the threshold moved, the limits did not.
const EDGE_STEP: i32 = 12;
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

/// What a case may relax, and why. Every relaxation is a named constant above.
#[derive(Clone, Copy)]
struct Limits {
    psnr_flat: f64,
    max_flat: i32,
    psnr_all: f64,
}

const STRICT: Limits = Limits {
    psnr_flat: PSNR_FLAT_MIN,
    max_flat: MAX_FLAT,
    psnr_all: PSNR_ALL_MIN,
};

const ROTATED: Limits = Limits {
    psnr_all: PSNR_ALL_MIN_ROTATED,
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
    C.get_or_init(|| Compositor::new(gpu()))
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
}

fn stream(w: u32, h: u32, image: bool) -> StreamInfo {
    StreamInfo {
        index: 0,
        kind: StreamKind::Video,
        codec: if image { "png" } else { "h264" }.into(),
        width: Some(w),
        height: Some(h),
        fps: (!image).then_some(30.0),
        sample_rate: None,
        channels: None,
        image,
        projection: None,
        rotation: 0,
        color_transfer: None,
        color_primaries: None,
    }
}

fn asset(name: &str, path: &Path, duration: f64, s: StreamInfo) -> Asset {
    Asset {
        id: Uuid::new_v4(),
        path: path.to_string_lossy().into_owned(),
        name: name.into(),
        duration,
        streams: vec![s],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
    }
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
        let bars = video("bars.mp4", "smptehdbars=size=640x360:rate=30:duration=2");
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
        let mut rotated_stream = stream(360, 640, false);
        rotated_stream.rotation = 90;
        Media {
            tenbit: asset("tenbit", &tenbit, 2.0, stream(640, 360, false)),
            yuv444: asset("yuv444", &yuv444, 2.0, stream(640, 360, false)),
            fullrange: asset("fullrange", &fullrange, 2.0, stream(640, 360, false)),
            jpeg: asset("jpeg", &jpeg, 5.0, stream(480, 270, true)),
            odd: asset("odd", &odd, 2.0, stream(641, 361, false)),
            rotated: asset("rotated", &rotated_path, 2.0, rotated_stream),
            testsrc: asset("testsrc", &testsrc, 2.0, stream(640, 360, false)),
            bars: asset("bars", &bars, 2.0, stream(640, 360, false)),
            gradient: asset("gradient", &gradient, 2.0, stream(640, 360, false)),
            portrait: asset("portrait", &portrait, 2.0, stream(360, 640, false)),
            still: asset("still", &still, 5.0, stream(480, 270, true)),
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

fn psnr(sq_err: f64, n: usize) -> f64 {
    if sq_err == 0.0 || n == 0 {
        return 99.0;
    }
    10.0 * (255.0 * 255.0 / (sq_err / n as f64)).log10()
}

/// The pixels within `BAND` of a strong gradient in `reference` (RGB, packed).
fn edge_band(reference: &[u8], w: usize, h: usize) -> Vec<bool> {
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
            if step > EDGE_STEP {
                // A step is between two pixels; both are on the edge.
                seed[y * w + x] = true;
                if x + 1 < w {
                    seed[y * w + x + 1] |= (0..3).any(|c| (px(x + 1, y, c) - px(x, y, c)).abs() > EDGE_STEP);
                }
                if y + 1 < h {
                    seed[(y + 1) * w + x] |= (0..3).any(|c| (px(x, y + 1, c) - px(x, y, c)).abs() > EDGE_STEP);
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

fn compare(reference: &[u8], gpu_rgb: &[u8], w: usize, h: usize) -> (Metrics, Vec<bool>) {
    let band = edge_band(reference, w, h);
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
        let plan = RenderPlan::at(tl, assets, &opts, t).expect("plan");
        assert!(
            plan.gpu_supported(),
            "{case} @ {t}: the plan says the GPU cannot draw this: {:?}",
            plan.unsupported_reasons()
        );
        let size = plan.size(u32::MAX);
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

        let (m, band) = compare(&reference, &gpu_rgb, w, h);
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

        let ok = m.psnr_flat >= limits.psnr_flat && m.max_flat <= limits.max_flat && m.psnr_all >= limits.psnr_all;
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
                    "{case} @ {t}s: flat PSNR {:.1} (>= {}), flat max {} (<= {}), whole PSNR {:.1} (>= {}) — images in {}",
                    m.psnr_flat,
                    limits.psnr_flat,
                    m.max_flat,
                    limits.max_flat,
                    m.psnr_all,
                    limits.psnr_all,
                    out_dir.display()
                ));
            }
        }
    }
    write_report();
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
fn opacity() {
    let m = media();
    let mut top = clip(&m.testsrc, 0.0, 2.0, 0.0);
    top.transform.opacity = 0.5;
    let tl = timeline(vec![vec![clip(&m.bars, 0.0, 2.0, 0.0)], vec![top]], None);
    check("opacity/0.5", &tl, &[m.bars.clone(), m.testsrc.clone()], &[0.0, 1.0], STRICT);

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
    // On top of video, scaled.
    let mut pic = clip(&m.still, 0.0, 5.0, 0.0);
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
        &[m.gradient.clone(), m.still.clone()],
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
//   flat region  PSNR >= 40 dB, max per-channel error <= 8/255,
//                outside a 2 px band around every >12-level reference step
//   whole image  PSNR >= 40 dB (>= 30 dB for a case with a rotated layer)
//
// One run on Mesa lavapipe (llvmpipe, LLVM 20.1.2, Vulkan) against FFmpeg 6.1.1.
// All 55 renders also pass against the pinned FFmpeg 9.0.2 build (what the Windows
// and macOS bundles ship) with the same figures, bar three: the two JPEG-sourced
// cases read 53.0 / 52.9 dB flat there (FFmpeg 9 decodes a JPEG slightly
// differently) and opacity/0.3+scale+rotate 45.0 dB. Every run rewrites
// `target/parity/report.txt` (these columns plus the decode / composite time of
// the GPU path).
//
// case                                               t    canvas | flat PSNR flat max |  all PSNR  all max |  band
// colour/brightness+contrast+saturation+gamma      0.5   640x360 |    48.8 dB        3 |    48.6 dB        3 | 15.7%
// colour/contrast+saturation-only                  0.5   640x360 |    47.9 dB        3 |    47.9 dB        3 | 10.0%
// colour/cool+brightness                           0.5   640x360 |    47.6 dB        3 |    47.6 dB        3 | 15.6%
// colour/on-a-transformed-clip                     0.5   640x360 |    48.8 dB        3 |    48.9 dB        6 | 16.1%
// colour/warm                                      0.5   640x360 |    45.6 dB        3 |    45.6 dB        3 |  0.0%
// contain/9x16                                     0.5   360x640 |    55.1 dB        3 |    53.6 dB        5 | 10.9%
// contain/9x16                                     1.2   360x640 |    55.0 dB        3 |    53.6 dB        5 | 10.9%
// contain/downscale-320x180                        0.5   320x180 |    48.9 dB        4 |    48.4 dB        5 | 31.5%
// contain/portrait-in-16x9                         0.7   640x360 |    55.4 dB        2 |    53.5 dB        5 | 13.1%
// contain/upscale-960x540                          0.5   960x540 |    48.8 dB        4 |    48.5 dB        5 | 15.4%
// cover/9x16                                       0.5   360x640 |    46.4 dB        5 |    46.4 dB        5 |  8.1%
// cover/9x16                                       1.2   360x640 |    46.3 dB        4 |    46.4 dB        5 |  7.1%
// cover/portrait-in-16x9                           0.7   640x360 |    49.0 dB        5 |    48.8 dB        6 | 12.3%
// gap                                              1.5   640x360 |    99.0 dB        0 |    99.0 dB        0 |  0.0%
// geometry/crop-then-cover                         0.5   360x640 |    48.2 dB        5 |    48.0 dB        5 | 14.6%
// geometry/fully-off-canvas                        0.5   640x360 |    46.3 dB        3 |    46.3 dB        3 |  0.0%
// geometry/odd-rotated-box                         0.5   640x360 |    46.4 dB        3 |    37.0 dB      251 |  7.1%
// geometry/odd-sized-layer                         0.5   640x360 |    46.3 dB        3 |    41.3 dB      207 |  5.8%
// geometry/zoom-off-canvas                         0.5   640x360 |    48.1 dB        4 |    48.0 dB        5 | 10.3%
// opacity/0.3+scale+rotate                         0.5   640x360 |    44.9 dB        6 |    41.5 dB      110 | 14.2%
// opacity/0.5                                        0   640x360 |    43.1 dB        6 |    42.0 dB       19 | 23.0%
// opacity/0.5                                        1   640x360 |    43.1 dB        6 |    41.8 dB       22 | 23.7%
// pip/cover-then-scale                             0.5   360x640 |    45.7 dB        5 |    45.8 dB        5 |  6.6%
// pip/scaled+offset                                  0   640x360 |    49.4 dB        2 |    49.3 dB        5 | 16.1%
// pip/scaled+offset                                  1   640x360 |    49.4 dB        3 |    49.3 dB        5 | 16.3%
// single/gradient                                    0   640x360 |    46.3 dB        3 |    46.3 dB        3 |  0.0%
// single/gradient                                    1   640x360 |    46.3 dB        3 |    46.3 dB        3 |  0.0%
// single/smptehdbars                                 0   640x360 |    49.8 dB        2 |    49.7 dB        2 | 10.4%
// single/smptehdbars                                 1   640x360 |    49.8 dB        2 |    49.7 dB        2 | 10.4%
// single/testsrc2                                    0   640x360 |    48.8 dB        3 |    48.7 dB        3 | 14.5%
// single/testsrc2                                  0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.9%
// single/testsrc2                               1.2345   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.9%
// source/10-bit                                    0.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.9%
// source/4:4:4                                     0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 20.7%
// source/full-range-jpeg                           0.5   640x360 |    48.9 dB        3 |    48.7 dB        3 | 16.8%
// source/odd-641x361                               0.5   640x360 |    48.8 dB        4 |    48.5 dB        8 | 20.0%
// source/rotated-by-metadata                       0.5   360x640 |    48.9 dB        3 |    48.8 dB        3 | 15.9%
// still/alone                                        0   480x270 |    48.4 dB        3 |    48.2 dB        3 | 19.1%
// still/alone                                      2.5   480x270 |    48.4 dB        3 |    48.2 dB        3 | 19.1%
// still/jpeg                                         1   480x270 |    48.8 dB        3 |    48.6 dB        3 | 20.0%
// still/pip-over-video                               1   640x360 |    46.5 dB        5 |    45.5 dB       15 |  9.8%
// time/keyframes                                   0.5   640x360 |    48.8 dB        3 |    48.7 dB        3 | 14.5%
// time/keyframes                                  1.25   640x360 |    43.6 dB        7 |    32.9 dB      242 | 25.9%
// time/keyframes                                     2   640x360 |    55.7 dB        5 |    38.5 dB      127 | 11.1%
// time/reversed                                    0.3   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.4%
// time/reversed                                    1.1   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.3%
// time/speed-2x-offset                               1   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.5%
// time/speed-2x-offset                             1.4   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.4%
// time/speed-2x-offset                             1.7   640x360 |    48.8 dB        3 |    48.7 dB        3 | 15.6%
// tracks/late-top-clip                             0.5   640x360 |    46.3 dB        3 |    46.3 dB        3 |  0.0%
// tracks/late-top-clip                             1.5   640x360 |    48.9 dB        3 |    48.8 dB        3 | 15.9%
// transform/crop-only                              0.5   640x360 |    47.7 dB        4 |    47.5 dB        5 | 20.7%
// transform/rotate-90                              0.5   640x360 |    47.2 dB        3 |    47.3 dB        3 |  8.3%
// transform/scale+rotate+crop                      0.5   640x360 |    47.0 dB        6 |    33.4 dB      255 | 17.1%
// transform/scale+rotate+crop                      1.5   640x360 |    46.9 dB        6 |    33.5 dB      255 | 17.1%
