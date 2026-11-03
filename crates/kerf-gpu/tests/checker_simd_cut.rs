//! `checker/simd/…` end to end: a cut with a masked clip and a title, the export's own planes against the
//! compositor's, to the sample, in a process whose SIMD path `KERF_SIMD` picked. Run it once per value and compare
//! the files it writes: the drawn frames must be the same bytes on every path (`target/parity/simd-cut-<value>.txt`).
//! Needs `ffmpeg` and an adapter (lavapipe is enough); `#[ignore]`d like the parity suites:
//!
//! ```text
//! KERF_SIMD=scalar cargo test -p kerf-gpu --no-default-features --test checker_simd_cut -- --ignored --nocapture
//! KERF_SIMD=avx2   cargo test -p kerf-gpu --no-default-features --test checker_simd_cut -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use kerf_core::{
    Asset, Clip, Container, ExportOptions, Mask, MaskShape, PlanRequest, Planner, Project, RateControl, StreamKind, Timeline,
    Track,
};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions, MotionFrames, YuvFrame};

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| {
        let _ = kerf_gpu::grade::kernels();
        let _ = kerf_gpu::scaler::kernels();
        let _ = kerf_gpu::convert::kernels();
        let _ = kerf_core::geq_edge();
        let _ = kerf_gpu::conversion::conversion();
        let _ = kerf_gpu::placement_probe::placement();
        let _ = kerf_gpu::text::model();
        let _ = kerf_gpu::blend::kernels();
        Compositor::new(gpu()).expect("a compositor")
    })
}

fn frame_source() -> Arc<FrameSource> {
    static S: OnceLock<Arc<FrameSource>> = OnceLock::new();
    S.get_or_init(|| FrameSource::new(FrameSourceConfig::default())).clone()
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

fn source() -> &'static Asset {
    static A: OnceLock<Asset> = OnceLock::new();
    A.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-simd-media");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("testsrc.mp4");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30:duration=4,setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown",
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
            path.to_str().unwrap(),
        ]);
        Project::probe_asset(&path).expect("probe")
    })
}

struct Played {
    w: usize,
    h: usize,
    yuv: Vec<u8>,
}

impl Played {
    fn planes(&self, k: usize) -> [&[u8]; 3] {
        let len = self.w * self.h * 3 / 2;
        let f = &self.yuv[k * len..(k + 1) * len];
        let (y, uv) = f.split_at(self.w * self.h);
        let (u, v) = uv.split_at(self.w * self.h / 4);
        [y, u, v]
    }
}

fn export(slug: &str, tl: &Timeline, assets: &[Asset]) -> Played {
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-simd-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    let file = scratch.join(format!("{slug}.mkv"));
    let opts = ExportOptions {
        fps: Some(30.0),
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..ExportOptions::default()
    };
    kerf_core::render_with(tl, assets, &file, &opts).expect("export");
    let yuv = ffmpeg(&[
        "-i",
        file.to_str().unwrap(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "pipe:1",
    ]);
    let info = Project::probe_asset(&file).expect("probe the export");
    let video = info
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Video)
        .expect("a video stream");
    let _ = std::fs::remove_file(&file);
    Played {
        w: video.width.unwrap() as usize,
        h: video.height.unwrap() as usize,
        yuv,
    }
}

fn planes_of(f: &YuvFrame) -> [&[u8]; 3] {
    [&f.y, &f.u, &f.v]
}

