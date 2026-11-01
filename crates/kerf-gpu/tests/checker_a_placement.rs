//! Work package A (keyed placement), checked independently of the implementer's cases: the placement's arithmetic
//! (`kerf_core::perspective`, `kerf_gpu::placement`, `placement.wgsl`) against FFmpeg's `pad`, `perspective` and `crop`
//! on poses chosen to break it: values on a rounding tie, odd widths down to one sample, zooms from 1/1000 to 5000, positions
//! far off the canvas, pictures of a handful of samples, and the per-axis tables against the filter's whole table.
//!
//! No tolerance anywhere: a differing sample fails the case. Needs `ffmpeg` (and an adapter for the shader legs); the
//! pure-arithmetic tests (`checker_a_axis_tables_*`) need neither. Run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_a_placement -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(clippy::print_stderr)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use kerf_core::perspective::{PlacementFrame, PlacementMap, Pose, Tables};
use kerf_gpu::placement::{reference, Picture};
use kerf_gpu::{Compositor, Gpu, GpuOptions};

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| {
        let gpu: Arc<Gpu> = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
        Compositor::new(gpu).expect("a compositor")
    })
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn unit(&mut self) -> f64 {
        (self.next() as f64) / ((1u64 << 31) as f64)
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// The filters `engine::cli::placement_chain` writes for a pose (`eval=init`, the still's form), by the same text.
fn chain(pose: &Pose, canvas: (u32, u32)) -> String {
    let (cw, ch) = canvas;
    let corner = |edge: &str, dim: &str, canvas: u32, pos: f64| {
        let centre = format!("{}+({})*{canvas}", f64::from(canvas) / 2.0, pos);
        let off = format!("{edge}+0.5-2*floor(({dim}-{canvas})/4)-({centre})");
        match pose.scale {
            Some(s) => format!("({off})/({s})+{dim}/2-0.5"),
            None => format!("{off}+{dim}/2-0.5"),
        }
    };
    let (x0, x1) = (corner("0", "W", cw, pose.pos.0), corner("W", "W", cw, pose.pos.0));
    let (y0, y1) = (corner("0", "H", ch, pose.pos.1), corner("H", "H", ch, pose.pos.1));
    format!(
        "format=yuva420p,pad=w='iw+4*ceil(max(0,{cw}-iw)/4)+4':h='ih+4*ceil(max(0,{ch}-ih)/4)+4':\
         x='2*ceil(max(0,{cw}-iw)/4)+2':y='2*ceil(max(0,{ch}-ih)/4)+2':color=black@0,\
         perspective=x0='{x0}':y0='{y0}':x1='{x1}':y1='{y0}':x2='{x0}':y2='{y1}':x3='{x1}':y3='{y1}':interpolation=linear,\
         crop=w={cw}:h={ch}:x='2*floor((iw-{cw})/4)':y='2*floor((ih-{ch})/4)'"
    )
}

struct Noise {
    size: (u32, u32),
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    a: Vec<u8>,
}

impl Noise {
    fn new(rng: &mut Lcg, size: (u32, u32)) -> Self {
        let (w, h) = (size.0 as usize, size.1 as usize);
        let c = w.div_ceil(2) * h.div_ceil(2);
        Self {
            size,
            y: rng.bytes(w * h),
            u: rng.bytes(c),
            v: rng.bytes(c),
            a: rng.bytes(w * h),
        }
    }

    fn picture(&self) -> Picture<'_> {
        Picture {
            size: self.size,
            y: &self.y,
            u: &self.u,
            v: &self.v,
            a: Some(&self.a),
            opaque: 255,
        }
    }
}

