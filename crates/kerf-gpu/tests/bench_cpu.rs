//! What the CPU-side per-frame work of the compositor costs, and what the SIMD paths of
//! `kerf_core::simd` / `kerf_core::mask_alpha_plane` save on it. No adapter and no FFmpeg: pure CPU.
//!
//! `#[ignore]`d *and* gated on `KERF_BENCH=1`:
//!
//! ```text
//! KERF_BENCH=1 cargo test -p kerf-gpu --no-default-features --release --test bench_cpu -- \
//!     --ignored --nocapture
//! ```
//!
//! The best of nine runs after a warm-up, at 1080p and 4K. Each row that has vector paths is timed on
//! every path this CPU can run (`KERF_SIMD` caps the default, the rows force their own). Every sample
//! of the compositor's CPU-side data is 8-bit: the decode is `yuv420p` (a 10-bit source is brought to
//! 8-bit by FFmpeg before it reaches the process, `y4m.rs`) and the readback is `Rgba8`, so there is
//! no 10-bit or 16-bit row to time. Numbers go to stderr and `target/parity/bench-cpu.txt`.

#![allow(clippy::print_stderr)]

use std::hint::black_box;
use std::io::Cursor;
use std::path::Path;
use std::time::Instant;

use kerf_core::simd::{self, Path as SimdPath};
use kerf_core::{mask_alpha_plane, Mask, MaskShape};
use kerf_gpu::effects;
use kerf_gpu::sws;
use kerf_gpu::y4m::Y4mReader;
use kerf_gpu::YuvFrame;

const SIZES: [(usize, usize); 2] = [(1920, 1080), (3840, 2160)];

fn best_ms<R>(mut f: impl FnMut() -> R) -> f64 {
    black_box(f());
    black_box(f());
    (0..9)
        .map(|_| {
            let t = Instant::now();
            black_box(f());
            t.elapsed().as_secs_f64() * 1e3
        })
        .fold(f64::INFINITY, f64::min)
}

/// One row: the work timed on each path (`name  Scalar 1.234  Sse2 0.123 ...`).
fn row<R>(report: &mut String, name: &str, mut f: impl FnMut() -> R) {
    let mut line = format!("{name:<46}");
    for p in simd::available() {
        let t = simd::with_path(p, || best_ms(&mut f));
        line += &format!(" {:>6} {t:8.3} ms", format!("{p:?}"));
    }
    report.push_str(&line);
    report.push('\n');
}

/// A row with no vector path: timed once.
fn plain<R>(report: &mut String, name: &str, f: impl FnMut() -> R) {
    report.push_str(&format!("{name:<46} {:>6} {:8.3} ms\n", "-", best_ms(f)));
}

/// What `Compositor::read_texels` does with a mapped, row-padded buffer.
fn unpad(mapped: &[u8], pitch: usize, w: usize, h: usize, bpp: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(w * h * bpp);
    for row in mapped.chunks(pitch).take(h) {
        data.extend_from_slice(&row[..w * bpp]);
    }
    data
}

/// `effects::vignette_map` as it was before it worked each magnitude once: every sample's `hypot` and `cos`.
fn vignette_map_every_sample(w: u32, h: u32) -> Vec<f32> {
    let angle = f64::from((std::f64::consts::PI / 5.0) as f32);
    let (x0, y0) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
    let dmax = (f64::from(w) / 2.0).hypot(f64::from(h) / 2.0);
    let mut map = Vec::with_capacity(w as usize * h as usize);
    for y in 0..h {
        let yy = (f64::from(y) - y0) as i32;
        for x in 0..w {
            let xx = (f64::from(x) - x0) as i32;
            let d = f64::from(xx).hypot(f64::from(yy)) / dmax;
            let v = if d > 1.0 {
                0.0
            } else {
                let c = (angle * d).cos();
                (c * c) * (c * c)
            };
            map.push(v as f32);
        }
    }
    map
}

fn y4m(w: usize, h: usize) -> Vec<u8> {
    let mut v = format!("YUV4MPEG2 W{w} H{h} F30:1 Ip A1:1 C420jpeg\nFRAME\n").into_bytes();
    v.extend((0..w * h * 3 / 2).map(|i| i as u8));
    v
}

