//! Shared by the `checker_p3_*` tests: the export-frame harness of `checker_p_pipeline` (a lossless export of a cut, the plan's verdict on
//! one frame of it and the GPU's planes against the export's), the narrowing of the calling thread's affinity from outside
//! (`taskset -p`, what a cpuset resize does to a task) and the re-run of a test binary in a child of its own.
#![allow(dead_code)]
#![allow(clippy::print_stderr)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use kerf_core::{
    Asset, Clip, Container, ExportOptions, PlanRequest, Planner, Project, RateControl, RenderPlan, StreamKind, Timeline, Track,
    Unsupported, VideoEffect,
};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions, MotionFrames, YuvFrame};

#[path = "child.rs"]
pub mod child;

pub fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

/// A compositor with the probes measured (what the app's `prepare` does), its slice threads pinned to `pin` or, `None`, the plan's own.
pub fn compositor(pin: Option<u32>) -> Arc<Compositor> {
    static ALL: OnceLock<Mutex<BTreeMap<u32, Arc<Compositor>>>> = OnceLock::new();
    let _ = kerf_gpu::grade::kernels();
    let _ = kerf_gpu::scaler::kernels();
    let _ = kerf_gpu::convert::kernels();
    let _ = kerf_gpu::blur_probe::blur();
    let _ = kerf_gpu::display_probe::display();
    let mut all = ALL.get_or_init(|| Mutex::new(BTreeMap::new())).lock().unwrap();
    all.entry(pin.unwrap_or(0))
        .or_insert_with(|| {
            let c = Compositor::new(gpu()).expect("a compositor");
            Arc::new(match pin {
                Some(t) => c.with_graph_threads(t),
                None => c,
            })
        })
        .clone()
}

pub fn frame_source() -> Arc<FrameSource> {
    static S: OnceLock<Arc<FrameSource>> = OnceLock::new();
    S.get_or_init(|| FrameSource::new(FrameSourceConfig::default())).clone()
}

pub fn scratch(sub: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(sub);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Makes `path` once, atomically: `make` writes a file of its own (named by process and thread) that is renamed into place.
pub fn made_once(path: &Path, make: impl FnOnce(&Path)) {
    if path.exists() {
        return;
    }
    let name = path.file_name().expect("a file name").to_string_lossy().into_owned();
    let own = path.with_file_name(format!("tmp-{}-{:?}-{name}", std::process::id(), std::thread::current().id()));
    make(&own);
    std::fs::rename(&own, path).expect("move the finished file into place");
}

/// A test-side FFmpeg run (the named binary, `KERF_FFMPEG` or `ffmpeg`), not one of the engine's: it makes the clips.
pub fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let mut cmd = Command::new(kerf_core::ffmpeg_path());
    // (The harness's own runs are not the engine's: they hold no binding variable, so a log of the children the engine starts has only
    // the engine's.)
    for var in ["OMP_PROC_BIND", "OMP_PLACES", "GOMP_CPU_AFFINITY", "KMP_AFFINITY"] {
        cmd.env_remove(var);
    }
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]).args(args);
    let out = child::output_within(cmd, Duration::from_secs(120)).unwrap_or_else(|e| panic!("ffmpeg {args:?}: {e}"));
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// 0.3 s of per-sample noise in all planes, `w` x `h` in `pix_fmt`, lossless.
pub fn noise_clip(w: u32, h: u32, pix_fmt: &str) -> Asset {
    let path = scratch("checker-p3").join(format!("noise-{w}x{h}-{pix_fmt}.mkv"));
    made_once(&path, |own| {
        let max = if pix_fmt.contains("10") { 1023 } else { 255 };
        let gen = match pix_fmt {
            "gray" => format!("lum='random(1)*{max}'"),
            _ => format!("lum='random(1)*{max}':cb='random(2)*{max}':cr='random(3)*{max}'"),
        };
        let lavfi = format!("nullsrc=size={w}x{h}:rate=30:duration=0.3,format={pix_fmt},geq={gen}");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &lavfi,
            "-c:v",
            "ffv1",
            "-pix_fmt",
            pix_fmt,
            own.to_str().unwrap(),
        ]);
    });
    Project::probe_asset(&path).expect("probe")
}

/// 0.4 s of `lavfi`'s picture, 8-bit 4:2:0, lossless.
pub fn lavfi_clip(name: &str, lavfi: &str) -> Asset {
    let path = scratch("checker-p3").join(format!("{name}.mkv"));
    made_once(&path, |own| {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            lavfi,
            "-c:v",
            "ffv1",
            "-pix_fmt",
            "yuv420p",
            own.to_str().unwrap(),
        ]);
    });
    Project::probe_asset(&path).expect("probe")
}

pub fn effect_clip(asset: &Asset, effects: Vec<VideoEffect>, len: f64) -> Clip {
    let mut c = Clip::new(asset.id, 0.0, len, 0.0);
    c.effects = effects;
    c
}