/// FFmpeg's placement of `noise`; `Err` is its stderr (a pose it refuses to build).
fn ffmpeg_placed(noise: &Noise, filter: &str, canvas: (u32, u32)) -> Result<[Vec<u8>; 4], String> {
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuva420p",
            "-s",
        ])
        .arg(format!("{}x{}", noise.size.0, noise.size.1))
        .args([
            "-i",
            "pipe:0",
            "-vf",
            filter,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuva420p",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let data: Vec<u8> = [&noise.y, &noise.u, &noise.v, &noise.a]
        .into_iter()
        .flatten()
        .copied()
        .collect();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let (cw, ch) = (canvas.0 as usize, canvas.1 as usize);
    if out.stdout.len() != cw * ch * 5 / 2 {
        return Err(format!("ffmpeg wrote {} bytes for a {cw}x{ch} canvas", out.stdout.len()));
    }
    let (y, rest) = out.stdout.split_at(cw * ch);
    let (u, rest) = rest.split_at(cw * ch / 4);
    let (v, a) = rest.split_at(cw * ch / 4);
    Ok([y.to_vec(), u.to_vec(), v.to_vec(), a.to_vec()])
}

fn differing(a: &[u8], b: &[u8]) -> (usize, i32, Option<usize>) {
    assert_eq!(a.len(), b.len());
    let mut n = 0;
    let mut max = 0;
    let mut first = None;
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if x != y {
            n += 1;
            max = max.max((i32::from(*x) - i32::from(*y)).abs());
            first.get_or_insert(i);
        }
    }
    (n, max, first)
}

/// FFmpeg against the model's reference resample (and the other table form) for one pose; `None` when they agree.
fn case(noise: &Noise, canvas: (u32, u32), pose: &Pose) -> Option<String> {
    let want = match ffmpeg_placed(noise, &chain(pose, canvas), canvas) {
        Ok(w) => w,
        Err(e) => {
            return Some(format!(
                "{canvas:?} <- {:?} {pose:?}: FFmpeg refused it: {}",
                noise.size,
                e.trim()
            ))
        }
    };
    let frame = PlacementFrame::new(noise.size, canvas);
    let mut bad = Vec::new();
    for (label, map) in [
        ("axes", PlacementMap::new(frame, pose)),
        ("whole", PlacementMap::exact(frame, pose)),
    ] {
        let map = match map {
            Ok(m) => m,
            Err(e) => return Some(format!("{canvas:?} <- {:?} {pose:?}: model refused it: {e}", noise.size)),
        };
        let used = matches!(map.tables, Tables::Axes { .. });
        let got = reference(&map, &noise.picture());
        let got = [got.y, got.u, got.v, got.a];
        for (p, name) in ["Y", "U", "V", "A"].iter().enumerate() {
            let (n, max, first) = differing(&want[p], &got[p]);
            if n != 0 {
                let w = if p == 1 || p == 2 { canvas.0 / 2 } else { canvas.0 } as usize;
                let first = first.unwrap();
                bad.push(format!(
                    "{label}{} {name}: {n} samples differ (max {max}, first at ({}, {}))",
                    if used { "(axes)" } else { "" },
                    first % w,
                    first / w
                ));
            }
        }
    }
    (!bad.is_empty()).then(|| format!("{canvas:?} <- {:?} {pose:?}: {}", noise.size, bad.join("; ")))
}

fn report(what: &str, cases: usize, bad: &[String]) {
    eprintln!("{what}: {cases} placements, {} differ", bad.len());
    for b in bad.iter().take(20) {
        eprintln!("  {b}");
    }
    assert!(bad.is_empty(), "{what}: {} of {cases} differ, first: {}", bad.len(), bad[0]);
}

