//! The placement's arithmetic against the real filters: `pad`, `perspective` (`interpolation=linear`) and
//! `crop` as `engine::cli::placement_chain` writes them, over a noise picture with an alpha plane, against
//! `kerf_core::perspective`'s map and `kerf_gpu::placement`'s resample, sample for sample. No tolerance:
//! a differing sample fails the case.
//!
//! Needs `ffmpeg` (and, for the shader legs, an adapter); `#[ignore]`d. Run with
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test placement -- --ignored --nocapture
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

/// A deterministic generator.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn unit(&mut self) -> f64 {
        (self.next() as f64) / ((1u64 << 31) as f64)
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// `placement_chain`'s filters for a pose, written the way the engine writes them (the still's, `eval=init`).
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

/// FFmpeg's placement of `noise`: the four planes of the `yuva420p` frame the chain hands on.
fn ffmpeg_placed(noise: &Noise, filter: &str, canvas: (u32, u32)) -> [Vec<u8>; 4] {
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
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    let (cw, ch) = (canvas.0 as usize, canvas.1 as usize);
    let (y, rest) = out.stdout.split_at(cw * ch);
    let (u, rest) = rest.split_at(cw * ch / 4);
    let (v, a) = rest.split_at(cw * ch / 4);
    assert_eq!(a.len(), cw * ch, "ffmpeg wrote {} bytes", out.stdout.len());
    [y.to_vec(), u.to_vec(), v.to_vec(), a.to_vec()]
}

fn differing(a: &[u8], b: &[u8]) -> usize {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_placement_model_is_the_filters_to_the_sample() {
    let mut rng = Lcg(0xC0FFEE);
    let mut cases = 0;
    let (mut axes, mut full) = (0, 0);
    let mut bad = Vec::new();
    // Canvas, picture: the picture at the canvas' size, smaller, larger, odd, thin.
    let shapes: [((u32, u32), (u32, u32)); 9] = [
        ((64, 36), (64, 36)),
        ((64, 36), (32, 18)),
        ((64, 36), (63, 36)),
        ((64, 36), (80, 46)),
        ((64, 36), (17, 12)),
        ((96, 54), (96, 54)),
        ((96, 54), (24, 54)),
        ((66, 40), (65, 40)),
        ((64, 36), (64, 20)),
    ];
    for round in 0..16 {
        for (canvas, input) in shapes {
            let noise = Noise::new(&mut rng, input);
            let scale = match round % 4 {
                0 => None,
                1 => Some(1.0),
                2 => Some(0.2 + rng.unit() * 0.9),
                _ => Some(1.0 + rng.unit() * 2.5),
            };
            let pose = Pose {
                scale,
                pos: (
                    if round % 5 == 0 { 0.0 } else { (rng.unit() - 0.5) * 0.9 },
                    if round % 5 == 0 { 0.0 } else { (rng.unit() - 0.5) * 0.9 },
                ),
            };
            let want = ffmpeg_placed(&noise, &chain(&pose, canvas), canvas);
            let frame = PlacementFrame::new(input, canvas);
            let map = PlacementMap::new(frame, &pose).expect("a map");
            match map.tables {
                Tables::Axes { .. } => axes += 1,
                Tables::Pixels { .. } => full += 1,
            }
            let got = reference(&map, &noise.picture());
            let got = [got.y, got.u, got.v, got.a];
            let off: Vec<usize> = (0..4).map(|p| differing(&want[p], &got[p])).collect();
            cases += 1;
            if off.iter().any(|d| *d != 0) {
                bad.push(format!("{canvas:?} <- {input:?} {pose:?}: differing samples Y/U/V/A {off:?}"));
            }
        }
    }
    eprintln!(
        "{cases} placements, {axes} with per-axis tables and {full} with the whole table; {} differ",
        bad.len()
    );
    assert!(
        bad.is_empty(),
        "{} of {cases}:\n{}",
        bad.len(),
        bad[..bad.len().min(12)].join("\n")
    );
}

/// The shader is the model, with either form of table: one per axis, and the filter's whole table (the form a
/// pose whose positions sit on a rounding boundary takes, which `PlacementMap::exact` makes for any pose), over noise
/// pictures with and without an alpha plane, every shape of pad (a picture the size of the canvas, smaller, larger,
/// an odd width) and every kind of pose (no zoom, a held one, a moving one, off the canvas, far from it).
#[test]
#[ignore = "needs an adapter (lavapipe is enough)"]
fn the_placement_shader_is_the_model() {
    let mut rng = Lcg(0xFEEDBEEF);
    let mut cases = 0;
    let shapes: [((u32, u32), (u32, u32)); 7] = [
        ((64, 36), (64, 36)),
        ((64, 36), (32, 18)),
        ((64, 36), (63, 36)),
        ((64, 36), (80, 46)),
        ((64, 36), (17, 12)),
        ((96, 54), (24, 54)),
        ((66, 40), (65, 40)),
    ];
    for round in 0..12 {
        for (canvas, input) in shapes {
            let noise = Noise::new(&mut rng, input);
            let scale = match round % 4 {
                0 => None,
                1 => Some(1.0),
                2 => Some(0.2 + rng.unit() * 0.9),
                _ => Some(1.0 + rng.unit() * 2.5),
            };
            let far = round % 6 == 5;
            let pose = Pose {
                scale,
                pos: (
                    if far { 1.5 } else { (rng.unit() - 0.5) * 0.9 },
                    if round % 5 == 0 { 0.0 } else { (rng.unit() - 0.5) * 0.9 },
                ),
            };
            let frame = PlacementFrame::new(input, canvas);
            let pic = noise.picture();
            let opaque = Picture {
                a: None,
                opaque: 200,
                ..pic
            };
            for (label, map) in [
                ("axes", PlacementMap::new(frame, &pose).expect("a map")),
                ("pixels", PlacementMap::exact(frame, &pose).expect("a map")),
            ] {
                for picture in [pic, opaque] {
                    let want = reference(&map, &picture);
                    let got = compositor().place(&picture, &map).expect("the pass");
                    cases += 1;
                    assert_eq!(
                        got,
                        want,
                        "{label} {canvas:?} <- {input:?} {pose:?} (alpha plane: {})",
                        picture.a.is_some()
                    );
                }
            }
        }
    }
    eprintln!("{cases} placements, shader against model");
}
