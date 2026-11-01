//! `checker/d/…` end to end: text in an export frame, the export's own planes against the compositor's, to the
//! sample. Self-contained (it shares no counter with `motion_parity.rs`). Needs `ffmpeg` and an adapter (lavapipe
//! is enough); `#[ignore]`d like the parity suites:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test motion_text_checker -- --ignored --nocapture
//! ```
//!
//! A frame the plan or the render refuses is FFmpeg's and counts as refused. A frame that is *drawn* must be the
//! export's: a text the sequence cannot be taken apart for must be refused, not approximated by one mask.

#![allow(clippy::print_stderr)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use kerf_core::{Asset, Clip, Container, ExportOptions, PlanRequest, Planner, Project, RateControl, StreamKind, Timeline, Track};
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
        let _ = kerf_gpu::text::model();
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
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-d-media");
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

fn timeline() -> Timeline {
    let a = source();
    Timeline {
        tracks: vec![Track {
            clips: vec![Clip::new(a.id, 0.0, 2.0, 0.0)],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    }
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
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-d-ref");
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

/// `(drawn, refused, differing samples, worst)` over the frames `looked` of the export of `tl`.
fn frames(case: &str, tl: &Timeline, looked: &[u64]) -> (usize, usize, usize, i32) {
    let assets = std::slice::from_ref(source());
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
    let (mut drawn, mut refused, mut differing, mut worst) = (0, 0, 0, 0);
    for &k in looked {
        let plan = planner.at_frame(k).expect("plan");
        if !plan.reasons(&compositor().caps(), size).is_empty() {
            refused += 1;
            continue;
        }
        let picture = motion.frames(&plan).unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
        let out = match compositor().composite_yuv_counted(&plan, &picture, motion.counts(), size) {
            Ok(out) => out,
            Err(kerf_gpu::GpuError::Unsupported(_)) => {
                refused += 1;
                continue;
            }
            Err(e) => panic!("{case}: frame {k}: {e}"),
        };
        drawn += 1;
        let (want, got) = (played.planes(k as usize), planes_of(&out));
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
            differing += n;
            worst = worst.max(max);
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
    eprintln!("{case:<62} drawn {drawn} refused {refused} | differing {differing} worst {worst}");
    (drawn, refused, differing, worst)
}

struct Spec {
    name: String,
    text: String,
    px: f64,
    bold: bool,
    font: Option<String>,
    pos: (f64, f64),
}

/// A family installed here with no bold face (a bold request in it is `drawtext` thickened with a border).
fn regular_only_family() -> Option<String> {
    kerf_core::list_system_fonts()
        .into_iter()
        .find(|family| kerf_core::fonts::resolve_font_file(family, true).is_some_and(|(_, bold)| !bold))
}

fn spec(name: impl Into<String>, text: impl Into<String>, px: f64) -> Spec {
    Spec {
        name: name.into(),
        text: text.into(),
        px,
        bold: false,
        font: None,
        pos: (0.37, 0.6),
    }
}

/// Run every spec; a drawn frame that differs is a failure; returns the report of the wrong ones.
fn judge(specs: Vec<Spec>) -> Vec<String> {
    let looked = [12u64, 20, 30, 45];
    let mut wrong = Vec::new();
    for s in specs {
        let mut tl = timeline();
        let mut o = kerf_core::TextOverlay::new(&s.text, 0.2, 1.8);
        (o.size, o.pos_x, o.pos_y) = (s.px / 360.0, s.pos.0, s.pos.1);
        o.bold = s.bold;
        o.font = s.font.clone();
        tl.overlays.push(o);
        let (drawn, _, differing, worst) = frames(&format!("checker/d/{}", s.name), &tl, &looked);
        if drawn > 0 && differing > 0 {
            wrong.push(format!(
                "{}: {differing} samples differ from the export's in drawn frames, worst {worst}",
                s.name
            ));
        }
    }
    wrong
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d_text_the_sequence_cannot_take_apart_is_drawn_exactly_or_refused() {
    let long = "Ay Te Wa To AVATAR fi ffi rn mm Tj. ".repeat(6);
    let mut specs = Vec::new();
    for px in [12.0, 13.0, 14.0, 16.0] {
        specs.push(spec(format!("tab {px}px"), "Hello,\tWorld. Ay Te Wa fi ffi", px));
        specs.push(spec(format!("combining {px}px"), "e\u{301}a\u{308}o\u{302} Ay Te Wa", px));
        specs.push(spec(format!("long {px}px"), long.clone(), px));
    }
    let wrong = judge(specs);
    assert!(
        wrong.is_empty(),
        "drawn frames that are not the export's:\n{}",
        wrong.join("\n")
    );
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d_ordinary_text_of_every_size_in_an_export_frame() {
    // Plain Latin text, the shipped font: from 3 px (small print) up. Every drawn frame must be the export's.
    let mut specs = Vec::new();
    for px in [3.0, 5.0, 8.0, 11.0, 12.0, 14.0, 16.0, 19.0, 24.0, 36.0, 60.0] {
        specs.push(spec(format!("plain {px}px"), "Hello, World. Ay Te Wa fi ffi", px));
    }
    if let Some(family) = regular_only_family() {
        for px in [10.0, 14.0, 24.0, 40.0] {
            let mut s = spec(format!("bold-in-{family} {px}px"), "Hello, World. Ay Te Wa fi ffi", px);
            (s.bold, s.font) = (true, Some(family.clone()));
            specs.push(s);
        }
    }
    let wrong = judge(specs);
    assert!(
        wrong.is_empty(),
        "drawn frames that are not the export's:\n{}",
        wrong.join("\n")
    );
}