pub fn blurred(asset: &Asset, sigma: f64, len: f64) -> Clip {
    effect_clip(asset, vec![VideoEffect::Blur { sigma }], len)
}

pub fn one_track(clips: Vec<Clip>) -> Timeline {
    Timeline {
        tracks: vec![Track {
            clips,
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        ..Timeline::new()
    }
}

/// The export of `tl` as lossless `yuv420p`: `(w, h, frames)`.
pub fn export(slug: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions) -> (usize, usize, Vec<u8>) {
    let file = scratch("checker-p3").join(format!("{slug}.mkv"));
    let lossless = ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..opts.clone()
    };
    kerf_core::render_with(tl, assets, &file, &lossless).expect("export");
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
    (video.width.unwrap() as usize, video.height.unwrap() as usize, yuv)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub n: usize,
    pub max: i32,
    pub first: (usize, usize),
}

pub fn gap(got: &[u8], want: &[u8], w: usize) -> Option<Gap> {
    assert_eq!(got.len(), want.len());
    let mut g: Option<Gap> = None;
    for (i, (a, b)) in got.iter().zip(want).enumerate() {
        let d = (i32::from(*a) - i32::from(*b)).abs();
        if d != 0 {
            let g = g.get_or_insert(Gap {
                n: 0,
                max: 0,
                first: (i % w, i / w),
            });
            g.n += 1;
            g.max = g.max.max(d);
        }
    }
    g
}

pub fn is_plane_hazard(u: &Unsupported) -> bool {
    matches!(u, Unsupported::Effects(_, r) if r.starts_with("a blur on a plane"))
}

/// What one export frame came to.
#[derive(Debug)]
pub enum Outcome {
    /// The plan cleared it and the GPU's planes are the export's.
    Exact,
    /// The plan cleared it and the planes differ: `[Y, U, V]`.
    Differs([Option<Gap>; 3]),
    /// Refused as a blur on a plane, and drawn anyway under a thread count the planes are clear at: the gap (`None`: no thread count made
    /// the plan draw it).
    Refused {
        drawn_anyway: Option<[Option<Gap>; 3]>,
        others: Vec<String>,
    },
    /// Refused for another reason.
    Other(Vec<String>),
}

impl Outcome {
    /// One word for the logs the parents read.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Exact => "EXACT",
            Self::Differs(_) => "DIFFERS",
            Self::Refused {
                drawn_anyway: Some(g), ..
            } if g.iter().all(Option::is_none) => "REFUSED-BUT-EXACT-DRAWN-ANYWAY",
            Self::Refused { .. } => "REFUSED",
            Self::Other(_) => "OTHER",
        }
    }
}

/// An export frame of `tl` against the GPU's draw of the plan for it: the plan's own verdict at the machine as it is now.
pub fn motion_frame(slug: &str, tl: &Timeline, assets: &[Asset], fps: f64, frame: u64) -> Outcome {
    let opts = ExportOptions {
        fps: Some(fps),
        ..ExportOptions::default()
    };
    let comp = compositor(None);
    let (w, h, yuv) = export(slug, tl, assets, &opts);
    let frame_len = w * h * 3 / 2;
    assert!(yuv.len() >= (frame as usize + 1) * frame_len, "{slug}: the export is short");
    let f = &yuv[frame as usize * frame_len..(frame as usize + 1) * frame_len];
    let (wy, rest) = f.split_at(w * h);
    let (wu, wv) = rest.split_at(w * h / 4);
    let planner = Planner::new(tl, assets, &opts, PlanRequest::motion(kerf_core::composite_color_policy())).expect("planner");
    let plan = planner.at_frame(frame).expect("plan");
    let size = (w as u32, h as u32);
    let why = plan.reasons(&comp.caps(), size);
    let draw = |c: &Compositor| -> Option<[Option<Gap>; 3]> {
        let mut motion = MotionFrames::new(frame_source());
        let picture = motion.frames(&plan).expect("frames");
        let drawn: YuvFrame = c.composite_yuv_counted(&plan, &picture, motion.counts(), size).ok()?;
        Some([gap(&drawn.y, wy, w), gap(&drawn.u, wu, w / 2), gap(&drawn.v, wv, w / 2)])
    };
    if why.is_empty() {
        let g = draw(&comp).expect("drawn");
        return if g.iter().all(Option::is_none) {
            Outcome::Exact
        } else {
            Outcome::Differs(g)
        };
    }
    if why.iter().any(is_plane_hazard) {
        let others: Vec<String> = why.iter().filter(|u| !is_plane_hazard(u)).map(ToString::to_string).collect();
        for t in [1u32, 2, 3, 4, 5, 8, 16, 32, 64, 128] {
            let pinned = compositor(Some(t));
            if pinned.caps().graph_threads == Some(t) && plan.reasons(&pinned.caps(), size).is_empty() {
                return Outcome::Refused {
                    drawn_anyway: draw(&pinned),
                    others,
                };
            }
        }
        return Outcome::Refused {
            drawn_anyway: None,
            others,
        };
    }
    Outcome::Other(why.iter().map(ToString::to_string).collect())
}

