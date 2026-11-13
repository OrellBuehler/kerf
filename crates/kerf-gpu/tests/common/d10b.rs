#![allow(dead_code)]

//! The independent checker's helpers for the 10-bit deliveries (`checker_d10b_*`): media, cuts, the export's planes (the engine's own argv with
//! its encoder swapped for the raw pipe, or an argv as the engine spawned it), and the exact comparison -- 0 differing samples in Y, U and V of every frame
//! the plan draws, a count of the frames it refuses, and a montage of the first frame that differs.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use kerf_core::{
    Asset, Clip, CompositeColorPolicy, Container, Delivery, ExportOptions, Fit, GpuCaps, PlanRequest, Planner, Project,
    RateControl, StreamKind, TenBitLayout, TextOverlay, Timeline, Track, Unsupported,
};
use kerf_gpu::delivery10::{self, Planes10};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuOptions, MotionFrames};

pub fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

/// The measurements a 10-bit frame is judged by, made once, with the FFmpeg in the environment.
pub fn measured() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = kerf_gpu::scaler::kernels();
        let _ = kerf_gpu::text::model();
        let verdict = delivery10::ten_bit();
        eprintln!(
            "this FFmpeg's 10-bit export: {verdict:?}, {:?} {:?}",
            delivery10::measured_kinds(),
            delivery10::details()
        );
    });
}

/// A compositor whose graph has `threads` slice threads (`Some`), or the plan's own judgement of them (`None`).
pub fn compositor(threads: Option<u32>) -> &'static Compositor {
    static COMPOSITORS: Mutex<Option<HashMap<Option<u32>, &'static Compositor>>> = Mutex::new(None);
    measured();
    let mut held = COMPOSITORS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let held = held.get_or_insert_with(HashMap::new);
    held.entry(threads).or_insert_with(|| {
        let c = Compositor::new(gpu()).expect("a compositor");
        Box::leak(Box::new(match threads {
            Some(n) => c.with_graph_threads(n),
            None => c,
        }))
    })
}

pub fn frame_source() -> Arc<FrameSource> {
    static S: OnceLock<Arc<FrameSource>> = OnceLock::new();
    S.get_or_init(|| FrameSource::new(FrameSourceConfig::default())).clone()
}

pub fn policy() -> CompositeColorPolicy {
    let measured = kerf_core::composite_color_policy();
    assert_ne!(
        measured,
        CompositeColorPolicy::Unknown,
        "the composite colour probe could not read this FFmpeg ({})",
        kerf_core::ffmpeg_path()
    );
    measured
}

pub fn build() -> &'static str {
    crate::common::build_key()
}

// ---- media ----------------------------------------------------------------------------------

const NOISE: &str = "lum='mod(X*X*37+Y*Y*91+X*Y*53+X*11+Y*29+N*5,256)':cb='mod(X*X*13+Y*Y*71+X*Y*29+X*7+Y*3+N*3,256)':cr='mod(X*X*53+Y*Y*19+X*Y*97+X*5+Y*17+N*7,256)'";
const UNTAGGED: &str = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown:range=unknown";

/// One synthesized file: `kind` is a lavfi source name (`noise`, `testsrc2`, `bars`), `fmt` a pixel format, `tags` extra `setparams` fields
/// (`colorspace=bt709:range=pc`...). Kept per FFmpeg build; lossless (FFV1 in Matroska, which both builds decode and keep the format of).
pub fn media(name: &str, kind: &str, (w, h): (u32, u32), rate: &str, secs: f64, fmt: &str, tags: Option<&str>) -> Asset {
    let dir = crate::common::media_dir("d10b-media");
    let path: PathBuf = dir.join(format!("{}.mkv", name.replace('/', "o")));
    if !path.exists() {
        let src = match kind {
            "noise" => format!("nullsrc=size={w}x{h}:rate={rate}:duration={secs},format=yuv420p,geq={NOISE}"),
            "testsrc2" => format!("testsrc2=size={w}x{h}:rate={rate}:duration={secs}"),
            "bars" => format!("smptehdbars=size={w}x{h}:rate={rate}:duration={secs}"),
            other => panic!("no source {other}"),
        };
        let tag = match tags {
            Some(t) => format!("setparams={t}"),
            // (A `yuvj` picture is full range by its format; the flag is left alone.)
            None if fmt.starts_with("yuvj") => {
                "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown".to_string()
            }
            None => UNTAGGED.to_string(),
        };
        let graph = format!("{src},format={fmt},{tag}");
        crate::common::ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &graph,
            "-c:v",
            "ffv1",
            "-pix_fmt",
            fmt,
            path.to_str().unwrap(),
        ]);
    }
    Project::probe_asset(&path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()))
}