#[test]
#[ignore = "benchmark: set KERF_BENCH=1"]
fn bench_cpu_side_per_frame_work() {
    if std::env::var_os("KERF_BENCH").is_none() {
        eprintln!("skipped: set KERF_BENCH=1 to run the benchmark");
        return;
    }
    let mut report = format!("paths: {:?}\n", simd::available());
    for (w, h) in SIZES {
        report += &format!("\n== {w}x{h}\n");
        report += "-- title masks and the opaque-alpha scan (kerf_core::simd)\n";
        let empty = vec![0u8; w * h];
        let mut glyphs = vec![0u8; w * h];
        for y in h * 4 / 5..h * 4 / 5 + h / 12 {
            for x in w / 4..w * 3 / 4 {
                glyphs[y * w + x] = if (x * 7 + y) % 5 == 0 { 255 } else { 0 };
            }
        }
        let opaque = vec![255u8; w * h];
        row(&mut report, "text mask bounds, glyph band", || {
            simd::plane_bounds(&glyphs, w, h)
        });
        row(&mut report, "text mask bounds, empty plane", || {
            simd::plane_bounds(&empty, w, h)
        });
        row(&mut report, "alpha scan / solid box check (opaque)", || {
            simd::position_ne(&opaque, 255)
        });

        report += "-- mask alpha plane (kerf_core::mask_alpha_plane), per (mask, size) the cache misses\n";
        let (w32, h32) = (w as u32, h as u32);
        for (shape, feather) in [
            (MaskShape::Rect, 0.0),
            (MaskShape::Rect, 0.15),
            (MaskShape::Ellipse, 0.0),
            (MaskShape::Ellipse, 0.15),
        ] {
            let m = Mask {
                shape,
                x: 0.5,
                y: 0.5,
                width: 0.6,
                height: 0.5,
                feather,
                inverted: false,
            };
            row(&mut report, &format!("mask {shape:?} feather {feather}"), || {
                mask_alpha_plane(&m, w32, h32)
            });
        }

        report += "-- no vector path: memcpy-bound, or libm / integer arithmetic\n";
        let pitch = (w * 4).next_multiple_of(256);
        let mapped = vec![7u8; pitch * h];
        plain(&mut report, "readback unpad, Rgba8", || unpad(&mapped, pitch, w, h, 4));
        let raw = vec![9u8; w * h * 3 / 2];
        plain(&mut report, "YuvFrame::from_yuv420p (the one-shot decode)", || {
            YuvFrame::from_yuv420p(w32, h32, &raw)
        });
        let stream = y4m(w, h);
        plain(&mut report, "Y4mReader, one frame from memory", || {
            Y4mReader::new(Cursor::new(&stream), (w32, h32)).next_frame()
        });
        let frame = YuvFrame::from_yuv420p(w32, h32, &raw).expect("a frame");
        let other = frame.clone();
        plain(&mut report, "cache conflict check (frame == frame)", || frame == other);
        for (name, (sw, sh), (dw, dh)) in [
            ("4K -> 1080p", (3840u32, 2160u32), (1920u32, 1080u32)),
            ("1080p -> 720p", (1920, 1080), (1280, 720)),
            ("720p -> 1080p", (1280, 720), (1920, 1080)),
        ] {
            let six = |table: fn(u32, u32, i32) -> Option<sws::Filter>| {
                let mut taps = 0;
                for (s, d, one) in [
                    (sw, dw, sws::ONE_HORIZONTAL),
                    (sh, dh, sws::ONE_VERTICAL),
                    (sw / 2, dw / 2, sws::ONE_HORIZONTAL),
                    (sh / 2, dh / 2, sws::ONE_VERTICAL),
                    (sw / 2, dw / 2, sws::ONE_HORIZONTAL),
                    (sh / 2, dh / 2, sws::ONE_VERTICAL),
                ] {
                    taps += table(s, d, one).map_or(0, |f| f.coeff.len());
                }
                taps
            };
            plain(&mut report, &format!("sws tables, 6, {name}, built"), || {
                six(sws::bicubic_uncached)
            });
            plain(&mut report, &format!("sws tables, 6, {name}, from the memo"), || {
                six(sws::bicubic)
            });
        }
        plain(&mut report, "vignette steps, 3 planes (per frame)", || {
            (
                effects::vignette_steps(12345, w32, h32),
                effects::vignette_steps(99, w32 / 2, h32 / 2),
                effects::vignette_steps(1999, w32 / 2, h32 / 2),
            )
        });
        plain(&mut report, "vignette map, every sample's hypot and cos", || {
            vignette_map_every_sample(w32, h32)
        });
        plain(&mut report, "vignette map (once per size)", || {
            effects::vignette_map(w32, h32)
        });
    }
    eprintln!("{report}");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap().join("parity");
    let _ = std::fs::create_dir_all(&out);
    let _ = std::fs::write(out.join("bench-cpu.txt"), &report);
    assert!(simd::available().contains(&SimdPath::Scalar));
}