/// The thread id of the calling thread, as `taskset -p` takes it.
pub fn thread_id() -> String {
    std::fs::read_link("/proc/thread-self")
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .expect("the thread id")
}

/// The calling thread's affinity set to `cpus` from outside (`taskset -p -c`): what a cpuset resize or `taskset -p` on a running task do.
pub fn narrow_thread(cpus: &str) {
    let out = Command::new("taskset")
        .args(["-p", "-c", cpus, &thread_id()])
        .output()
        .expect("taskset");
    assert!(
        out.status.success(),
        "taskset -p -c {cpus}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn have_taskset() -> bool {
    Command::new("taskset")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// This test binary run again for `test` (`--exact --ignored`), under `taskset -c <mask>` when given, with `envs` added; its stdout and
/// stderr, and whether it succeeded.
pub fn rerun(test: &str, mask: Option<&str>, envs: &[(&str, &str)], limit: Duration) -> (bool, String) {
    let exe = std::env::current_exe().expect("the test binary");
    let mut cmd = match mask {
        Some(m) => {
            let mut c = Command::new("taskset");
            c.args(["-c", m]).arg(&exe);
            c
        }
        None => Command::new(&exe),
    };
    cmd.args([test, "--exact", "--ignored", "--nocapture", "--test-threads=1"]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = child::output_within(cmd, limit).unwrap_or_else(|e| panic!("the child {test}: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

/// FFmpeg over `input` on its stdin (the harness's own run), its stdout.
pub fn ffmpeg_over(args: &[&str], input: Vec<u8>) -> Vec<u8> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("ffmpeg");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = child.stdout.take().expect("stdout");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let _ = writer.join();
    reader.join().expect("reader")
}

/// What `comp` draws of a Still plan, against FFmpeg's still, plane by plane (the still's 4:4:4 planes: the composite's 4:2:0 ones are put
/// through FFmpeg's own conversion to compare, which is injective on them).
pub fn still_gaps(
    plan: &RenderPlan,
    tl: &Timeline,
    read: &[Asset],
    opts: &ExportOptions,
    t: f64,
    comp: &Compositor,
) -> [Option<Gap>; 3] {
    let size = plan.size(u32::MAX);
    let (w, h) = (size.0 as usize, size.1 as usize);
    let (_, _, want) = kerf_core::timeline_frame_yuv_cancellable(tl, read, opts, t, size.0, &|| false).expect("the FFmpeg still");
    let frames = kerf_gpu::decode_layers(&plan.layers).expect("decode");
    let drawn = comp.composite_yuv(plan, &frames, size).expect("draw");
    let mut raw = drawn.y.clone();
    raw.extend(&drawn.u);
    raw.extend(&drawn.v);
    let s = format!("{w}x{h}");
    let got = ffmpeg_over(
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &s,
            "-i",
            "pipe:0",
            "-vf",
            "format=yuv444p",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv444p",
            "pipe:1",
        ],
        raw,
    );
    assert_eq!(got.len(), want.len());
    let n = w * h;
    [
        gap(&got[..n], &want[..n], w),
        gap(&got[n..2 * n], &want[n..2 * n], w),
        gap(&got[2 * n..], &want[2 * n..], w),
    ]
}

/// A still at `t` of `tl` over `assets` (original media): the plan's verdict on it at the machine as it is now, and the GPU's draw against
/// FFmpeg's still when the plan clears it (a refusal for a blur on a plane is drawn anyway under a thread count that clears it).
pub fn still_outcome(tl: &Timeline, assets: &[Asset], t: f64) -> Outcome {
    let opts = ExportOptions::default();
    let plan = Planner::new(
        tl,
        assets,
        &opts,
        PlanRequest::still(kerf_core::composite_color_policy()).with_media(&kerf_core::OriginalMedia),
    )
    .expect("planner")
    .at(t)
    .expect("plan");
    let size = plan.size(u32::MAX);
    let comp = compositor(None);
    let why = plan.reasons(&comp.caps(), size);
    if why.is_empty() {
        let g = still_gaps(&plan, tl, assets, &opts, t, &comp);
        return if g.iter().all(Option::is_none) {
            Outcome::Exact
        } else {
            Outcome::Differs(g)
        };
    }
    if why.iter().any(is_plane_hazard) {
        let others: Vec<String> = why.iter().filter(|u| !is_plane_hazard(u)).map(ToString::to_string).collect();
        for pin in [1u32, 2, 3, 4, 5, 8, 16, 32, 64, 128] {
            let pinned = compositor(Some(pin));
            if plan.reasons(&pinned.caps(), size).is_empty() {
                return Outcome::Refused {
                    drawn_anyway: Some(still_gaps(&plan, tl, assets, &opts, t, &pinned)),
                    others,
                };
            }
        }
        return Outcome::Refused {
            drawn_anyway: None,
            others,
        };
    }
    Outcome::Other(why.iter().map(ToString::to_string).collect())
}