pub fn timeline(tracks: Vec<Vec<Clip>>, overlays: Vec<TextOverlay>, canvas: (u32, u32), fit: Fit, fps: f64) -> Timeline {
    Timeline {
        tracks: tracks
            .into_iter()
            .enumerate()
            .map(|(i, clips)| Track {
                clips,
                ..Track::new(StreamKind::Video, format!("V{}", i + 1))
            })
            .collect(),
        overlays,
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Delivery::new(canvas.0, canvas.1, fit).with_fps(Some(fps)).ok(),
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

pub fn clip(a: &Asset, source_in: f64, dur: f64, start: f64) -> Clip {
    Clip::new(a.id, source_in, source_in + dur, start)
}

pub fn options(layout: TenBitLayout, fps: f64) -> ExportOptions {
    ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        fps: Some(fps),
        pix_fmt: Some(layout.pix_fmt().into()),
        ..ExportOptions::default()
    }
}

pub const LAYOUTS: [TenBitLayout; 2] = [TenBitLayout::P420, TenBitLayout::P422];

// ---- the export's planes --------------------------------------------------------------------

/// An argv the engine built (`export_args`, or the one it spawned, `-progress` and thread caps and all) turned into a run that writes the graph's
/// terminal planes to the pipe: its encoder, its rate control and its sink removed. `threads` adds `-filter_complex_threads` ahead of everything
/// (`None`: leave the argv's own); `extra` goes at the head (`-cpucount`).
pub fn raw_run(args: &[String], threads: Option<u32>, extra: &[&str], upto: u64) -> Vec<String> {
    let mut run: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "error".into(), "-nostdin".into()];
    run.extend(extra.iter().map(|s| (*s).to_string()));
    if let Some(n) = threads {
        run.extend(["-filter_complex_threads".into(), n.to_string()]);
    }
    let mut it = args.iter().cloned().peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-hide_banner" | "-nostats" => {}
            "-progress" | "-stats_period" | "-c:v" | "-crf" | "-preset" => {
                it.next();
            }
            _ if it.peek().is_none() => {}
            _ => run.push(arg),
        }
    }
    run.extend([
        "-frames:v".into(),
        (upto + 1).to_string(),
        "-f".into(),
        "rawvideo".into(),
        "pipe:1".into(),
    ]);
    run
}

/// The frames `keep` says yes to of a run that writes raw frames of `frame_bytes` to its stdout.
pub fn run_frames(run: &[String], frame_bytes: usize, keep: &dyn Fn(u64) -> bool) -> BTreeMap<u64, Vec<u8>> {
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args(run)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stderr = child.stderr.take().expect("piped");
    let log = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let mut stdout = child.stdout.take().expect("piped");
    let mut out = BTreeMap::new();
    let mut k = 0u64;
    loop {
        let mut frame = vec![0u8; frame_bytes];
        if stdout.read_exact(&mut frame).is_err() {
            break;
        }
        if keep(k) {
            out.insert(k, frame);
        }
        k += 1;
    }
    let status = child.wait().expect("wait for ffmpeg");
    let log = log.join().unwrap_or_default();
    assert!(status.success(), "ffmpeg {run:?}: {log}");
    out
}

pub fn samples(bytes: &[u8]) -> Vec<u16> {
    bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect()
}

