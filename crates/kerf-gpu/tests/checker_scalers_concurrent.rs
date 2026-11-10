//! `checker/scalers/concurrent/...`: many callers at once, as a busy machine has them (previews, a player, an export's checks), in a fresh
//! process with nothing measured. Each asks the compositor to prepare a plan of a delivery that names a different scaler (or none), then
//! draws it where the plan says the compositor can, and the picture is held to FFmpeg's still **to the sample**. The scaler's probe is
//! measured once, whoever asks (the wrapper counts the FFmpegs it starts: one measurement of the base, of Lanczos, of the spline, of
//! neighbor), a caller that asks for a name is told it once the name is measured, and no frame is drawn that is not exact. Needs `ffmpeg`
//! and a GPU adapter (lavapipe is enough); `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_scalers_concurrent -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier, Mutex};

use kerf_core::{Asset, Clip, Delivery, ExportOptions, Fit, Project, RenderPlan, StreamKind, Timeline, Track};
use kerf_gpu::{Compositor, Gpu, GpuOptions};

const DIR: &str = "KERF_SC_CONC_DIR";
const THREADS: usize = 12;
const DELIVERIES: [Option<&str>; 6] = [
    None,
    Some("bicubic"),
    Some("bilinear"),
    Some("lanczos"),
    Some("neighbor"),
    Some("spline"),
];

