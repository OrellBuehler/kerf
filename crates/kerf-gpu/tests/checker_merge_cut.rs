//! `checker/merge/…` end to end: a masked clip and titles cut and resized (`Delivery`: shrunk, enlarged, odd chroma, cover,
//! a blurred fill), the export's own planes against the compositor's, to the sample, in a process whose SIMD path
//! `KERF_SIMD` picked. Every drawn frame is drawn three times: as it came, after 300 other keys went through the swscale table
//! memo, and while three threads keep churning it; the three must be the same bytes. Run it once per `KERF_SIMD` value
//! and compare the files it writes (`target/parity/merge-cut-<value>-<distro|pinned>.txt`).
//! Needs `ffmpeg` and an adapter (lavapipe is enough); `#[ignore]`d like the parity suites.

#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use kerf_core::{
    Asset, Clip, Container, Delivery, ExportOptions, Fit, Mask, MaskShape, PlanRequest, Planner, Project, RateControl,
    StreamKind, Timeline, Track,
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
        let c = Compositor::new(gpu()).expect("a compositor");
        eprintln!("the scaler this FFmpeg runs, as measured: {:?}", kerf_gpu::scaler::kernels());
        match std::env::var("KERF_MERGE_PIN_SCALER").as_deref() {
            Ok(pin @ ("c" | "aligned" | "x86")) => {
                let kernel = match pin {
                    "c" => kerf_gpu::scaler::Kernel::C,
                    "aligned" => kerf_gpu::scaler::Kernel::Aligned,
                    _ => kerf_gpu::scaler::Kernel::X86,
                };
                eprintln!("the scaler is pinned to {kernel:?} (a mutation check: the probe is not asked)");
                c.with_scaler_kernels(kerf_gpu::scaler::Kernels::Match(kerf_gpu::scaler::Model {
                    kernel,
                    default: kerf_gpu::sws::Scaler::Bicubic,
                }))
            }
            _ => c,
        }
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
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-merge-media");
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
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-merge-ref");
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

fn planes_of(f: &YuvFrame) -> [&[u8]; 3] {
    [&f.y, &f.u, &f.v]
}

/// Other keys than the picture's in the swscale table memo: far more than it holds, of every scaler and alignment.
fn churn(seed: u32) {
    for i in 0..300u32 {
        let n = seed.wrapping_mul(2654435761).wrapping_add(i * 977);
        let scaler = if n.is_multiple_of(2) {
            kerf_gpu::sws::Scaler::Bicubic
        } else {
            kerf_gpu::sws::Scaler::Bilinear
        };
        let align = [1usize, 2, 4][(n / 2 % 3) as usize];
        let one = if n % 5 < 2 {
            kerf_gpu::sws::ONE_HORIZONTAL
        } else {
            kerf_gpu::sws::ONE_VERTICAL
        };
        let _ = kerf_gpu::sws::table_aligned(scaler, 20 + n % 1900, 20 + (n / 7) % 1100, one, align);
    }
}

struct Judged {
    drawn: usize,
    refused: usize,
    differing: usize,
    worst: i32,
    unstable: usize,
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
        unstable: 0,
        hashes: Vec::new(),
    };
    for &k in looked {
        let plan = planner.at_frame(k).expect("plan");
        let reasons = plan.reasons(&compositor().caps(), size);
        if !reasons.is_empty() {
            eprintln!("  {case} frame {k}: refused by the plan: {reasons:?}");
            j.refused += 1;
            continue;
        }
        let picture = motion.frames(&plan).unwrap_or_else(|e| panic!("{case}: frame {k}: {e}"));
        let draw = |motion: &MotionFrames| compositor().composite_yuv_counted(&plan, &picture, motion.counts(), size);
        let out = match draw(&motion) {
            Ok(out) => out,
            Err(kerf_gpu::GpuError::Unsupported(why)) => {
                eprintln!("  {case} frame {k}: refused by the compositor: {why}");
                j.refused += 1;
                continue;
            }
            Err(e) => panic!("{case}: frame {k}: {e}"),
        };
        j.drawn += 1;
        let (want, got) = (played.planes(k as usize), planes_of(&out));
        let first_hash = fnv(got);
        j.hashes.push((k, first_hash));
        // The same frame after the memo was flushed by other keys, and while other threads are flushing it.
        churn(k as u32);
        let after = draw(&motion).expect("the frame again");
        let stop = std::sync::atomic::AtomicBool::new(false);
        let during = std::thread::scope(|scope| {
            for t in 0..3u32 {
                let stop = &stop;
                scope.spawn(move || {
                    let mut round = t * 100;
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        churn(round + k as u32);
                        round += 1;
                    }
                });
            }
            let r = draw(&motion).expect("the frame while the memo is churned");
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            r
        });
        for (what, f) in [("after a churn", &after), ("during a churn", &during)] {
            if fnv(planes_of(f)) != first_hash {
                j.unstable += 1;
                eprintln!("  {case} frame {k}: the frame {what} differs from the first draw");
            }
        }
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
        "{case:<70} {}x{} drawn {} refused {} | differing {} worst {} unstable {}",
        played.w, played.h, j.drawn, j.refused, j.differing, j.worst, j.unstable
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