pub fn frame_samples(layout: TenBitLayout, (w, h): (usize, usize)) -> usize {
    w * h + 2 * (w / 2) * if layout == TenBitLayout::P422 { h } else { h / 2 }
}

// ---- the comparison -------------------------------------------------------------------------

/// What a case found: frames drawn and compared, frames the plan refused (with the reasons' kinds), samples compared, and every difference.
#[derive(Default, Debug)]
pub struct Stats {
    pub drawn: usize,
    pub refused: usize,
    pub kinds: BTreeMap<String, usize>,
    pub compared: usize,
    pub differing: usize,
    pub max_diff: u32,
    /// (frame, plane, count, max, first (x, y), montage)
    pub failures: Vec<String>,
}

impl Stats {
    pub fn add(&mut self, other: Stats) {
        self.drawn += other.drawn;
        self.refused += other.refused;
        self.compared += other.compared;
        self.differing += other.differing;
        self.max_diff = self.max_diff.max(other.max_diff);
        for (k, v) in other.kinds {
            *self.kinds.entry(k).or_default() += v;
        }
        self.failures.extend(other.failures);
    }
}

/// How a frame is exported and which compositor draws it.
pub struct Run<'a> {
    pub case: &'a str,
    pub tl: &'a Timeline,
    pub assets: &'a [Asset],
    pub layout: TenBitLayout,
    pub opts: ExportOptions,
    /// `Some(n)`: both the export (`-filter_complex_threads`) and the compositor are at `n` slice threads. `None`: neither is told.
    pub threads: Option<u32>,
    /// An argv to run instead of the engine's own (as it was spawned), and extra flags at the head.
    pub argv: Option<Vec<String>>,
    pub extra: Vec<&'a str>,
    pub frames: Vec<Range<u64>>,
    pub allow_refused: bool,
    /// The export's frames already made (by [`real_export`]); `None`: run the engine's argv with the encoder swapped for the raw pipe.
    pub exported: Option<BTreeMap<u64, Vec<u8>>>,
}

pub fn target_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).parent().expect("target").to_path_buf()
}

fn slug(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// Y, U and V of the export and the compositor side by side with the difference (x8), as one PNG in `target/parity/d10b/`.
pub fn montage10(case: &str, k: u64, layout: TenBitLayout, (w, h): (usize, usize), want: &[u16], got: &Planes10) -> String {
    let fmt = layout.pix_fmt();
    let mut got_raw = Vec::new();
    for p in [&got.y, &got.u, &got.v] {
        for v in &p.data {
            got_raw.extend_from_slice(&v.to_le_bytes());
        }
    }
    let mut want_raw = Vec::new();
    for v in want {
        want_raw.extend_from_slice(&v.to_le_bytes());
    }
    let rgb = |raw: &[u8]| -> Vec<u8> {
        let size = format!("{w}x{h}");
        let mut child = Command::new(kerf_core::ffmpeg_path())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "rawvideo",
                "-pix_fmt",
                fmt,
                "-s",
                &size,
                "-i",
                "pipe:0",
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("ffmpeg");
        let mut stdin = child.stdin.take().expect("stdin");
        let raw = raw.to_vec();
        let feeder = std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(&raw);
        });
        let mut out = Vec::new();
        child.stdout.take().expect("stdout").read_to_end(&mut out).expect("read");
        let _ = child.wait();
        let _ = feeder.join();
        out
    };
    let (a, b) = (rgb(&want_raw), rgb(&got_raw));
    if a.len() != w * h * 3 || b.len() != w * h * 3 {
        return "(no montage)".into();
    }
    let mut strip = Vec::with_capacity(w * h * 9);
    for row in 0..h {
        strip.extend_from_slice(&a[row * w * 3..(row + 1) * w * 3]);
        strip.extend_from_slice(&b[row * w * 3..(row + 1) * w * 3]);
        for i in 0..w {
            let d = |c: usize| (i32::from(a[(row * w + i) * 3 + c]) - i32::from(b[(row * w + i) * 3 + c])).unsigned_abs();
            let m = (d(0).max(d(1)).max(d(2)) * 8).min(255) as u8;
            strip.extend_from_slice(&[m, m, m]);
        }
    }
    let dir = target_dir().join("parity").join("d10b");
    std::fs::create_dir_all(&dir).expect("montage dir");
    let path = dir.join(format!("{}-{}-{}-f{k:03}.png", slug(case), build(), fmt));
    let size = format!("{}x{h}", w * 3);
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-s",
            &size,
            "-i",
            "pipe:0",
            path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("ffmpeg");
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("stdin");
        let _ = stdin.write_all(&strip);
    }
    let _ = child.wait();
    path.display().to_string()
}