/// Poses whose 256 * position is exactly k + 1/2 on many pixels: the canvas is a power of two, the position a multiple of
/// 1/(512 * canvas) and the zoom a power of two (or none), so every number the filter's expressions evaluate is a dyadic
/// rational that `f64` holds exactly, and `lrint` of a tie rounds to even. The model has to agree to the sample,
/// on whichever table it builds.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_a_poses_on_a_rounding_tie_are_the_filters() {
    let mut rng = Lcg(0x71E5);
    let mut bad = Vec::new();
    let mut cases = 0;
    let mut ties = 0usize;
    for (canvas, input) in [
        ((64u32, 32u32), (64u32, 32u32)),
        ((64, 32), (32, 16)),
        ((128, 64), (128, 64)),
        ((64, 32), (62, 32)),
        ((32, 32), (48, 40)),
    ] {
        let noise = Noise::new(&mut rng, input);
        for scale in [
            None,
            Some(1.0),
            Some(2.0),
            Some(4.0),
            Some(0.5),
            Some(0.25),
            Some(8.0),
            Some(0.125),
        ] {
            for (mx, my) in [
                (1i64, 0i64),
                (3, 1),
                (-1, -1),
                (255, 129),
                (-257, 511),
                (1025, -3),
                (0, 0),
                (2, 1),
                (-4095, 4097),
            ] {
                // 256 * u = k + 1/2 needs pos * canvas = odd / 512 at a zoom of 1; a zoom of s puts that over s.
                let pos = (
                    mx as f64 / (512.0 * f64::from(canvas.0)),
                    my as f64 / (512.0 * f64::from(canvas.1)),
                );
                let pose = Pose { scale, pos };
                let frame = PlacementFrame::new(input, canvas);
                // Count the pixels of this pose whose 256 * position is exactly half way (all numbers dyadic: exact in f64).
                let r = frame.corners(&pose);
                let w = f64::from(frame.frame.0);
                for i in 0..canvas.0 {
                    let x = f64::from(frame.crop.0 + i);
                    let u = r[0][0] + (r[1][0] - r[0][0]) * x / w;
                    let k = 256.0 * u;
                    ties += usize::from((k - k.floor()) == 0.5);
                }
                cases += 1;
                if let Some(b) = case(&noise, canvas, &pose) {
                    bad.push(b);
                }
            }
        }
    }
    eprintln!("columns whose position is exactly on a tie: {ties}");
    assert!(ties > 100, "the cases do not reach ties ({ties})");
    report("rounding ties", cases, &bad);
}

/// A picture one to a few samples wide, odd widths of every size, even heights from 2.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_a_odd_and_tiny_widths_are_the_filters() {
    let mut rng = Lcg(0x0DD);
    let mut bad = Vec::new();
    let mut cases = 0;
    let canvas = (64, 36);
    for w in [1u32, 2, 3, 4, 5, 7, 9, 15, 17, 31, 33, 63, 64, 65, 67, 127, 129] {
        for h in [2u32, 4, 6, 18, 36, 38, 72] {
            let noise = Noise::new(&mut rng, (w, h));
            for pose in [
                Pose {
                    scale: None,
                    pos: (0.1, -0.2),
                },
                Pose {
                    scale: Some(1.0),
                    pos: (0.0, 0.0),
                },
                Pose {
                    scale: Some(1.7),
                    pos: (0.3, 0.1),
                },
                Pose {
                    scale: Some(0.3),
                    pos: (-0.4, 0.4),
                },
                Pose {
                    scale: Some(23.0),
                    pos: (0.0, 0.0),
                },
            ] {
                cases += 1;
                if let Some(b) = case(&noise, canvas, &pose) {
                    bad.push(b);
                }
            }
        }
    }
    report("odd and tiny widths", cases, &bad);
}

/// Odd canvases are not made (4:2:0), but the canvas can be any even size and the picture any size: an even canvas that
/// is not a multiple of 4 (the offsets `pad` and `crop` round), a thin one and a canvas smaller than the picture.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_a_canvases_that_are_not_multiples_of_four_are_the_filters() {
    let mut rng = Lcg(0xCA7);
    let mut bad = Vec::new();
    let mut cases = 0;
    for canvas in [(66u32, 38u32), (70, 34), (30, 18), (2, 2), (4, 2), (2, 40), (90, 6), (34, 66)] {
        for input in [
            canvas,
            (canvas.0 + 2, canvas.1 + 4),
            (canvas.0 / 2 * 2 + 6, 8),
            (10, canvas.1 + 2),
            (1, 2),
            (3, 4),
        ] {
            if input.0 == 0 || input.1 == 0 || input.1 % 2 == 1 {
                continue;
            }
            let noise = Noise::new(&mut rng, input);
            for pose in [
                Pose {
                    scale: None,
                    pos: (0.07, 0.03),
                },
                Pose {
                    scale: Some(1.0),
                    pos: (0.0, 0.0),
                },
                Pose {
                    scale: Some(2.2),
                    pos: (-0.2, 0.2),
                },
                Pose {
                    scale: Some(0.6),
                    pos: (0.0, 0.01),
                },
            ] {
                cases += 1;
                if let Some(b) = case(&noise, canvas, &pose) {
                    bad.push(b);
                }
            }
        }
    }
    report("canvases", cases, &bad);
}