fn ffmpeg(args: &[&str]) {
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

fn cut(asset: &Asset) -> Timeline {
    Timeline {
        tracks: vec![Track {
            clips: vec![Clip::new(asset.id, 0.0, 0.4, 0.0)],
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Some(Delivery::new(320, 180, Fit::Contain)),
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

/// FFmpeg's still of the cut against the compositor's planes taken to 4:4:4 by FFmpeg itself: the number of samples that differ.
fn differing(
    comp: &Compositor,
    plan: &RenderPlan,
    tl: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
) -> Result<usize, String> {
    let size = plan.size(u32::MAX);
    let (w, h, want) =
        kerf_core::timeline_frame_yuv_cancellable(tl, assets, opts, t, size.0, &|| false).map_err(|e| e.to_string())?;
    let frames = kerf_gpu::decode_layers(&plan.layers).map_err(|e| e.to_string())?;
    let drawn = comp.composite_yuv(plan, &frames, size).map_err(|e| e.to_string())?;
    let mut raw = drawn.y.clone();
    raw.extend_from_slice(&drawn.u);
    raw.extend_from_slice(&drawn.v);
    let dims = format!("{w}x{h}");
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &dims,
            "-i",
            "pipe:0",
        ])
        .args([
            "-filter_complex",
            "[0:v]format=yuv444p[o]",
            "-map",
            "[o]",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&raw);
    });
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    if !out.status.success() || out.stdout.len() != want.len() {
        return Err(format!("conversion: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(want.iter().zip(&out.stdout).filter(|(a, b)| a != b).count())
}

/// The child: twelve callers released together, each preparing and drawing the plan of every delivery in its own order.
#[test]
#[ignore = "run by the test below, in a child process"]
fn checker_scalers_concurrent_child() {
    let Some(dir) = std::env::var_os(DIR).map(PathBuf::from) else {
        return;
    };
    let path = dir.join("source.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "nullsrc=size=960x540:rate=15:duration=0.6,format=yuv420p,geq=lum='random(1)*255':cb='random(2)*255':cr='random(3)*255'",
        "-c:v",
        "libx264",
        "-qp",
        "0",
        "-pix_fmt",
        "yuv420p",
        path.to_str().unwrap(),
    ]);
    let asset = Project::probe_asset(&path).expect("probe the source");
    let tl = cut(&asset);
    let assets = [asset];
    let policy = kerf_core::composite_color_policy();
    let comp = Arc::new(Compositor::new(Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter")).expect("a compositor"));
    let start = Arc::new(Barrier::new(THREADS));
    let tally: Arc<Mutex<BTreeMap<String, [usize; 4]>>> = Arc::new(Mutex::new(BTreeMap::new()));
    std::thread::scope(|scope| {
        for i in 0..THREADS {
            let (comp, start, tally, tl, assets) = (&comp, &start, &tally, &tl, &assets);
            scope.spawn(move || {
                start.wait();
                for k in 0..DELIVERIES.len() {
                    let name = DELIVERIES[(i + k) % DELIVERIES.len()];
                    let opts = ExportOptions {
                        scaler: name.map(str::to_string),
                        ..ExportOptions::default()
                    };
                    let plan = RenderPlan::at(tl, assets, &opts, 0.2, policy).expect("a plan");
                    let size = plan.size(u32::MAX);
                    let (mut drawn, mut refused, mut inexact, mut late) = (0, 0, 0, 0);
                    // (A caller that arrives while another's measurement of the base is flying is not the one that asked for its name, so its
                    // `prepare` may return before the name is measured: the plan then refuses the name, and FFmpeg draws the frame. The next
                    // `prepare` asks for it.)
                    for attempt in 0..2 {
                        kerf_gpu::prepare(&plan);
                        let why = plan.reasons(&comp.caps(), size);
                        if why.is_empty() {
                            match differing(comp, &plan, tl, assets, &opts, 0.2) {
                                Ok(n) => {
                                    drawn = 1;
                                    inexact = n;
                                }
                                Err(e) => panic!("{name:?}: {e}"),
                            }
                            break;
                        }
                        assert!(
                            why.iter().all(|u| matches!(u, kerf_core::Unsupported::Scaler(_))),
                            "{name:?}: refused for more than the name not being measured yet: {why:?}"
                        );
                        late += 1;
                        if attempt == 1 {
                            refused = 1;
                        }
                    }
                    let mut t = tally.lock().unwrap();
                    let e = t.entry(name.unwrap_or("default").to_string()).or_default();
                    e[0] += drawn;
                    e[1] += refused;
                    e[2] += inexact;
                    e[3] += late;
                }
            });
        }
    });
    let t = tally.lock().unwrap();
    for (name, [drawn, refused, inexact, late]) in t.iter() {
        eprintln!("CONC {name} drawn={drawn} refused={refused} inexact={inexact} late={late}");
    }
}

fn count(log: &Path, needle: &str) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(needle))
        .count()
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_scalers_many_concurrent_preview_and_play_callers_stay_exact_and_measure_each_question_once() {
    let real = kerf_core::ffmpeg_path();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-sc-concurrent");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("runs.log");
    let wrapper = dir.join("ffmpeg-wrapper");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\necho \"$$ $*\" >> \"{}\"\nexec '{real}' \"$@\"\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "--nocapture",
            "--test-threads=1",
            "checker_scalers_concurrent_child",
        ])
        .env(DIR, &dir)
        .env("KERF_FFMPEG", &wrapper)
        .stdin(Stdio::null())
        .output()
        .expect("run the child");
    let text = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "the child failed: {text}");
    let conc: Vec<&str> = text.lines().filter(|l| l.starts_with("CONC ")).collect();
    for l in text.lines().filter(|l| l.starts_with("REFUSED")) {
        eprintln!("{l}");
    }
    eprintln!("{conc:#?}");
    assert_eq!(conc.len(), DELIVERIES.len(), "{text}");
    for l in &conc {
        let field = |k: &str| -> usize { l.split(k).nth(1).unwrap().split(' ').next().unwrap().parse().unwrap() };
        assert_eq!(
            field("inexact="),
            0,
            "a frame drawn under concurrent callers is not FFmpeg's: {l}"
        );
        // Every one of the twelve draws every delivery by its second `prepare` at the latest.
        assert_eq!(field("drawn="), THREADS, "{l}");
        assert_eq!(field("refused="), 0, "{l}");
    }
    // Each question was measured once, however many callers asked (a single flight): the base's source picture, each name's chains, `sin`.
    // (The source picture's child is the one `mod(X*X*37` graph that is read as a lavfi input; every chain carries it in a filter graph.)
    let source_children = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("mod(X*X*37") && l.contains("-f lavfi"))
        .count();
    assert_eq!(source_children, 1, "the base was measured more than once");
    assert_eq!(
        count(&log, "scale=300:200:flags=bilinear"),
        1,
        "the base's chains more than once"
    );
    assert_eq!(count(&log, "sin("), 1, "lanczos's sin check more than once");
    for name in ["lanczos", "spline", "neighbor"] {
        for size in ["22:20", "35:18", "300:200"] {
            assert_eq!(count(&log, &format!("scale={size}:flags={name}")), 1, "{name} {size}");
        }
    }
}
