//! swscale's `scale` of an 8-bit planar 4:4:4 picture into `yuv420p` (and `yuva420p`) against the scaler model
//! (`kerf_gpu::scaler::scale_plane`), plane for plane, sample for sample: no tolerance. Needs `ffmpeg`; no adapter.
//!
//! The export's fit `scale` of a 4:4:4 clip is one swscale call from `yuv444p` straight to the composite's `yuva420p` (the chain
//! after it is 4:2:0 throughout), so the chroma planes are scaled from the **full-resolution** planes -- `W x H` to `w/2 x h/2`,
//! twice the ratio of the luma's -- by the same tables the luma uses, with the destination's centred chroma positions. This file
//! holds the model of that call to FFmpeg's own, over many sizes, scalers, filter thread counts and contents.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_444_scaler -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(clippy::print_stderr)]

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use kerf_gpu::scaler::{self, Kernels, Model};
use kerf_gpu::sws::Scaler;

/// A noise-like `yuv444p` source made by `geq` from the coordinates (integers, so the same on every build and thread count) in
/// all three planes, `w` x `h`.
fn source(w: usize, h: usize) -> String {
    format!(
        "nullsrc=s={w}x{h}:r=1:d=1,format=yuv444p,\
         geq=lum='mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29,256)':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17,256)'"
    )
}

/// A smoother source: bars of gradients and hard steps, the shape of real footage's edges, and a one-pixel chroma stripe pattern
/// (which a 4:2:0 reduction of the picture would average away).
fn smooth_source(w: usize, h: usize) -> String {
    format!(
        "nullsrc=s={w}x{h}:r=1:d=1,format=yuv444p,\
         geq=lum='clip(16+mod(floor(X/37),2)*180+mod(Y,64)*0.6+abs(X-Y)*0.05,16,235)':cb='128+60*sin(X/23)+40*mod(X,2)':cr='128+50*cos(Y/17)-30*mod(Y,2)'"
    )
}

static SCALES: AtomicUsize = AtomicUsize::new(0);
static SAMPLES: AtomicUsize = AtomicUsize::new(0);

/// One frame of a complex graph (`graph` is a chain; `[o]` is appended), as `pix_fmt` planes.
fn frame(graph: &str, extra: &[&str], pix_fmt: &str) -> Vec<u8> {
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(extra)
        .args(["-filter_complex", &format!("{graph}[o]"), "-map", "[o]"])
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", pix_fmt, "pipe:1"])
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {graph}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn diff(got: &[u8], want: &[u8], width: usize) -> (usize, i32, Option<(usize, usize)>) {
    let mut n = 0;
    let mut max = 0;
    let mut first = None;
    for (i, (a, b)) in got.iter().zip(want).enumerate() {
        let d = (i32::from(*a) - i32::from(*b)).abs();
        if d != 0 {
            n += 1;
            max = max.max(d);
            first.get_or_insert((i % width, i / width));
        }
    }
    (n, max, first)
}

fn flag_text(flag: Option<Scaler>) -> &'static str {
    match flag {
        None => "",
        Some(Scaler::Bicubic) => ":flags=bicubic",
        Some(Scaler::Bilinear) => ":flags=bilinear",
        Some(Scaler::Lanczos) => ":flags=lanczos",
        Some(Scaler::Spline) => ":flags=spline",
        Some(Scaler::Neighbor) => ":flags=neighbor",
    }
}

/// The model of one 4:4:4 -> 4:2:0 scale: luma `W x H -> w x h`, chroma `W x H -> w/2 x h/2` (both from the full-size planes).
fn model_planes(
    src: &[u8],
    (sw, sh): (usize, usize),
    (dw, dh): (usize, usize),
    scaler: Scaler,
    kernel: scaler::Kernel,
) -> Option<[Vec<u8>; 3]> {
    let n = sw * sh;
    let (ocw, och) = (dw.div_ceil(2), dh.div_ceil(2));
    Some([
        scaler::scale_plane(&src[..n], (sw, sh), (dw, dh), scaler, kernel, 2)?,
        scaler::scale_plane(&src[n..2 * n], (sw, sh), (ocw, och), scaler, kernel, 1)?,
        scaler::scale_plane(&src[2 * n..], (sw, sh), (ocw, och), scaler, kernel, 1)?,
    ])
}

/// The window `(x, y, w, h)` of a `yuv444p` frame `(sw, sh)`, as three planes.
fn window(src: &[u8], (sw, sh): (usize, usize), (x, y, w, h): (usize, usize, usize, usize)) -> Vec<u8> {
    let n = sw * sh;
    let mut out = Vec::with_capacity(3 * w * h);
    for plane in 0..3 {
        for row in y..y + h {
            out.extend_from_slice(&src[plane * n + row * sw + x..plane * n + row * sw + x + w]);
        }
    }
    out
}