/// Zooms from 1/1000 to 5000, positions from a hair to hundreds of canvases off: every pose the model calls reachable
/// is drawn to the sample; the others are counted (a refusal is the plan's, `PlacementFrame::reachable`).
#[test]
#[ignore = "needs ffmpeg"]
fn checker_a_extreme_zooms_and_positions_are_the_filters() {
    let mut rng = Lcg(0xE57);
    let mut bad = Vec::new();
    let mut cases = 0;
    let mut unreachable = 0;
    for (canvas, input) in [
        ((64u32, 36u32), (64u32, 36u32)),
        ((64, 36), (20, 12)),
        ((96, 54), (97, 54)),
        ((32, 18), (200, 100)),
    ] {
        let noise = Noise::new(&mut rng, input);
        let frame = PlacementFrame::new(input, canvas);
        for scale in [
            None,
            Some(0.0004),
            Some(0.001),
            Some(0.0071),
            Some(0.05),
            Some(0.99999),
            Some(1.0),
            Some(1.00001),
            Some(7.3),
            Some(60.0),
            Some(900.0),
            Some(5000.0),
            Some(1e5),
        ] {
            for pos in [
                (0.0, 0.0),
                (0.5, 0.5),
                (-0.5, 0.5),
                (1.0, -1.0),
                (-3.7, 2.1),
                (25.0, -25.0),
                (-400.0, 130.0),
                (0.000001, -0.000001),
                (1234.5, 0.0),
            ] {
                let pose = Pose { scale, pos };
                if !frame.reachable(&pose) {
                    unreachable += 1;
                    continue;
                }
                cases += 1;
                if let Some(b) = case(&noise, canvas, &pose) {
                    bad.push(b);
                }
            }
        }
    }
    eprintln!("{unreachable} poses are past the model's reach and refused");
    report("extreme poses", cases, &bad);
}

/// Poses a little inside the model's reach (`2^21` samples): the table is built, so it has to be the filter's.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_a_poses_at_the_edge_of_the_reach_are_the_filters() {
    let mut rng = Lcg(0x2121);
    let mut bad = Vec::new();
    let mut cases = 0;
    let canvas = (32, 18);
    let noise = Noise::new(&mut rng, (32, 18));
    let frame = PlacementFrame::new((32, 18), canvas);
    let mut nearest = 0.0f64;
    for k in 0..60 {
        // A zoom out whose corners are within a few percent of 2^21 samples, a pan that puts one corner there.
        let reach = f64::from(1u32 << 21);
        let frac = 0.5 + 0.0083 * f64::from(k);
        let s = 32.0 / (2.0 * reach * frac);
        let pose = Pose {
            scale: Some(s),
            pos: (rng.range(-0.2, 0.2), rng.range(-0.2, 0.2)),
        };
        let c = frame.corners(&pose);
        let m = c.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
        if frame.reachable(&pose) {
            nearest = nearest.max(m);
            cases += 1;
            if let Some(b) = case(&noise, canvas, &pose) {
                bad.push(b);
            }
        }
    }
    let pan = f64::from(1u32 << 21) / 32.0 * 0.97;
    for sign in [1.0, -1.0] {
        for (s, a) in [(None, 1.0), (Some(1.0), 0.5), (Some(3.0), 2.0)] {
            let pose = Pose {
                scale: s,
                pos: (sign * pan * a, -sign * pan * a * 0.5),
            };
            if frame.reachable(&pose) {
                let c = frame.corners(&pose);
                nearest = nearest.max(c.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs())));
                cases += 1;
                if let Some(b) = case(&noise, canvas, &pose) {
                    bad.push(b);
                }
            }
        }
    }
    eprintln!("largest corner reached: {nearest} samples (limit {})", 1u32 << 21);
    assert!(nearest > f64::from(1u32 << 20), "the cases do not reach the edge ({nearest})");
    report("the edge of the reach", cases, &bad);
}

