//! swscale's `scale` against the scaler model (`kerf_gpu::scaler::scale_plane`), plane for plane, sample for sample:
//! no tolerance. Needs `ffmpeg`; no adapter. The shader is held to the same model in `tests/parity.rs`
//! (`the_scaler_matches_ffmpegs_scale_plane_by_plane`) and in the compositor's own plane test.
//!
//! The scales run in a **complex** filter graph, as the product's do: an unflagged `scale` there gets the filter's own
//! default algorithm (bilinear before FFmpeg 5.0, bicubic since), where a simple `-vf` graph is handed the command
//! line's bicubic.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test scaler -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(clippy::print_stderr)]

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use kerf_gpu::scaler::{self, Kernels, Model};
use kerf_gpu::sws::Scaler;

/// A noise-like source made by `geq` from the coordinates (integers, so the same on every build and thread count) in all
/// three planes, `w` x `h`.
fn source(w: usize, h: usize) -> String {
    format!(
        "nullsrc=s={w}x{h}:r=1:d=1,format=yuv420p,\
         geq=lum='mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29,256)':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17,256)'"
    )
}

/// A smoother source: bars of gradients and hard steps, the shape of real footage's edges.
fn smooth_source(w: usize, h: usize) -> String {
    format!(
        "nullsrc=s={w}x{h}:r=1:d=1,format=yuv420p,\
         geq=lum='clip(16+mod(floor(X/37),2)*180+mod(Y,64)*0.6+abs(X-Y)*0.05,16,235)':cb='128+60*sin(X/23)':cr='128+50*cos(Y/17)'"
    )
}

/// How many scales were compared with FFmpeg's and how many samples (planes counted) were held equal, for the run's report.
static SCALES: AtomicUsize = AtomicUsize::new(0);
static SAMPLES: AtomicUsize = AtomicUsize::new(0);