fn fnv(planes: [&[u8]; 3]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for p in planes {
        for b in p {
            h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
        h = (h ^ 0xFF).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

struct Judged {
    drawn: usize,
    refused: usize,
    differing: usize,
    worst: i32,
    hashes: Vec<(u64, u64)>,
}

fn frames(case: &str, tl: &Timeline, assets: &[Asset], looked: &[u64]) -> Judged {
    let slug: String = case
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let opts = ExportOptions {
        fps: Some(30.0),
        ..ExportOptions::default()
    };
    let played = export(&slug, tl, assets);
    let policy = kerf_core::composite_color_policy();
    let planner = Planner::new(tl, assets, &opts, PlanRequest::motion(policy)).expect("planner");
    let size = (played.w as u32, played.h as u32);
    let mut motion = MotionFrames::new(frame_source());
    let mut j = Judged {
        drawn: 0,
        refused: 0,
        differing: 0,
        worst: 0,
        hashes: Vec::new(),
    };
    for &k in looked {
        let plan = planner.at_frame(k).expect("plan");
        if !plan.reasons(&compositor().caps(), size).is_empty() {
            j.refused += 1;
            continue;
        }
        let picture = motion.frames(&plan).unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
        let out = match compositor().composite_yuv_counted(&plan, &picture, motion.counts(), size) {
            Ok(out) => out,
            Err(kerf_gpu::GpuError::Unsupported(_)) => {
                j.refused += 1;
                continue;
            }
            Err(e) => panic!("{case}: frame {k}: {e}"),
        };
        j.drawn += 1;
        let (want, got) = (played.planes(k as usize), planes_of(&out));
        j.hashes.push((k, fnv(got)));
        for p in 0..3 {
            let (mut n, mut max, mut first) = (0usize, 0i32, None);
            for (i, (a, b)) in want[p].iter().zip(got[p]).enumerate() {
                let d = (i32::from(*a) - i32::from(*b)).abs();
                if d != 0 {
                    n += 1;
                    max = max.max(d);
                    first.get_or_insert(i);
                }
            }
            j.differing += n;
            j.worst = j.worst.max(max);
            if let Some(i) = first {
                let pw = if p == 0 { played.w } else { played.w / 2 };
                eprintln!(
                    "  {case} frame {k} plane {}: {n} samples differ, worst {max}, first ({}, {})",
                    ['Y', 'U', 'V'][p],
                    i % pw,
                    i / pw
                );
            }
        }
    }
    eprintln!(
        "{case:<62} drawn {} refused {} | differing {} worst {}",
        j.drawn, j.refused, j.differing, j.worst
    );
    j
}

fn mask(shape: MaskShape, x: f64, y: f64, w: f64, h: f64, feather: f64, inverted: bool) -> Mask {
    Mask {
        shape,
        x,
        y,
        width: w,
        height: h,
        feather,
        inverted,
    }
}

fn title(text: &str, px: f64, pos: (f64, f64), bg: Option<&str>) -> kerf_core::TextOverlay {
    let mut o = kerf_core::TextOverlay::new(text, 0.2, 1.8);
    (o.size, o.pos_x, o.pos_y) = (px / 360.0, pos.0, pos.1);
    o.bg = bg.map(str::to_string);
    o
}

fn cut(masks: &[Option<Mask>], overlays: Vec<kerf_core::TextOverlay>) -> Timeline {
    let a = source();
    let tracks = masks
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut c = Clip::new(a.id, 0.2 * i as f64, 2.0 + 0.2 * i as f64, 0.0);
            c.mask = *m;
            Track {
                clips: vec![c],
                ..Track::new(StreamKind::Video, format!("V{}", i + 1))
            }
        })
        .collect();
    Timeline {
        tracks,
        overlays,
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

fn parity_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap().join("parity");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); run once per KERF_SIMD value"]
fn checker_simd_a_masked_and_titled_cut_is_the_exports_on_the_path_this_process_picked() {
    let label = std::env::var("KERF_SIMD").unwrap_or_else(|_| "default".into());
    eprintln!(
        "KERF_SIMD={label}: the process runs on {:?} ({:?} available)",
        kerf_core::simd::path(),
        kerf_core::simd::available()
    );
    let one = Some(mask(MaskShape::Ellipse, 0.5, 0.5, 0.6, 0.6, 0.2, false));
    let cases: Vec<(&str, Timeline)> = vec![
        (
            "soft ellipse + boxed title",
            cut(
                &[one],
                vec![title("Hello, World. Ay Te Wa fi ffi", 24.0, (0.37, 0.6), Some("#102030@0.6"))],
            ),
        ),
        (
            "hard inverted rect + plain title",
            cut(
                &[Some(mask(MaskShape::Rect, 0.4, 0.55, 0.5, 0.45, 0.0, true))],
                vec![title("Hello, World. Ay Te Wa fi ffi", 36.0, (0.2, 0.3), None)],
            ),
        ),
        (
            "off-centre soft inverted ellipse over another track + boxed title",
            cut(
                &[None, Some(mask(MaskShape::Ellipse, 0.3, 0.7, 0.83, 0.37, 0.35, true))],
                vec![title(
                    "Ay Te Wa To AVATAR fi ffi rn mm Tj.",
                    14.0,
                    (0.1, 0.8),
                    Some("black@0.5"),
                )],
            ),
        ),
        (
            "feathered rect, two titles",
            cut(
                &[Some(mask(MaskShape::Rect, 0.55, 0.45, 0.77, 0.69, 0.5, false))],
                vec![
                    title("Top left", 11.0, (0.05, 0.05), Some("#ffffff@0.3")),
                    title("Bottom right, bigger", 28.0, (0.5, 0.75), None),
                ],
            ),
        ),
    ];
    let looked = [12u64, 20, 30, 45];
    let mut report = String::new();
    let mut wrong = Vec::new();
    for (name, tl) in &cases {
        let j = frames(&format!("checker/simd/{name}"), tl, std::slice::from_ref(source()), &looked);
        for (k, h) in &j.hashes {
            report.push_str(&format!("{name} | frame {k} | {h:016x}\n"));
        }
        if j.drawn == 0 {
            wrong.push(format!(
                "{name}: no frame was drawn by the compositor (all {} refused)",
                j.refused
            ));
        }
        if j.differing > 0 {
            wrong.push(format!(
                "{name}: {} samples differ from the export's, worst {}",
                j.differing, j.worst
            ));
        }
    }
    std::fs::write(parity_dir().join(format!("simd-cut-{label}.txt")), report).unwrap();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
