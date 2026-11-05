//! What an animated SVG title costs a compositor, measured: ffmpeg never sees an SVG (`svg_layer.rs`
//! draws it with resvg into a straight-alpha RGBA PNG), it `overlay`s the PNG on the composite. On a
//! `yuv420p` composite `overlay` takes its picture as `yuva420p`, so the PNG is converted by an
//! auto-inserted `scale` first, and blended with `overlay`'s integer arithmetic (`vf_overlay.c`,
//! `blend_plane`).
//!
//! This holds the claim a GPU path would stand on: **the planes ffmpeg converts the picture to, inside a
//! `-filter_complex` graph of the same build, blended with `overlay`'s arithmetic (the alpha averaged over
//! each 2x2 chroma block, `(a + a + a + a) >> 2`, and at the last chroma column and row the
//! `(alpha_v + alpha_h) >> 1` of what is there; `FAST_DIV255`), are the overlay's own output, sample for
//! sample, on both builds.** It also records the trap: a *simple* graph (`-vf format=yuva420p`) converts
//! with other filters on 4.4.2 (bilinear where a complex graph's default is bicubic) and gives planes
//! 87 to 1472 samples of 9216 away, so the conversion must be asked of a `-filter_complex` graph, as the overlay's is.
//!
//! Needs `ffmpeg` (`KERF_FFMPEG`), no adapter; `#[ignore]`d.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test svg_overlay -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

use std::io::Write;
use std::process::{Command, Stdio};

const W: usize = 96;
const H: usize = 64;

fn ffmpeg(args: &[&str], stdin: Option<&[u8]>) -> Vec<u8> {
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    if let Some(bytes) = stdin {
        child.stdin.take().expect("stdin").write_all(bytes).expect("write");
    }
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// A picture like a title card's: colour that varies, opaque and clear areas, soft edges and every alpha.
fn picture(seed: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(W * H * 4);
    let mut state = (seed as u32).wrapping_mul(2_654_435_761).wrapping_add(1);
    for y in 0..H {
        for x in 0..W {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let alpha = match (state >> 24) % 6 {
                0 => 0,
                1..=3 => 255,
                4 => (state >> 8) as u8,
                _ => (x * 4) as u8,
            };
            out.extend_from_slice(&[(x * 3 + y) as u8, (y * 5) as u8, (x * y) as u8, alpha]);
        }
    }
    out
}

fn fast_div255(v: u32) -> u8 {
    let v = v + 128;
    ((v + (v >> 8)) >> 8) as u8
}

/// `overlay`'s blend of a straight-alpha `yuva420p` picture at `(x, y)` (even) on a `yuv420p` base.
fn overlay_model(base: &[u8], planes: &[u8], x: usize, y: usize) -> Vec<u8> {
    let (ys, cs) = (W * H, (W / 2) * (H / 2));
    let alpha = &planes[ys + 2 * cs..];
    let mut out = base.to_vec();
    let a_at = |r: usize, c: usize| u32::from(alpha[r * W + c]);
    for j in 0..H {
        for k in 0..W {
            if y + j >= H || x + k >= W {
                continue;
            }
            let i = (y + j) * W + x + k;
            let a = a_at(j, k);
            out[i] = fast_div255(u32::from(base[i]) * (255 - a) + u32::from(planes[j * W + k]) * a);
        }
    }
    let (cw, ch) = (W / 2, H / 2);
    for j in 0..ch {
        for k in 0..cw {
            if y / 2 + j >= ch || x / 2 + k >= cw {
                continue;
            }
            // `overlay` averages the alpha over the block; at the last chroma column or row there is no
            // neighbour to average with and it halves what is there.
            let a = if j + 1 < ch && k + 1 < cw {
                (a_at(2 * j, 2 * k) + a_at(2 * j + 1, 2 * k) + a_at(2 * j, 2 * k + 1) + a_at(2 * j + 1, 2 * k + 1)) >> 2
            } else {
                let h = if k + 1 < cw {
                    (a_at(2 * j, 2 * k) + a_at(2 * j, 2 * k + 1)) >> 1
                } else {
                    a_at(2 * j, 2 * k)
                };
                let v = if j + 1 < ch {
                    (a_at(2 * j, 2 * k) + a_at(2 * j + 1, 2 * k)) >> 1
                } else {
                    a_at(2 * j, 2 * k)
                };
                (v + h) >> 1
            };
            for plane in 0..2 {
                let at = ys + plane * cs;
                let i = at + (y / 2 + j) * cw + x / 2 + k;
                out[i] = fast_div255(u32::from(base[i]) * (255 - a) + u32::from(planes[at + j * cw + k]) * a);
            }
        }
    }
    out
}

#[test]
#[ignore = "needs ffmpeg"]
fn a_title_picture_overlaid_is_ffmpegs_conversion_blended_with_overlays_own_arithmetic() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let base_graph = format!("testsrc2=s={W}x{H}:d=1:r=1,format=yuv420p");
    let base = ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            &base_graph,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ],
        None,
    );
    let mut simple_differs = 0;
    for seed in 0..4usize {
        let png = dir.join(format!("svg-overlay-{}-{seed}.png", std::process::id()));
        let png_arg = png.to_string_lossy().into_owned();
        ffmpeg(
            &[
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "-s",
                &format!("{W}x{H}"),
                "-i",
                "-",
                "-frames:v",
                "1",
                &png_arg,
            ],
            Some(&picture(seed)),
        );
        let convert = |args: &[&str]| {
            let mut full = vec!["-f", "image2", "-pattern_type", "none", "-i", png_arg.as_str()];
            full.extend_from_slice(args);
            full.extend(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuva420p", "-"]);
            ffmpeg(&full, None)
        };
        let complex = convert(&["-filter_complex", "[0:v]format=yuva420p[o]", "-map", "[o]"]);
        let simple = convert(&["-vf", "format=yuva420p"]);
        simple_differs += usize::from(simple != complex);
        for (x, y) in [(0usize, 0usize), (20, 10), (64, 40)] {
            let graph = format!("[0:v][1:v]overlay=x={x}:y={y}[o]");
            let real = ffmpeg(
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    &base_graph,
                    "-f",
                    "image2",
                    "-pattern_type",
                    "none",
                    "-i",
                    &png_arg,
                    "-filter_complex",
                    &graph,
                    "-map",
                    "[o]",
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                    "-",
                ],
                None,
            );
            let mine = overlay_model(&base, &complex, x, y);
            let wrong = mine.iter().zip(&real).filter(|(a, b)| a != b).count();
            let wrong_simple = overlay_model(&base, &simple, x, y)
                .iter()
                .zip(&real)
                .filter(|(a, b)| a != b)
                .count();
            eprintln!(
                "picture {seed} at ({x}, {y}): {wrong} of {} samples differ from the real overlay (a simple graph's conversion: {wrong_simple})",
                real.len()
            );
            assert_eq!(wrong, 0, "picture {seed} at ({x}, {y})");
        }
        let _ = std::fs::remove_file(&png);
    }
    eprintln!("a simple graph's conversion differed from a complex one's in {simple_differs} of 4 pictures");
}
