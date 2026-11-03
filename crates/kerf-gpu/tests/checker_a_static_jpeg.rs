//! A still picture that does not move (a JPEG, a PNG; the path the moving-still package did not touch), drawn by the compositor and held to
//! FFmpeg's own still with no bound. It uses nothing the moving-still package added, so the same file runs on the base commit and on the branch:
//! the frames it finds drawn and different are the base's, not the branch's.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_a_static_jpeg -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr, clippy::semicolon_if_nothing_returned)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use kerf_core::{Asset, Clip, Delivery, ExportOptions, Fit, Project, RenderPlan, StreamKind, Timeline, Track};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions};

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
        Compositor::new(gpu()).expect("a compositor")
    })
}

fn frame_source() -> Arc<FrameSource> {
    static S: OnceLock<Arc<FrameSource>> = OnceLock::new();
    S.get_or_init(|| FrameSource::new(FrameSourceConfig::default())).clone()
}

fn piped(args: &[&str], input: Vec<u8>) -> Vec<u8> {
    use std::io::Write;
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn photo(name: &str, w: u32, h: u32, args: &[&str]) -> Asset {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-a-static-jpeg");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let lavfi = format!(
        "nullsrc=size={w}x{h}:rate=1:duration=1,format=rgb24,geq=r='clip((X*255/W+Y*255/H)/2+random(1)*70-35,0,255)':g='clip(255-X*255/W+random(2)*60-30,0,255)':b='clip(Y*255/H+random(3)*90-45,0,255)'"
    );
    let mut full = vec![
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &lavfi,
        "-frames:v",
        "1",
    ];
    full.extend_from_slice(args);
    full.push(path.to_str().unwrap());
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(&full)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {full:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Project::probe_asset(&path).expect("probe")
}

fn timeline(clips: Vec<Clip>, w: u32, h: u32, fit: Fit) -> Timeline {
    Timeline {
        tracks: vec![Track {
            clips,
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Some(Delivery::new(w, h, fit).with_fps(Some(10.0)).expect("a rate")),
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

#[derive(Debug)]
#[allow(dead_code)]
enum Out {
    Exact,
    Off(String),
    Refused(String),
}

fn frame(tl: &Timeline, asset: &Asset, t: f64) -> Out {
    let opts = ExportOptions::default();
    let policy = kerf_core::composite_color_policy();
    let plan = RenderPlan::at(tl, std::slice::from_ref(asset), &opts, t, policy).expect("plan");
    let size = plan.size(u32::MAX);
    let why = plan.reasons(&compositor().caps(), size);
    if !why.is_empty() {
        return Out::Refused(why.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "));
    }
    let (w, h, want) = kerf_core::timeline_frame_yuv_cancellable(tl, std::slice::from_ref(asset), &opts, t, u32::MAX, &|| false)
        .expect("FFmpeg's still");
    assert_eq!((w, h), size);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let frames = loop {
        match frame_source().frames(&plan.layers, kerf_gpu::Hint::Scrub) {
            Err(kerf_gpu::GpuError::Busy(_)) if std::time::Instant::now() < until => {
                std::thread::sleep(std::time::Duration::from_millis(25))
            }
            other => break other.expect("the decode"),
        }
    };
    let got = match compositor().composite_yuv_shared(&plan, &frames, size) {
        Ok(f) => f,
        Err(kerf_gpu::GpuError::Unsupported(e)) => return Out::Refused(format!("render: {e}")),
        Err(e) => panic!("composite: {e}"),
    };
    let data: Vec<u8> = [&got.y, &got.u, &got.v].into_iter().flatten().copied().collect();
    let got444 = piped(
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{w}x{h}"),
            "-i",
            "pipe:0",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv444p",
            "pipe:1",
        ],
        data,
    );
    let n = w as usize * h as usize;
    let mut lines = Vec::new();
    for (p, name) in ["Y", "U", "V"].into_iter().enumerate() {
        let (a, b) = (&want[p * n..(p + 1) * n], &got444[p * n..(p + 1) * n]);
        let differing = a.iter().zip(b).filter(|(x, y)| x != y).count();
        if differing > 0 {
            let max = a
                .iter()
                .zip(b)
                .map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs())
                .max()
                .unwrap_or(0);
            let first = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(0);
            lines.push(format!(
                "{name}: {differing} samples differ, max {max}, first ({}, {})",
                first % w as usize,
                first / w as usize
            ));
        }
    }
    if lines.is_empty() {
        Out::Exact
    } else {
        Out::Off(lines.join("; "))
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_static_stills_are_drawn_exactly_or_refused() {
    let mut off = Vec::new();
    let (mut exact, mut refused) = (0, 0);
    for (kind, ext, args) in [
        ("jpeg", "jpg", vec!["-q:v", "3", "-pix_fmt", "yuvj420p"]),
        ("png", "png", vec!["-pix_fmt", "rgb24"]),
    ] {
        for (pw, ph) in [
            (320, 180),
            (640, 360),
            (1920, 1080),
            (1200, 800),
            (641, 359),
            (1001, 667),
            (321, 181),
            (100, 56),
            (64, 36),
        ] {
            let asset = photo(&format!("{kind}-{pw}x{ph}.{ext}"), pw, ph, &args);
            for (cw, ch) in [(320, 180), (180, 320)] {
                for fit in [Fit::Cover, Fit::Contain] {
                    let tl = timeline(vec![Clip::new(asset.id, 0.0, 2.0, 0.0)], cw, ch, fit);
                    for t in [0.0, 1.0] {
                        let out = frame(&tl, &asset, t);
                        let name = format!("{kind} {pw}x{ph} into {cw}x{ch} {fit:?} t={t}");
                        eprintln!("A6 STATIC {name:<52} {out:?}");
                        match out {
                            Out::Exact => exact += 1,
                            Out::Refused(_) => refused += 1,
                            Out::Off(w) => off.push(format!("{name}: {w}")),
                        }
                    }
                }
            }
        }
    }
    eprintln!(
        "A6 STATIC SUMMARY: drawn {} exact {exact} inexact {} refused {refused}",
        exact + off.len(),
        off.len()
    );
    for o in &off {
        eprintln!("A6 STATIC OFF {o}");
    }
    assert!(
        off.is_empty(),
        "{} drawn frames differ from FFmpeg's still; first: {}",
        off.len(),
        off[0]
    );
}

fn footage(name: &str, w: u32, h: u32, vf: &str, pix: &str) -> Asset {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-a-static-jpeg");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.mp4"));
    let src = format!("testsrc2=size={w}x{h}:rate=10:duration=3");
    let filter = format!("{vf},format={pix}");
    let out = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &src,
            "-vf",
            &filter,
        ])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-qp",
            "0",
            "-g",
            "8",
            "-pix_fmt",
            pix,
        ])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    Project::probe_asset(&path).expect("probe")
}