/// The shader against the model on the ties and the extremes above, with both forms of table.
#[test]
#[ignore = "needs an adapter (lavapipe is enough)"]
fn checker_a_the_shader_is_the_model_on_ties_and_extremes() {
    let mut rng = Lcg(0x5AD);
    let mut cases = 0;
    for (canvas, input) in [
        ((64u32, 32u32), (64u32, 32u32)),
        ((64, 32), (33, 16)),
        ((32, 18), (5, 2)),
        ((32, 18), (1, 2)),
        ((66, 38), (70, 6)),
    ] {
        let noise = Noise::new(&mut rng, input);
        let frame = PlacementFrame::new(input, canvas);
        for scale in [None, Some(0.5), Some(2.0), Some(0.0013), Some(4000.0), Some(1.0)] {
            for pos in [
                (1.0 / (512.0 * f64::from(canvas.0)), 3.0 / (512.0 * f64::from(canvas.1))),
                (0.0, 0.0),
                (-77.0, 31.0),
                (0.5, -0.5),
                (500.0, 500.0),
            ] {
                let pose = Pose { scale, pos };
                if !frame.reachable(&pose) {
                    continue;
                }
                for map in [
                    PlacementMap::new(frame, &pose).unwrap(),
                    PlacementMap::exact(frame, &pose).unwrap(),
                ] {
                    for pic in [
                        noise.picture(),
                        Picture {
                            a: None,
                            opaque: 77,
                            ..noise.picture()
                        },
                    ] {
                        let want = reference(&map, &pic);
                        let got = compositor().place(&pic, &map).expect("the pass");
                        cases += 1;
                        for (plane, (g, w)) in [(&got.y, &want.y), (&got.u, &want.u), (&got.v, &want.v), (&got.a, &want.a)]
                            .into_iter()
                            .enumerate()
                        {
                            let (n, max, first) = differing(g, w);
                            assert!(
                                n == 0,
                                "{canvas:?} <- {input:?} {pose:?}: plane {plane}: {n} samples differ between the shader and the model (max {max}, first at {first:?})"
                            );
                        }
                    }
                }
            }
        }
    }
    eprintln!("{cases} placements, shader against model");
}

// ---- the per-axis tables against the filter's whole table (no ffmpeg) ------------------------------------------

/// `PlacementMap::new` against `PlacementMap::exact` on every pixel of the window. Returns whether the per-axis tables were used.
fn axes_equal_whole(frame: PlacementFrame, pose: &Pose) -> Result<bool, String> {
    let (Ok(fast), Ok(slow)) = (PlacementMap::new(frame, pose), PlacementMap::exact(frame, pose)) else {
        return Ok(false);
    };
    for y in 0..frame.canvas.1 {
        for x in 0..frame.canvas.0 {
            if fast.entry(x, y) != slow.entry(x, y) {
                return Err(format!(
                    "{:?} <- {:?} {pose:?}: at ({x}, {y}) the per-axis table says {:?}, the filter's {:?}",
                    frame.canvas,
                    frame.picture,
                    fast.entry(x, y),
                    slow.entry(x, y)
                ));
            }
        }
    }
    Ok(matches!(fast.tables, Tables::Axes { .. }))
}