/// Run the case: the export's planes against the compositor's, frame by frame. A frame the plan refuses is counted (and its reasons' kinds);
/// a frame it draws is held to the export with no tolerance. Never panics on a difference: the caller asserts on [`Stats`].
pub fn run(r: &Run) -> Stats {
    measured();
    let planner = Planner::new(r.tl, r.assets, &r.opts, PlanRequest::motion(policy()).alone()).expect("planner");
    let first = planner.at_frame(0).expect("plan");
    let (w, h) = (first.canvas.width as usize, first.canvas.height as usize);
    let frame_bytes = frame_samples(r.layout, (w, h)) * 2;
    let upto = r.frames.iter().map(|x| x.end).max().unwrap_or(1) - 1;
    let wanted = |k: u64| r.frames.iter().any(|x| x.contains(&k));
    let args = match &r.argv {
        Some(a) => a.clone(),
        None => kerf_core::export_args(r.tl, r.assets, "OUT", &r.opts).expect("the export's argv"),
    };
    let exported = match &r.exported {
        Some(e) => e.clone(),
        None => {
            let run = raw_run(&args, r.threads, &r.extra, upto);
            run_frames(&run, frame_bytes, &wanted)
        }
    };
    let comp = compositor(r.threads);
    let caps = comp.caps();
    let mut stats = Stats::default();
    let have = exported.keys().max().map_or(0, |m| m + 1);
    for frames in &r.frames {
        let mut motion = MotionFrames::new(frame_source());
        for k in frames.clone() {
            if k >= have {
                // (Past the end of the cut: the export has no such frame.)
                *stats.kinds.entry("past_the_end_of_the_cut".to_string()).or_default() += 1;
                continue;
            }
            let plan = planner.at_frame(k).expect("plan");
            let size = (plan.canvas.width, plan.canvas.height);
            let why = plan.reasons(&caps, size);
            if !why.is_empty() {
                stats.refused += 1;
                for u in &why {
                    *stats.kinds.entry(u.kind().to_string()).or_default() += 1;
                }
                continue;
            }
            let Some(bytes) = exported.get(&k) else {
                stats.failures.push(format!("the export has no frame {k}"));
                stats.differing += 1;
                continue;
            };
            let want = samples(bytes);
            let plan = plan.for_decode(&caps, size);
            let picture = match motion.frames(&plan) {
                Ok(p) => p,
                Err(e) => {
                    stats.failures.push(format!("frame {k}: decode: {e}"));
                    stats.differing += 1;
                    continue;
                }
            };
            let drawn = match comp.composite_yuv10_counted(&plan, &picture, motion.counts(), size) {
                Ok(d) => d,
                Err(e) => {
                    stats
                        .failures
                        .push(format!("frame {k}: the plan draws it but the compositor refuses: {e}"));
                    stats.differing += 1;
                    continue;
                }
            };
            stats.drawn += 1;
            let mut at = 0;
            let mut shown = false;
            for (name, plane) in [("Y", &drawn.y), ("U", &drawn.u), ("V", &drawn.v)] {
                let reference = &want[at..at + plane.data.len()];
                let apart = plane.data.iter().zip(reference).filter(|(a, b)| a != b).count();
                stats.compared += plane.data.len();
                if apart != 0 {
                    let first = plane.data.iter().zip(reference).position(|(a, b)| a != b).unwrap_or(0);
                    let max = plane
                        .data
                        .iter()
                        .zip(reference)
                        .map(|(a, b)| u32::from(a.abs_diff(*b)))
                        .max()
                        .unwrap_or(0);
                    stats.differing += apart;
                    stats.max_diff = stats.max_diff.max(max);
                    let m = if shown {
                        String::new()
                    } else {
                        shown = true;
                        format!(" montage {}", montage10(r.case, k, r.layout, (w, h), &want, &drawn))
                    };
                    stats.failures.push(format!(
                        "{} {} frame {k} plane {name}: {apart} of {} samples differ, max {max}, first at ({}, {}): the compositor's {}, FFmpeg's {}{m}",
                        r.case,
                        r.layout.pix_fmt(),
                        plane.data.len(),
                        first % plane.w,
                        first / plane.w,
                        plane.data[first],
                        reference[first]
                    ));
                }
                at += plane.data.len();
            }
        }
    }
    stats
}