/// Footage alone (no still anywhere in the cut), in every range and tag: drawn exactly or refused.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_footage_alone_in_every_range_and_tag_is_drawn_exactly_or_refused() {
    let untagged = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
    let bt709 = "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709";
    let mut off = Vec::new();
    let (mut exact, mut refused) = (0, 0);
    for (name, vf, pix) in [
        ("tv-untagged", untagged.to_string(), "yuv420p"),
        ("tv-bt709", bt709.to_string(), "yuv420p"),
        ("full-untagged", format!("{untagged},setparams=range=pc"), "yuvj420p"),
        ("full-bt709", format!("{bt709},setparams=range=pc"), "yuvj420p"),
        ("full-flag-on-420", format!("{untagged},setparams=range=pc"), "yuv420p"),
    ] {
        let asset = footage(name, 320, 180, &vf, pix);
        let tl = timeline(vec![Clip::new(asset.id, 0.0, 2.0, 0.0)], 320, 180, Fit::Cover);
        for t in [0.2, 1.0] {
            let out = frame(&tl, &asset, t);
            eprintln!(
                "A6 FOOTAGE {name:<20} [{}] range {:?} t={t}: {out:?}",
                asset.streams.iter().find_map(|s| s.pix_fmt.clone()).unwrap_or_default(),
                asset.streams.iter().find_map(|s| s.color_range.clone())
            );
            match out {
                Out::Exact => exact += 1,
                Out::Refused(_) => refused += 1,
                Out::Off(w) => off.push(format!("{name} t={t}: {w}")),
            }
        }
    }
    eprintln!(
        "A6 FOOTAGE SUMMARY: drawn {} exact {exact} inexact {} refused {refused}",
        exact + off.len(),
        off.len()
    );
    assert!(off.is_empty(), "{} frames of footage drawn and different: {off:?}", off.len());
}

