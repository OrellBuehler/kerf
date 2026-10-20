//! The grade (`lut3d`, `lutrgb`, `colorbalance`) against FFmpeg, plane for plane, on pictures of random samples.
//!
//! Three things are held to each other, on every case, and **all three must be equal to the level**:
//!
//! * FFmpeg: the very filter text the graph writes for the grade (`kerf_core::grade_filter_chain`), run over a
//!   raw `yuv420p` picture and read back as `yuv420p`;
//! * [`kerf_gpu::grade::reference`], the scalar arithmetic the shaders follow (written from the sources of
//!   FFmpeg 6.1.1 and 9.0.2: swscale's x86 `yuv420p -> rgb24` kernel, the filters' `float` arithmetic, the
//!   x86 vertical scaler for the way back, `src/grade.rs`);
//! * the compositor's passes (`Compositor::grade_yuv`).
//!
//! The pictures are noise in all three planes — every Y, U, V combination, clipped RGB included — so every entry
//! of every table and every branch of the interpolation is looked at; sizes include odd chroma sizes and 1080p.
//! Needs `ffmpeg` and an adapter; `#[ignore]`d. Run it against both builds:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test grade -- --ignored
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ... --test grade -- --ignored
//! ```

#![allow(clippy::print_stderr)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use kerf_core::color_lut::Lut3d;
use kerf_core::{ClipLut, CurvePoint, Curves, Grade, GradeRoute, Wheel, Wheels, YuvMatrix};
use kerf_gpu::grade::{reference, GradeStages};
use kerf_gpu::{Compositor, Gpu, GpuOptions, YuvFrame};

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| Compositor::new(gpu()).expect("a compositor"))
}

/// Noise in all three planes, from a seed.
fn noise(w: u32, h: u32, seed: u64) -> YuvFrame {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 24) as u8
    };
    let (cw, ch) = (w / 2, h / 2);
    YuvFrame {
        width: w,
        height: h,
        y: (0..w * h).map(|_| next()).collect(),
        u: (0..cw * ch).map(|_| next()).collect(),
        v: (0..cw * ch).map(|_| next()).collect(),
    }
}

/// A smooth picture: a luma ramp against chroma ramps, so that neighbouring samples are close and the tables are
/// walked in order (the noise visits them in no order at all).
fn ramps(w: u32, h: u32) -> YuvFrame {
    let (cw, ch) = (w / 2, h / 2);
    YuvFrame {
        width: w,
        height: h,
        y: (0..w * h).map(|i| ((i % w) * 255 / w.max(1)) as u8).collect(),
        u: (0..cw * ch).map(|i| ((i / cw.max(1)) * 255 / ch.max(1)) as u8).collect(),
        v: (0..cw * ch).map(|i| (255 - (i % cw) * 255 / cw.max(1)) as u8).collect(),
    }
}