fn poses_to_try() -> usize {
    std::env::var("KERF_CHECKER_POSES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500)
}

/// Random poses across the whole range the plan lets through, small canvases so the whole table is cheap.
#[test]
fn checker_a_axis_tables_are_the_whole_table_on_random_poses() {
    let mut rng = Lcg(0xAC15);
    let (mut axes, mut total) = (0usize, 0usize);
    for n in 0..poses_to_try() {
        let canvas = [(16u32, 8u32), (24, 14), (32, 18), (30, 10), (64, 6), (6, 64)][n % 6];
        let input = match n % 7 {
            0 => canvas,
            1 => (canvas.0 / 2 + 2, canvas.1 / 2),
            2 => (canvas.0 + 6, canvas.1 + 2),
            3 => (canvas.0 - 1, canvas.1),
            4 => (3, 2),
            5 => (canvas.0 * 4, canvas.1 * 4),
            _ => (1 + canvas.0 / 3, 2 * (1 + canvas.1 / 5)),
        };
        let input = (input.0.max(1), (input.1.max(2)) & !1);
        let frame = PlacementFrame::new(input, canvas);
        // Log-uniform zooms from 1e-4 to 1e4, positions from a hair to a thousand canvases.
        let log = |rng: &mut Lcg, lo: f64, hi: f64| (lo.ln() + (hi.ln() - lo.ln()) * rng.unit()).exp();
        let sign = |rng: &mut Lcg| if rng.next() & 1 == 0 { 1.0 } else { -1.0 };
        let pose = Pose {
            scale: (n % 5 != 0).then(|| log(&mut rng, 1e-4, 1e4)),
            pos: (
                sign(&mut rng) * log(&mut rng, 1e-6, 1000.0),
                sign(&mut rng) * log(&mut rng, 1e-6, 1000.0),
            ),
        };
        match axes_equal_whole(frame, &pose) {
            Ok(a) => {
                total += 1;
                axes += usize::from(a);
            }
            Err(e) => panic!("{e}"),
        }
    }
    eprintln!("{axes} of {total} random poses took the per-axis tables");
}

/// Poses built to sit at the boundary the per-axis proof reasons about: for a column of the window, move the position by
/// a fraction of a sub-pixel so that 256 * u lies within a few 1e-9, 1e-7 or 1e-5 of a rounding tie, with the corners
/// large (so the rounding noise `(x0 - x1) - x0 + x1` is not zero) and the window tall (so the noise has room to act).
#[test]
fn checker_a_axis_tables_are_the_whole_table_next_to_a_tie() {
    let mut rng = Lcg(0xB0D);
    let mut noisy_axes = 0usize;
    let mut noisy = 0usize;
    let mut tried = 0usize;
    for n in 0..poses_to_try() {
        let canvas = [(16u32, 64u32), (8, 128), (32, 32), (4, 256)][n % 4];
        let input = [canvas, (canvas.0 + 6, canvas.1 + 6), (canvas.0 / 2 + 4, canvas.1 / 2)][n % 3];
        let frame = PlacementFrame::new(input, canvas);
        let scale = [0.37, 3.0, 0.011, 41.0, 1.0 / 3.0, 1e-3, 700.0][n % 7];
        let big = [3.0, 40.0, 900.0, 15_000.0][n % 4];
        let mut pose = Pose {
            scale: Some(scale),
            pos: (rng.range(-1.0, 1.0) * big, rng.range(-1.0, 1.0) * big),
        };
        if !frame.reachable(&pose) {
            continue;
        }
        // Slide the x position so that a chosen column's value is `off` from a tie: u (in 1/256) falls by
        // 256 * canvas * dpos / scale.
        let col = (rng.next() % u64::from(canvas.0)) as u32;
        let x = f64::from(frame.crop.0 + col);
        let c = frame.corners(&pose);
        let w = f64::from(frame.frame.0);
        let v = 256.0 * (c[0][0] + (c[1][0] - c[0][0]) * x / w);
        let off = [0.0, 1e-9, -1e-9, 1e-7, -1e-7, 1e-5, -3e-5, 2e-4, -1e-3][n % 9];
        let want = (v - 0.5).floor() + 0.5 + off;
        pose.pos.0 += (v - want) * scale / (256.0 * f64::from(canvas.0));
        if !frame.reachable(&pose) {
            continue;
        }
        tried += 1;
        let c = frame.corners(&pose);
        noisy += usize::from(((c[0][0] - c[1][0]) - c[0][0]) + c[1][0] != 0.0);
        match axes_equal_whole(frame, &pose) {
            Ok(a) => noisy_axes += usize::from(a),
            Err(e) => panic!("{e}"),
        }
    }
    eprintln!(
        "{noisy_axes} of {tried} near-tie poses took the per-axis tables, {noisy} of them with rounding noise in the matrix"
    );
    assert!(
        noisy * 100 > tried,
        "the poses do not reach the rounding noise ({noisy} of {tried})"
    );
}

/// Random pictures and poses against the filters: log-uniform zoom, position from a hair to several canvases, any size of
/// picture and canvas the plan would let through (even height).
#[test]
#[ignore = "needs ffmpeg"]
fn checker_a_random_poses_are_the_filters() {
    let n: usize = std::env::var("KERF_CHECKER_FFMPEG_POSES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500);
    let mut rng = Lcg(0x5EED_0A);
    let mut bad = Vec::new();
    let (mut cases, mut axes) = (0usize, 0usize);
    for i in 0..n {
        let canvas = (2 * (8 + rng.next() % 60) as u32, 2 * (4 + rng.next() % 30) as u32);
        let input = (1 + (rng.next() % 150) as u32, 2 * (1 + (rng.next() % 70) as u32));
        let noise = Noise::new(&mut rng, input);
        let log = |rng: &mut Lcg, lo: f64, hi: f64| (lo.ln() + (hi.ln() - lo.ln()) * rng.unit()).exp();
        let sign = |rng: &mut Lcg| if rng.next() & 1 == 0 { 1.0 } else { -1.0 };
        let pose = Pose {
            scale: (i % 6 != 0).then(|| log(&mut rng, 0.01, 100.0)),
            pos: (
                sign(&mut rng) * log(&mut rng, 1e-4, 3.0),
                sign(&mut rng) * log(&mut rng, 1e-4, 3.0),
            ),
        };
        if !PlacementFrame::new(input, canvas).reachable(&pose) {
            continue;
        }
        cases += 1;
        axes += usize::from(matches!(
            PlacementMap::new(PlacementFrame::new(input, canvas), &pose).map(|m| m.tables),
            Ok(Tables::Axes { .. })
        ));
        if let Some(b) = case(&noise, canvas, &pose) {
            bad.push(b);
        }
    }
    eprintln!("{axes} of {cases} random placements took the per-axis tables");
    report("random placements", cases, &bad);
}

/// Only poses whose matrix carries rounding noise (`(x0 - x1) - x0 + x1 != 0`), tall windows, the column nudged to sit between
/// 1e-12 and 1e-3 of a sub-sample from a tie: the case the per-axis proof is about.
#[test]
fn checker_a_axis_tables_are_the_whole_table_where_the_matrix_is_noisy() {
    let want: usize = std::env::var("KERF_CHECKER_NOISY_POSES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500);
    let mut rng = Lcg(0x4015E);
    let (mut found, mut tried, mut on_axes) = (0usize, 0usize, 0usize);
    while found < want && tried < want * 400 {
        tried += 1;
        let canvas = [(8u32, 128u32), (4, 256), (16, 64), (6, 200)][tried % 4];
        let input = [canvas, (canvas.0 + 4, canvas.1 + 6), (canvas.0 / 2 + 2, canvas.1 / 2)][tried % 3];
        let frame = PlacementFrame::new(input, canvas);
        let log = |rng: &mut Lcg, lo: f64, hi: f64| (lo.ln() + (hi.ln() - lo.ln()) * rng.unit()).exp();
        let sign = |rng: &mut Lcg| if rng.next() & 1 == 0 { 1.0 } else { -1.0 };
        let scale = log(&mut rng, 1e-3, 1e3);
        let mut pose = Pose {
            scale: Some(scale),
            pos: (
                sign(&mut rng) * log(&mut rng, 0.5, 2000.0),
                sign(&mut rng) * log(&mut rng, 0.5, 2000.0),
            ),
        };
        if !frame.reachable(&pose) {
            continue;
        }
        let col = (rng.next() % u64::from(canvas.0)) as u32;
        let x = f64::from(frame.crop.0 + col);
        let c = frame.corners(&pose);
        let w = f64::from(frame.frame.0);
        let v = 256.0 * (c[0][0] + (c[1][0] - c[0][0]) * x / w);
        let off = log(&mut rng, 1e-12, 1e-3) * sign(&mut rng);
        let tie = (v - 0.5).floor() + 0.5 + off;
        pose.pos.0 += (v - tie) * scale / (256.0 * f64::from(canvas.0));
        if !frame.reachable(&pose) {
            continue;
        }
        let c = frame.corners(&pose);
        if ((c[0][0] - c[1][0]) - c[0][0]) + c[1][0] == 0.0 {
            continue;
        }
        found += 1;
        match axes_equal_whole(frame, &pose) {
            Ok(a) => on_axes += usize::from(a),
            Err(e) => panic!("{e}"),
        }
    }
    eprintln!("{found} noisy near-tie poses ({on_axes} on the per-axis tables) out of {tried} tried");
    assert!(found >= want / 2, "too few noisy poses were found: {found} of {want}");
}