/// One frame of a complex graph (`graph` is a chain; `[o]` is appended).
fn frame(graph: &str, extra: &[&str]) -> Vec<u8> {
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(extra)
        .args(["-filter_complex", &format!("{graph}[o]"), "-map", "[o]"])
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"])
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

/// How many samples of `got` differ from `want`, the largest difference, and the first one's (x, y).
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

/// Every plane of the scale of `(sw, sh)` to `(dw, dh)` in FFmpeg against the model's, `Err` with a description when
/// anything differs. `flag` is the `flags=` of the filter (`None`: unflagged).
fn check(
    src: &str,
    (sw, sh): (usize, usize),
    (dw, dh): (usize, usize),
    flag: Option<Scaler>,
    model: Model,
    extra: &[&str],
) -> Result<(), String> {
    let flags = match flag {
        None => "",
        Some(Scaler::Bicubic) => ":flags=bicubic",
        Some(Scaler::Bilinear) => ":flags=bilinear",
        Some(Scaler::Lanczos) => ":flags=lanczos",
        Some(Scaler::Spline) => ":flags=spline",
        Some(Scaler::Neighbor) => ":flags=neighbor",
    };
    SCALES.fetch_add(1, Ordering::Relaxed);
    let source = frame(src, &[]);
    let (cw, ch) = (sw.div_ceil(2), sh.div_ceil(2));
    let (ocw, och) = (dw.div_ceil(2), dh.div_ceil(2));
    let want = frame(&format!("{src},scale={dw}:{dh}{flags}"), extra);
    let (y, rest) = source.split_at(sw * sh);
    let (u, v) = rest.split_at(cw * ch);
    let (wy, rest) = want.split_at(dw * dh);
    let (wu, wv) = rest.split_at(ocw * och);
    let scaler = model.scaler(flag);
    let mut errors = Vec::new();
    for (name, plane, from, to, rows, expect) in [
        ("Y", y, (sw, sh), (dw, dh), 2, wy),
        ("U", u, (cw, ch), (ocw, och), 1, wu),
        ("V", v, (cw, ch), (ocw, och), 1, wv),
    ] {
        let got = scaler::scale_plane(plane, from, to, scaler, model.kernel, rows).ok_or("no table")?;
        SAMPLES.fetch_add(got.len(), Ordering::Relaxed);
        let (n, max, first) = diff(&got, expect, to.0);
        if n > 0 {
            errors.push(format!("{name}: {n} samples differ, max {max}, first at {first:?}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{sw}x{sh} -> {dw}x{dh} {flag:?} {model:?} {extra:?}: {}",
            errors.join("; ")
        ))
    }
}

/// The named scalers and the names the engine writes for them.
const NAMED: [(Scaler, &str); 4] = [
    (Scaler::Bilinear, "bilinear"),
    (Scaler::Lanczos, "lanczos"),
    (Scaler::Spline, "spline"),
    (Scaler::Neighbor, "neighbor"),
];

/// Scales at the steepest shrink the plan lets each name be drawn to (`kerf_core::max_shrink_for`) and just inside it, with odd sizes: the
/// source, the output.
fn at_the_limit(name: &str) -> Vec<((usize, usize), (usize, usize))> {
    match name {
        "lanczos" => vec![
            ((1260, 756), (30, 18)),
            ((1260, 756), (31, 19)),
            ((1281, 757), (31, 19)),
            ((1279, 714), (31, 17)),
        ],
        "spline" => vec![((1260, 720), (70, 40)), ((1283, 723), (72, 41)), ((1280, 720), (72, 40))],
        _ => vec![((1260, 756), (20, 12)), ((1281, 757), (21, 13)), ((1279, 719), (21, 12))],
    }
}

#[test]
fn the_cases_at_the_limit_are_inside_it_and_at_it() {
    for (_, name) in NAMED {
        let limit = kerf_core::max_shrink_for(Some(name)) as usize;
        let cases = at_the_limit(name);
        for (from, to) in &cases {
            assert!(
                from.0 <= limit * to.0 && from.1 <= limit * to.1,
                "{name}: {from:?} -> {to:?} is past the plan's limit"
            );
        }
        assert!(
            cases
                .iter()
                .any(|(from, to)| from.0 == limit * to.0 || from.1 == limit * to.1),
            "{name}: no case is at the limit exactly"
        );
    }
}

/// The model this machine's FFmpeg is, per the probe: the rest of the file follows it.
fn model() -> Model {
    match scaler::kernels() {
        Kernels::Match(m) => m,
        other => panic!("the probe says {other:?} for {}", kerf_core::ffmpeg_path()),
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn this_ffmpegs_scaler_is_the_model() {
    let started = std::time::Instant::now();
    let k = scaler::kernels();
    eprintln!(
        "scaler probe: {k:?} in {:.0} ms ({})",
        started.elapsed().as_secs_f64() * 1e3,
        kerf_core::ffmpeg_path()
    );
    assert!(k.exact(), "{k:?} for {}", kerf_core::ffmpeg_path());
    assert_eq!(scaler::measured(), k);
    // The named scalers a delivery can ask for: Lanczos and spline are `double` arithmetic and `libm`, so this is a finding about the
    // machine, not about the model, and a machine whose `sin` differs would say so here.
    assert_eq!(
        k.named(),
        kerf_core::ScalerKinds::ALL,
        "the probe proved only {:?} of the named scalers for {}",
        k.named(),
        kerf_core::ffmpeg_path()
    );
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_scaler_model_is_ffmpegs_sample_for_sample() {
    let model = model();
    let mut failures = Vec::new();
    // Shrinks from 1280x720 (2:1 to 63:1, the plan's `MAX_SHRINK`), odd outputs, and enlargements, noise and a smoother picture, both scalers.
    let sizes: &[(usize, usize)] = &[
        (640, 360),
        (853, 480),
        (480, 270),
        (320, 180),
        (161, 91),
        (160, 90),
        (120, 68),
        (97, 55),
        (64, 36),
        (41, 23),
        (40, 22),
        (31, 17),
        (26, 15),
        (21, 12),
        (21, 13),
        (32, 18),
        (1920, 1080),
        (1281, 721),
        (1600, 900),
        (2561, 1441),
    ];
    for (name, graph) in [("noise", source(1280, 720)), ("smooth", smooth_source(1280, 720))] {
        for flag in [None, Some(Scaler::Bicubic), Some(Scaler::Bilinear)] {
            for &to in sizes {
                if let Err(e) = check(&graph, (1280, 720), to, flag, model, &[]) {
                    failures.push(format!("{name}: {e}"));
                }
            }
        }
    }
    // The scalers a delivery can name besides bicubic, bilinear: Lanczos, spline and neighbor, over the same sizes within the shrink the plan
    // draws them to (`kerf_core::max_shrink_for`: a table does not exist past it, where swscale cascades a second scaler).
    for (name, graph) in [("noise", source(1280, 720)), ("smooth", smooth_source(1280, 720))] {
        for (flag, flag_name) in NAMED {
            let limit = kerf_core::max_shrink_for(Some(flag_name)) as usize;
            for &to in sizes {
                if 1280 > limit * to.0 || 720 > limit * to.1 {
                    continue;
                }
                if let Err(e) = check(&graph, (1280, 720), to, Some(flag), model, &[]) {
                    failures.push(format!("{name} {flag_name}: {e}"));
                }
            }
        }
    }
    for (flag, flag_name) in NAMED {
        for (from, to) in at_the_limit(flag_name) {
            for (name, graph) in [("noise", source(from.0, from.1)), ("smooth", smooth_source(from.0, from.1))] {
                if let Err(e) = check(&graph, from, to, Some(flag), model, &[]) {
                    failures.push(format!("{name} {flag_name}: {e}"));
                }
            }
        }
    }
    // The thumbnail the blurred fill makes and the fill it scales back up: ratios of about 4.5:1 and 8:1; and the
    // picture's own fit.
    let portrait = source(1080, 1920);
    for (to, flag) in [
        ((240, 427), Some(Scaler::Bilinear)),
        ((608, 1080), None),
        ((608, 1080), Some(Scaler::Bicubic)),
    ] {
        if let Err(e) = check(&portrait, (1080, 1920), to, flag, model, &[]) {
            failures.push(format!("portrait: {e}"));
        }
    }
    let thumb = source(240, 134);
    for to in [(1920, 1080), (1280, 720), (480, 270)] {
        if let Err(e) = check(&thumb, (240, 134), to, Some(Scaler::Bilinear), model, &[]) {
            failures.push(format!("thumbnail: {e}"));
        }
    }
    for f in &failures {
        eprintln!("{f}");
    }
    eprintln!(
        "scaler sweep: {} scales against {}, {} samples compared, {} differ",
        SCALES.load(Ordering::Relaxed),
        kerf_core::ffmpeg_path(),
        SAMPLES.load(Ordering::Relaxed),
        failures.len()
    );
    assert!(failures.is_empty(), "{} scales differ", failures.len());
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_scalers_answer_does_not_depend_on_the_filter_thread_count() {
    let model = model();
    let graph = source(640, 360);
    for threads in [
        ["-filter_complex_threads", "1"],
        ["-filter_complex_threads", "4"],
        ["-filter_complex_threads", "16"],
    ] {
        for flag in [
            None,
            Some(Scaler::Bilinear),
            Some(Scaler::Lanczos),
            Some(Scaler::Spline),
            Some(Scaler::Neighbor),
        ] {
            for to in [(320, 180), (333, 187), (1280, 720)] {
                if let Err(e) = check(&graph, (640, 360), to, flag, model, &threads) {
                    panic!("{e}");
                }
            }
        }
    }
}

/// `boxblur=r:2` on a noise picture against the scalar port of `blur8` (`kerf_gpu::fill`), every plane at its own size
/// with the same radius (the chroma radius defaults to the luma's), sample for sample.
#[test]
#[ignore = "needs ffmpeg"]
fn the_box_blur_reference_is_ffmpegs_sample_for_sample() {
    let mut failures = Vec::new();
    // The thumbnail sizes a frame gives (`2*floor(w/16)` by `2*floor(h/16)`), small, odd chroma sizes and every radius up
    // to what a plane allows.
    for (w, h, radii) in [
        (240usize, 134usize, vec![1usize, 2, 8, 16, 33]),
        (134, 240, vec![1, 8, 16]),
        (80, 44, vec![1, 2, 5, 10]),
        (30, 16, vec![1, 3, 7]),
        (12, 8, vec![1]),
        (481, 271, vec![1, 12, 30]),
        (9, 9, vec![1, 2, 4]),
    ] {
        let source = frame(&source(w, h), &[]);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (y, rest) = source.split_at(w * h);
        let (u, v) = rest.split_at(cw * ch);
        for r in radii {
            // (FFmpeg refuses a radius over half the smaller side of a chroma plane, `<=` in 4.4 and `<` in 9.0, of the
            // plane's size rounded down.)
            if 2 * r + 1 > (w / 2).min(h / 2) {
                continue;
            }
            let want = frame(&format!("{},boxblur={r}:2", self::source(w, h)), &[]);
            let (wy, rest) = want.split_at(w * h);
            let (wu, wv) = rest.split_at(cw * ch);
            for (name, plane, (pw, ph), expect) in [("Y", y, (w, h), wy), ("U", u, (cw, ch), wu), ("V", v, (cw, ch), wv)] {
                // (The model refuses a radius the running sum reads past the line with.)
                let Some(got) = kerf_gpu::fill::box_blur(plane, pw, ph, r, 2) else {
                    continue;
                };
                let (n, max, first) = diff(&got, expect, pw);
                if n > 0 {
                    failures.push(format!("{w}x{h} r{r} {name}: {n} differ, max {max}, first {first:?}"));
                }
            }
        }
    }
    for f in &failures {
        eprintln!("{f}");
    }
    assert!(failures.is_empty());
}