/// What a case is: the source graph and its size, the `crop` window FFmpeg is told (and the model reads), the output size, the
/// scaler's flag, extra arguments and the destination format.
struct Case<'a> {
    src: &'a str,
    from: (usize, usize),
    crop: Option<(usize, usize, usize, usize)>,
    to: (usize, usize),
    flag: Option<Scaler>,
    extra: &'a [&'a str],
    pix_fmt: &'a str,
}

impl<'a> Case<'a> {
    fn new(src: &'a str, from: (usize, usize), to: (usize, usize), flag: Option<Scaler>) -> Self {
        Self {
            src,
            from,
            crop: None,
            to,
            flag,
            extra: &[],
            pix_fmt: "yuv420p",
        }
    }
}

/// Every plane of FFmpeg's scale of a `yuv444p` picture (or a window of it) to `pix_fmt` against the model's.
fn check(case: &Case<'_>, model: Model) -> Result<(), String> {
    SCALES.fetch_add(1, Ordering::Relaxed);
    let Case {
        src,
        from,
        crop,
        to: (dw, dh),
        flag,
        extra,
        pix_fmt,
    } = *case;
    let source = frame(src, &[], "yuv444p");
    let (win, pre) = match crop {
        Some((x, y, w, h)) => ((x, y, w, h), format!("crop={w}:{h}:{x}:{y},")),
        None => ((0, 0, from.0, from.1), String::new()),
    };
    let planes = window(&source, from, win);
    let (ocw, och) = (dw.div_ceil(2), dh.div_ceil(2));
    let want = frame(
        &format!("{src},{pre}scale={dw}:{dh}{},format={pix_fmt}", flag_text(flag)),
        extra,
        pix_fmt,
    );
    let got = model_planes(&planes, (win.2, win.3), (dw, dh), model.scaler(flag), model.kernel).ok_or("no table")?;
    let (wy, rest) = want.split_at(dw * dh);
    let (wu, rest) = rest.split_at(ocw * och);
    let (wv, alpha) = rest.split_at(ocw * och);
    let mut errors = Vec::new();
    for (name, plane, expect, width) in [("Y", &got[0], wy, dw), ("U", &got[1], wu, ocw), ("V", &got[2], wv, ocw)] {
        SAMPLES.fetch_add(plane.len(), Ordering::Relaxed);
        let (n, max, first) = diff(plane, expect, width);
        if n > 0 {
            errors.push(format!("{name}: {n} samples differ, max {max}, first at {first:?}"));
        }
    }
    if pix_fmt == "yuva420p" {
        SAMPLES.fetch_add(alpha.len(), Ordering::Relaxed);
        let n = alpha.iter().filter(|a| **a != 255).count();
        if n > 0 || alpha.len() != dw * dh {
            errors.push(format!("A: {n} samples are not 255 ({} bytes)", alpha.len()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{}x{} {crop:?} -> {dw}x{dh} {flag:?} {model:?} {extra:?} {pix_fmt}: {}",
            from.0,
            from.1,
            errors.join("; ")
        ))
    }
}

fn model() -> Model {
    match scaler::kernels() {
        Kernels::Match(m) => m,
        other => panic!("the probe says {other:?} for {}", kerf_core::ffmpeg_path()),
    }
}

/// Whether the chroma of a shrink `from` -> `to` has a table: its ratio is twice the luma's, and swscale's filter outgrows 256 taps
/// at 64:1 (bicubic and bilinear; `kerf_core::max_shrink_for` for the others).
fn chroma_has_a_table(name: &str, from: (usize, usize), to: (usize, usize)) -> bool {
    let limit = kerf_core::max_shrink_for(Some(name)) as usize;
    from.0 <= limit * to.0.div_ceil(2) && from.1 <= limit * to.1.div_ceil(2)
}

fn finish(name: &str, failures: Vec<String>) {
    for f in &failures {
        eprintln!("{f}");
    }
    eprintln!(
        "{name}: {} scales against {}, {} samples compared, {} differ",
        SCALES.load(Ordering::Relaxed),
        kerf_core::ffmpeg_path(),
        SAMPLES.load(Ordering::Relaxed),
        failures.len()
    );
    assert!(failures.is_empty(), "{} scales differ", failures.len());
}

/// The probe the compositor's caps are made from, on this FFmpeg: the export's own graph and the still's over toy cuts, every plane held to
/// the model. Needs a measured scaler, so it is `Match` only where `scaler::kernels()` is.
#[test]
#[ignore = "needs ffmpeg"]
fn this_ffmpeg_resizes_a_444_picture_as_the_probe_says() {
    let started = std::time::Instant::now();
    let verdict = kerf_gpu::resize444::resize_444();
    eprintln!(
        "4:4:4 resize probe: {verdict:?} in {:.0} ms ({})",
        started.elapsed().as_secs_f64() * 1e3,
        kerf_core::ffmpeg_path()
    );
    assert_eq!(verdict, kerf_gpu::resize444::Resize444::Match);
    assert_eq!(kerf_gpu::resize444::measured(), verdict);
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_444_model_is_ffmpegs_sample_for_sample() {
    let model = model();
    let mut failures = Vec::new();
    // Shrinks to the steepest the chroma has a table for (twice the luma's ratio), enlargements, the 2:1 that is the conversion alone
    // horizontally, and sizes whose halves are odd.
    let sizes: &[(usize, usize)] = &[
        (640, 360),
        (1280, 720),
        (853, 480),
        (480, 270),
        (320, 180),
        (160, 90),
        (120, 68),
        (96, 54),
        (64, 36),
        (42, 24),
        (1920, 1080),
        (1600, 900),
        (2560, 1440),
        (1280, 722),
        (642, 360),
        (1280, 360),
        (640, 720),
        (1274, 718),
    ];
    for (name, graph) in [("noise", source(1280, 720)), ("smooth", smooth_source(1280, 720))] {
        for flag in [None, Some(Scaler::Bicubic), Some(Scaler::Bilinear)] {
            for &to in sizes {
                if !chroma_has_a_table("bicubic", (1280, 720), to) {
                    continue;
                }
                if let Err(e) = check(&Case::new(&graph, (1280, 720), to, flag), model) {
                    failures.push(format!("{name}: {e}"));
                }
            }
        }
    }
    finish("444 sweep", failures);
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_444_models_steepest_shrinks_and_where_it_has_no_table() {
    let model = model();
    let mut failures = Vec::new();
    // At the limit (chroma 63:1, luma 31.5:1) and just inside, with odd output halves; and past it, where swscale cascades two scalers
    // and the model has no table: it must say so rather than guess.
    for (from, to) in [
        ((1260, 756), (40, 24)),
        ((1260, 756), (42, 24)),
        ((1282, 722), (42, 24)),
        ((1256, 720), (40, 22)),
    ] {
        for (name, graph) in [("noise", source(from.0, from.1)), ("smooth", smooth_source(from.0, from.1))] {
            let case = Case::new(&graph, from, to, None);
            if chroma_has_a_table("bicubic", from, to) {
                if let Err(e) = check(&case, model) {
                    failures.push(format!("{name}: {e}"));
                }
            } else {
                let source = frame(&graph, &[], "yuv444p");
                let none = model_planes(&source, from, to, model.scaler(None), model.kernel);
                if none.is_some() {
                    failures.push(format!("{name}: {from:?} -> {to:?} has a model past the limit"));
                }
            }
        }
    }
    finish("444 steepest", failures);
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_444_model_holds_for_every_scaler_a_delivery_names() {
    let model = model();
    let mut failures = Vec::new();
    for (flag, name) in [
        (Scaler::Bilinear, "bilinear"),
        (Scaler::Lanczos, "lanczos"),
        (Scaler::Spline, "spline"),
        (Scaler::Neighbor, "neighbor"),
    ] {
        for to in [(640, 360), (426, 240), (320, 180), (1920, 1080), (1282, 722)] {
            if !chroma_has_a_table(name, (1280, 720), to) {
                continue;
            }
            for (content, graph) in [("noise", source(1280, 720)), ("smooth", smooth_source(1280, 720))] {
                if let Err(e) = check(&Case::new(&graph, (1280, 720), to, Some(flag)), model) {
                    failures.push(format!("{content} {name}: {e}"));
                }
            }
        }
    }
    finish("444 named scalers", failures);
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_444_models_answer_does_not_depend_on_the_filter_thread_count() {
    let model = model();
    let mut failures = Vec::new();
    let graph = source(640, 360);
    for threads in [
        ["-filter_complex_threads", "1"],
        ["-filter_complex_threads", "4"],
        ["-filter_complex_threads", "16"],
    ] {
        for flag in [None, Some(Scaler::Bilinear), Some(Scaler::Lanczos)] {
            for to in [(320, 180), (426, 240), (1280, 720)] {
                let case = Case {
                    extra: &threads,
                    ..Case::new(&graph, (640, 360), to, flag)
                };
                if let Err(e) = check(&case, model) {
                    failures.push(e);
                }
            }
        }
    }
    finish("444 threads", failures);
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_444_model_holds_for_a_window_and_for_an_alpha_destination() {
    let model = model();
    let mut failures = Vec::new();
    let graph = source(1280, 720);
    // The engine's static crop is a `crop` ahead of the scale, on the picture's own (4:4:4) grid: the scaler clamps at the window's edges.
    for crop in [
        (320, 72, 640, 360),
        (0, 0, 640, 360),
        (640, 360, 640, 360),
        (160, 80, 960, 540),
        (2, 2, 1000, 600),
    ] {
        for to in [(640, 360), (320, 180), (960, 540), (1280, 720)] {
            let case = Case {
                crop: Some(crop),
                ..Case::new(&graph, (1280, 720), to, None)
            };
            if let Err(e) = check(&case, model) {
                failures.push(e);
            }
        }
    }
    for to in [(640, 360), (1920, 1080)] {
        let case = Case {
            pix_fmt: "yuva420p",
            ..Case::new(&graph, (1280, 720), to, None)
        };
        if let Err(e) = check(&case, model) {
            failures.push(e);
        }
    }
    finish("444 windows and alpha", failures);
}