fn cut(masks: &[Option<Mask>], overlays: Vec<kerf_core::TextOverlay>, to: Delivery) -> Timeline {
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
        format: Some(to),
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
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); run once per KERF_SIMD value and FFmpeg build"]
fn checker_merge_a_masked_titled_resized_cut_is_the_exports_and_the_same_bytes_on_every_path_and_memo_state() {
    let label = std::env::var("KERF_SIMD").unwrap_or_else(|_| "default".into());
    let build = if std::env::var("KERF_FFMPEG").is_ok() {
        "pinned"
    } else {
        "distro"
    };
    eprintln!(
        "KERF_SIMD={label}, {build} FFmpeg: the process runs on {:?} ({:?} available)",
        kerf_core::simd::path(),
        kerf_core::simd::available()
    );
    let soft = Some(mask(MaskShape::Ellipse, 0.5, 0.5, 0.6, 0.6, 0.2, false));
    let hard = Some(mask(MaskShape::Rect, 0.4, 0.55, 0.5, 0.45, 0.0, true));
    let boxed = || title("Hello, World. Ay Te Wa fi ffi", 24.0, (0.37, 0.6), Some("#102030@0.6"));
    let plain = || title("Ay Te Wa To AVATAR fi ffi rn mm Tj.", 30.0, (0.2, 0.3), None);
    let cases: Vec<(&str, Timeline)> = vec![
        (
            "soft ellipse + boxed title, shrunk 2:1 to 320x180",
            cut(&[soft], vec![boxed()], Delivery::new(320, 180, Fit::Contain)),
        ),
        (
            "hard inverted rect + plain title, enlarged 1.5x to 960x540",
            cut(&[hard], vec![plain()], Delivery::new(960, 540, Fit::Contain)),
        ),
        (
            "soft ellipse + boxed title, shrunk to 426x240 (odd chroma)",
            cut(&[soft], vec![boxed()], Delivery::new(426, 240, Fit::Contain)),
        ),
        (
            "two tracks, the upper under a hard mask, two titles, 480x270",
            cut(
                &[None, hard],
                vec![
                    title("Top left", 11.0, (0.05, 0.05), Some("#ffffff@0.3")),
                    title("Bottom right, bigger", 28.0, (0.5, 0.75), None),
                ],
                Delivery::new(480, 270, Fit::Contain),
            ),
        ),
        (
            "feathered rect + boxed title, cover into portrait 360x640",
            cut(
                &[Some(mask(MaskShape::Rect, 0.55, 0.45, 0.77, 0.69, 0.5, false))],
                vec![boxed()],
                Delivery::new(360, 640, Fit::Cover),
            ),
        ),
        (
            "soft ellipse + plain title, contain into portrait 360x640 over a blurred fill",
            cut(&[soft], vec![plain()], Delivery::new(360, 640, Fit::Blur)),
        ),
        (
            "unscaled control: soft ellipse + boxed title at 640x360",
            cut(&[soft], vec![boxed()], Delivery::new(640, 360, Fit::Contain)),
        ),
    ];
    let looked = [12u64, 20, 30, 45];
    let mut report = String::new();
    let mut wrong = Vec::new();
    let mut drawn = 0;
    for (name, tl) in &cases {
        let j = frames(&format!("checker/merge/{name}"), tl, std::slice::from_ref(source()), &looked);
        drawn += j.drawn;
        for (k, h) in &j.hashes {
            report.push_str(&format!("{name} | frame {k} | {h:016x}\n"));
        }
        if j.differing > 0 {
            wrong.push(format!(
                "{name}: {} samples differ from the export's, worst {}",
                j.differing, j.worst
            ));
        }
        if j.unstable > 0 {
            wrong.push(format!("{name}: {} frames moved when the memo was churned", j.unstable));
        }
    }
    std::fs::write(parity_dir().join(format!("merge-cut-{label}-{build}.txt")), report).unwrap();
    assert!(drawn > 0, "no frame of any case was drawn by the compositor");
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