fn ffmpeg_graded(frame: &YuvFrame, vf: &str) -> YuvFrame {
    use std::io::Write;
    let (w, h) = (frame.width, frame.height);
    let size = format!("{w}x{h}");
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &size,
            "-i",
            "pipe:0",
        ])
        .args(["-vf", vf, "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().expect("stdin");
    let input: Vec<u8> = [&frame.y[..], &frame.u[..], &frame.v[..]].concat();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    assert!(
        out.status.success(),
        "ffmpeg -vf {vf}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (n, c) = ((w * h) as usize, ((w / 2) * (h / 2)) as usize);
    assert_eq!(out.stdout.len(), n + 2 * c, "ffmpeg wrote another size");
    YuvFrame {
        width: w,
        height: h,
        y: out.stdout[..n].to_vec(),
        u: out.stdout[n..n + c].to_vec(),
        v: out.stdout[n + c..].to_vec(),
    }
}

fn pts(p: &[(f64, f64)]) -> Vec<CurvePoint> {
    p.iter().map(|&(x, y)| CurvePoint { x, y }).collect()
}

fn wheel(x: f64, y: f64, lightness: f64) -> Wheel {
    Wheel { x, y, lightness }
}

/// A `.cube` of `size`^3 rows written to the target directory; `f` maps a lattice point in `0..=1` to a row.
fn cube(name: &str, size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> String {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("grade-luts");
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
    let table = Lut3d {
        title: String::new(),
        size,
        data,
    };
    let path = dir.join(format!("{name}.cube"));
    std::fs::write(&path, table.to_cube(1.0)).unwrap();
    path.to_string_lossy().into_owned()
}

fn lut(path: String, intensity: f64) -> ClipLut {
    ClipLut { path, intensity }
}

/// A look with cross-channel terms (every tetrahedron of a cell is walked) and a tint, mostly in `0..=1`.
fn look(c: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = c;
    [
        0.92 * r + 0.08 * g * b + 0.03,
        0.05 * r * r + 0.9 * g + 0.04 * b,
        (0.1 + 0.8 * b + 0.15 * r * g).min(1.0),
    ]
}

struct Case {
    name: &'static str,
    grade: Grade,
    /// The matrix tag of the stream, when it names one.
    tag: Option<&'static str>,
}

fn cases() -> Vec<Case> {
    let s_curve = pts(&[(0.0, 0.0), (0.25, 0.15), (0.75, 0.85), (1.0, 1.0)]);
    let zigzag: Vec<(f64, f64)> = (0..16)
        .map(|i| (i as f64 / 15.0, if i % 2 == 0 { 0.2 } else { 0.8 }))
        .collect();
    let all_wheels = Wheels {
        shadows: wheel(0.4, -0.2, 0.1),
        midtones: wheel(-0.3, 0.5, -0.05),
        highlights: wheel(0.1, 0.7, 0.2),
    };
    let rim = Wheels {
        shadows: wheel(1.0, 0.0, 1.0),
        midtones: wheel(-0.5, 0.866, -1.0),
        highlights: wheel(-0.5, -0.866, 0.5),
    };
    let wheels = |w: Wheels| Grade {
        wheels: Some(w),
        ..Grade::default()
    };
    let curves = |c: Curves| Grade {
        curves: Some(c),
        ..Grade::default()
    };
    let identity_lut = cube("identity17", 17, |c| c);
    let look17 = cube("look17", 17, look);
    let look33 = cube("look33", 33, look);
    let tiny = cube("tiny2", 2, |c| [1.0 - c[0], c[1], c[2] * 0.5]);
    let wild = cube("wild9", 9, |c| [c[0] * 1.4 - 0.2, c[1] * c[1] * 1.3, 1.0 - c[2] * 1.2]);
    let big = cube("look65", 65, look);
    let mut v = vec![
        Case {
            name: "wheels/midtone lift",
            grade: wheels(Wheels {
                midtones: wheel(0.0, 0.0, 0.1),
                ..Wheels::default()
            }),
            tag: None,
        },
        Case {
            name: "wheels/all three",
            grade: wheels(all_wheels),
            tag: None,
        },
        Case {
            name: "wheels/the rim",
            grade: wheels(rim),
            tag: None,
        },
        Case {
            name: "wheels/shadows red",
            grade: wheels(Wheels {
                shadows: wheel(1.0, 0.0, 0.0),
                ..Wheels::default()
            }),
            tag: None,
        },
        Case {
            name: "wheels/all three, BT.709",
            grade: wheels(all_wheels),
            tag: Some("bt709"),
        },
        Case {
            name: "wheels/the rim, smpte170m",
            grade: wheels(rim),
            tag: Some("smpte170m"),
        },
        Case {
            name: "curves/master S",
            grade: curves(Curves {
                master: s_curve.clone(),
                ..Curves::default()
            }),
            tag: None,
        },
        Case {
            name: "curves/red invert",
            grade: curves(Curves {
                red: pts(&[(0.0, 1.0), (1.0, 0.0)]),
                ..Curves::default()
            }),
            tag: None,
        },
        Case {
            name: "curves/three channels and a master",
            grade: curves(Curves {
                master: pts(&[(0.0, 0.05), (0.5, 0.55), (1.0, 0.95)]),
                red: s_curve.clone(),
                green: pts(&[(0.0, 0.0), (0.4, 0.5), (1.0, 1.0)]),
                blue: pts(&[(0.0, 0.1), (1.0, 0.8)]),
            }),
            tag: None,
        },
        Case {
            name: "curves/sixteen points",
            grade: curves(Curves {
                master: pts(&zigzag),
                ..Curves::default()
            }),
            tag: None,
        },
        Case {
            name: "curves/flat ends",
            grade: curves(Curves {
                master: pts(&[(0.2, 0.2), (0.8, 0.8)]),
                ..Curves::default()
            }),
            tag: None,
        },
        Case {
            name: "curves/master S, BT.709",
            grade: curves(Curves {
                master: s_curve,
                blue: pts(&[(0.0, 0.1), (1.0, 0.8)]),
                ..Curves::default()
            }),
            tag: Some("bt709"),
        },
        Case {
            name: "lut/identity 17",
            grade: Grade {
                lut: Some(lut(identity_lut, 1.0)),
                ..Grade::default()
            },
            tag: None,
        },
        Case {
            name: "lut/look 17",
            grade: Grade {
                lut: Some(lut(look17.clone(), 1.0)),
                ..Grade::default()
            },
            tag: None,
        },
        Case {
            name: "lut/look 33 at 40%",
            grade: Grade {
                lut: Some(lut(look33, 0.4)),
                ..Grade::default()
            },
            tag: None,
        },
        Case {
            name: "lut/two points",
            grade: Grade {
                lut: Some(lut(tiny, 1.0)),
                ..Grade::default()
            },
            tag: None,
        },
        Case {
            name: "lut/values out of range",
            grade: Grade {
                lut: Some(lut(wild, 1.0)),
                ..Grade::default()
            },
            tag: None,
        },
        Case {
            name: "lut/65 points",
            grade: Grade {
                lut: Some(lut(big, 1.0)),
                ..Grade::default()
            },
            tag: None,
        },
        Case {
            name: "lut/look 17, BT.709",
            grade: Grade {
                lut: Some(lut(look17.clone(), 1.0)),
                ..Grade::default()
            },
            tag: Some("bt709"),
        },
        Case {
            name: "all three parts",
            grade: Grade {
                lut: Some(lut(look17.clone(), 0.75)),
                curves: Some(Curves {
                    master: pts(&[(0.0, 0.02), (0.5, 0.45), (1.0, 0.97)]),
                    ..Curves::default()
                }),
                wheels: Some(all_wheels),
            },
            tag: None,
        },
        Case {
            name: "all three parts, BT.709",
            grade: Grade {
                lut: Some(lut(look17, 0.75)),
                curves: Some(Curves {
                    green: pts(&[(0.0, 0.0), (0.4, 0.5), (1.0, 1.0)]),
                    ..Curves::default()
                }),
                wheels: Some(all_wheels),
            },
            tag: Some("bt709"),
        },
    ];
    v.retain(|c| !c.grade.is_identity());
    v
}

fn route_of(tag: Option<&str>) -> GradeRoute {
    match tag {
        None => GradeRoute {
            matrix: YuvMatrix::Bt601,
            via_444: false,
        },
        Some("bt709") => GradeRoute {
            matrix: YuvMatrix::Bt709,
            via_444: true,
        },
        Some(_) => GradeRoute {
            matrix: YuvMatrix::Bt601,
            via_444: true,
        },
    }
}

/// The text of the graph's chain for a picture the stream tags (`setparams` plays the tag), ending where the
/// compositor's output does: in `yuv420p`.
fn graph(case: &Case) -> String {
    let mut chain = Vec::new();
    if let Some(tag) = case.tag {
        chain.push(format!("setparams=colorspace={tag}"));
    }
    let filters = kerf_core::grade_filter_chain(&case.grade, case.tag);
    assert!(!filters.is_empty(), "{}: the graph has no filters for it", case.name);
    chain.extend(filters);
    chain.push("format=yuv420p".into());
    chain.join(",")
}

struct Diff {
    max: [i32; 3],
    differing: [usize; 3],
}

fn diff(a: &YuvFrame, b: &YuvFrame) -> Diff {
    let mut d = Diff {
        max: [0; 3],
        differing: [0; 3],
    };
    for (i, (p, q)) in [(&a.y, &b.y), (&a.u, &b.u), (&a.v, &b.v)].into_iter().enumerate() {
        for (x, y) in p.iter().zip(q) {
            let e = (i32::from(*x) - i32::from(*y)).abs();
            d.max[i] = d.max[i].max(e);
            d.differing[i] += usize::from(e != 0);
        }
    }
    d
}

fn planes(f: &YuvFrame) -> [&[u8]; 3] {
    [&f.y, &f.u, &f.v]
}

fn as_frame(like: &YuvFrame, p: [Vec<u8>; 3]) -> YuvFrame {
    let [y, u, v] = p;
    YuvFrame {
        width: like.width,
        height: like.height,
        y,
        u,
        v,
    }
}

fn frames() -> Vec<(String, YuvFrame)> {
    vec![
        ("noise 64x48".into(), noise(64, 48, 1)),
        ("noise 322x180".into(), noise(322, 180, 2)),
        ("noise 50x30".into(), noise(50, 30, 3)),
        ("noise 2x2".into(), noise(2, 2, 4)),
        ("noise 16x10".into(), noise(16, 10, 5)),
        ("ramps 256x144".into(), ramps(256, 144)),
    ]
}

/// The scalar reference is FFmpeg's, plane for plane: no sample differs. (The reference is what the shaders are
/// then held to below; if it were wrong, both would be.)
#[test]
#[ignore = "needs ffmpeg"]
fn the_reference_is_ffmpegs_grade_to_the_level() {
    let mut failures = Vec::new();
    for case in cases() {
        let stages = GradeStages::of(&case.grade, route_of(case.tag)).unwrap_or_else(|| panic!("{}: no stages", case.name));
        let vf = graph(&case);
        for (name, frame) in frames() {
            let want = ffmpeg_graded(&frame, &vf);
            let got = as_frame(
                &frame,
                reference(&stages, planes(&frame), frame.width as usize, frame.height as usize),
            );
            let d = diff(&want, &got);
            let line = format!("{:<44} {:<16} max {:?} differing {:?}", case.name, name, d.max, d.differing);
            eprintln!("{line}");
            if d.max != [0; 3] {
                failures.push(line);
            }
        }
    }
    assert!(failures.is_empty(), "the reference is not FFmpeg's:\n{}", failures.join("\n"));
}

/// The compositor's passes are the reference (and so FFmpeg's), plane for plane, on an adapter.
#[test]
#[ignore = "needs an adapter"]
fn the_shaders_are_the_reference_to_the_level() {
    let mut failures = Vec::new();
    for case in cases() {
        let stages = GradeStages::of(&case.grade, route_of(case.tag)).unwrap_or_else(|| panic!("{}: no stages", case.name));
        for (name, frame) in frames() {
            let want = as_frame(
                &frame,
                reference(&stages, planes(&frame), frame.width as usize, frame.height as usize),
            );
            let got = compositor().grade_yuv(&frame, &stages).expect("the GPU's grade");
            let d = diff(&want, &got);
            let line = format!("{:<44} {:<16} max {:?} differing {:?}", case.name, name, d.max, d.differing);
            eprintln!("{line}");
            if d.max != [0; 3] {
                failures.push(line);
            }
        }
    }
    assert!(
        failures.is_empty(),
        "the shaders are not the reference:\n{}",
        failures.join("\n")
    );
}

/// 1080p, the picture size that matters: one noise frame through FFmpeg and the compositor.
#[test]
#[ignore = "needs ffmpeg and an adapter"]
fn a_1080p_frame_is_ffmpegs_to_the_level() {
    let frame = noise(1920, 1080, 9);
    for case in cases()
        .into_iter()
        .filter(|c| matches!(c.name, "all three parts" | "all three parts, BT.709" | "wheels/all three"))
    {
        let stages = GradeStages::of(&case.grade, route_of(case.tag)).expect("stages");
        let want = ffmpeg_graded(&frame, &graph(&case));
        let got = compositor().grade_yuv(&frame, &stages).expect("the GPU's grade");
        let d = diff(&want, &got);
        eprintln!("{:<44} 1080p max {:?} differing {:?}", case.name, d.max, d.differing);
        assert_eq!(d.max, [0; 3], "{}: {:?}", case.name, d.differing);
    }
}