/// The same frames held to FFmpeg's PNG still (RGB), as `parity.rs`'s `check_exact` does, beside the 4:4:4 planes the other tests here use:
/// where the two references disagree on a frame, the frame is drawn exactly for one and not the other.
fn rgb_reference(tl: &Timeline, asset: &Asset, t: f64, tag: &str) -> String {
    let opts = ExportOptions::default();
    let policy = kerf_core::composite_color_policy();
    let plan = RenderPlan::at(tl, std::slice::from_ref(asset), &opts, t, policy).expect("plan");
    let size = plan.size(u32::MAX);
    let why = plan.reasons(&compositor().caps(), size);
    if !why.is_empty() {
        return "refused".to_string();
    }
    let (w, h) = (size.0 as usize, size.1 as usize);
    let png = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("checker-a-static-jpeg/rgb-ref-{tag}.png"));
    kerf_core::export_still(
        tl,
        std::slice::from_ref(asset),
        &opts,
        t,
        &png,
        kerf_core::ImageFormat::Png,
        0,
    )
    .expect("FFmpeg still");
    let reference = piped(
        &["-i", png.to_str().unwrap(), "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"],
        Vec::new(),
    );
    let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
    let planes = compositor().composite_yuv(&plan, &frames, size).expect("composite");
    let matrix = match plan.canvas.matrix {
        kerf_core::YuvMatrix::Bt601 => "bt601",
        kerf_core::YuvMatrix::Bt709 => "bt709",
        kerf_core::YuvMatrix::Bt2020 => "bt2020nc",
    };
    let data: Vec<u8> = [&planes.y, &planes.u, &planes.v].into_iter().flatten().copied().collect();
    let got = piped(
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{w}x{h}"),
            "-i",
            "pipe:0",
            "-vf",
            &format!("scale=in_color_matrix={matrix}:in_range=tv"),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ],
        data,
    );
    assert_eq!(got.len(), reference.len());
    let differing = got.iter().zip(&reference).filter(|(a, b)| a != b).count();
    let max = got
        .iter()
        .zip(&reference)
        .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
        .max()
        .unwrap_or(0);
    format!("RGB reference: differing samples {differing} (max {max})")
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_a_full_range_pictures_against_both_references() {
    let untagged = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
    let footage_full = footage(
        "full-untagged",
        320,
        180,
        &format!("{untagged},setparams=range=pc"),
        "yuvj420p",
    );
    let jpeg_unscaled = photo("jpeg-320x180.jpg", 320, 180, &["-q:v", "3", "-pix_fmt", "yuvj420p"]);
    let jpeg_scaled = photo("jpeg-1200x800.jpg", 1200, 800, &["-q:v", "3", "-pix_fmt", "yuvj420p"]);
    for (name, asset) in [
        ("full-range footage", &footage_full),
        ("jpeg at the canvas's size", &jpeg_unscaled),
        ("jpeg resized 1200x800", &jpeg_scaled),
    ] {
        let tl = timeline(vec![Clip::new(asset.id, 0.0, 2.0, 0.0)], 320, 180, Fit::Cover);
        let planes = frame(&tl, asset, 0.5);
        let rgb = rgb_reference(&tl, asset, 0.5, &name.replace([' ', '\''], "_"));
        eprintln!("A6 TWOREF {name:<28} 4:4:4 planes: {planes:?}; {rgb}");
    }
}
