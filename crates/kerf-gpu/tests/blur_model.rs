//! `gblur` against the model the shader reproduces (`effects::blur_plane`: every step of the recursion a fused multiply-add, as the
//! x86 AVX2 / AVX-512 kernels run it), plane by plane, over noise and over a smooth gradient with sparse noise (values near half-way
//! points), at sigmas from 0.5 to 1024 and four sizes: **no sample may differ**. The unfused order (the portable C loop) runs beside
//! it for contrast: it reads a few samples in a million a level off, which is why the model fuses.
//!
//! `cargo test -p kerf-gpu --no-default-features --test blur_model -- --ignored --nocapture`

#![allow(clippy::print_stderr)]

use std::io::Write;
use std::process::{Command, Stdio};

use kerf_gpu::effects::{blur_plane, gaussian, Gaussian};

fn lcg(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 33) as u8
        })
        .collect()
}

/// `gblur` over one 8-bit plane, by FFmpeg.
fn ffmpeg_blur(src: &[u8], w: usize, h: usize, sigma: f64) -> Vec<u8> {
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-s",
        ])
        .arg(format!("{w}x{h}"))
        .args([
            "-i",
            "pipe:0",
            "-vf",
            &kerf_core::blur_filter(sigma),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let data = src.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().unwrap();
    let _ = writer.join();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// The portable C loop: `ptr[x] += nu * ptr[x - 1]` with the product rounded first.
fn blur_c_order(src: &[u8], w: usize, h: usize, g: &Gaussian) -> Vec<u8> {
    let mut buf: Vec<f32> = src.iter().map(|v| f32::from(*v)).collect();
    for row in buf.chunks_exact_mut(w) {
        row[0] *= g.boundary;
        for x in 1..w {
            row[x] += g.nu * row[x - 1];
        }
        row[w - 1] *= g.boundary;
        for x in (1..w).rev() {
            row[x - 1] += g.nu * row[x];
        }
    }
    for x in 0..w {
        buf[x] *= g.boundary;
        for y in 1..h {
            buf[y * w + x] += g.nu * buf[(y - 1) * w + x];
        }
        buf[(h - 1) * w + x] *= g.boundary;
        for y in (1..h).rev() {
            buf[(y - 1) * w + x] += g.nu * buf[y * w + x];
        }
    }
    buf.iter()
        .map(|v| (v * g.post).clamp(0.0, 255.0).round_ties_even() as u8)
        .collect()
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_fused_recursion_is_gblur_on_every_sample_and_the_unfused_one_is_not() {
    let mut total = [0usize; 2];
    let mut cases = 0;
    let mut failures = Vec::new();
    for (w, h) in [(640usize, 360usize), (321, 181), (480, 270), (1280, 720)] {
        for sigma in [
            0.5, 1.0, 1.7, 2.0, 3.0, 4.0, 6.3, 10.0, 20.0, 37.0, 64.0, 100.0, 250.0, 1024.0,
        ] {
            for seed in 0..2u64 {
                let mut src = lcg(w as u64 * 31 + h as u64 + seed, w * h);
                if seed == 1 {
                    // Smooth with sparse noise: large flat areas, values near half-way points.
                    for (i, v) in src.iter_mut().enumerate() {
                        *v = ((i % w) * 255 / w) as u8 / 2 + (*v & 3);
                    }
                }
                let want = ffmpeg_blur(&src, w, h, sigma);
                let g = gaussian(sigma).unwrap();
                let differing = |a: &[u8]| a.iter().zip(&want).filter(|(x, y)| x != y).count();
                let (fused, unfused) = (
                    differing(&blur_plane(&src, w, h, &g)),
                    differing(&blur_c_order(&src, w, h, &g)),
                );
                total[0] += fused;
                total[1] += unfused;
                cases += 1;
                if fused != 0 {
                    failures.push(format!("{w}x{h} sigma {sigma} seed {seed}: {fused} samples differ"));
                }
            }
        }
    }
    eprintln!(
        "{cases} pictures: the fused recursion differs in {} samples, the unfused one in {}",
        total[0], total[1]
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