/// Report a case's [`Stats`] on stderr, one line.
pub fn report(case: &str, layout: TenBitLayout, threads: Option<u32>, s: &Stats) {
    eprintln!(
        "{case:<52} {} threads {:>4} | drawn {:>3} refused {:>3} | {:>10} samples, {} differ (max {}){}",
        layout.pix_fmt(),
        threads.map_or("auto".to_string(), |n| n.to_string()),
        s.drawn,
        s.refused,
        s.compared,
        s.differing,
        s.max_diff,
        if s.kinds.is_empty() {
            String::new()
        } else {
            format!(" | refused as {:?}", s.kinds)
        }
    );
    for f in &s.failures {
        eprintln!("   FAIL {f}");
    }
}

pub fn caps_of(threads: Option<u32>) -> GpuCaps {
    compositor(threads).caps()
}

/// The reasons the plan gives at frame `k`.
pub fn reasons(
    tl: &Timeline,
    assets: &[Asset],
    layout: TenBitLayout,
    fps: f64,
    threads: Option<u32>,
    k: u64,
) -> Vec<Unsupported> {
    measured();
    let opts = options(layout, fps);
    let planner = Planner::new(tl, assets, &opts, PlanRequest::motion(policy()).alone()).expect("planner");
    let plan = planner.at_frame(k).expect("plan");
    plan.reasons(&caps_of(threads), (plan.canvas.width, plan.canvas.height))
}

/// The export of `tl` as the product makes it: `render_with` (its lease, its thread caps, its parts and the pass that joins them) to a lossless 10-bit
/// VP9 file (`-lossless 1`: the one lossless 10-bit encoder the engine's list has), decoded to the delivery's raw planes. `env` is set for the call only.
pub fn real_export(
    tl: &Timeline,
    assets: &[Asset],
    layout: TenBitLayout,
    fps: f64,
    env: &[(&str, &str)],
) -> BTreeMap<u64, Vec<u8>> {
    let dir = crate::common::media_dir("d10b-real");
    let out = dir.join(format!("real-{}.mkv", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let opts = ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libvpx-vp9".into()),
        rate_control: RateControl::Lossless,
        fps: Some(fps),
        pix_fmt: Some(layout.pix_fmt().into()),
        include_audio: false,
        ..ExportOptions::default()
    };
    let saved: Vec<_> = env.iter().map(|(k, _)| (*k, std::env::var_os(k))).collect();
    for (k, v) in env {
        std::env::set_var(k, v);
    }
    let result = kerf_core::render_with(tl, assets, &out, &opts);
    for (k, v) in saved {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
    result.unwrap_or_else(|e| panic!("the export: {e}"));
    let first = Planner::new(tl, assets, &opts, PlanRequest::motion(policy()).alone())
        .expect("planner")
        .at_frame(0)
        .expect("plan");
    let frame_bytes = frame_samples(layout, (first.canvas.width as usize, first.canvas.height as usize)) * 2;
    let run: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-i",
        out.to_str().unwrap(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        layout.pix_fmt(),
        "pipe:1",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let frames = run_frames(&run, frame_bytes, &|_| true);
    let _ = std::fs::remove_file(&out);
    frames
}
