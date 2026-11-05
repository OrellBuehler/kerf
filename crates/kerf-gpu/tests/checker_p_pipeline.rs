//! `checker/p/pipeline`: the blur rule through the whole path, the plan and the compositor against FFmpeg's own export and still, on
//! pictures small enough to meet the kernel's hazards, at the budgets the app can set.
//!
//! * `checker_p_pipeline_export_*`: an export frame (a lossless export, the engine's own argv at the budget in force) of a noisy clip with
//!   a blur, at budgets 10, 25, 50, 75 and 100 %, on canvases whose planes sit on each side of the rule for that budget's slice
//!   threads, and a small picture placed in a larger canvas. The plan clears it and the GPU's planes are the export's, with no sample
//!   different, or the plan refuses the blur-on-a-plane way. A refused frame is drawn anyway (the compositor is told a thread count the
//!   plane is clear at) and must not be the export's.
//! * `checker_p_pipeline_still_*`: a still (`RenderPlan::at`) of an image (one slice thread, `cpu::single_frame_args`) and of footage (the
//!   budget's).
//! * `checker_p_pipeline_other_formats_*`: a blur on a 4:4:4, 4:2:2, gray or 10-bit source.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_p_pipeline -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```
#![allow(clippy::print_stderr)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use kerf_core::{
    Asset, Clip, Container, Delivery, ExportOptions, Fit, PlanRequest, Planner, Project, RateControl, RenderPlan, StreamKind,
    Timeline, Track, Unsupported, VideoEffect,
};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions, MotionFrames, YuvFrame};

#[path = "common/child.rs"]
mod child;

static SERIAL: Mutex<()> = Mutex::new(());

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

/// A compositor with the probes measured (what the app's `prepare` does), its slice threads pinned to `pin` or, `None`, the plan's own.
fn compositor(pin: Option<u32>) -> Arc<Compositor> {
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

fn frame_source() -> Arc<FrameSource> {
    static S: OnceLock<Arc<FrameSource>> = OnceLock::new();
    S.get_or_init(|| FrameSource::new(FrameSourceConfig::default())).clone()
}

fn scratch(sub: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(sub);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Makes `path` once, atomically: `make` writes a file of its own (named by process and thread) that is renamed into place, so that a
/// test racing this one never probes a half-written file (a second test saw `path` exist while the first was still writing it:
/// "EBML header parsing failed").
fn made_once(path: &Path, make: impl FnOnce(&Path)) {
    if path.exists() {
        return;
    }
    let name = path.file_name().expect("a file name").to_string_lossy().into_owned();
    let own = path.with_file_name(format!("tmp-{}-{:?}-{name}", std::process::id(), std::thread::current().id()));
    make(&own);
    std::fs::rename(&own, path).expect("move the finished file into place");
}

fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let mut cmd = Command::new(kerf_core::ffmpeg_path());
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]).args(args);
    let out = child::output_within(cmd, Duration::from_secs(120)).unwrap_or_else(|e| panic!("ffmpeg {args:?}: {e}"));
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// FFmpeg over `input` on its stdin, killed after a minute: its stdout.
fn ffmpeg_over(args: &[&str], input: Vec<u8>) -> Vec<u8> {
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
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
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

/// 0.3 s of per-sample noise in all planes, `w` x `h` in `pix_fmt` (the noise is made in that format, so a 4:4:4 picture has noise in
/// every chroma sample), lossless.
fn noise_clip(w: u32, h: u32, pix_fmt: &str) -> Asset {
    let path = scratch("checker-p-pipeline").join(format!("noise-{w}x{h}-{pix_fmt}.mkv"));
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

/// A still image of noise (PNG): an image clip's graph runs on one slice thread.
fn noise_image(w: u32, h: u32) -> Asset {
    let path = scratch("checker-p-pipeline").join(format!("noise-{w}x{h}.png"));
    made_once(&path, |own| {
        let lavfi = format!(
            "nullsrc=size={w}x{h}:rate=30:duration=0.1,format=rgb24,geq=r='random(1)*255':g='random(2)*255':b='random(3)*255'"
        );
        ffmpeg(&["-f", "lavfi", "-i", &lavfi, "-frames:v", "1", own.to_str().unwrap()]);
    });
    Project::probe_asset(&path).expect("probe")
}

fn blurred(asset: &Asset, sigma: f64, len: f64) -> Clip {
    let mut c = Clip::new(asset.id, 0.0, len, 0.0);
    c.effects = vec![VideoEffect::Blur { sigma }];
    c
}

fn one_track(clips: Vec<Clip>, format: Option<Delivery>) -> Timeline {
    Timeline {
        tracks: vec![Track {
            clips,
            ..Track::new(StreamKind::Video, "V1".to_string())
        }],
        format,
        ..Timeline::new()
    }
}

/// The export of `tl` as lossless `yuv420p`: `(w, h, frames)`.
fn export(slug: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions) -> (usize, usize, Vec<u8>) {
    let file = scratch("checker-p-pipeline").join(format!("{slug}.mkv"));
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
struct Gap {
    n: usize,
    max: i32,
    first: (usize, usize),
}

fn gap(got: &[u8], want: &[u8], w: usize) -> Option<Gap> {
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

fn is_plane_hazard(u: &Unsupported) -> bool {
    matches!(u, Unsupported::Effects(_, r) if r.starts_with("a blur on a plane"))
}

/// What one export frame came to.
#[derive(Debug)]
enum Outcome {
    /// The plan cleared it and the GPU's planes are the export's.
    Exact,
    /// The plan cleared it and the planes differ: `[Y, U, V]`.
    Differs([Option<Gap>; 3]),
    /// Refused as a blur on a plane (what the plan said), and drawn anyway under a thread count the planes are clear at: the gap, `None`
    /// when no thread count made the plan draw it, or the drawing was the export's.
    Refused {
        drawn_anyway: Option<[Option<Gap>; 3]>,
        others: Vec<String>,
        /// The plan clears the picture at the low end of the machine's thread range (what FFmpeg's graph runs on, here): the refusal is
        /// for the thread counts above it, which only a cgroup quota hiding cores from this process can make real.
        band: bool,
    },
    /// Refused for another reason.
    Other(Vec<String>),
}

fn motion_frame(slug: &str, tl: &Timeline, assets: &[Asset], fps: f64, frame: u64) -> Outcome {
    let opts = ExportOptions {
        fps: Some(fps),
        ..ExportOptions::default()
    };
    let (w, h, yuv) = export(slug, tl, assets, &opts);
    let frame_len = w * h * 3 / 2;
    assert!(yuv.len() >= (frame as usize + 1) * frame_len, "{slug}: the export is short");
    let f = &yuv[frame as usize * frame_len..(frame as usize + 1) * frame_len];
    let (wy, rest) = f.split_at(w * h);
    let (wu, wv) = rest.split_at(w * h / 4);
    let planner = Planner::new(tl, assets, &opts, PlanRequest::motion(kerf_core::composite_color_policy())).expect("planner");
    let plan = planner.at_frame(frame).expect("plan");
    let size = (w as u32, h as u32);
    let comp = compositor(None);
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
        let (lo, _) = kerf_core::cpu_graph_thread_range();
        let at_lo = compositor(Some(u32::try_from(lo).unwrap_or(1)));
        let band = plan.reasons(&at_lo.caps(), size).is_empty();
        // Told a thread count at which the planes are clear, the plan draws it.
        for t in [1u32, 2, 3, 4, 5, 8, 16, 32, 64, 128] {
            let pinned = compositor(Some(t));
            if pinned.caps().graph_threads == Some(t) && plan.reasons(&pinned.caps(), size).is_empty() {
                return Outcome::Refused {
                    drawn_anyway: draw(&pinned),
                    others,
                    band,
                };
            }
        }
        return Outcome::Refused {
            drawn_anyway: None,
            others,
            band,
        };
    }
    Outcome::Other(why.iter().map(ToString::to_string).collect())
}

struct Tally {
    exact: usize,
    refused: usize,
    /// Refused, and exact when drawn anyway, though the plan refuses it at the thread count FFmpeg's graph runs on: the rule is wider
    /// than the kernel inside its own domain.
    unnecessary: Vec<String>,
    /// Refused, and exact, but only for the thread counts above the one FFmpeg runs on here (the range a cgroup quota can make real).
    band: Vec<String>,
    failures: Vec<String>,
    /// Refused for a reason that is not the blur's planes.
    other: usize,
}

impl Tally {
    fn new() -> Self {
        Self {
            exact: 0,
            refused: 0,
            unnecessary: Vec::new(),
            band: Vec::new(),
            failures: Vec::new(),
            other: 0,
        }
    }

    fn take(&mut self, case: &str, out: Outcome) {
        match out {
            Outcome::Exact => {
                self.exact += 1;
                eprintln!("CHECKER-P {case}: drawn, exact");
            }
            Outcome::Differs(g) => self.failures.push(format!(
                "{case}: the plan cleared it and the GPU differs from the export: {g:?}"
            )),
            Outcome::Refused {
                drawn_anyway,
                others,
                band,
            } => {
                self.refused += 1;
                match drawn_anyway {
                    Some(g) if g.iter().all(Option::is_none) => {
                        let line = format!("{case}: refused as a blur on a plane, drawn anyway it is the export's {others:?}");
                        if band {
                            eprintln!("CHECKER-P {case}: refused for the thread counts above this machine's, and the draw anyway is EXACT");
                            self.band.push(line);
                        } else {
                            eprintln!(
                                "CHECKER-P {case}: refused at this machine's own thread count and the draw anyway is EXACT"
                            );
                            self.unnecessary.push(line);
                        }
                    }
                    Some(g) => eprintln!("CHECKER-P {case}: refused; drawn anyway {g:?}"),
                    None => {
                        eprintln!("CHECKER-P {case}: refused (no thread count draws it) {others:?}");
                        // (Pinned to one thread the plan clears every picture but one with a plane a sample across: a pin it ignores
                        // is a refusal nothing can lift.)
                        if others.is_empty() {
                            self.failures.push(format!(
                                "{case}: refused as a blur on a plane at every pin: the plan ignores the compositor's pin"
                            ));
                        }
                    }
                }
            }
            Outcome::Other(why) => {
                self.other += 1;
                eprintln!("CHECKER-P {case}: refused for another reason {why:?}");
            }
        }
    }
}

/// The budgets (percent of the cores) the app can set: the machine's slice-thread range at each.
fn budgets() -> Vec<(u8, (usize, usize))> {
    let cores = kerf_core::cpu_cores();
    let mut seen = Vec::new();
    for p in [10u8, 25, 50, 75, 90, 100] {
        kerf_core::set_cpu_percent(p);
        let r = kerf_core::cpu_graph_thread_range();
        if !seen.iter().any(|(_, q)| *q == r) {
            seen.push((p, r));
        }
    }
    eprintln!("CHECKER-P budgets on {cores} cores: {seen:?}");
    seen
}

/// Canvases for a slice-thread range: the plane heights at a flip of the rule (chroma rows against the threads), and a column of rows over
/// a narrow plane.
fn canvases((lo, hi): (usize, usize)) -> Vec<(u32, u32)> {
    // (8x8 and 8x12: a hazard for the 8-lane kernel's rows and not the 16-lane one's; 12x16 and 16x16 the other way and both.)
    let mut v = vec![
        (16, 32),
        (32, 16),
        (16, 64),
        (24, 32),
        (34, 34),
        (64, 64),
        (8, 8),
        (8, 12),
        (12, 16),
    ];
    for t in [lo, hi] {
        for chroma_rows in [t.saturating_sub(1), t, t + 1] {
            let h = (2 * chroma_rows).max(16) as u32;
            v.push((64, h));
        }
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// **Soundness through the real path**: an export frame at each budget on each canvas.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p_pipeline_export_a_cleared_blur_is_the_exports_at_every_budget() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let was = kerf_core::cpu_percent();
    let mut tally = Tally::new();
    for (percent, range) in budgets() {
        kerf_core::set_cpu_percent(percent);
        for (w, h) in canvases(range) {
            for sigma in [0.5, 6.0] {
                let asset = noise_clip(w, h, "yuv420p");
                let tl = one_track(vec![blurred(&asset, sigma, 0.2)], None);
                let case = format!("export budget {percent}% {range:?} {w}x{h} sigma {sigma}");
                let out = motion_frame(
                    &format!("p-exp-{percent}-{w}x{h}-{sigma}"),
                    &tl,
                    std::slice::from_ref(&asset),
                    30.0,
                    2,
                );
                tally.take(&case, out);
            }
        }
    }
    kerf_core::set_cpu_percent(was);
    eprintln!(
        "CHECKER-P export: {} drawn exact, {} refused, of which {} exact at this machine's own thread count (refused for the range above it) {:#?}",
        tally.exact,
        tally.refused,
        tally.band.len(),
        tally.band
    );
    assert!(tally.exact > 0 && tally.refused > 0, "the sweep should meet both");
    assert!(tally.failures.is_empty(), "{:#?}", tally.failures);
    assert!(
        tally.unnecessary.is_empty(),
        "refusals the draw does not need: {:#?}",
        tally.unnecessary
    );
}

/// A small picture placed in a larger canvas: the plane judged is the layer's, not the canvas's. A 32x128 clip at 0.4 is a 13x51 picture
/// (odd width, a column of rows) over a 64x128 base.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p_pipeline_export_a_placed_picture_is_judged_by_its_own_planes() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let was = kerf_core::cpu_percent();
    let mut tally = Tally::new();
    for (percent, range) in budgets() {
        kerf_core::set_cpu_percent(percent);
        for (pw, ph, scale) in [
            (32u32, 128u32, 0.4),
            (32, 128, 0.3),
            (48, 96, 0.35),
            (20, 160, 0.5),
            (64, 64, 0.2),
        ] {
            let base = noise_clip(64, 128, "yuv420p");
            let small = noise_clip(pw, ph, "yuv420p");
            let mut pip = blurred(&small, 3.0, 0.2);
            pip.transform.scale = scale;
            let tl = Timeline {
                tracks: vec![
                    Track {
                        clips: vec![Clip::new(base.id, 0.0, 0.2, 0.0)],
                        ..Track::new(StreamKind::Video, "V1".to_string())
                    },
                    Track {
                        clips: vec![pip],
                        ..Track::new(StreamKind::Video, "V2".to_string())
                    },
                ],
                format: Some(Delivery::new(64, 128, Fit::Contain)),
                ..Timeline::new()
            };
            let case = format!("placed budget {percent}% {range:?} {pw}x{ph} at {scale}");
            let assets = [base.clone(), small.clone()];
            let out = motion_frame(&format!("p-pip-{percent}-{pw}x{ph}-{scale}"), &tl, &assets, 30.0, 2);
            tally.take(&case, out);
        }
    }
    kerf_core::set_cpu_percent(was);
    eprintln!(
        "CHECKER-P placed: {} drawn exact, {} refused, of which {} exact at this machine's own thread count {:#?}",
        tally.exact,
        tally.refused,
        tally.band.len(),
        tally.band
    );
    assert!(tally.failures.is_empty(), "{:#?}", tally.failures);
    assert!(
        tally.unnecessary.is_empty(),
        "refusals the draw does not need: {:#?}",
        tally.unnecessary
    );
}

/// What `comp` draws of a Still plan, against FFmpeg's still, plane by plane (the still's 4:4:4 planes: the composite's 4:2:0 ones are put
/// through FFmpeg's own conversion to compare, which is injective on them).
fn still_gaps(
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

/// One still, judged: the plan clears it and the GPU is the still's, or the plan refuses it (as a blur on a plane) and drawing it anyway,
/// under a thread count the plan clears it at, is not the still's. `assets` are what the plan is made over (through `media`), `read` what
/// FFmpeg's still reads (the same, but for a proxy: the plan resolves the asset to its proxy and the still is handed the proxy's own).
fn judge_still(
    case: &str,
    tl: &Timeline,
    (assets, read): (&[Asset], &[Asset]),
    media: &dyn kerf_core::MediaResolver,
    opts: &ExportOptions,
    t: f64,
    tally: &mut Tally,
) {
    let plan = Planner::new(
        tl,
        assets,
        opts,
        PlanRequest::still(kerf_core::composite_color_policy()).with_media(media),
    )
    .expect("planner")
    .at(t)
    .expect("plan");
    let size = plan.size(u32::MAX);
    let comp = compositor(None);
    let why = plan.reasons(&comp.caps(), size);
    if why.is_empty() {
        let g = still_gaps(&plan, tl, read, opts, t, &comp);
        if g.iter().all(Option::is_none) {
            tally.exact += 1;
            eprintln!("CHECKER-P {case}: drawn, exact");
        } else {
            tally.failures.push(format!(
                "{case}: the plan cleared it and the GPU differs from the still: {g:?}"
            ));
        }
        return;
    }
    if !why.iter().any(is_plane_hazard) {
        tally.other += 1;
        eprintln!(
            "CHECKER-P {case}: refused for another reason {:?}",
            why.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
        return;
    }
    let others: Vec<String> = why.iter().filter(|u| !is_plane_hazard(u)).map(ToString::to_string).collect();
    // (What the still runs on: the plan's own low end, the machine's count or the single thread of an image or a proxy.)
    let actual = plan.graph_thread_range(&comp.caps()).0;
    let at_actual = compositor(Some(actual));
    let band = plan.reasons(&at_actual.caps(), size).is_empty();
    for pin in [1u32, 2, 3, 4, 5, 8, 16, 32, 64, 128] {
        let pinned = compositor(Some(pin));
        if plan.reasons(&pinned.caps(), size).is_empty() {
            let g = still_gaps(&plan, tl, read, opts, t, &pinned);
            tally.refused += 1;
            if g.iter().all(Option::is_none) {
                let line = format!("{case}: refused as a blur on a plane, drawn anyway it is the still's {others:?}");
                if band {
                    tally.band.push(line);
                } else {
                    tally.unnecessary.push(line);
                }
                eprintln!("CHECKER-P {case}: refused and the draw anyway is EXACT (actual threads {actual}, band {band})");
            } else {
                eprintln!("CHECKER-P {case}: refused; drawn anyway {g:?}");
            }
            return;
        }
    }
    tally.refused += 1;
    eprintln!("CHECKER-P {case}: refused ({others:?}), no thread count draws it");
    if others.is_empty() {
        tally.failures.push(format!(
            "{case}: refused as a blur on a plane at every pin: the plan ignores the compositor's pin"
        ));
    }
}

/// A still of an image runs its graph on one slice thread whatever the budget (`single_frame_args`), a still of footage on the budget's: the
/// plan judges each at its own count.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p_pipeline_still_is_judged_at_the_threads_the_still_runs_on() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let was = kerf_core::cpu_percent();
    let opts = ExportOptions::default();
    let mut tally = Tally::new();
    for (percent, range) in budgets() {
        kerf_core::set_cpu_percent(percent);
        for (w, h) in canvases(range)
            .into_iter()
            .chain([(40, 4), (40, 6), (12, 16), (16, 16), (64, 6), (34, 20)])
        {
            if h < 4 || w < 4 {
                continue;
            }
            for (kind, asset) in [("image", noise_image(w, h)), ("footage", noise_clip(w, h, "yuv420p"))] {
                let tl = one_track(vec![blurred(&asset, 3.0, 0.2)], None);
                let case = format!("still {kind} budget {percent}% {range:?} {w}x{h}");
                judge_still(
                    &case,
                    &tl,
                    (std::slice::from_ref(&asset), std::slice::from_ref(&asset)),
                    &kerf_core::OriginalMedia,
                    &opts,
                    0.05,
                    &mut tally,
                );
            }
        }
    }
    kerf_core::set_cpu_percent(was);
    eprintln!(
        "CHECKER-P still: {} drawn exact, {} refused ({} exact at the thread count the still runs on) {:#?}",
        tally.exact,
        tally.refused,
        tally.band.len(),
        tally.band
    );
    assert!(tally.exact > 0 && tally.refused > 0);
    assert!(tally.failures.is_empty(), "{:#?}", tally.failures);
    assert!(
        tally.unnecessary.is_empty(),
        "refusals the draw does not need: {:#?}",
        tally.unnecessary
    );
}

/// A preview proxy is read on one thread (`single_frame_args`: every input a proxy or an image), at any budget: a picture whose hazard is
/// the one-thread block rule (a vector of rows over a narrow plane) is refused, one whose hazard is the budget's slice count is not, the
/// other way round from the same picture as footage.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p_pipeline_still_of_a_proxy_is_judged_on_one_thread() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let _one_at_a_time = PROXIES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let was = kerf_core::cpu_percent();
    let opts = ExportOptions::default();
    let mut tally = Tally::new();
    let mut made = Vec::new();
    for (percent, range) in budgets() {
        kerf_core::set_cpu_percent(percent);
        for (w, h) in [
            (12u32, 16u32),
            (16, 16),
            (24, 32),
            (40, 4),
            (40, 6),
            (64, 6),
            (34, 20),
            (16, 64),
        ] {
            let original = noise_clip(w, h, "yuv420p");
            let (proxied, files) = proxy_of(&original);
            made.extend(files);
            let video = proxied.streams.iter().find(|s| s.kind == StreamKind::Video).expect("video");
            assert_eq!(
                (video.width, video.height),
                (Some(w), Some(h)),
                "the proxy is the size of its small source"
            );
            let tl = one_track(vec![blurred(&original, 3.0, 0.2)], None);
            let case = format!("still proxy budget {percent}% {range:?} {w}x{h}");
            let planned = Planner::new(
                &tl,
                std::slice::from_ref(&original),
                &opts,
                PlanRequest::still(kerf_core::composite_color_policy()).with_media(&kerf_core::ProxyMedia),
            )
            .expect("planner")
            .at(0.05)
            .expect("plan");
            assert!(
                planned.layers.iter().all(|l| l.source.proxy),
                "{case}: the plan reads the proxy"
            );
            judge_still(
                &case,
                &tl,
                (std::slice::from_ref(&original), std::slice::from_ref(&proxied)),
                &kerf_core::ProxyMedia,
                &opts,
                0.05,
                &mut tally,
            );
        }
    }
    kerf_core::set_cpu_percent(was);
    for f in made {
        let _ = std::fs::remove_file(f);
    }
    eprintln!(
        "CHECKER-P still of a proxy: {} drawn exact, {} refused ({} exact at the thread count the still runs on)",
        tally.exact,
        tally.refused,
        tally.band.len()
    );
    assert!(tally.exact > 0 && tally.refused > 0);
    assert!(tally.failures.is_empty(), "{:#?}", tally.failures);
    assert!(
        tally.unnecessary.is_empty() && tally.band.is_empty(),
        "refusals the draw does not need (a proxy's still runs on one thread: nothing is band): {:#?} {:#?}",
        tally.unnecessary,
        tally.band
    );
}

static PROXIES: Mutex<()> = Mutex::new(());

/// `a` as the preview decodes it: its proxy made by the product's own `generate_proxy`, and the stream the proxy's sidecar describes (the
/// cache is the user's: the files are returned to be removed).
fn proxy_of(a: &Asset) -> (Asset, Vec<PathBuf>) {
    let proxy = kerf_core::generate_proxy(Path::new(&a.path), kerf_core::proxy_width(a.projection()))
        .unwrap_or_else(|e| panic!("proxy of {}: {e}", a.path));
    let media = kerf_core::MediaResolver::resolve(&kerf_core::ProxyMedia, a);
    assert!(
        media.proxy && Path::new(&media.path) == proxy,
        "{}: the preview does not resolve to the proxy that was just made",
        a.path
    );
    let mut proxied = a.clone();
    proxied.path = media.path;
    let video = media.video.expect("the proxy has a video stream");
    *proxied
        .streams
        .iter_mut()
        .find(|s| s.kind == StreamKind::Video)
        .expect("a video stream") = video;
    let sidecar = proxy.with_extension("json");
    (proxied, vec![proxy, sidecar])
}

/// A blur on a source that is not 8-bit 4:2:0. The chain is `... scale, pad, setsar, fps, trim, gblur, format=yuv420p`: `gblur` and the
/// filters around it share one pixel-format class, so the `scale` ahead of them is where FFmpeg converts to the `yuv420p` the last filter
/// asks for, and `gblur` runs on 4:2:0 planes whatever the file was. The rule judges luma and half-size chroma; this holds the plan to
/// that for the shapes where the *source's own* chroma plane would be a hazard under one slice thread (4:2:2: `cw x h`, 4:1:1: `w/4 x h`)
/// and the 4:2:0 ones are not. Whatever the plan clears must be the still's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p_pipeline_other_formats_a_cleared_blur_is_the_stills() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let was = kerf_core::cpu_percent();
    let opts = ExportOptions::default();
    let mut tally = Tally::new();
    for percent in [10u8, 75] {
        kerf_core::set_cpu_percent(percent);
        for pix_fmt in [
            "yuv444p",
            "yuv422p",
            "yuv440p",
            "yuv411p",
            "gray",
            "yuv420p10le",
            "yuv444p10le",
            "yuv422p10le",
        ] {
            // (32x16..30: a 4:2:2 chroma plane of 16 wide and 16 rows or more, where the 4:2:0 one has 8 to 15 rows; 64x16..32: a 4:1:1
            // one 16 wide, where the 4:2:0 one is 32 wide; the first four are the ones with nothing special.)
            for (w, h) in [
                (64, 64),
                (96, 48),
                (66, 34),
                (34, 66),
                (32, 16),
                (32, 24),
                (32, 30),
                (64, 16),
                (64, 32),
                (16, 8),
                (16, 32),
            ] {
                let asset = noise_clip(w, h, pix_fmt);
                for sigma in [1.5, 6.0] {
                    let tl = one_track(vec![blurred(&asset, sigma, 0.2)], None);
                    let case = format!("other format {pix_fmt} budget {percent}% {w}x{h} sigma {sigma}");
                    judge_still(
                        &case,
                        &tl,
                        (std::slice::from_ref(&asset), std::slice::from_ref(&asset)),
                        &kerf_core::OriginalMedia,
                        &opts,
                        0.05,
                        &mut tally,
                    );
                }
            }
        }
    }
    kerf_core::set_cpu_percent(was);
    eprintln!(
        "CHECKER-P other formats: {} drawn exact, {} refused for the blur's planes, {} refused for another reason",
        tally.exact, tally.refused, tally.other
    );
    assert!(tally.exact > 0 && tally.refused > 0);
    assert!(tally.failures.is_empty(), "{:#?}", tally.failures);
}

/// FFmpeg's still at `t` as its 4:4:4 planes, `width` wide.
fn still_reference(tl: &Timeline, read: &[Asset], opts: &ExportOptions, t: f64, width: u32) -> Vec<u8> {
    kerf_core::timeline_frame_yuv_cancellable(tl, read, opts, t, width, &|| false)
        .expect("the FFmpeg still")
        .2
}

/// A clip of `lavfi`'s picture, 8-bit 4:2:0, lossless, 0.4 s.
fn lavfi_clip(name: &str, lavfi: &str) -> Asset {
    let path = scratch("checker-p-pipeline").join(format!("{name}.mkv"));
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

/// A chroma key after a scale, over noise, is on screen (the keyed layer is in the plan, and it is not the bars alone: FFmpeg's still
/// differs from the still without it), is the GPU's to draw where the scaler is measured (`caps.exact_scaler`) and is FFmpeg's where it
/// is not (the refusal names the key after a scale), and where drawn is the still's to the sample. The shapes are `parity.rs`'s key cases:
/// shrunk by a zoom, and enlarged (a 640x360 clip into 960x540 and 1280x720 under Cover and Contain, and shrunk back after the fit), with
/// the key at every position among the other effects.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p_pipeline_a_keyed_noise_layer_is_on_screen_and_drawn_exactly() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    use VideoEffect as E;
    let opts = ExportOptions::default();
    let comp = compositor(None);
    assert!(
        comp.caps().exact_scaler,
        "the control: this FFmpeg's scaler is the model's, so a key after a scale is the GPU's"
    );
    let mut not_exact = comp.caps();
    not_exact.exact_scaler = false;
    let bars = lavfi_clip(
        "keyed-bars-640x360",
        "testsrc2=size=640x360:rate=30:duration=0.4,format=yuv420p",
    );
    let noise = noise_clip(640, 360, "yuv420p");
    let key = |color: &str, similarity: f64, blend: f64| E::ChromaKey {
        color: color.into(),
        similarity,
        blend,
    };
    let mut cases: Vec<(String, Option<Delivery>, f64, Vec<E>)> = Vec::new();
    for scale in [0.6, 0.4985, 0.9] {
        for (color, sim, blend) in [
            ("0x808080", 0.25, 0.2),
            ("green", 0.15, 0.1),
            ("0x808080", 0.00001, 0.0),
            ("0x808080", 1.0, 1.0),
        ] {
            cases.push((
                format!("scaled {scale}, key {color} {sim} {blend}"),
                None,
                scale,
                vec![key(color, sim, blend)],
            ));
        }
    }
    for (frame, scale) in [
        (Delivery::new(960, 540, Fit::Cover), 1.0),
        (Delivery::new(1280, 720, Fit::Cover), 1.0),
        (Delivery::new(960, 540, Fit::Contain), 1.0),
        (Delivery::new(960, 540, Fit::Contain), 0.9),
    ] {
        for (name, effects) in [
            ("alone", vec![]),
            ("blur", vec![E::Blur { sigma: 2.0 }]),
            ("sharpen", vec![E::Sharpen { amount: 1.5 }]),
            ("invert, grayscale", vec![E::Invert, E::Grayscale]),
        ] {
            for at in 0..=effects.len() {
                let mut list = effects.clone();
                list.insert(at, key("0x808080", 0.25, 0.2));
                cases.push((
                    format!(
                        "enlarged to {}x{} {:?} x{scale}, {name}, key at {at}",
                        frame.width, frame.height, frame.fit
                    ),
                    Some(frame),
                    scale,
                    list,
                ));
            }
        }
    }
    let (mut drawn, mut failures) = (0, Vec::new());
    for (name, delivery, scale, effects) in cases {
        let mut fg = Clip::new(noise.id, 0.0, 0.3, 0.0);
        fg.transform.scale = scale;
        fg.effects = effects;
        let under = Clip::new(bars.id, 0.0, 0.3, 0.0);
        let assets = [bars.clone(), noise.clone()];
        let tl = Timeline {
            tracks: vec![
                Track {
                    clips: vec![under.clone()],
                    ..Track::new(StreamKind::Video, "V1".to_string())
                },
                Track {
                    clips: vec![fg],
                    ..Track::new(StreamKind::Video, "V2".to_string())
                },
            ],
            format: delivery,
            ..Timeline::new()
        };
        let alone = Timeline {
            tracks: vec![tl.tracks[0].clone()],
            format: delivery,
            ..Timeline::new()
        };
        let t = 0.1;
        let plan = RenderPlan::at(&tl, &assets, &opts, t, kerf_core::composite_color_policy()).expect("plan");
        let size = plan.size(u32::MAX);
        if plan.layers.len() != 2 {
            failures.push(format!(
                "{name}: the keyed layer is not in the plan at {t} s ({} layers)",
                plan.layers.len()
            ));
            continue;
        }
        let why = plan.reasons(&comp.caps(), size);
        if !why.is_empty() {
            failures.push(format!("{name}: the GPU does not draw it: {why:?}"));
            continue;
        }
        let scaled = plan.layers[1].transform.scale != 1.0 || delivery.is_some();
        let without = plan.reasons(&not_exact, size);
        let named = without
            .iter()
            .any(|u| matches!(u, Unsupported::Effects(_, r) if r.starts_with("a chroma key after a scale")));
        if scaled && !named && delivery.is_none() {
            failures.push(format!(
                "{name}: a scaler that is not measured does not refuse the key after the scale: {without:?}"
            ));
        }
        // On screen: the layer without its key is not the bars alone (the case is not a plain frame of bars), and the key does something to
        // it (on noise a key near the middle of the chroma square, behind a blur or a grayscale, leaves little of the picture; none of it
        // is a key that does nothing).
        let full = still_reference(&tl, &assets, &opts, t, size.0);
        let bare = still_reference(&alone, &assets[..1], &opts, t, size.0);
        let mut unkeyed_tl = tl.clone();
        unkeyed_tl.tracks[1].clips[0]
            .effects
            .retain(|e| !matches!(e, E::ChromaKey { .. }));
        let unkeyed = still_reference(&unkeyed_tl, &assets, &opts, t, size.0);
        let n = (size.0 * size.1) as usize;
        let count = |a: &[u8], b: &[u8]| a[..n].iter().zip(&b[..n]).filter(|(x, y)| x != y).count();
        let (layer, keyed_away) = (count(&unkeyed, &bare), count(&full, &unkeyed));
        if layer < n / 10 {
            failures.push(format!(
                "{name}: the layer is hardly on screen: {layer} of {n} luma samples differ from the bars alone"
            ));
            continue;
        }
        // (A similarity of 0.00001 keys out what is within it of the key colour: on noise, next to nothing.)
        if keyed_away == 0 && !name.contains("0.00001") {
            failures.push(format!("{name}: the key does nothing to the layer"));
            continue;
        }
        let g = still_gaps(&plan, &tl, &assets, &opts, t, &comp);
        if g.iter().all(Option::is_none) {
            drawn += 1;
            eprintln!("CHECKER-P keyed {name}: drawn, exact ({layer} of {n} luma samples are the layer's, the key changes {keyed_away})");
        } else {
            failures.push(format!("{name}: differs from the still: {g:?}"));
        }
    }
    eprintln!("CHECKER-P keyed: {drawn} drawn exact");
    assert!(drawn > 20 && failures.is_empty(), "{drawn} drawn; {failures:#?}");
}

/// An export frame of a non-4:2:0 source, at a budget of one slice thread (the shapes where the *source's* chroma plane would be a hazard
/// and the 4:2:0 one is not: `gblur` runs on 4:2:0 behind the chain's first `scale`): whatever the plan clears is the export's.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_p_pipeline_export_of_other_formats_at_one_thread_is_the_exports() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let was = kerf_core::cpu_percent();
    kerf_core::set_cpu_percent(10);
    let mut tally = Tally::new();
    for pix_fmt in ["yuv422p", "yuv411p", "yuv444p10le", "gray"] {
        for (w, h) in [(32u32, 16u32), (32, 30), (64, 16), (16, 32), (64, 64)] {
            let asset = noise_clip(w, h, pix_fmt);
            let tl = one_track(vec![blurred(&asset, 3.0, 0.2)], None);
            let case = format!("export {pix_fmt} at one thread {w}x{h}");
            let out = motion_frame(
                &format!("p-fmt-{pix_fmt}-{w}x{h}"),
                &tl,
                std::slice::from_ref(&asset),
                30.0,
                2,
            );
            tally.take(&case, out);
        }
    }
    kerf_core::set_cpu_percent(was);
    eprintln!(
        "CHECKER-P export other formats: {} drawn exact, {} refused",
        tally.exact, tally.refused
    );
    assert!(tally.exact > 0 && tally.refused > 0);
    assert!(tally.failures.is_empty(), "{:#?}", tally.failures);
    assert!(tally.unnecessary.is_empty(), "{:#?}", tally.unnecessary);
}
